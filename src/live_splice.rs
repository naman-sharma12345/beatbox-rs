//! Live edits during playback: work out which bars an edit touched, render
//! only those bars (old and new, the same window) and splice the difference
//! into the mix that is already playing. Outside the window the result is the
//! playing mix bit for bit; inside it the edit is heard right away, and the
//! player swaps the buffer at its next block boundary with a short crossfade
//! (`gui::player`). A full render still follows in the background and swaps
//! in the same way, so meters and the master bus settle exactly.

use crate::project::Project;
use crate::render::{self, Mix, RenderOptions};
use crate::samples::SampleBank;
use anyhow::Result;

/// What an edit changed, as far as playback is concerned.
#[derive(Clone, Debug, PartialEq)]
pub enum Dirty {
    /// Nothing audible changed.
    Nothing,
    /// Only notes moved, inside this step range [from, to) of the song.
    Steps(u32, u32),
    /// Something global changed (a mixer move, an effect, the song's
    /// length or tempo): re-render the whole song.
    Everything,
}

/// Bars of context rendered before and after the touched steps, so effects
/// and compressors have settled in both windows before the edit sounds.
pub const CONTEXT_BARS: u32 = 1;

/// How far below the input an effect's ring-out must fall before the
/// window may end: 1e-9 (-180 dB), the splice's exactness target.
const TAIL_FLOOR: f32 = 1e-9;

fn repeats_to_floor(feedback: f32) -> f32 {
    let fb = feedback.abs().min(0.999);
    if fb <= 1e-6 {
        1.0
    } else {
        TAIL_FLOOR.ln() / fb.ln()
    }
}

/// Seconds an effect keeps ringing after its input stops, down to
/// `TAIL_FLOOR`: feedback delays and reverbs (0 for everything else, whose
/// memory is shorter than the window's 1.5 s tail).
pub fn fx_tail_secs(fx: &crate::fx::Effect, step_secs: f32) -> f32 {
    use crate::fx::{Effect, ReverbMode};
    let sr = crate::dsp::SR;
    let t = match fx {
        Effect::Delay(d) if !d.bypass => repeats_to_floor(d.feedback.clamp(0.0, 0.95)) * d.steps.max(0.0) * step_secs,
        Effect::ModDelay(d) if !d.bypass => repeats_to_floor(d.feedback) * d.time_ms.max(0.0) / 1000.0,
        Effect::Flanger(d) if !d.bypass => repeats_to_floor(d.feedback) * (d.delay_ms + d.depth_ms).max(0.0) / 1000.0,
        Effect::Reverb(r) if !r.bypass => {
            let rt60 = match r.mode {
                // the longest comb (1617 + 23 samples) at its feedback
                ReverbMode::Freeverb => repeats_to_floor(0.7 + r.size.clamp(0.0, 1.0) * 0.28) * 1640.0 / sr / 3.0,
                m => {
                    let d = match m {
                        ReverbMode::FdnPlate => 2.5,
                        ReverbMode::FdnHall => 3.5,
                        _ => 1.2,
                    };
                    if r.decay_s > 0.0 {
                        r.decay_s
                    } else {
                        d * (0.4 + 1.2 * r.size.clamp(0.0, 1.0))
                    }
                }
            };
            // -60 dB per RT60, so -180 dB takes three
            3.0 * rt60 + r.predelay_ms.max(0.0) / 1000.0
        }
        Effect::Convolution(c) if !c.bypass => 3.0 * c.decay_s.max(0.0) + c.predelay_ms.max(0.0) / 1000.0,
        _ => 0.0,
    };
    if t.is_finite() {
        t.max(0.0)
    } else {
        0.0
    }
}

/// The longest ring-out an edit can have in this project: the master
/// chain's tails, plus the longest track chain, plus every bus chain
/// (a conservative sum: a send can pass through several buses).
pub fn chain_tail_secs(p: &Project) -> f32 {
    let st = p.step_secs();
    let sum = |fx: &[crate::fx::Effect]| fx.iter().map(|f| fx_tail_secs(f, st)).sum::<f32>();
    let tracks = p.tracks.iter().map(|t| sum(&t.effects)).fold(0.0f32, f32::max);
    let buses: f32 = p.buses.iter().map(|b| sum(&b.effects)).sum();
    sum(&p.master_effects) + tracks + buses
}

fn note_key(e: &render::Event) -> (usize, u32, u32, u32, u32) {
    (e.start, e.gate.to_bits(), e.pitch.to_bits(), e.vel.to_bits(), e.slide_to.map(|s| s.to_bits()).unwrap_or(u32::MAX))
}

