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
    let mut w0 = (from / bar).saturating_sub(CONTEXT_BARS) * bar;
    let w1 = ((to + bar - 1) / bar + CONTEXT_BARS) * bar;
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

/// True when splicing pays: the two window renders together are at most
/// half the audio of one full render (else the full render is about as
/// quick, and exact).
pub fn worth_splicing(old: &Project, new: &Project, from: u32, to: u32) -> bool {
    let (w0, w1) = window(old, new, from, to);
    let tail = RenderOptions::default().tail as f64;
    let step = old.step_secs() as f64;
    let splice = 2.0 * ((w1 - w0) as f64 * step + tail);
    let full = old.song_steps() as f64 * step + tail;
    splice * 2.0 <= full
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
    let ma = render::render_window(old, bank, &opts, &base(old))?;
    let mb = render::render_window(new, bank, &opts, &base(new))?;
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