/// The steps an edit from `old` to `new` touched. Only note content may
/// differ for a partial answer; anything else is `Everything`.
pub fn dirty_steps(old: &Project, new: &Project) -> Dirty {
    if old == new {
        return Dirty::Nothing;
    }
    // everything but the patterns' notes must be equal
    let shape = |p: &Project| -> Vec<(String, u32, Option<(u32, u32)>)> { p.patterns.iter().map(|q| (q.name.clone(), q.bars, q.meter)).collect() };
    if shape(old) != shape(new) || old.song_steps() != new.song_steps() || !old.tempo_points.is_empty() || !new.tempo_points.is_empty() {
        return Dirty::Everything;
    }
    let (mut a, mut b) = (old.clone(), new.clone());
    a.patterns.clear();
    b.patterns.clear();
    b.id_seq = a.id_seq;
    if a != b {
        return Dirty::Everything;
    }
    let opts = RenderOptions::default();
    let (ea, _) = render::schedule(old, &opts);
    let (eb, _) = render::schedule(new, &opts);
    if ea.len() != eb.len() {
        return Dirty::Everything;
    }
    let step_samples = (old.step_secs() * crate::dsp::SR) as f64;
    let (mut lo, mut hi) = (usize::MAX, 0usize);
    for (ta, tb) in ea.iter().zip(eb.iter()) {
        let mut ka: Vec<_> = ta.iter().map(|e| (note_key(e), e.start, e.gate)).collect();
        let mut kb: Vec<_> = tb.iter().map(|e| (note_key(e), e.start, e.gate)).collect();
        ka.sort_by(|x, y| x.0.cmp(&y.0));
        kb.sort_by(|x, y| x.0.cmp(&y.0));
        // symmetric difference of the two sorted lists
        let (mut i, mut j) = (0, 0);
        let mut mark = |s: usize, g: f32| {
            lo = lo.min(s);
            hi = hi.max(s + (g.max(0.0) * crate::dsp::SR) as usize);
        };
        while i < ka.len() || j < kb.len() {
            match (ka.get(i), kb.get(j)) {
                (Some(x), Some(y)) if x.0 == y.0 => {
                    i += 1;
                    j += 1;
                }
                (Some(x), Some(y)) if x.0 < y.0 => {
                    mark(x.1, x.2);
                    i += 1;
                }
                (Some(_), Some(y)) => {
                    mark(y.1, y.2);
                    j += 1;
                }
                (Some(x), None) => {
                    mark(x.1, x.2);
                    i += 1;
                }
                (None, Some(y)) => {
                    mark(y.1, y.2);
                    j += 1;
                }
                (None, None) => break,
            }
        }
    }
    if lo == usize::MAX {
        return Dirty::Nothing;
    }
    let s0 = (lo as f64 / step_samples).floor() as u32;
    let s1 = ((hi as f64 / step_samples).ceil() as u32).max(s0 + 1);
    Dirty::Steps(s0, s1.min(old.song_steps().max(s0 + 1)))
}

/// The window rendered for a dirty step range: whole bars, with
/// `CONTEXT_BARS` either side, clamped to the song. A note still sounding
/// at the window's start (in either project) would be left out of both
/// window renders, so the window starts at that note's bar instead.
pub fn window(old: &Project, new: &Project, from: u32, to: u32) -> (u32, u32) {
    let bar = crate::project::STEPS_PER_BAR;
    // an echo or reverb that rings past the context bar widens the window
    // by its ring-out (down to TAIL_FLOOR) on both sides:
    // - after the edit, so the edit's own tail is in the window, rendered
    //   with the song's following notes (compressors downstream see the
    //   same music as in a full render);
    // - before it, so the tails of earlier notes are in both windows too:
    //   for linear effects they would cancel in the difference anyway, but
    //   the master compressor and limiter are not linear, and a window that
    //   starts without those tails drives them differently (B40 measured
    //   5e-5 max error with only the end widened)
    let ring = chain_tail_secs(old).max(chain_tail_secs(new));
    let ring_bars = ((ring - RenderOptions::default().tail).max(0.0) / (old.step_secs() * bar as f32)).ceil() as u32;
    let mut w0 = (from / bar).saturating_sub(CONTEXT_BARS + ring_bars) * bar;
    let w1 = ((to + bar - 1) / bar + CONTEXT_BARS + ring_bars) * bar;
    let sps = (old.step_secs() * crate::dsp::SR) as f64;
    let opts = RenderOptions::default();
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for p in [old, new] {
        for t in render::schedule(p, &opts).0 {
            // a beat of release after the gate counts as still sounding
            spans.extend(t.iter().map(|e| (e.start as f64 / sps, (e.start as f64 + e.gate as f64 * crate::dsp::SR as f64) / sps + 4.0)));
        }
    }
    loop {
        let ws = w0 as f64;
        let earliest = spans.iter().filter(|(a, b)| *a < ws && *b > ws).map(|(a, _)| *a).fold(ws, f64::min);
        if earliest >= ws {
            break;
        }
        w0 = (earliest.max(0.0) as u32 / bar) * bar;
    }
    (w0, w1.min(old.song_steps()).max(w0 + 1))
}

/// A live edit that renders the whole song instead of splicing, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Fallback {
    pub reason: String,
    /// The effects' ring-out that widened the window (chain_tail_secs).
    pub ring_secs: f32,
    /// The window, in steps, and its audio with the render tail.
    pub window: (u32, u32),
    pub window_secs: f32,
    pub full_secs: f32,
}

thread_local! {
    static LAST_FALLBACK: std::cell::RefCell<Option<Fallback>> = const { std::cell::RefCell::new(None) };
}

/// The last whole-song fallback `worth_splicing` decided on this thread.
pub fn last_fallback() -> Option<Fallback> {
    LAST_FALLBACK.with(|f| f.borrow().clone())
}

/// True when splicing pays: one window (the old and new windows render at
/// the same time) is at most half the audio of a full render (else the
/// full render is about as quick, and exact). A fallback is logged to
/// stderr with its reason, the ring-out and the window, and kept for
/// `last_fallback`.
pub fn worth_splicing(old: &Project, new: &Project, from: u32, to: u32) -> bool {
    let (w0, w1) = window(old, new, from, to);
    let tail = RenderOptions::default().tail as f64;
    let step = old.step_secs() as f64;
    let splice = (w1 - w0) as f64 * step + tail;
    let full = old.song_steps() as f64 * step + tail;
    let worth = splice * 2.0 <= full;
    if !worth {
        let ring = chain_tail_secs(old).max(chain_tail_secs(new));
        let reason = if ring > tail as f32 {
            format!("effect tails ring {ring:.1} s, widening the window past half the song")
        } else {
            "the edited bars plus context are more than half the song".to_string()
        };
        eprintln!("[live_splice] full render instead of a splice: {reason} (window steps {w0}..{w1} = {splice:.1} s of a {full:.1} s render)");
        let fb = Fallback { reason, ring_secs: ring, window: (w0, w1), window_secs: splice as f32, full_secs: full as f32 };
        LAST_FALLBACK.with(|f| *f.borrow_mut() = Some(fb));
    }
    worth
}

/// `playing` (a full render of `old`) with the edit to `new` spliced in:
/// both projects render only the window around `from..to`, and their
/// difference is added to the playing mix. Returns the new mix and the
/// sample range that changed. Stems are not carried (playback only).
pub fn splice(old: &Project, new: &Project, bank: &SampleBank, playing: &Mix, from: u32, to: u32) -> Result<(Mix, (usize, usize))> {
    let (w0, w1) = window(old, new, from, to);
    let opts = RenderOptions { step_range: Some((w0, w1)), allow_missing_clips: true, ..Default::default() };
    // each note keeps its whole-song round-robin index in the window
    let base = |p: &Project| -> Vec<usize> {
        let at = (w0 as f32 * p.step_secs() * crate::dsp::SR) as usize;
        render::schedule(p, &RenderOptions::default()).0.iter().map(|t| t.iter().filter(|e| e.start < at).count()).collect()
    };
    // the two windows share nothing mutable (each render builds its own
    // voices, effect state and buffers; the project and samples are read
    // only), so they render at the same time
    let (bo, bn) = (base(old), base(new));
    let (ma, mb) = rayon::join(|| render::render_window(old, bank, &opts, &bo), || render::render_window(new, bank, &opts, &bn));
    let (ma, mb) = (ma?, mb?);
    let off = (w0 as f64 * old.step_secs() as f64 * crate::dsp::SR as f64) as usize;
    let mut left = playing.left.clone();
    let mut right = playing.right.clone();
    let n = ma.left.len().min(mb.left.len());
    let (mut first, mut last) = (usize::MAX, 0usize);
    for k in 0..n {
        let i = off + k;
        if i >= left.len() {
            break;
        }
        let dl = mb.left[k] - ma.left[k];
        let dr = mb.right[k] - ma.right[k];
        if dl != 0.0 || dr != 0.0 {
            left[i] += dl;
            right[i] += dr;
            first = first.min(i);
            last = i + 1;
        }
    }
    let changed = if first == usize::MAX { (off, off) } else { (first, last) };
    Ok((Mix { left, right, stems: vec![], track_info: vec![], bus_stems: vec![], seconds: playing.seconds }, changed))
}

/// Largest sample-to-sample step of a stereo signal over [a, b).
pub fn max_step(l: &[f32], r: &[f32], a: usize, b: usize) -> f32 {
    let b = b.min(l.len()).min(r.len());
    let mut m = 0f32;
    for i in (a + 1)..b {
        m = m.max((l[i] - l[i - 1]).abs()).max((r[i] - r[i - 1]).abs());
    }
    m
}

#[cfg(test)]
pub(crate) mod bench {
    //! Timing harness for live re-render (run with `--ignored --nocapture`).
    use super::*;
    use crate::engine::Engine;
    use serde_json::json;
    use std::time::Instant;

    /// The live-playback test song: kick on every downbeat, an epiano
    /// chord per bar, `bars` long, placed on the playlist.
    pub fn song(bars: u32, tag: &str) -> Engine {
        let d = std::env::temp_dir().join(format!("live_{tag}_{}_{bars}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("add_track", &json!({"name": "kick", "preset": "kick"})).unwrap();
        e.call("add_track", &json!({"name": "keys", "preset": "epiano"})).unwrap();
        e.call("add_pattern", &json!({"name": "verse", "bars": bars})).unwrap();
        for st in (0..bars).map(|b| b * 16) {
            e.call("toggle_step", &json!({"track": "kick", "step": st, "pattern": "verse"})).unwrap();
        }
        let chords: Vec<serde_json::Value> = (0..bars).map(|b| json!({"start": b * 16, "len": 12, "pitch": 60, "vel": 0.6})).collect();
        e.call("add_notes", &json!({"track": "keys", "pattern": "verse", "notes": chords})).unwrap();
        e.call("place_pattern", &json!({"pattern": "verse", "start_bar": 0, "lane": 0})).unwrap();
        e
    }

    fn ms(t: Instant) -> f64 {
        t.elapsed().as_secs_f64() * 1000.0
    }

    fn median(mut v: Vec<f64>) -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }

    #[test]
    #[ignore]
    fn profile_live_splice() {
        for bars in [8u32, 16] {
            let mut e = song(bars, "prof");
            let p0 = e.project.clone();
            let opts = RenderOptions::default();
            // first render in the process pays one-time setup
            let t = Instant::now();
            let m0 = render::render(&p0, &e.bank, &opts).unwrap();
            eprintln!("[{bars} bars] first full render (cold): {:.1} ms", ms(t));
            e.call("toggle_step", &json!({"track": "kick", "step": 36, "pattern": "verse"})).unwrap();
            let p1 = e.project.clone();
            let reps = 5;
            let (mut full, mut spl, mut dirty, mut win, mut base, mut wa, mut wb) = (vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
            for _ in 0..reps {
                let t = Instant::now();
                let _ = render::render(&p1, &e.bank, &opts).unwrap();
                full.push(ms(t));
                let t = Instant::now();
                let Dirty::Steps(a, b) = dirty_steps(&p0, &p1) else { panic!() };
                dirty.push(ms(t));
                let t = Instant::now();
                let (w0, w1) = window(&p0, &p1, a, b);
                win.push(ms(t));
                let t = Instant::now();
                let at = (w0 as f32 * p0.step_secs() * crate::dsp::SR) as usize;
                let bo: Vec<usize> = render::schedule(&p0, &opts).0.iter().map(|t| t.iter().filter(|e| e.start < at).count()).collect();
                let bn: Vec<usize> = render::schedule(&p1, &opts).0.iter().map(|t| t.iter().filter(|e| e.start < at).count()).collect();
                base.push(ms(t));
                let wo = RenderOptions { step_range: Some((w0, w1)), allow_missing_clips: true, ..Default::default() };
                let t = Instant::now();
                let _ = render::render_window(&p0, &e.bank, &wo, &bo).unwrap();
                wa.push(ms(t));
                let t = Instant::now();
                let _ = render::render_window(&p1, &e.bank, &wo, &bn).unwrap();
                wb.push(ms(t));
                let t = Instant::now();
                let _ = splice(&p0, &p1, &e.bank, &m0, a, b).unwrap();
                spl.push(ms(t));
            }
            let Dirty::Steps(a, b) = dirty_steps(&p0, &p1) else { panic!() };
            let (w0, w1) = window(&p0, &p1, a, b);
            eprintln!("[{bars} bars] dirty {a}..{b} window {w0}..{w1} ({} bars + {:.1} s tail); medians of {reps}:", (w1 - w0) / 16, opts.tail);
            eprintln!("  full render        {:7.1} ms", median(full));
            eprintln!("  splice() total     {:7.1} ms", median(spl));
            eprintln!("    dirty_steps      {:7.1} ms", median(dirty));
            eprintln!("    window()         {:7.1} ms", median(win));
            eprintln!("    index bases      {:7.1} ms", median(base));
            eprintln!("    window old       {:7.1} ms", median(wa));
            eprintln!("    window new       {:7.1} ms", median(wb));
            // where a window render's time goes: phases, voices, effects
            crate::producer::set_profiling(true);
            let wo = RenderOptions { step_range: Some((w0, w1)), allow_missing_clips: true, ..Default::default() };
            let _ = render::render_window(&p1, &e.bank, &wo, &vec![0; p1.tracks.len()]).unwrap();
            eprintln!("  window phases: {}", crate::producer::prof_take());
            let _ = render::render(&p1, &e.bank, &opts).unwrap();
            eprintln!("  full phases:   {}", crate::producer::prof_take());
            crate::producer::set_profiling(false);
            // window size sweep: the intercept is the fixed per-render cost
            let mut pts = vec![];
            for wb_ in [1u32, 2, 4, 8].into_iter().filter(|x| *x <= bars) {
                let wo = RenderOptions { step_range: Some((0, wb_ * 16)), allow_missing_clips: true, ..Default::default() };
                let v: Vec<f64> = (0..reps).map(|_| { let t = Instant::now(); let _ = render::render_window(&p1, &e.bank, &wo, &vec![0; p1.tracks.len()]).unwrap(); ms(t) }).collect();
                pts.push((wb_, median(v)));
            }
            eprintln!("  window sweep (bars, ms): {pts:?}");
            // effects alone on window-length and full-length buffers
            let ctx_tr = std::collections::HashMap::new();
            let ctx = crate::fx::FxContext { step_secs: p1.step_secs(), triggers: &ctx_tr, offset: 0, levels: None };
            let wlen = (((w1 - w0) as f32 * p1.step_secs() + opts.tail) * crate::dsp::SR) as usize;
            let flen = ((p1.song_steps() as f32 * p1.step_secs() + opts.tail) * crate::dsp::SR) as usize;
            let mut fx_line = String::new();
            let all_fx: Vec<(String, &crate::fx::Effect)> = p1.tracks.iter().flat_map(|t| t.effects.iter().map(move |f| (t.name.clone(), f)))
                .chain(p1.buses.iter().flat_map(|b| b.effects.iter().map(move |f| (b.name.clone(), f))))
                .chain(p1.master_effects.iter().map(|f| ("master".to_string(), f))).collect();
            for (owner, fx) in &all_fx {
                let kind = serde_json::to_value(fx).ok().and_then(|v| v.get("type").and_then(|x| x.as_str()).map(String::from)).unwrap_or_default();
                let mut row = vec![];
                for n in [64usize, wlen, flen] {
                    let (mut l, mut r) = ((0..n).map(|i| (i as f32 * 0.01).sin() * 0.3).collect::<Vec<f32>>(), vec![0.1f32; n]);
                    let t = Instant::now();
                    fx.process(&mut l, &mut r, &ctx);
                    row.push(ms(t));
                }
                fx_line += &format!("\n    {owner}/{kind}: 64 smp {:.2} ms, window {:.1} ms, full {:.1} ms", row[0], row[1], row[2]);
            }
            eprintln!("  effects:{fx_line}");
            // one voice per distinct (track, pitch): synthesis cost
            for t in &p1.tracks {
                let tt = Instant::now();
                let v = crate::instruments::render_note_slide(&t.instrument, 60.0, 0.6, 0.75, &e.bank, 1, None);
                eprintln!("  voice {}: {:.1} ms ({} samples)", t.name, ms(tt), v.len());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use serde_json::json;

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len().max(1) as f64).sqrt() as f32
    }

    /// kick on every downbeat, an epiano chord per bar, `bars` long, with
    /// `fx` on the keys
    fn song(bars: u32, tag: &str, fx: &[serde_json::Value]) -> Engine {
        let mut e = bench::song(bars, tag);
        for f in fx {
            e.call("add_effect", f).unwrap();
        }
        e
    }

    /// Splice the edit and compare with a full render over the whole song
    /// from the window start on: (rms err, max abs err, hit rms, window).
    /// The per-sample rule for splice exactness (Naman, 2026-10-13, as
    /// refined by B40): a spliced sample may differ from the full render's
    /// by max(1e-8, one f32 ulp of max(|full|, |playing|)). The splice
    /// computes playing + (new - old), and that add rounds at its larger
    /// operand's ulp: for |x| >= 0.5 that is 5.96e-8, above any absolute
    /// 1e-8 or 3e-8 cap, and when a loud playing sample is pulled down by
    /// the edit the result's own ulp is smaller than the rounding that made it.
    fn sample_cap(full: f32, playing: f32) -> f32 {
        let a = full.abs().max(playing.abs());
        let ulp = f32::from_bits(a.to_bits() + 1) - a;
        ulp.max(1e-8)
    }

    /// Samples (both channels, from `from` on) where the splice breaks the
    /// per-sample rule.
    fn over_sample_cap(m0: &Mix, m1: &Mix, full: &Mix, from: usize) -> usize {
        (from..full.left.len().min(m1.left.len()))
            .flat_map(|i| [(m1.left[i], full.left[i], m0.left[i]), (m1.right[i], full.right[i], m0.right[i])])
            .filter(|(s, f, p)| (s - f).abs() > sample_cap(*f, *p))
            .count()
    }

    fn splice_vs_full(e: &mut Engine, edit: &serde_json::Value) -> (f32, f32, f32, (u32, u32), Mix, Mix, Mix) {
        let opts = RenderOptions::default();
        let p0 = e.project.clone();
        let m0 = render::render(&p0, &e.bank, &opts).unwrap();
        e.call("add_notes", edit).unwrap();
        let p1 = e.project.clone();
        let Dirty::Steps(a, b) = dirty_steps(&p0, &p1) else { panic!("a note edit is partial") };
        let (m1, _) = splice(&p0, &p1, &e.bank, &m0, a, b).unwrap();
        let full = render::render(&p1, &e.bank, &opts).unwrap();
        let (w0, w1) = window(&p0, &p1, a, b);
        let ws = (w0 as f32 * p0.step_secs() * crate::dsp::SR) as usize;
        assert_eq!(&m1.left[..ws], &m0.left[..ws], "before the window: the playing mix bit for bit");
        let err: Vec<f32> = (ws..full.left.len()).flat_map(|i| [m1.left[i] - full.left[i], m1.right[i] - full.right[i]]).collect();
        let maxe = err.iter().fold(0f32, |m, v| m.max(v.abs()));
        let diff: Vec<f32> = (ws..full.left.len()).map(|i| full.left[i] - m0.left[i]).collect();
        (rms(&err), maxe, rms(&diff), (w0, w1), m0, m1, full)
    }

    /// No click where the spliced region starts and ends: the largest
    /// sample step there is the full render's own.
    fn assert_no_seam_click(m1: &Mix, full: &Mix, edges: &[usize]) {
        for &at in edges {
            let at = at.min(full.left.len().saturating_sub(2001));
            let (a, b) = (at.saturating_sub(2000), at + 2000);
            let played = max_step(&m1.left, &m1.right, a, b);
            let stat = max_step(&full.left, &full.right, a, b);
            assert!(played <= stat * 1.01 + 1e-7, "seam at sample {at}: step {played} vs full render {stat}");
        }
    }

    /// Phase alignment: the change the splice made (spliced - playing)
    /// lines up with the change a full render makes (full - playing) at lag
    /// 0, by cross-correlation over +-256 samples.
    fn assert_change_at_lag_zero(m0: &Mix, m1: &Mix, full: &Mix, from: usize) {
        let n = full.left.len().min(m1.left.len());
        let a: Vec<f64> = (from..n).map(|i| (m1.left[i] - m0.left[i]) as f64).collect();
        let b: Vec<f64> = (from..n).map(|i| (full.left[i] - m0.left[i]) as f64).collect();
        let xc = |lag: i64| -> f64 {
            (0..a.len() as i64).filter_map(|i| {
                let j = i + lag;
                (j >= 0 && (j as usize) < b.len()).then(|| a[i as usize] * b[j as usize])
            }).sum()
        };
        let best = (-256i64..=256).max_by(|x, y| xc(*x).partial_cmp(&xc(*y)).unwrap()).unwrap();
        assert_eq!(best, 0, "cross-correlation peak at lag {best}, not 0");
    }

    /// Continuity: at each boundary of the spliced region the spliced mix
    /// is the full render's sample, and so is its step from the sample
    /// before (no jump where the splice starts or stops).
    fn assert_continuous_at(m1: &Mix, full: &Mix, edges: &[usize]) {
        for &i in edges {
            let i = i.clamp(1, full.left.len() - 1);
            for (x, y) in [(&m1.left, &full.left), (&m1.right, &full.right)] {
                // the per-sample allowance (3e-8), and twice it for a step
                assert!((x[i] - y[i]).abs() <= 3e-8, "sample {i}: {} vs full {}", x[i], y[i]);
                assert!(((x[i] - x[i - 1]) - (y[i] - y[i - 1])).abs() <= 6e-8, "step into sample {i}");
            }
        }
    }

    /// Thresholds (Naman, 2026-10-13): rms of splice minus full render
    /// <= 4e-9 over the whole song from the window start (the original
    /// live-playback contract's metric); any single sample <= 3e-8, the
    /// allowance for this padded window (ring-out bars on both sides),
    /// where the two renders sum the same audio in a different order:
    /// 3e-8 is one f32 ulp for samples in 0.25..0.5. Measured: rms 2.7e-10,
    /// max 2.98e-8. Plus no click at either seam, continuity of value and
    /// step at the window edges and the edit, and the change at lag 0.
    #[test]
    fn a_note_right_before_an_echo_and_reverb_tail_splices_exactly() {
        // a dotted-8th echo and a mid-size freeverb on the keys: together
        // they ring ~11 s, 5 bars past the context bar; the edit is a stab
        // at the end of bar 16 of 32, right before its own tail
        let fx = [
            json!({"track": "keys", "type": "delay", "params": {"steps": 3.0, "feedback": 0.3, "mix": 0.4}}),
            json!({"track": "keys", "type": "reverb", "params": {"size": 0.5, "mix": 0.35}}),
        ];
        let mut e = song(32, "tail", &fx);
        let ring = chain_tail_secs(&e.project);
        let st = e.project.step_secs();
        assert!(ring > 2.0 * 16.0 * st, "the tail outlasts the context bar: {ring} s");
        let (rms_err, max_err, change, (w0, w1), _m0, m1, full) =
            splice_vs_full(&mut e, &json!({"track": "keys", "pattern": "verse", "notes": [{"start": 252, "len": 4, "pitch": 67, "vel": 0.9}]}));
        let ring_steps = ring / st;
        eprintln!("echo+reverb tail: ring {ring:.2} s ({ring_steps:.0} steps), window {w0}..{w1} of 512, change rms {change:.3e}, splice-vs-full rms {rms_err:.3e}, max {max_err:.3e}");
        assert!(w0 > 0 && w1 < 512, "a part of the song, not all of it");
        assert!(w1 as f32 + RenderOptions::default().tail / st >= 256.0 + ring_steps, "the window covers the edit's ring-out");
        assert!(w0 as f32 <= 240.0 - ring_steps + 16.0, "and the ring-in of the notes before it");
        assert!(change > 1e-3, "the edit is audible");
        assert!(rms_err <= 4e-9, "splice within 4e-9 rms of a full render over the rest of the song: {rms_err:e}");
        assert!(max_err <= 3e-8, "no sample off by more than 3e-8: {max_err:e}");
        let sps = st * crate::dsp::SR;
        let (ws, we) = ((w0 as f32 * sps) as usize, ((w1 as f32 * st + RenderOptions::default().tail) * crate::dsp::SR) as usize);
        let edit = (252.0 * sps) as usize;
        assert_no_seam_click(&m1, &full, &[ws, we]);
        assert_continuous_at(&m1, &full, &[ws, edit, we.min(full.left.len() - 1), we.saturating_sub(1)]);
        assert_change_at_lag_zero(&_m0, &m1, &full, ws);
    }

    /// Thresholds: rms <= 4e-9, any sample <= 3e-8 (the padded window is
    /// the whole song here; same reason as above). Measured: rms 3.5e-10,
    /// max 2.98e-8. The fallback must be logged with reason, ring-out and
    /// window.
    #[test]
    fn a_tail_longer_than_half_the_song_falls_back_to_a_full_render() {
        // 0.6 feedback echo + a big reverb ring ~27 s: the window grows to
        // the whole 16-bar song, so splicing does not pay, and it is exact
        let fx = [
            json!({"track": "keys", "type": "delay", "params": {"steps": 3.0, "feedback": 0.6, "mix": 0.4}}),
            json!({"track": "keys", "type": "reverb", "params": {"size": 0.85, "mix": 0.35}}),
        ];
        let mut e = song(16, "longtail", &fx);
        let p0 = e.project.clone();
        let (rms_err, max_err, _, (w0, w1), _m0, m1, full) =
            splice_vs_full(&mut e, &json!({"track": "keys", "pattern": "verse", "notes": [{"start": 60, "len": 4, "pitch": 67, "vel": 0.9}]}));
        eprintln!("long tail: window {w0}..{w1}, splice-vs-full rms {rms_err:.3e}, max {max_err:.3e}");
        assert_eq!((w0, w1), (0, 256));
        assert!(!worth_splicing(&p0, &e.project, 60, 64), "the GUI renders the whole song instead");
        // the bailout is visible: reason, the ring-out that caused it, the window
        let fb = last_fallback().expect("a fallback event");
        assert!(fb.reason.contains("effect tails ring"), "{fb:?}");
        assert!(fb.ring_secs > 20.0 && fb.window == (0, 256) && fb.window_secs * 2.0 > fb.full_secs, "{fb:?}");
        assert!(rms_err <= 4e-9 && max_err <= 3e-8, "rms {rms_err:e} max {max_err:e}");
        assert_no_seam_click(&m1, &full, &[0, full.left.len()]);
    }

    /// Thresholds (unpadded window): rms <= 4e-9, any sample <= 1e-8. The
    /// per-sample allowance covers the compressor's nonlinear envelope
    /// state, which starts fresh at the window and settles within the
    /// context bar (not summation order: there is no ring-out padding
    /// here). Unpadded tests use 1e-8, padded ones 3e-8. Measured: rms
    /// 8.4e-11, max 7.5e-9.
    #[test]
    fn a_compressor_envelope_crossing_the_window_start_settles_in_the_context_bar() {
        // a slow, hard keys compressor: its gain at the window start depends
        // on everything before it, which the window does not render
        let fx = [json!({"track": "keys", "type": "compressor", "params": {"threshold_db": -30.0, "ratio": 8.0, "attack_ms": 10.0, "release_ms": 400.0}})];
        let mut e = song(16, "comp", &fx);
        let (rms_err, max_err, change, (w0, w1), ..) =
            splice_vs_full(&mut e, &json!({"track": "keys", "pattern": "verse", "notes": [{"start": 100, "len": 4, "pitch": 67, "vel": 0.9}]}));
        eprintln!("compressor: window {w0}..{w1}, change rms {change:.3e}, splice-vs-full rms {rms_err:.3e}, max {max_err:.3e}");
        assert!(change > 1e-3);
        assert!(rms_err <= 4e-9 && max_err <= 1e-8, "rms {rms_err:e} max {max_err:e}");
    }

    /// Commit 4 acceptance (Naman 2026-10-13): rendering with subnormals
    /// flushed (FTZ/DAZ) stays within SNR > 140 dB of the IEEE render, over
    /// the whole song and over its decaying tail. Also reports (does not
    /// assert) whether samples above the subnormal floor stay bit-identical,
    /// and the two render times.
    #[test]
    fn flushing_subnormals_keeps_a_long_tail_render_above_140_db_snr() {
        let fx = [
            json!({"track": "keys", "type": "delay", "params": {"steps": 3.0, "feedback": 0.6, "mix": 0.4}}),
            json!({"track": "keys", "type": "reverb", "params": {"size": 0.85, "mix": 0.35}}),
        ];
        let e = song(8, "ftz", &fx);
        // a 60 s render tail: the echo and reverb ring-out decays into
        // subnormals (about -760 dBFS) well before it ends
        let opts = RenderOptions { tail: 60.0, ..Default::default() };
        let t = std::time::Instant::now();
        crate::dsp::set_flush_denormals(false);
        let ieee = render::render(&e.project, &e.bank, &opts).unwrap();
        let t_ieee = t.elapsed();
        crate::dsp::set_flush_denormals(true);
        let t = std::time::Instant::now();
        let ftz = render::render(&e.project, &e.bank, &opts).unwrap();
        let t_ftz = t.elapsed();
        let snr = |a: &[f32], b: &[f32]| -> f64 {
            let s: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum();
            let n: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).powi(2)).sum();
            if n == 0.0 { f64::INFINITY } else { 10.0 * (s / n).log10() }
        };
        let body = (8.0 * 16.0 * e.project.step_secs() * crate::dsp::SR) as usize;
        let sub = ieee.left.iter().filter(|v| **v != 0.0 && v.abs() < f32::MIN_POSITIVE).count();
        let above_differ = ieee.left.iter().zip(&ftz.left).chain(ieee.right.iter().zip(&ftz.right))
            .filter(|(a, b)| a.abs() >= f32::MIN_POSITIVE && a.to_bits() != b.to_bits()).count();
        let maxd = ieee.left.iter().zip(&ftz.left).fold(0f32, |m, (a, b)| m.max((a - b).abs()));
        let (s_all, s_tail) = (snr(&ieee.left, &ftz.left).min(snr(&ieee.right, &ftz.right)), snr(&ieee.left[body..], &ftz.left[body..]).min(snr(&ieee.right[body..], &ftz.right[body..])));
        eprintln!("ftz: {sub} subnormal samples in the IEEE render; SNR all {s_all:.1} dB, tail {s_tail:.1} dB; max |diff| {maxd:e}; samples above 1.2e-38 that differ: {above_differ}; render {t_ieee:?} IEEE vs {t_ftz:?} FTZ");
        assert!(sub > 1000, "the IEEE render does reach subnormals: {sub}");
        assert!(s_all > 140.0 && s_tail > 140.0, "SNR all {s_all} tail {s_tail}");
    }

    /// Commit 5: effects with LFOs take their phase from the song position
    /// (FxContext::offset), so a window modulates as the same samples of a
    /// full render do. One case per LFO effect, each on the keys of a
    /// 16-bar song, edit = a stab in bar 9. Thresholds (Naman, 2026-10-13):
    /// rms <= 4e-9 and every sample within max(1e-8, 1 ulp of max(|full|,
    /// |playing|)) (`sample_cap`; an absolute 3e-8 is below the f32 ulp for
    /// |x| >= 0.5). Measured max: 2.98e-8 or 5.96e-8, 0 samples over the
    /// rule. The FDN hall meets the same rule since issue #1's fix: its
    /// old ~2e-6 rms residual was f32 rounding in the feedback loop (a
    /// render of history + edit minus a render of the edit alone did not
    /// cancel the history exactly); with the FDN's state in f64 it measures
    /// rms 2.8e-10, max 2.98e-8, 0 samples over the rule.
#[test]
    fn lfo_effects_splice_like_a_full_render() {
        let cases = [
            ("chorus", json!({"type": "chorus", "params": {"rate_hz": 0.7, "depth_ms": 4.0, "mix": 0.5}})),
            ("phaser", json!({"type": "phaser", "params": {"rate_hz": 0.37, "depth": 0.8, "feedback": 0.5, "stages": 6, "mix": 0.6}})),
            ("flanger", json!({"type": "flanger", "params": {"rate_hz": 0.23, "feedback": 0.6, "mix": 0.6}})),
            ("autopan", json!({"type": "autopan", "params": {"steps": 6.0, "depth": 0.9}})),
            ("tremolo", json!({"type": "autopan", "params": {"steps": 5.0, "depth": 0.8, "tremolo": true}})),
            ("fdn_hall", json!({"type": "reverb", "params": {"mode": "fdn_hall", "size": 0.6, "mix": 0.35}})),
            ("mod_delay", json!({"type": "mod_delay", "params": {"time_ms": 180.0, "feedback": 0.4, "mix": 0.3, "mod_rate_hz": 0.6, "mod_depth": 0.8}})),
        ];
        let mut bad = Vec::new();
        for (name, fx) in cases {
            let mut fx = fx;
            fx["track"] = json!("keys");
            let mut e = song(16, &format!("lfo_{name}"), &[fx]);
            let (rms_err, max_err, change, (w0, w1), _m0, m1, full) =
                splice_vs_full(&mut e, &json!({"track": "keys", "pattern": "verse", "notes": [{"start": 132, "len": 4, "pitch": 67, "vel": 0.9}]}));
            let ws = (w0 as f32 * e.project.step_secs() * crate::dsp::SR) as usize;
            let over = over_sample_cap(&_m0, &m1, &full, ws);
            eprintln!("lfo {name}: window {w0}..{w1}, change rms {change:.3e}, splice-vs-full rms {rms_err:.3e}, max {max_err:.3e}, samples over the per-sample rule: {over}");
            assert!(w0 > 0, "{name}: a window, not the whole song");
            assert!(change > 1e-3, "{name}: the edit is audible");
            let ok = rms_err <= 4e-9 && over == 0;
            if !ok {
                bad.push(format!("{name}: rms {rms_err:e} max {max_err:e} over-ulp {over}"));
            }
        }
        assert!(bad.is_empty(), "{bad:?}");
    }

    /// The FDN reverb's delay-line modulation takes its phase from the song
    /// position: a buffer that starts at `offset` (silent before it) comes
    /// out bit-identical to the same samples of a whole-buffer pass, and
    /// differs without the offset.
    #[test]
    fn the_fdn_reverb_modulation_follows_the_song_position() {
        use crate::fx::{Effect, ReverbFx, ReverbMode};
        let fx = Effect::Reverb(ReverbFx { mode: ReverbMode::FdnHall, size: 0.6, mix: 0.35, ..Default::default() });
        let n = 400_000;
        let off = 88_200;
        let x: Vec<f32> = (0..n).map(|i| if i >= off + 1000 && i < off + 3000 { ((i as f32) * 0.03).sin() * 0.5 } else { 0.0 }).collect();
        let tr = std::collections::HashMap::new();
        let (mut fl, mut fr) = (x.clone(), x.clone());
        fx.process(&mut fl, &mut fr, &crate::fx::FxContext { step_secs: 0.125, triggers: &tr, offset: 0, levels: None });
        let (mut wl, mut wr) = (x[off..].to_vec(), x[off..].to_vec());
        fx.process(&mut wl, &mut wr, &crate::fx::FxContext { step_secs: 0.125, triggers: &tr, offset: off, levels: None });
        let (mut zl, mut zr) = (x[off..].to_vec(), x[off..].to_vec());
        fx.process(&mut zl, &mut zr, &crate::fx::FxContext { step_secs: 0.125, triggers: &tr, offset: 0, levels: None });
        let m = (0..wl.len()).map(|k| (fl[off + k] - wl[k]).abs()).fold(0f32, f32::max);
        let mz = (0..zl.len()).map(|k| (fl[off + k] - zl[k]).abs()).fold(0f32, f32::max);
        let tail = fl[n - 1000..].iter().fold(0f32, |a, v| a.max(v.abs()));
        eprintln!("fdn unit: max diff with offset {m:e}, without offset {mz:e}; |out| 7 s after the burst {tail:e}");
        assert_eq!(m, 0.0, "with the song offset: bit-identical");
        assert!(mz > 1e-3, "without it the modulation is out of phase: {mz}");
    }

    #[test]
    fn the_two_windows_render_in_parallel_exactly_as_in_series() {
        let mut e = song(8, "par", &[]);
        let opts = RenderOptions::default();
        let p0 = e.project.clone();
        let m0 = render::render(&p0, &e.bank, &opts).unwrap();
        e.call("toggle_step", &json!({"track": "kick", "step": 36, "pattern": "verse"})).unwrap();
        let p1 = e.project.clone();
        let Dirty::Steps(a, b) = dirty_steps(&p0, &p1) else { panic!() };
        let (m1, changed) = splice(&p0, &p1, &e.bank, &m0, a, b).unwrap();
        // the same splice, windows one after the other
        let (w0, w1) = window(&p0, &p1, a, b);
        let wo = RenderOptions { step_range: Some((w0, w1)), allow_missing_clips: true, ..Default::default() };
        let at = (w0 as f32 * p0.step_secs() * crate::dsp::SR) as usize;
        let base = |p: &Project| -> Vec<usize> { render::schedule(p, &opts).0.iter().map(|t| t.iter().filter(|e| e.start < at).count()).collect() };
        let ma = render::render_window(&p0, &e.bank, &wo, &base(&p0)).unwrap();
        let mb = render::render_window(&p1, &e.bank, &wo, &base(&p1)).unwrap();
        let mut l = m0.left.clone();
        for k in 0..ma.left.len().min(mb.left.len()) {
            if at + k < l.len() {
                let d = mb.left[k] - ma.left[k];
                if d != 0.0 {
                    l[at + k] += d;
                }
            }
        }
        assert!(changed.1 > changed.0);
        assert!(l.iter().zip(&m1.left).all(|(x, y)| x.to_bits() == y.to_bits()), "parallel == serial, bit for bit");
    }

    #[test]
    fn tails_are_estimated_from_the_effects_that_ring() {
        use crate::fx::{DelayFx, Effect, ReverbFx, ReverbMode};
        let st = 0.125;
        assert_eq!(fx_tail_secs(&Effect::Delay(DelayFx { feedback: 0.0, steps: 3.0, ..Default::default() }), st), 0.375);
        let d = fx_tail_secs(&Effect::Delay(DelayFx { feedback: 0.5, steps: 4.0, ..Default::default() }), st);
        assert!((d - 0.5 * (1e-9f32).ln() / 0.5f32.ln()).abs() < 1e-3, "{d}");
        let hall = fx_tail_secs(&Effect::Reverb(ReverbFx { mode: ReverbMode::FdnHall, decay_s: 4.0, predelay_ms: 0.0, ..Default::default() }), st);
        assert_eq!(hall, 12.0);
        let mut off = DelayFx { feedback: 0.9, ..Default::default() };
        off.bypass = true;
        assert_eq!(fx_tail_secs(&Effect::Delay(off), st), 0.0, "a bypassed effect does not ring");
        assert_eq!(chain_tail_secs(&bench::song(4, "tailest").project), 0.0, "no ringing effects in the test song");
    }
}
