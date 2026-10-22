//! Offline renderer: arrangement -> per-track audio -> FX -> mix -> master.

use crate::automation::{self, parse_target, AutomationLane, Target};
use crate::dsp::*;
use crate::fx::{Effect, FxContext};
use crate::instruments::{render_note_slide, Instrument};
use crate::project::Project;
use crate::samples::SampleBank;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

/// One scheduled note.
#[derive(Clone, Debug)]
pub struct Event {
    pub start: usize,
    pub gate: f32,
    pub pitch: f32,
    pub vel: f32,
    /// Glide target pitch (808 / synth slides).
    pub slide_to: Option<f32>,
}

pub struct Stem {
    pub name: String,
    /// Empty when the render kept only this stem's band levels (`bands`).
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Band energies of the stem's mono sum, dB: [20-20k, 2-5k, 5-10k].
    /// Always set for a summarized stem (no audio kept).
    pub bands: Option<[f32; 3]>,
}

/// The bands a summarized stem keeps (Hz): the ones the mix rules read.
pub const STEM_BANDS: [(f32, f32); 3] = [(20.0, 20000.0), (2000.0, 5000.0), (5000.0, 10000.0)];

impl Stem {
    /// Band energy (dB) of the stem's mono sum: from the audio when it was
    /// kept, else from the summary (one of STEM_BANDS; -200 for another).
    pub fn band_db(&self, lo: f32, hi: f32) -> f32 {
        if !self.left.is_empty() {
            let m: Vec<f32> = self
                .left
                .iter()
                .zip(self.right.iter())
                .map(|(a, b)| 0.5 * (a + b))
                .collect();
            return crate::mix_rules::band_energy_db(&m, lo, hi);
        }
        match (
            self.bands,
            STEM_BANDS
                .iter()
                .position(|b| (b.0 - lo).abs() < 0.5 && (b.1 - hi).abs() < 0.5),
        ) {
            (Some(b), Some(i)) => b[i],
            _ => -200.0,
        }
    }

    fn summarize(name: String, l: &[f32], r: &[f32]) -> Stem {
        let m: Vec<f32> = l.iter().zip(r.iter()).map(|(a, b)| 0.5 * (a + b)).collect();
        let b = STEM_BANDS.map(|(lo, hi)| crate::mix_rules::band_energy_db(&m, lo, hi));
        Stem {
            name,
            left: Vec::new(),
            right: Vec::new(),
            bands: Some(b),
        }
    }
}

/// Memory budgets for long renders (MB; env overrides): the tracks rendered
/// at once, and the pre-fader track cache kept between renders. A 5:00 song
/// is ~106 MB per stereo buffer, so 12 tracks with stems, cache and four
/// tracks in flight reached 3.7 GB (the 2026-10-12 brutal test was killed).
pub const RENDER_BATCH_MB: f32 = 700.0;

/// The stems a voice-aware mix rule reads sample by sample: voice-named
/// tracks and tracks that play audio clips (a recorded take).
pub fn voice_stem(p: &Project, name: &str) -> bool {
    crate::mix_rules::is_voice_name(name)
        || p.audio_clips
            .iter()
            .any(|c| c.track.eq_ignore_ascii_case(name))
}
pub const TRACK_CACHE_MB: f32 = 900.0;

fn budget_mb(var: &str, default: f32) -> f32 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(default)
}

/// Per-track measurements taken during the render (no audio kept), so the
/// analyzer works on 30+ track songs without holding every stem in RAM.
#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub name: String,
    pub stats: crate::analysis::Stats,
    /// Sum of squares over both channels.
    pub energy: f64,
    /// RMS (dBFS) measured only over 400 ms blocks where the track sounds.
    pub active_rms_db: f32,
    /// Percent of the song where the track sounds.
    pub active_percent: f32,
    /// Mean-square energy per 400 ms block (for per-section analysis).
    pub blocks: Vec<f32>,
    /// Mean-square energy below ~150 Hz per 10 ms block (kick/bass masking).
    pub low_env: Vec<f32>,
}

/// Length of the blocks in `TrackInfo::low_env`.
pub const LOW_BLOCK: usize = 441;

/// Low-band (< ~150 Hz, 2-pole) mean-square envelope in 10 ms blocks.
pub fn low_envelope(l: &[f32], r: &[f32]) -> Vec<f32> {
    let n = l.len().min(r.len());
    let a = (-2.0 * std::f32::consts::PI * 150.0 / SR).exp();
    let (mut y1, mut y2) = (0.0f32, 0.0f32);
    let mut out = Vec::with_capacity(n / LOW_BLOCK + 1);
    let mut acc = 0.0f32;
    for i in 0..n {
        let x = 0.5 * (l[i] + r[i]);
        y1 = x + (y1 - x) * a;
        y2 = y1 + (y2 - y1) * a;
        acc += y2 * y2;
        if (i + 1) % LOW_BLOCK == 0 {
            out.push(acc / LOW_BLOCK as f32);
            acc = 0.0;
        }
    }
    out
}

/// Share of `b`'s low-band energy that sounds while `a` is also loud in the
/// low band (within 6 dB of `b` or louder): the masking that sidechain
/// ducking should remove. 0..1.
pub fn low_overlap(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (mut tot, mut ov) = (0.0f64, 0.0f64);
    for i in 0..n {
        let (x, y) = (a[i] as f64, b[i] as f64);
        tot += y;
        if x > y * 0.25 && x > 1e-7 {
            ov += y.min(x);
        }
    }
    if tot < 1e-12 {
        0.0
    } else {
        (ov / tot) as f32
    }
}

/// Length of the analysis blocks in `TrackInfo::blocks`.
pub const INFO_BLOCK: usize = (SR as usize) * 2 / 5;

pub fn track_info(name: &str, l: &[f32], r: &[f32]) -> TrackInfo {
    let stats = crate::analysis::stats(l, r);
    let n = l.len().min(r.len());
    let mut energy = 0.0f64;
    let mut blocks = Vec::with_capacity(n / INFO_BLOCK + 1);
    for c in 0..n.div_ceil(INFO_BLOCK) {
        let (a, b) = (c * INFO_BLOCK, ((c + 1) * INFO_BLOCK).min(n));
        let mut e = 0.0f64;
        for i in a..b {
            e += (l[i] as f64).powi(2) + (r[i] as f64).powi(2);
        }
        energy += e;
        blocks.push((e / (2.0 * (b - a).max(1) as f64)) as f32);
    }
    let gate = 1e-6f32; // -60 dBFS mean square
    let active: Vec<f32> = blocks.iter().copied().filter(|b| *b > gate).collect();
    let active_rms_db = if active.is_empty() {
        -120.0
    } else {
        let ms = active.iter().sum::<f32>() / active.len() as f32;
        ((10.0 * ms.max(1e-12).log10()) * 10.0).round() / 10.0
    };
    TrackInfo {
        name: name.to_string(),
        stats,
        energy,
        active_rms_db,
        active_percent: (100.0 * active.len() as f32 / blocks.len().max(1) as f32).round(),
        blocks,
        low_env: low_envelope(l, r),
    }
}

pub struct Mix {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub stems: Vec<Stem>,
    /// Per-track measurements (with `keep_stems` or `track_stats`).
    pub track_info: Vec<TrackInfo>,
    /// Post-fader output of every bus (only with `keep_stems`).
    pub bus_stems: Vec<Stem>,
    pub seconds: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    /// Play the whole song this many times.
    pub loops: u32,
    /// Seconds of tail after the last bar (reverb/delay ring-out).
    pub tail: f32,
    pub keep_stems: bool,
    /// Measure every track (TrackInfo) without keeping its audio.
    pub track_stats: bool,
    /// Render only this section range of the song (in steps), if set.
    pub step_range: Option<(u32, u32)>,
    /// Render even when audio clips point at samples that are not loaded
    /// (they stay silent). Off by default: such a render fails and names
    /// every missing clip, so a vocal can never vanish without an error.
    pub allow_missing_clips: bool,
    /// With keep_stems: only tracks this says yes to (project, track name)
    /// keep their audio; every other track stem keeps its band levels only
    /// (Stem::band_db), so a long song's stems do not hold the whole mix
    /// again in RAM. None = every stem keeps its audio.
    pub full_stems: Option<fn(&Project, &str) -> bool>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            loops: 1,
            tail: 1.5,
            keep_stems: false,
            track_stats: false,
            step_range: None,
            allow_missing_clips: false,
            full_stems: None,
        }
    }
}

/// Audio clips whose sample is not loaded in `bank` (case-insensitive), as
/// "'name' on track (clip i, beat b)" lines. Clips on tracks that do not
/// exist are not counted (they never render).
pub fn missing_clip_samples(p: &Project, bank: &SampleBank) -> Vec<String> {
    p.audio_clips
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            p.tracks
                .iter()
                .any(|t| t.name.eq_ignore_ascii_case(&c.track))
        })
        .filter(|(_, c)| bank.get(&c.sample).is_none())
        .map(|(i, c)| {
            let known = p
                .samples
                .iter()
                .any(|s| s.name.eq_ignore_ascii_case(&c.sample));
            format!(
                "'{}' on track '{}' (clip {i}, beat {}){}",
                c.sample,
                c.track,
                (c.start_beat * 100.0).round() / 100.0,
                if known {
                    ": registered but its file could not be read"
                } else {
                    ": no such sample"
                }
            )
        })
        .collect()
}

/// Deterministic per-pass dice roll for a note with probability `p`
/// (same project = same render, but each pattern pass rolls differently).
pub fn chance_hit(track: usize, note: usize, pass_offset: u32, p: f32) -> bool {
    let mut x = (track as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (note as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (pass_offset as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    ((x >> 11) as f64 / (1u64 << 53) as f64) < p as f64
}

/// Build the event list for every track (index aligned with project.tracks).
/// Seconds from song start to an absolute step of a (possibly looped) render:
/// each loop replays the tempo curve from the top.
pub fn song_secs_at_step(
    curve: &crate::tempo_curve::TempoCurve,
    song_steps: f64,
    abs_step: f64,
) -> f64 {
    if song_steps <= 0.0 || abs_step < song_steps {
        return curve.secs_at_step(abs_step);
    }
    let lp = (abs_step / song_steps).floor();
    lp * curve.secs_at_step(song_steps) + curve.secs_at_step(abs_step - lp * song_steps)
}

pub fn schedule(p: &Project, opts: &RenderOptions) -> (Vec<Vec<Event>>, usize) {
    let step = p.step_secs();
    // tempo automation: positions go through the tempo curve; with no tempo
    // points the constant-step arithmetic below is kept bit for bit
    let curve = crate::tempo_curve::TempoCurve::new(p);
    let song_steps = p.song_steps() as f64;
    let tsec = |abs: f32| song_secs_at_step(&curve, song_steps, abs as f64);
    let swing_steps = p.swing.clamp(0.0, 1.0) * 0.5;
    let mut events: Vec<Vec<Event>> = vec![Vec::new(); p.tracks.len()];
    // concert-pitch offset (semitones) per track: Project::tuning_cents on
    // pitched instruments, 0 on drums/kits and whenever tuning_cents is 0
    let tunes: Vec<f32> = p
        .tracks
        .iter()
        .map(|t| {
            if p.tuning_cents != 0.0 && t.instrument.follows_tuning() {
                p.tuning_cents / 100.0
            } else {
                0.0
            }
        })
        .collect();
    let mut offset: u32 = 0;
    let (r0, r1) = opts.step_range.unwrap_or((0, u32::MAX));
    for _ in 0..opts.loops.max(1) {
        for sec in p.song_sections() {
            let Ok(pi) = p.pattern_index(&sec.pattern) else {
                continue;
            };
            let pat = &p.patterns[pi];
            for _ in 0..sec.repeats.max(1) {
                for (ti, track) in p.tracks.iter().enumerate() {
                    let tune = tunes[ti];
                    for (ni, n) in pat.notes(&track.name).iter().enumerate() {
                        if n.prob < 1.0 && !chance_hit(ti, ni, offset, n.prob) {
                            continue;
                        }
                        let mut s = n.start;
                        if s.fract() == 0.0 && (s as i64) % 2 == 1 {
                            s += swing_steps;
                        }
                        s += n.offset.clamp(-0.5, 0.5);
                        let abs = (offset as f32 + s).max(0.0);
                        if abs < r0 as f32 || abs >= r1 as f32 {
                            continue;
                        }
                        let rel = abs - r0 as f32;
                        let (start, gate) = if curve.is_constant() {
                            ((rel * step * SR) as usize, (n.len * step).max(0.01))
                        } else {
                            let t = tsec(abs);
                            (
                                ((t - tsec(r0 as f32)) * SR as f64) as usize,
                                ((tsec(abs + n.len) - t) as f32).max(0.01),
                            )
                        };
                        let (pitch, slide_to) = if tune != 0.0 {
                            (n.pitch as f32 + tune, n.slide_to.map(|x| x as f32 + tune))
                        } else {
                            (n.pitch as f32, n.slide_to.map(|x| x as f32))
                        };
                        events[ti].push(Event {
                            start,
                            gate,
                            pitch,
                            vel: n.vel,
                            slide_to,
                        });
                    }
                }
                offset += pat.steps();
            }
        }
    }
    let end_step = offset.min(r1).saturating_sub(r0);
    let len = if curve.is_constant() {
        (end_step as f32 * step * SR) as usize
    } else {
        ((tsec(offset.min(r1) as f32) - tsec(r0 as f32)) * SR as f64) as usize
    };
    for e in events.iter_mut() {
        e.sort_by_key(|x| x.start);
    }
    (events, len)
}

/// Song-time mapping for automation during one render.
#[derive(Clone, Debug)]
pub struct Timeline {
    /// Song beat at sample 0 of this render.
    pub beat0: f32,
    pub beats_per_sample: f32,
    /// Song length in beats (automation wraps on multi-loop renders).
    pub song_beats: f32,
    /// Number of loops rendered; past the last one (the reverb tail) every
    /// lane holds its final value instead of wrapping back to the start.
    pub loops: u32,
    /// Tempo automation: (curve, seconds at beat0, seconds per loop).
    pub tempo: Option<(std::sync::Arc<crate::tempo_curve::TempoCurve>, f64, f64)>,
    /// Song sample at sample 0 of this render (a live-splice window's place
    /// in the song; 0 for a whole-song render).
    pub sample0: usize,
}

impl Timeline {
    pub fn new(p: &Project, opts: &RenderOptions) -> Self {
        let r0 = opts.step_range.map(|r| r.0).unwrap_or(0);
        let curve = crate::tempo_curve::TempoCurve::new(p);
        let tempo = if curve.is_constant() {
            None
        } else {
            let t0 = curve.secs_at_step(r0 as f64);
            let loop_secs = curve.secs_at_beat(p.song_beats() as f64);
            Some((std::sync::Arc::new(curve), t0, loop_secs))
        };
        Timeline {
            beat0: r0 as f32 / 4.0,
            beats_per_sample: 1.0 / (4.0 * p.step_secs() * SR),
            song_beats: p.song_beats(),
            loops: opts.loops.max(1),
            tempo,
            sample0: 0,
        }
    }

    /// The song beat at song sample `abs`, computed as a whole-song render
    /// computes it (beat 0 at sample 0), so a window reads automation on the
    /// whole song's grid and gets the same values. With tempo automation
    /// (which never splices) it is the window's own reading.
    pub fn song_beat_at(&self, abs: usize) -> f32 {
        if self.sample0 == 0 || self.tempo.is_some() {
            return self.beat_at(abs.saturating_sub(self.sample0));
        }
        let b = 0.0 + abs as f32 * self.beats_per_sample;
        if self.song_beats <= 0.0 || b < self.song_beats {
            return b;
        }
        if b >= self.song_beats * self.loops as f32 {
            return self.song_beats;
        }
        b % self.song_beats
    }

    pub fn beat_at(&self, sample: usize) -> f32 {
        if let Some((curve, t0, loop_secs)) = &self.tempo {
            let t = t0 + sample as f64 / SR as f64;
            let lp = if *loop_secs > 0.0 {
                (t / loop_secs).floor()
            } else {
                0.0
            };
            if lp >= self.loops as f64 {
                return self.song_beats;
            }
            return curve.beat_at_secs(t - lp * loop_secs) as f32;
        }
        let b = self.beat0 + sample as f32 * self.beats_per_sample;
        if self.song_beats <= 0.0 || b < self.song_beats {
            return b;
        }
        if b >= self.song_beats * self.loops as f32 {
            // tail: hold the end of the song
            return self.song_beats;
        }
        b % self.song_beats
    }
}

/// Automation is evaluated once per block and interpolated per sample.
pub const AUTO_BLOCK: usize = 64;

pub(crate) fn owner_lanes<'a>(p: &'a Project, owner: &str) -> Vec<&'a AutomationLane> {
    p.automation
        .iter()
        .filter(|l| l.enabled && !l.points.is_empty() && l.target.eq_ignore_ascii_case(owner))
        .collect()
}

/// Per-sample values of a lane (block-evaluated, linearly smoothed).
pub(crate) fn lane_curve(
    lane: &AutomationLane,
    n: usize,
    tl: &Timeline,
    map: impl Fn(f32) -> f32,
) -> Vec<f32> {
    // frequencies sweep geometrically (equal time per octave); the whole
    // curve is flattened here, once, before any sample loop runs
    let log = automation::is_log_param(&lane.param);
    // the grid is the song's: blocks start on multiples of AUTO_BLOCK song
    // samples, so a window starting mid-block reads the whole song's values
    let s0 = tl.sample0;
    let k0 = s0 / AUTO_BLOCK;
    let blocks: Vec<f32> = (k0..=(s0 + n) / AUTO_BLOCK + 1)
        .map(|k| {
            map(lane
                .value_at_in(tl.song_beat_at(k * AUTO_BLOCK), log)
                .unwrap_or(0.0))
        })
        .collect();
    let mut out: Vec<f32> = (0..n)
        .map(|i| {
            let a = i + s0;
            let k = a / AUTO_BLOCK - k0;
            let f = (a % AUTO_BLOCK) as f32 / AUTO_BLOCK as f32;
            blocks[k] + (blocks[k + 1] - blocks[k]) * f
        })
        .collect();
    // a one-pole smoother on top of the per-block ramps: a step point (or a
    // jump faster than a block) lands over a few ms instead of 64 samples,
    // so step corners do not click; sweeps lag by ~AUTO_SMOOTH_MS at most
    let a = auto_smooth_coeff(SR);
    let mut y = out.first().copied().unwrap_or(0.0);
    for v in out.iter_mut() {
        y += a * (*v - y);
        *v = y;
    }
    out
}

/// Time constant of the automation smoother (ms).
pub const AUTO_SMOOTH_MS: f32 = 3.0;

/// The one-pole automation-smoother coefficient for a given sample rate,
/// computed in f32 exactly as lane_curve has always computed it:
/// tau_samples = AUTO_SMOOTH_MS * 0.001 * sr; a = 1 - exp(-1/tau_samples).
/// Single source shared by the offline renderer and the RT engine/oracle
/// (milestone-1 determinism contract). Block size never enters here.
pub fn auto_smooth_coeff(sr: f32) -> f32 {
    1.0 - (-1.0 / (AUTO_SMOOTH_MS * 0.001 * sr)).exp()
}

/// Raw (pre-smoother) lane-curve value at absolute song sample `a`,
/// computed with lane_curve's exact ops at s0 = 0: song-absolute
/// AUTO_BLOCK grid, `map` applied at grid points, f32 lerp. The RT
/// engine's smoothed lanes (rt::MixNode) carry the one-pole state across
/// blocks on top of this.
pub(crate) fn lane_raw_at(
    lane: &AutomationLane,
    tl: &Timeline,
    a: usize,
    map: impl Fn(f32) -> f32,
) -> f32 {
    let log = automation::is_log_param(&lane.param);
    let k = a / AUTO_BLOCK;
    let f = (a % AUTO_BLOCK) as f32 / AUTO_BLOCK as f32;
    let at = |k: usize| {
        map(lane
            .value_at_in(tl.song_beat_at(k * AUTO_BLOCK), log)
            .unwrap_or(0.0))
    };
    let b0 = at(k);
    let b1 = at(k + 1);
    b0 + (b1 - b0) * f
}

/// Run an effect chain, honouring `fx.<i>.<param>` automation lanes.
///
/// Effects are whole-buffer processors with internal state, so an automated
/// effect is rendered at a small grid of parameter values (each a normal,
/// continuous render) and the outputs are blended per sample with
/// multilinear weights from the automation curve. Up to three automated
/// parameters per effect are honoured.
pub fn process_chain(
    effects: &[Effect],
    lanes: &[&AutomationLane],
    l: &mut Vec<f32>,
    r: &mut Vec<f32>,
    ctx: &FxContext,
    tl: &Timeline,
) {
    for (fi, fx) in effects.iter().enumerate() {
        let mine: Vec<(&AutomationLane, Vec<String>)> = lanes
            .iter()
            .filter_map(|lane| match parse_target(&lane.param) {
                Some(Target::Fx(i, path)) if i == fi => Some((*lane, path)),
                _ => None,
            })
            .take(3)
            .collect();
        let base = serde_json::to_value(fx).unwrap_or_default();
        let mine: Vec<_> = mine
            .into_iter()
            .filter(|(_, path)| automation::get_path(&base, path).is_some())
            .collect();
        if mine.is_empty() {
            fx.process(l, r, ctx);
            continue;
        }
        let n = l.len();
        let k_per = match mine.len() {
            1 => 10,
            2 => 4,
            _ => 3,
        };
        // per lane: the level values and the per-sample fractional level index
        let mut levels: Vec<Vec<f32>> = Vec::new();
        let mut pos: Vec<Vec<f32>> = Vec::new();
        for (lane, path) in &mine {
            let (lo, hi) = lane.range().unwrap_or((0.0, 0.0));
            let log = automation::is_log_param(&lane.param) && lo > 0.0;
            let k = if (hi - lo).abs() < 1e-9 { 1 } else { k_per };
            let to_axis = |v: f32| if log { v.max(1e-6).ln() } else { v };
            let (a0, a1) = (to_axis(lo), to_axis(hi));
            let lv: Vec<f32> = (0..k)
                .map(|j| {
                    let t = if k == 1 {
                        0.0
                    } else {
                        j as f32 / (k - 1) as f32
                    };
                    let x = a0 + (a1 - a0) * t;
                    if log {
                        x.exp()
                    } else {
                        x
                    }
                })
                .collect();
            let span = (a1 - a0).abs().max(1e-12);
            let kk = (k - 1) as f32;
            pos.push(lane_curve(lane, n, tl, |v| {
                if k == 1 {
                    0.0
                } else {
                    ((to_axis(v) - a0) / span * kk).clamp(0.0, kk)
                }
            }));
            levels.push(lv);
            let _ = path;
        }
        let dims: Vec<usize> = levels.iter().map(|v| v.len()).collect();
        let total: usize = dims.iter().product();
        let (src_l, src_r) = (l.clone(), r.clone());
        let mut acc_l = vec![0.0f32; n];
        let mut acc_r = vec![0.0f32; n];
        for combo in 0..total {
            // decode the multi-index
            let mut idx = Vec::with_capacity(dims.len());
            let mut c = combo;
            for d in &dims {
                idx.push(c % d);
                c /= d;
            }
            // weights: product of hat functions
            let mut w = vec![1.0f32; n];
            for (li, &j) in idx.iter().enumerate() {
                for (wi, p) in w.iter_mut().zip(pos[li].iter()) {
                    *wi *= (1.0 - (p - j as f32).abs()).max(0.0);
                }
            }
            if w.iter().all(|x| *x <= 1e-6) {
                continue;
            }
            let mut v = base.clone();
            for (li, (_, path)) in mine.iter().enumerate() {
                automation::set_path(&mut v, path, levels[li][idx[li]]);
            }
            let Ok(fxv) = serde_json::from_value::<Effect>(v) else {
                continue;
            };
            let (mut tl_, mut tr_) = (src_l.clone(), src_r.clone());
            fxv.process(&mut tl_, &mut tr_, ctx);
            for i in 0..n {
                acc_l[i] += w[i] * tl_[i];
                acc_r[i] += w[i] * tr_[i];
            }
        }
        *l = acc_l;
        *r = acc_r;
    }
}

/// Apply fader (with optional `volume` lane, in dB) and stereo balance
/// (with optional `pan` lane) to a stereo buffer.
pub(crate) fn apply_fader(
    l: &mut [f32],
    r: &mut [f32],
    volume_db: f32,
    lanes: &[&AutomationLane],
    tl: &Timeline,
    balance: Option<f32>,
) {
    let n = l.len();
    let vol_lane = lanes
        .iter()
        .find(|x| parse_target(&x.param) == Some(Target::Volume));
    match vol_lane {
        Some(lane) => {
            let g = lane_curve(lane, n, tl, |db| db_to_gain(db.clamp(-120.0, 24.0)));
            for i in 0..n {
                l[i] *= g[i];
                r[i] *= g[i];
            }
        }
        None => {
            let g = db_to_gain(volume_db);
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                *a *= g;
                *b *= g;
            }
        }
    }
    if let Some(pan) = balance {
        let pan_lane = lanes
            .iter()
            .find(|x| parse_target(&x.param) == Some(Target::Pan));
        let curve = pan_lane.map(|lane| lane_curve(lane, n, tl, |v| v.clamp(-1.0, 1.0)));
        for i in 0..n {
            let p = curve.as_ref().map(|c| c[i]).unwrap_or(pan);
            if p > 0.0 {
                l[i] *= 1.0 - p;
            } else if p < 0.0 {
                r[i] *= 1.0 + p;
            }
        }
    }
}

/// Bus processing order: a bus is processed after every bus that feeds
/// it. Cycles (rejected by route_bus) fall back to declaration order.
/// The order is the rack runtime's ([`crate::rack::compile::bus_order`]).
pub fn bus_order(p: &Project) -> Vec<usize> {
    crate::rack::compile::bus_order(p)
}

/// Fade applied where a voice is cut short (mono choke).
pub const CHOKE_FADE_S: f32 = 0.005;
/// Fade-out applied to the last samples of every voice so a buffer that
/// ends above zero (a truncated tail) never steps to silence.
pub const END_FADE: usize = 96;
/// Fade-in on every voice (0.5 ms): kills the step of a sample/oscillator
/// that starts away from zero without softening the transient audibly.
pub const START_FADE: usize = 24;

/// Mix one voice into `out` at `at` without discontinuities: a 0.5 ms fade-in, a fade-out over its last
/// samples, and a raised-cosine choke when a mono voice is cut by the next
/// note (`choke_after` samples after its start).
pub fn add_declicked(out: &mut [f32], at: usize, buf: &[f32], choke_after: Option<usize>) {
    add_voice(out, at, buf, choke_after, false);
}

/// `add_declicked`, optionally fading the voice in with the exact
/// complement of the choke fade (`xfade_in`): a legato voice that continues
/// the choked one at the same phase then sums to a constant level.
pub fn add_voice(
    out: &mut [f32],
    at: usize,
    buf: &[f32],
    choke_after: Option<usize>,
    xfade_in: bool,
) {
    add_voice_faded(out, at, buf, choke_after, xfade_in, START_FADE)
}

/// Fade-in for sample-based voices (2 ms): a sample cut out of a longer
/// recording starts mid-waveform, and 0.5 ms still ticks (critic v15).
pub const SAMPLE_START_FADE: usize = 96;

/// `add_voice` with an explicit linear fade-in length (samples).
pub fn add_voice_faded(
    out: &mut [f32],
    at: usize,
    buf: &[f32],
    choke_after: Option<usize>,
    xfade_in: bool,
    start_fade_len: usize,
) {
    if at >= out.len() || buf.is_empty() {
        return;
    }
    let fade = (CHOKE_FADE_S * SR) as usize;
    let mut n = buf.len().min(out.len() - at);
    let mut choke_from = usize::MAX;
    if let Some(c) = choke_after {
        if c + fade < n {
            n = c + fade;
            choke_from = c;
        }
    }
    let end_fade = END_FADE.min(n / 8).max(1);
    let start_fade = start_fade_len.min(n / 8);
    for (j, (o, s)) in out[at..at + n].iter_mut().zip(buf.iter()).enumerate() {
        let mut g = 1.0f32;
        if xfade_in {
            if j < fade {
                let x = j as f32 / fade as f32;
                g *= 0.5 - 0.5 * (std::f32::consts::PI * x).cos();
            }
        } else if j < start_fade {
            g *= (j as f32 + 0.5) / start_fade as f32;
        }
        let left = n - j;
        if left <= end_fade {
            g *= (left as f32 - 0.5).max(0.0) / end_fade as f32;
        }
        if j >= choke_from {
            let x = (j - choke_from) as f32 / fade as f32;
            g *= 0.5 + 0.5 * (std::f32::consts::PI * x.min(1.0)).cos();
        }
        *o += *s * g;
    }
}

/// Pre-fader audio of every track (instrument + track effects), reusable
/// while nothing but faders and the master changes. Gain staging and
/// mastering re-render the song several times with only those moved; with a
/// cache the tracks are synthesised once and the output stays bit-identical.
#[derive(Default)]
pub struct TrackCache {
    /// Per track index: the key it was synthesised under, and its buffer.
    tracks: HashMap<usize, (TrackKey, std::sync::Arc<(Vec<f32>, Vec<f32>)>)>,
    /// Per bus index: the key its output was computed under, and the output
    /// (own chain, fader and balance applied; what it sends on).
    buses: HashMap<usize, (TrackKey, std::sync::Arc<(Vec<f32>, Vec<f32>)>)>,
}

/// What a cached track buffer was synthesised from, kept whole (not a
/// hash) so a lookup is an exact comparison of state: the canonical JSON of
/// what every track of one render reads (`TrackCache::shared_key`, one
/// copy shared by all tracks) and of what this track alone reads
/// (`TrackCache::track_key`). Two different states can never compare
/// equal, so no hash collision can serve a stale buffer.
///
/// A track's key holds only what its pre-fader buffer (instrument voices,
/// pan, its own effect chain) is computed from, so an edit re-synthesises
/// exactly the tracks whose buffer it changes; the buses, sends, faders and
/// master are never cached and are re-mixed on every render.
#[derive(Clone, PartialEq, Eq)]
pub struct TrackKey {
    shared: std::sync::Arc<String>,
    own: String,
}

impl std::fmt::Debug for TrackKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut h: u64 = 0xcbf29ce484222325;
        fnv(&mut h, &self.shared);
        fnv(&mut h, &self.own);
        write!(
            f,
            "TrackKey({} + {} bytes, fnv {h:016x})",
            self.shared.len(),
            self.own.len()
        )
    }
}

fn fnv(h: &mut u64, s: &str) {
    for b in s.bytes() {
        *h ^= b as u64;
        *h = h.wrapping_mul(0x100000001b3);
    }
    *h ^= 0xff;
    *h = h.wrapping_mul(0x100000001b3);
}

/// Every string value under a "source" key, anywhere in an effect chain's
/// JSON (lowercase, sorted, unique): the tracks a sidechain or keyed
/// dynamic EQ listens to.
fn chain_sources(effects: &[crate::fx::Effect]) -> Vec<String> {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    if k == "source" {
                        if let Value::String(s) = x {
                            out.push(s.to_lowercase());
                        }
                    }
                    walk(x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(&serde_json::to_value(effects).unwrap_or_default(), &mut out);
    out.sort();
    out.dedup();
    out
}

impl TrackCache {
    /// Key spec, shared part (one Arc per render, read by every track):
    /// canonical JSON (serde_json; floats as their IEEE bits) of the tempo
    /// (bpm, tempo points: the automation timeline, LFO/delay sync and
    /// every position), the song length in steps and the buffer length in
    /// samples (body + tail), the sample registry (instruments and clips
    /// look samples up by name), and the render options that shape audio
    /// (loops, tail, step_range, allow_missing_clips). The sample rate
    /// (dsp::SR) and the automation block (64 samples) are constants.
    fn shared_key(p: &Project, opts: &RenderOptions, total: usize) -> std::sync::Arc<String> {
        let s = serde_json::json!({
            "bpm": p.bpm.to_bits(),
            "tempo_points": p.tempo_points,
            "song_steps": p.song_steps(),
            "total": total,
            "samples": p.samples,
            "loops": opts.loops,
            "tail": opts.tail.to_bits(),
            "step_range": opts.step_range,
            "allow_missing_clips": opts.allow_missing_clips,
        });
        std::sync::Arc::new(s.to_string())
    }

    /// Key spec, own part: canonical JSON of
    /// - "ti": the track index (voice seeds and note dice use it);
    /// - "track": the track with volume_db (0), sends, output, mute and
    ///   solo set aside: id, name, instrument with every parameter, pan,
    ///   and the effect chain with each effect's type, id, bypass and
    ///   every parameter;
    /// - "lanes": each automation lane on the track that is not its volume
    ///   (instrument.*, fx.N.*, pan, ...);
    /// - "events": the track's scheduled notes exactly as the voices are
    ///   rendered from them: [start sample, gate, pitch, velocity, slide]
    ///   (floats as bits). This is where pattern notes, the arrangement and
    ///   playlist, swing, note probability, tempo and tuning_cents reach a
    ///   track: tuning moves only tracks whose instrument follows tuning,
    ///   and only through their pitches;
    /// - "clips": the audio clips on the track;
    /// - "chokers": for an open hat, the note-ons of the unmuted closed
    ///   hats that choke it (empty otherwise);
    /// - "deps": for each track its effect chain listens to (any "source"),
    ///   that source's sidechain triggers exactly as the chain reads them
    ///   (its note-ons and clip onsets) and the audio clips on it (the
    ///   audio a level-keyed dynamic EQ follows).
    ///
    /// Set aside, because they act after the cached buffer or are never
    /// read by a track: faders and their automation, sends, routing,
    /// mute/solo (except as hat chokers), buses, the master, other tracks'
    /// notes, instruments, pans and chains (except as deps/chokers), key and
    /// scale, macros, markers, saved fx chains, the vocal map, song spec
    /// and history, and id_seq.
    fn track_key(
        shared: &std::sync::Arc<String>,
        p: &Project,
        ti: usize,
        events: &[Vec<Event>],
        triggers: &HashMap<String, Vec<usize>>,
    ) -> TrackKey {
        let mut t = p.tracks[ti].clone();
        t.volume_db = 0.0;
        t.sends.clear();
        t.output = None;
        t.mute = false;
        t.solo = false;
        let name = &p.tracks[ti].name;
        let lanes: Vec<&AutomationLane> = p
            .automation
            .iter()
            .filter(|l| {
                l.target.eq_ignore_ascii_case(name)
                    && parse_target(&l.param) != Some(Target::Volume)
            })
            .collect();
        let ev: Vec<(usize, u32, u32, u32, Option<u32>)> = events[ti]
            .iter()
            .map(|e| {
                (
                    e.start,
                    e.gate.to_bits(),
                    e.pitch.to_bits(),
                    e.vel.to_bits(),
                    e.slide_to.map(f32::to_bits),
                )
            })
            .collect();
        let clips: Vec<&crate::project::AudioClip> = p
            .audio_clips
            .iter()
            .filter(|c| c.track.eq_ignore_ascii_case(name))
            .collect();
        let deps: Vec<Value> = chain_sources(&t.effects)
            .into_iter()
            .map(|s| {
                let on: Vec<&crate::project::AudioClip> = p
                    .audio_clips
                    .iter()
                    .filter(|c| c.track.to_lowercase() == s)
                    .collect();
                serde_json::json!({ "source": s, "triggers": triggers.get(&s), "clips": on })
            })
            .collect();
        let own = serde_json::json!({
            "ti": ti,
            "track": t,
            "lanes": lanes,
            "events": ev,
            "clips": clips,
            "chokers": hat_chokers(p, events, ti),
            "deps": deps,
        });
        TrackKey {
            shared: shared.clone(),
            own: own.to_string(),
        }
    }

    /// Key spec, bus part (the DAG's second level; the master, which every
    /// edit reaches, is never cached and re-mixes on every render). A bus
    /// output is its own chain, fader and balance over the sum of what
    /// feeds it, so its key is the canonical JSON of
    /// - "bi" and "bus": the bus index and the whole bus (effects, volume,
    ///   pan, mute, output), and every automation lane on its name;
    /// - "deps": the sidechain inputs its chain listens to (as for tracks);
    /// - "tracks": in track order, each audible track that feeds it (its
    ///   output routed here, or a send here): the track's own key (its
    ///   pre-fader buffer), volume_db, its volume lanes, whether its output
    ///   lands here, and each send here with its level, pre/post fader and
    ///   the send lane that drives it;
    /// - "buses": in processing order, the full key of each bus routed into
    ///   it.
    ///
    /// So an edit dirties exactly the buses downstream of the tracks it
    /// re-synthesises or whose fader/send it moves (a sidechain target's
    /// buses included); a send added to one bus leaves every other bus's
    /// key alone. The shared part is the tracks' shared part.
    fn bus_keys(
        p: &Project,
        track_keys: &[TrackKey],
        triggers: &HashMap<String, Vec<usize>>,
    ) -> Vec<Option<TrackKey>> {
        let Some(shared) = track_keys.first().map(|k| k.shared.clone()) else {
            return vec![None; p.buses.len()];
        };
        let any_solo = p.tracks.iter().any(|t| t.solo);
        // which buses feed which (the render's own routing: feeders first,
        // a destination already processed sends to the master instead)
        let order = bus_order(p);
        let mut taken = vec![false; p.buses.len()];
        let mut feeders: Vec<Vec<usize>> = vec![Vec::new(); p.buses.len()];
        for &bi in &order {
            if taken[bi] {
                continue;
            }
            taken[bi] = true;
            let b = &p.buses[bi];
            if b.mute {
                continue;
            }
            if let Some(d) = b
                .output
                .as_deref()
                .and_then(|o| p.bus_index(o).ok())
                .filter(|d| !taken[*d])
            {
                feeders[d].push(bi);
            }
        }
        let mut out: Vec<Option<TrackKey>> = vec![None; p.buses.len()];
        let mut seen = vec![false; p.buses.len()];
        for &bi in &order {
            if seen[bi] {
                continue;
            }
            seen[bi] = true;
            let bus = &p.buses[bi];
            let lanes: Vec<&AutomationLane> = owner_lanes(p, &bus.name);
            let deps: Vec<Value> = chain_sources(&bus.effects)
                .into_iter()
                .map(|s| {
                    let on: Vec<&crate::project::AudioClip> = p
                        .audio_clips
                        .iter()
                        .filter(|c| c.track.to_lowercase() == s)
                        .collect();
                    serde_json::json!({ "source": s, "triggers": triggers.get(&s), "clips": on })
                })
                .collect();
            let mut tracks: Vec<Value> = Vec::new();
            for (ti, t) in p.tracks.iter().enumerate() {
                let audible = !t.mute && (!any_solo || t.solo);
                if !audible {
                    continue;
                }
                let here = t.output.as_deref().and_then(|b| p.bus_index(b).ok()) == Some(bi);
                let tl_lanes = owner_lanes(p, &t.name);
                let sends: Vec<Value> = t
                    .sends
                    .iter()
                    .filter(|s| p.bus_index(&s.bus).ok() == Some(bi))
                    .map(|s| {
                        let lane = tl_lanes.iter().find(|x| match parse_target(&x.param) {
                            Some(Target::Send(b)) => s.bus.eq_ignore_ascii_case(&b) || (!s.id.is_empty() && s.id.eq_ignore_ascii_case(&b)),
                            _ => false,
                        });
                        serde_json::json!({ "db": s.db.to_bits(), "pre_fader": s.pre_fader, "lane": lane })
                    })
                    .collect();
                if !here && sends.is_empty() {
                    continue;
                }
                let vol: Vec<&&AutomationLane> = tl_lanes
                    .iter()
                    .filter(|x| parse_target(&x.param) == Some(Target::Volume))
                    .collect();
                tracks.push(serde_json::json!({
                    "ti": ti,
                    "own": track_keys[ti].own,
                    "volume_db": t.volume_db.to_bits(),
                    "volume_lanes": vol,
                    "output_here": here,
                    "sends": sends,
                }));
            }
            let buses: Vec<&str> = feeders[bi]
                .iter()
                .filter_map(|&f| out[f].as_ref().map(|k| k.own.as_str()))
                .collect();
            let own = serde_json::json!({
                "bi": bi,
                "bus": bus,
                "lanes": lanes,
                "deps": deps,
                "tracks": tracks,
                "buses": buses,
            });
            out[bi] = Some(TrackKey {
                shared: shared.clone(),
                own: own.to_string(),
            });
        }
        out
    }

    /// Every track's key for a render of `p` (what a cached buffer must
    /// match to be reused).
    pub fn keys(p: &Project, bank: &SampleBank, opts: &RenderOptions) -> Vec<TrackKey> {
        let (events, body_len) = schedule(p, opts);
        let total = body_len + (opts.tail.max(0.0) * SR) as usize;
        let (triggers, _) = sidechain_inputs(p, bank, opts, &events, total);
        Self::keys_of(p, opts, &events, total, &triggers)
    }

    fn keys_of(
        p: &Project,
        opts: &RenderOptions,
        events: &[Vec<Event>],
        total: usize,
        triggers: &HashMap<String, Vec<usize>>,
    ) -> Vec<TrackKey> {
        let g = Self::shared_key(p, opts, total);
        (0..p.tracks.len())
            .map(|ti| Self::track_key(&g, p, ti, events, triggers))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}

/// Where the closed hats of every unmuted track choke an open hat (sorted
/// note-on positions); empty for any other track.
fn hat_chokers(p: &Project, events: &[Vec<Event>], ti: usize) -> Vec<usize> {
    let open_hat = matches!(&p.tracks[ti].instrument,
        Instrument::Drum(d) if d.kind == crate::instruments::DrumKind::OpenHat);
    if !open_hat {
        return Vec::new();
    }
    let mut v: Vec<usize> = p
        .tracks
        .iter()
        .zip(events.iter())
        .filter(|(t, _)| {
            !t.mute
                && matches!(&t.instrument, Instrument::Drum(d)
                    if d.kind == crate::instruments::DrumKind::ClosedHat)
        })
        .flat_map(|(_, ev)| ev.iter().map(|x| x.start))
        .collect();
    v.sort_unstable();
    v
}

/// The sidechain inputs of one render, shared by every track's effect
/// chain (FxContext): note-on positions per track name (lowercase) plus the
/// onsets of the audio clips on it, and the clip audio of each track a
/// level-keyed dynamic EQ follows.
pub(crate) fn sidechain_inputs(
    p: &Project,
    bank: &SampleBank,
    opts: &RenderOptions,
    events: &[Vec<Event>],
    total: usize,
) -> (HashMap<String, Vec<usize>>, HashMap<String, Vec<f32>>) {
    let tcurve = crate::tempo_curve::TempoCurve::new(p);
    let mut triggers: HashMap<String, Vec<usize>> = p
        .tracks
        .iter()
        .zip(events.iter())
        .map(|(t, ev)| (t.name.to_lowercase(), ev.iter().map(|e| e.start).collect()))
        .collect();
    // tracks a level-keyed dynamic EQ follows get their clips' mono audio laid
    // out on this buffer's timeline (the key the band's envelope follower reads)
    let level_srcs: std::collections::HashSet<String> = p
        .tracks
        .iter()
        .flat_map(|t| t.effects.iter())
        .filter_map(|f| match f {
            crate::fx::Effect::DynamicEq(d)
                if d.key == "level" && !d.source.is_empty() && !d.bypass =>
            {
                Some(d.source.to_lowercase())
            }
            _ => None,
        })
        .collect();
    let mut levels: HashMap<String, Vec<f32>> = HashMap::new();
    // an audio clip (a vocal take) triggers a sidechain at every onset in it,
    // so "duck the pads under the vocal" follows the words, not just the
    // clip's first sample
    for c in &p.audio_clips {
        let Some(data) = bank.get(&c.sample) else {
            continue;
        };
        let r0 = opts.step_range.map(|r| r.0).unwrap_or(0) as f32;
        let off = ((c.offset_s.max(0.0) * SR) as usize).min(data.len());
        let avail = data.len() - off;
        let len = c
            .length_s
            .map(|l| ((l.max(0.0) * SR) as usize).min(avail))
            .unwrap_or(avail);
        let song_steps = p.song_steps() as f32;
        // a stretched clip plays its take slower/faster on the timeline, so
        // its ducking must follow the stretched material, not the source
        let stretched;
        let span: &[f32] = if c.stretch != 1.0 && len > 0 {
            stretched = crate::audio_edit::time_stretch(&data[off..off + len], c.stretch);
            &stretched
        } else {
            &data[off..off + len]
        };
        let ons = audio_onsets(span);
        let tr = triggers.entry(c.track.to_lowercase()).or_default();
        for lp in 0..opts.loops.max(1) {
            let start = if tcurve.is_constant() {
                ((c.start_beat * 4.0 + lp as f32 * song_steps - r0) * p.step_secs() * SR).round()
                    as i64
            } else {
                let abs = (c.start_beat * 4.0 + lp as f32 * song_steps) as f64;
                ((song_secs_at_step(&tcurve, song_steps as f64, abs)
                    - song_secs_at_step(&tcurve, song_steps as f64, r0 as f64))
                    * SR as f64)
                    .round() as i64
            };
            for &o in &ons {
                let at = start + o as i64;
                if at >= 0 && (at as usize) < total {
                    tr.push(at as usize);
                }
            }
            if level_srcs.contains(&c.track.to_lowercase()) {
                let buf = levels
                    .entry(c.track.to_lowercase())
                    .or_insert_with(|| vec![0.0; total]);
                for (k, &v) in span.iter().enumerate() {
                    let at = start + k as i64;
                    if at >= 0 && (at as usize) < total {
                        buf[at as usize] += v;
                    }
                }
            }
        }
        tr.sort_unstable();
        tr.dedup();
    }
    (triggers, levels)
}

/// Onsets in a mono take (sample positions): 10 ms frames whose level jumps
/// 6 dB over the quietest of the previous 60 ms, above -45 dBFS, at most one
/// every 90 ms; a held sound re-triggers every 120 ms so a ducker stays down.
pub fn audio_onsets(x: &[f32]) -> Vec<usize> {
    let hop = (0.01 * SR) as usize;
    let n = x.len() / hop.max(1);
    let db: Vec<f32> = (0..n)
        .map(|i| {
            let s = &x[i * hop..(i + 1) * hop];
            let ms = s.iter().map(|v| v * v).sum::<f32>() / hop as f32;
            10.0 * ms.max(1e-12).log10()
        })
        .collect();
    let mut out = Vec::new();
    let mut last: i64 = -100;
    for i in 0..n {
        if db[i] < -45.0 {
            continue;
        }
        let lo = db[i.saturating_sub(6)..i]
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min);
        let jump = i == 0 || db[i] - lo > 6.0;
        let gap = i as i64 - last;
        if (jump && gap >= 9) || gap >= 12 {
            out.push(i * hop);
            last = i as i64;
        }
    }
    out
}

pub fn render(p: &Project, bank: &SampleBank, opts: &RenderOptions) -> Result<Mix> {
    render_cached(p, bank, opts, None)
}

/// `render`, reusing (and filling) a pre-fader track cache.
pub fn render_cached(
    p: &Project,
    bank: &SampleBank,
    opts: &RenderOptions,
    cache: Option<&mut TrackCache>,
) -> Result<Mix> {
    render_impl(p, bank, opts, cache, None)
}

/// A `step_range` render whose notes keep the round-robin / variation
/// index they have in the whole song: `index_base[track]` is how many of
/// that track's notes sound before the range. A window rendered this way
/// matches the same bars of a full render (crate::live_splice).
pub fn render_window(
    p: &Project,
    bank: &SampleBank,
    opts: &RenderOptions,
    index_base: &[usize],
) -> Result<Mix> {
    render_impl(p, bank, opts, None, Some(index_base))
}

/// The song-time mapping of one render and the buffer's place in the song
/// (in samples, for LFO phase; the same arithmetic as live_splice's splice
/// offset). render() builds its timeline here, and post_master stem export
/// uses the same one so master automation lands on the same samples.
pub(crate) fn render_timeline(p: &Project, opts: &RenderOptions) -> (Timeline, usize) {
    let mut tl = Timeline::new(p, opts);
    let tcurve = crate::tempo_curve::TempoCurve::new(p);
    let offset = match opts.step_range {
        Some((r0, _)) if r0 > 0 => {
            if tcurve.is_constant() {
                (r0 as f64 * p.step_secs() as f64 * SR as f64) as usize
            } else {
                (song_secs_at_step(&tcurve, p.song_steps() as f64, r0 as f64) * SR as f64) as usize
            }
        }
        _ => 0,
    };
    tl.sample0 = offset;
    (tl, offset)
}

/// The sidechain context and timeline a render of `p` with `opts` gives
/// its master chain: triggers and level keys (sidechain_inputs), the song
/// timeline and the buffer offset (render_timeline), and the buffer length.
/// post_master stem export builds its FxContext from these, so a master
/// effect keyed off a track (a ducker, a level-keyed dynamic EQ) or an LFO
/// sees exactly what it sees in render().
pub(crate) struct MasterInputs {
    pub triggers: HashMap<String, Vec<usize>>,
    pub levels: HashMap<String, Vec<f32>>,
    pub tl: Timeline,
    pub offset: usize,
    pub total: usize,
}

pub(crate) fn master_inputs(p: &Project, bank: &SampleBank, opts: &RenderOptions) -> MasterInputs {
    let (events, body_len) = schedule(p, opts);
    let total = body_len + (opts.tail.max(0.0) * SR) as usize;
    let (triggers, levels) = sidechain_inputs(p, bank, opts, &events, total);
    let (tl, offset) = render_timeline(p, opts);
    MasterInputs {
        triggers,
        levels,
        tl,
        offset,
        total,
    }
}

/// The master section, in render order: master fader (with its `volume`
/// lane), the master DC blocker (R 0.995, see [`DcBlock`]), the master
/// effect chain (with its automation lanes and the render's sidechain
/// context), then non-finite samples to zero. render() runs it on the
/// master sum; post_master stem export runs it on each stem.
pub(crate) fn master_section(
    p: &Project,
    ml: &mut Vec<f32>,
    mr: &mut Vec<f32>,
    ctx: &FxContext,
    tl: &Timeline,
) {
    let master_lanes = owner_lanes(p, "master");
    apply_fader(ml, mr, p.master_volume_db, &master_lanes, tl, None);
    // DC block before the master chain so the limiter has the last word
    let (mut dl, mut dr) = (DcBlock::default(), DcBlock::default());
    for (a, b) in ml.iter_mut().zip(mr.iter_mut()) {
        *a = dl.process(*a);
        *b = dr.process(*b);
    }
    process_chain(&p.master_effects, &master_lanes, ml, mr, ctx, tl);
    for (a, b) in ml.iter_mut().zip(mr.iter_mut()) {
        if !a.is_finite() {
            *a = 0.0;
        }
        if !b.is_finite() {
            *b = 0.0;
        }
    }
}

/// Track synthesis for one track, before the fader: voices, audio clips,
/// pan/spread and the track's effect chain. Extracted verbatim from
/// render_impl's per-track closure so the RT engine's MixNode (rt.rs) and
/// mix() run the SAME code bit-for-bit. Pure move - no math, order or
/// value changes.
pub(crate) struct TrackSynth<'a> {
    pub p: &'a Project,
    pub bank: &'a SampleBank,
    pub opts: &'a RenderOptions,
    pub events: &'a [Vec<Event>],
    pub total: usize,
    pub tl: &'a Timeline,
    pub tcurve: &'a crate::tempo_curve::TempoCurve,
    pub ctx: &'a FxContext<'a>,
    pub index_base: Option<&'a [usize]>,
}

/// Pre-chain track synthesis: voices (mono/mono2 with stereo-spread
/// second set), audio clips, pan/spread into (l, r) - everything before
/// the track FX chain. Factored out of render_track_synth so the RT
/// engine's chunked voice streamer (rt.rs) can be tested bit-identically
/// against this exact pre-chain output. Pure refactor, no behavior change.
pub(crate) struct PlannedVoice {
    pub start: usize,
    pub audio: std::sync::Arc<(Vec<f32>, Vec<f32>)>,
    pub choke_after: Option<usize>,
    pub xfade_in: bool,
    pub start_fade: usize,
}

/// Voice schedule for one track: every note as a separately-owned buffer
/// with its fade/choke parameters, in onset order. Pure extraction of the
/// render_track_synth voice loop - same renders, same cache keys, same
/// order - so both the one-shot renderer and the RT engine's chunked
/// streamer build on one source (bit-identity by construction).
pub(crate) fn plan_track_voices(
    p: &Project,
    bank: &SampleBank,
    events: &[Vec<Event>],
    total: usize,
    tl: &Timeline,
    index_base: Option<&[usize]>,
    ti: usize,
) -> Vec<PlannedVoice> {
    let track = &p.tracks[ti];
    let lanes = owner_lanes(p, &track.name);
    let inst_lanes: Vec<(&AutomationLane, Vec<String>)> = lanes
        .iter()
        .filter_map(|l| match parse_target(&l.param) {
            Some(Target::Instrument(path)) => Some((*l, path)),
            _ => None,
        })
        .collect();
    let base_inst = if inst_lanes.is_empty() {
        Value::Null
    } else {
        serde_json::to_value(&track.instrument).unwrap_or_default()
    };
    let spread = track.instrument.stereo_spread().clamp(0.0, 1.0);
    let open_hat = matches!(&track.instrument,
        Instrument::Drum(d) if d.kind == crate::instruments::DrumKind::OpenHat);
    // mono voices choke themselves: 808s, and open hats (which
    // the closed hats of any other track also choke)
    // a kick chokes its own previous hit too (critic v17: a long
    // lo-fi kick still rang at 0.6 full scale when the syncopated
    // kick on step 7 landed, and the two summed into a ~200 Hz
    // step every 2 bars, the "x.32" events)
    let kick = matches!(&track.instrument,
        Instrument::Drum(d) if d.kind == crate::instruments::DrumKind::Kick);
    let mono_voice = matches!(track.instrument, Instrument::Bass808(_)) || open_hat || kick;
    let hat_chokers = hat_chokers(p, events, ti);
    // phase / time a slid-into 808 note continues from (legato)
    let legato_of = |k: usize| -> Option<crate::instruments::Legato808> {
        let Instrument::Bass808(b) = &track.instrument else {
            return None;
        };
        let e = &events[ti][k];
        let prev = events[ti][..k].iter().rev().find(|x| x.start < e.start)?;
        let target = prev.slide_to?;
        let off = e.start - prev.start;
        // the previous voice glided into this pitch and is still
        // held (its gate runs up to this onset) when it is choked
        let held = (off as f32) <= (prev.gate * SR) + 0.01 * SR;
        let next_of_prev = events[ti][..=k]
            .iter()
            .map(|x| x.start)
            .find(|&s| s > prev.start);
        ((target - e.pitch).abs() < 0.5 && held && next_of_prev == Some(e.start)).then(|| {
            crate::instruments::Legato808 {
                phase: crate::instruments::phase_808_after(
                    b,
                    prev.pitch,
                    prev.gate,
                    prev.slide_to,
                    off,
                ),
                elapsed_s: off as f32 / SR,
            }
        })
    };
    // round-robin: drum hits cycle through 4 renders (each with
    // its own micro-variation) instead of one cached buffer
    let round_robin = track.instrument.is_drum();
    let mut cache: HashMap<
        (i32, u8, u32, Vec<i64>, i32, u8),
        std::sync::Arc<(Vec<f32>, Vec<f32>)>,
    > = HashMap::new();
    let mut voices = Vec::new();
    // sample-cut voices get a longer fade-in than synthesized ones
    let start_fade = if matches!(
        track.instrument,
        Instrument::Sampler(_) | Instrument::Multisample(_) | Instrument::Granular(_)
    ) || matches!(&track.instrument, Instrument::Drum(d) if matches!(d.kind,
        crate::instruments::DrumKind::Crash
            | crate::instruments::DrumKind::Shaker
            | crate::instruments::DrumKind::Cowbell
            | crate::instruments::DrumKind::Tom
            | crate::instruments::DrumKind::Kick))
    {
        // noise cymbals/perc and the kick's attack start at full
        // scale: 6-7 kHz / 1 kHz edges on the downbeats (critic v16)
        SAMPLE_START_FADE
    } else {
        START_FADE
    };
    // where the previous mono voice (808) would still be ringing:
    // a retrigger over a ringing 808 crossfades in with the exact
    // complement of the choke instead of stepping in at phase 0
    // (critic v15: a low kink at every 808 retrigger)
    let mut prev_voice_end = 0usize;
    for (k, e) in events[ti].iter().enumerate() {
        if e.start >= total {
            continue;
        }
        // instrument automation is sampled at note-on
        let beat = tl.beat_at(e.start);
        let vals: Vec<f32> = inst_lanes
            .iter()
            .map(|(l, _)| {
                l.value_at_in(beat, automation::is_log_param(&l.param))
                    .unwrap_or(0.0)
            })
            .collect();
        // the note's index in the whole song (a window render
        // starts later in the track's notes)
        let kr = k + index_base.and_then(|b| b.get(ti).copied()).unwrap_or(0);
        let key = (
            (e.pitch * 10.0) as i32,
            (e.vel * 127.0) as u8,
            (e.gate * 1000.0) as u32,
            vals.iter().map(|v| (v * 1000.0).round() as i64).collect(),
            e.slide_to.map(|x| (x * 10.0) as i32).unwrap_or(-1),
            if round_robin { (kr % 4) as u8 } else { 0 },
        );
        let seed = if round_robin {
            (ti as u64) << 32 | (kr as u64 % 4)
        } else {
            (ti as u64) << 32 | (kr as u64 % 7)
        };
        let legato = if mono_voice { legato_of(k) } else { None };
        let audio: std::sync::Arc<(Vec<f32>, Vec<f32>)> =
            if let (Some(lg), Instrument::Bass808(b)) = (legato, &track.instrument) {
                // legato voices depend on the hand-off phase: not cached
                std::sync::Arc::new((
                    crate::instruments::render_808_voice(
                        b,
                        e.pitch,
                        e.vel.clamp(0.0, 1.0),
                        e.gate,
                        e.slide_to,
                        Some(lg),
                    ),
                    Vec::new(),
                ))
            } else {
                cache
                    .entry(key)
                    .or_insert_with(|| {
                        let inst: Instrument = if inst_lanes.is_empty() {
                            track.instrument.clone()
                        } else {
                            let mut v = base_inst.clone();
                            for ((_, path), x) in inst_lanes.iter().zip(vals.iter()) {
                                automation::set_path(&mut v, path, *x);
                            }
                            serde_json::from_value(v).unwrap_or_else(|_| track.instrument.clone())
                        };
                        if let Instrument::SoundFont(sp) = &inst {
                            // recorded instruments render in stereo
                            return std::sync::Arc::new(crate::soundfont::render_note_stereo(
                                sp, e.pitch, e.vel, e.gate,
                            ));
                        }
                        let a = render_note_slide(
                            &inst, e.pitch, e.vel, e.gate, bank, seed, e.slide_to,
                        );
                        let b = if spread > 0.0 {
                            render_note_slide(
                                &inst,
                                e.pitch,
                                e.vel,
                                e.gate,
                                bank,
                                seed ^ 0xA5A5_5A5A,
                                e.slide_to,
                            )
                        } else {
                            Vec::new()
                        };
                        std::sync::Arc::new((a, b))
                    })
                    .clone()
            };
        // mono voices (808s) choke the previous note at the next onset
        let choke_at = if mono_voice {
            let own = events[ti][k + 1..]
                .iter()
                .map(|x| x.start)
                .find(|&s| s > e.start);
            let hat = hat_chokers
                .get(hat_chokers.partition_point(|&s| s <= e.start))
                .copied();
            match (own, hat) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
            .map(|s| s - e.start)
        } else {
            None
        };
        let retrig =
            matches!(track.instrument, Instrument::Bass808(_)) && k > 0 && prev_voice_end > e.start;
        let xf = legato.is_some() || retrig;
        // the unchoked length: was it still ringing when the next onset choked it?
        prev_voice_end = e.start + audio.0.len();
        voices.push(PlannedVoice {
            start: e.start,
            audio,
            choke_after: choke_at,
            xfade_in: xf,
            start_fade,
        });
    }
    voices
}

pub(crate) struct PlannedClip<'a> {
    pub start: i64,
    pub data: &'a [f32],
    /// The clip's audio transformed by `AudioClip` (stretch and/or
    /// `pitch_st`; unstretched pitch keeps the source length, a stretch
    /// resizes `len` to the placed span); Arc so the block renderer (rt)
    /// can share it without a borrow tie. None renders the bank audio
    /// untouched, bit for bit.
    pub shifted: Option<std::sync::Arc<Vec<f32>>>,
    pub off: usize,
    pub len: usize,
    pub gain: f32,
    pub fade_in_ms: Option<f32>,
    pub fade_out_ms: Option<f32>,
}

impl PlannedClip<'_> {
    /// The samples that actually play: the transformed span when the clip
    /// is stretched and/or pitched, else the bank span. Either way exactly
    /// `len` samples (a stretch makes `len` the placed length).
    pub(crate) fn span(&self) -> &[f32] {
        self.shifted
            .as_deref()
            .map_or_else(|| &self.data[self.off..self.off + self.len], |v| &v[..])
    }
}

/// Audio-clip schedule for one track: one entry per (clip, loop pass), in
/// the one-shot renderer's iteration order, with the same start-sample
/// arithmetic (tempo curve or constant steps). Pure extraction.
pub(crate) fn plan_track_clips<'a>(
    p: &'a Project,
    bank: &'a SampleBank,
    opts: &RenderOptions,
    tcurve: &crate::tempo_curve::TempoCurve,
    ti: usize,
) -> Vec<PlannedClip<'a>> {
    let track = &p.tracks[ti];
    // audio clips on this track's channel (a vocal take, a long sample)
    let mut out = Vec::new();
    for c in p
        .audio_clips
        .iter()
        .filter(|c| c.track.eq_ignore_ascii_case(&track.name))
    {
        let Some(data) = bank.get(&c.sample) else {
            continue;
        };
        let step = p.step_secs();
        let r0 = opts.step_range.map(|r| r.0).unwrap_or(0) as f32;
        let song_steps = p.song_steps() as f32;
        let off = ((c.offset_s.max(0.0) * SR) as usize).min(data.len());
        let avail = data.len() - off;
        let len = c
            .length_s
            .map(|l| ((l.max(0.0) * SR) as usize).min(avail))
            .unwrap_or(avail);
        let g = crate::dsp::db_to_gain(c.gain_db);
        // Stretch changes the placed length (the clip's end follows it from
        // its beat); pitch, either mode, keeps the length of what it gets.
        let stretched = c.stretch != 1.0 && len > 0;
        let (shifted, placed) = if (c.pitch_st != 0.0 || stretched) && len > 0 {
            let span = &data[off..off + len];
            let mut audio = if stretched {
                crate::audio_edit::time_stretch(span, c.stretch)
            } else {
                span.to_vec()
            };
            if c.pitch_st != 0.0 {
                audio = if c.pitch_voice {
                    // voice mode: constant-correction TD-PSOLA (formants
                    // kept), cascaded in steps of at most 6 semitones. A
                    // single big step lands the synthesis hop on a ratio
                    // commensurate with the period (2:1 at +12, 1:2 at -12)
                    // where the windowed overlap cancels a steady tone; half
                    // steps stay incommensurate.
                    let steps = ((c.pitch_st.abs() / 6.0).ceil() as usize).max(1);
                    let per = c.pitch_st / steps as f32;
                    let mut cur = audio;
                    for _ in 0..steps {
                        let frames = crate::vocal::pitch_track(&cur, crate::dsp::SR);
                        let corr = vec![per; frames.len()];
                        cur = crate::vocal::psola(&cur, crate::dsp::SR, &frames, &corr, 0.5, 2.0);
                    }
                    cur
                } else {
                    crate::audio_edit::pitch_shift(&audio, c.pitch_st)
                };
            }
            if !stretched {
                audio.resize(len, 0.0); // span() promises exactly len samples
            }
            let placed = audio.len();
            (Some(std::sync::Arc::new(audio)), placed)
        } else {
            (None, len)
        };
        for lp in 0..opts.loops.max(1) {
            let start_step = c.start_beat * 4.0 + lp as f32 * song_steps - r0;
            let start = if tcurve.is_constant() {
                (start_step * step * SR).round() as i64
            } else {
                let abs = (c.start_beat * 4.0 + lp as f32 * song_steps) as f64;
                ((song_secs_at_step(tcurve, song_steps as f64, abs)
                    - song_secs_at_step(tcurve, song_steps as f64, r0 as f64))
                    * SR as f64)
                    .round() as i64
            };
            out.push(PlannedClip {
                start,
                data,
                shifted: shifted.clone(),
                off,
                len: placed,
                gain: g,
                fade_in_ms: c.fade_in_ms,
                fade_out_ms: c.fade_out_ms,
            });
        }
    }
    out
}

/// Pre-chain track synthesis: voices (mono/mono2 with stereo-spread
pub(crate) fn render_track_prechain(args: &TrackSynth<'_>, ti: usize) -> (Vec<f32>, Vec<f32>) {
    let TrackSynth {
        p,
        total,
        tl,
        bank: _,
        opts: _,
        events: _,
        tcurve: _,
        ctx: _,
        index_base: _,
    } = *args;
    let track = &p.tracks[ti];
    let lanes = owner_lanes(p, &track.name);
    let spread = track.instrument.stereo_spread().clamp(0.0, 1.0);
    let TrackSynth {
        bank,
        opts,
        events,
        tcurve,
        index_base,
        ..
    } = *args;
    let voices = plan_track_voices(p, bank, events, total, tl, index_base, ti);
    let clips = plan_track_clips(p, bank, opts, tcurve, ti);
    let mut mono = vec![0.0f32; total];
    // decorrelated second render for stereo unison spread
    let mut mono2 = if spread > 0.0 {
        vec![0.0f32; total]
    } else {
        Vec::new()
    };
    for v in &voices {
        add_voice_faded(
            &mut mono,
            v.start,
            &v.audio.0,
            v.choke_after,
            v.xfade_in,
            v.start_fade,
        );
        if spread > 0.0 && !v.audio.1.is_empty() {
            add_voice_faded(
                &mut mono2,
                v.start,
                &v.audio.1,
                v.choke_after,
                v.xfade_in,
                v.start_fade,
            );
        }
    }
    let fade = (0.004 * SR) as usize;
    for c in &clips {
        let span = c.span();
        for i in 0..c.len {
            let o = c.start + i as i64;
            if o < 0 {
                continue;
            }
            let o = o as usize;
            if o >= total {
                break;
            }
            let env = if c.fade_in_ms.is_some() || c.fade_out_ms.is_some() {
                // equal-power edges (seamless edits)
                let fi = (c.fade_in_ms.unwrap_or(4.0).max(0.5) / 1000.0 * SR).max(1.0);
                let fo = (c.fade_out_ms.unwrap_or(4.0).max(0.5) / 1000.0 * SR).max(1.0);
                let a = (i as f32 / fi).min(1.0);
                let b = ((c.len - i) as f32 / fo).min(1.0);
                (a * std::f32::consts::FRAC_PI_2).sin() * (b * std::f32::consts::FRAC_PI_2).sin()
            } else {
                (i as f32 / fade as f32)
                    .min((c.len - i) as f32 / fade as f32)
                    .min(1.0)
            };
            let v = span[i] * c.gain * env;
            mono[o] += v;
            if spread > 0.0 {
                mono2[o] += v;
            }
        }
    }
    // pan (equal power), optionally automated
    let pan_lane = lanes
        .iter()
        .find(|x| parse_target(&x.param) == Some(Target::Pan));
    let pan_curve = pan_lane.map(|lane| lane_curve(lane, total, tl, |v| v.clamp(-1.0, 1.0)));
    let gains = |pan: f32| {
        let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
        (
            angle.cos() * std::f32::consts::SQRT_2,
            angle.sin() * std::f32::consts::SQRT_2,
        )
    };
    let (gl, gr) = gains(track.pan);
    let mut l = Vec::with_capacity(total);
    let mut r = Vec::with_capacity(total);
    for (i, x) in mono.iter().enumerate() {
        let (a, b) = match &pan_curve {
            Some(c) => gains(c[i]),
            None => (gl, gr),
        };
        let xr = if spread > 0.0 {
            // left = voice set A, right = blend toward the independent set B
            x * (1.0 - spread) + mono2[i] * spread
        } else {
            *x
        };
        l.push(x * a);
        r.push(xr * b);
    }
    drop(mono);
    drop(mono2);
    (l, r)
}

pub(crate) fn render_track_synth(
    args: &TrackSynth<'_>,
    ti: usize,
    cache_ok: bool,
) -> (
    Vec<f32>,
    Vec<f32>,
    Option<std::sync::Arc<(Vec<f32>, Vec<f32>)>>,
) {
    let TrackSynth { p, ctx, tl, .. } = *args;
    let track = &p.tracks[ti];
    let lanes = owner_lanes(p, &track.name);
    let (mut l, mut r) = render_track_prechain(args, ti);
    process_chain(&track.effects, &lanes, &mut l, &mut r, ctx, tl);
    let fresh = cache_ok.then(|| std::sync::Arc::new((l.clone(), r.clone())));
    (l, r, fresh)
}

fn render_impl(
    p: &Project,
    bank: &SampleBank,
    opts: &RenderOptions,
    cache: Option<&mut TrackCache>,
    index_base: Option<&[usize]>,
) -> Result<Mix> {
    // subnormals off for this render (rayon's workers set it per track below)
    let ftz = crate::dsp::flush_denormals();
    let _no_denormals = ftz.then(crate::dsp::NoDenormals::new);
    if !opts.allow_missing_clips {
        let missing = missing_clip_samples(p, bank);
        if !missing.is_empty() {
            anyhow::bail!(
                "{} audio clip(s) point at samples that are not loaded, so they would render silent: {}. Fix the clip's sample name (list_samples / list_audio_clips) or import the sample; allow_missing_clips renders anyway.",
                missing.len(),
                missing.iter().take(8).cloned().collect::<Vec<_>>().join("; ")
            );
        }
    }
    let mut cache = cache;
    let (events, body_len) = schedule(p, opts);
    let total = body_len + (opts.tail.max(0.0) * SR) as usize;
    let (triggers, levels) = sidechain_inputs(p, bank, opts, &events, total);
    let track_keys: Vec<TrackKey> = if cache.is_some() {
        TrackCache::keys_of(p, opts, &events, total, &triggers)
    } else {
        Vec::new()
    };
    // a cache over its budget first lets go of buffers no track can use
    // any more (their track changed since)
    if let Some(c) = cache.as_deref_mut() {
        let est_mb = ((p.song_steps() as f32 * p.step_secs() * opts.loops.max(1) as f32
            + opts.tail.max(0.0))
            * SR
            * 8.0)
            / 1_048_576.0;
        if c.tracks.len() as f32 * est_mb > budget_mb("BEATBOX_TRACK_CACHE_MB", TRACK_CACHE_MB) {
            c.tracks.retain(|ti, (k, _)| track_keys.get(*ti) == Some(k));
        }
    }
    let empty = HashMap::new();
    let cached: HashMap<usize, std::sync::Arc<(Vec<f32>, Vec<f32>)>> = cache
        .as_deref()
        .map(|c| &c.tracks)
        .unwrap_or(&empty)
        .iter()
        .filter(|(ti, (k, _))| track_keys.get(**ti) == Some(k))
        .map(|(ti, (_, b))| (*ti, b.clone()))
        .collect();
    let keep = cache.is_some();
    // one stereo buffer of this render, MB
    let buf_mb = (total * 8) as f32 / 1_048_576.0;
    // the track cache stays under its budget: tracks already cached keep
    // their slot, new ones are cached in track order while room is left
    // (the rest re-synthesise on the next render: slower, never OOM)
    let cache_ok: Vec<bool> = {
        let slots = (budget_mb("BEATBOX_TRACK_CACHE_MB", TRACK_CACHE_MB) / buf_mb.max(1e-3)).floor()
            as usize;
        let mut used = cached.len();
        (0..p.tracks.len())
            .map(|ti| {
                if !keep || cached.contains_key(&ti) {
                    return false;
                }
                if used < slots {
                    used += 1;
                    true
                } else {
                    false
                }
            })
            .collect()
    };
    let full_stem = |name: &str| -> bool {
        match opts.full_stems {
            None => true,
            Some(f) => f(p, name),
        }
    };
    let any_solo = p.tracks.iter().any(|t| t.solo);
    crate::dsp::memtrace(&format!(
        "render start: {} tracks, {} buses, {:.0} s, stems {}, cached {}",
        p.tracks.len(),
        p.buses.len(),
        total as f32 / SR,
        opts.keep_stems,
        cached.len()
    ));
    let (tl, offset) = render_timeline(p, opts);
    let tcurve = crate::tempo_curve::TempoCurve::new(p);
    let ctx = FxContext {
        step_secs: p.step_secs(),
        triggers: &triggers,
        offset,
        levels: Some(&levels),
    };

    let mut ml = vec![0.0f32; total];
    let mut mr = vec![0.0f32; total];
    let mut bus_in: Vec<(Vec<f32>, Vec<f32>)> = p
        .buses
        .iter()
        .map(|_| (vec![0.0f32; total], vec![0.0f32; total]))
        .collect();
    let mut stems = Vec::new();
    let mut track_infos = Vec::new();

    // Tracks are independent until they are summed: render each track's
    // chain in parallel (a bounded batch at a time to cap memory), then sum
    // in track order so the mix is bit-identical to a serial render.
    struct TrackOut {
        audible: bool,
        l: Vec<f32>,
        r: Vec<f32>,
        pre: Option<(Vec<f32>, Vec<f32>)>,
        info: Option<TrackInfo>,
        fresh: Option<std::sync::Arc<(Vec<f32>, Vec<f32>)>>,
    }
    let render_track = |ti: usize| -> Option<TrackOut> {
        // rayon's threads carry their own float mode
        let _no_denormals = ftz.then(crate::dsp::NoDenormals::new);
        let track = &p.tracks[ti];
        let audible = !track.mute && (!any_solo || track.solo);
        if !audible && !opts.keep_stems && !opts.track_stats {
            return None;
        }
        let lanes = owner_lanes(p, &track.name);
        let (mut l, mut r, fresh) = match cached.get(&ti) {
            Some(b) => (b.0.clone(), b.1.clone(), None),
            None => render_track_synth(
                &TrackSynth {
                    p,
                    bank,
                    opts,
                    events: &events,
                    total,
                    tl: &tl,
                    tcurve: &tcurve,
                    ctx: &ctx,
                    index_base,
                },
                ti,
                cache_ok[ti],
            ),
        };
        let pre: Option<(Vec<f32>, Vec<f32>)> =
            if audible && track.sends.iter().any(|s| s.pre_fader) {
                Some((l.clone(), r.clone()))
            } else {
                None
            };
        apply_fader(&mut l, &mut r, track.volume_db, &lanes, &tl, None);
        let info = (opts.keep_stems || opts.track_stats).then(|| track_info(&track.name, &l, &r));
        Some(TrackOut {
            audible,
            l,
            r,
            pre,
            info,
            fresh,
        })
    };
    let mut fresh_tracks = Vec::new();
    let t_tracks = std::time::Instant::now();
    // tracks in flight at once: each holds up to ~4 stereo buffers (its
    // voices, the panned pair, a pre-fader copy, the cache copy)
    let batch = rayon::current_num_threads().clamp(1, 4).min(
        ((budget_mb("BEATBOX_RENDER_BATCH_MB", RENDER_BATCH_MB) / (4.0 * buf_mb).max(1e-3)).floor()
            as usize)
            .max(1),
    );
    let order: Vec<usize> = (0..p.tracks.len()).collect();
    for chunk in order.chunks(batch) {
        use rayon::prelude::*;
        let outs: Vec<Option<TrackOut>> = chunk.par_iter().map(|&ti| render_track(ti)).collect();
        for (&ti, out) in chunk.iter().zip(outs) {
            let Some(TrackOut {
                audible,
                l,
                r,
                pre,
                info,
                fresh,
            }) = out
            else {
                continue;
            };
            if let Some(f) = fresh {
                fresh_tracks.push((ti, f));
            }
            let track = &p.tracks[ti];
            if audible {
                let send_lanes = owner_lanes(p, &track.name);
                for s in &track.sends {
                    let Ok(bi) = p.bus_index(&s.bus) else {
                        continue;
                    };
                    let (sl, sr) = match (&pre, s.pre_fader) {
                        (Some((pl, pr)), true) => (pl, pr),
                        _ => (&l, &r),
                    };
                    // a `send.<bus>` lane drives the level per sample
                    let lane = send_lanes.iter().find(|x| match parse_target(&x.param) {
                        Some(Target::Send(b)) => {
                            s.bus.eq_ignore_ascii_case(&b)
                                || (!s.id.is_empty() && s.id.eq_ignore_ascii_case(&b))
                        }
                        _ => false,
                    });
                    let (bl, br) = &mut bus_in[bi];
                    match lane {
                        Some(lane) => {
                            let g = lane_curve(lane, total, &tl, |db| {
                                db_to_gain(db.clamp(-120.0, 12.0))
                            });
                            for i in 0..total {
                                bl[i] += sl[i] * g[i];
                                br[i] += sr[i] * g[i];
                            }
                        }
                        None => {
                            let g = db_to_gain(s.db.clamp(-120.0, 12.0));
                            for i in 0..total {
                                bl[i] += sl[i] * g;
                                br[i] += sr[i] * g;
                            }
                        }
                    }
                }
                let dest = track.output.as_deref().and_then(|b| p.bus_index(b).ok());
                let (dl, dr) = match dest {
                    Some(bi) => {
                        let (bl, br) = &mut bus_in[bi];
                        (bl, br)
                    }
                    None => (&mut ml, &mut mr),
                };
                for i in 0..total {
                    dl[i] += l[i];
                    dr[i] += r[i];
                }
            }
            if let Some(info) = info {
                track_infos.push(info);
            }
            if opts.keep_stems {
                if full_stem(&track.name) {
                    stems.push(Stem {
                        name: track.name.clone(),
                        left: l,
                        right: r,
                        bands: None,
                    });
                } else {
                    stems.push(Stem::summarize(track.name.clone(), &l, &r));
                }
            }
        }
    }

    if let Some(c) = cache.as_deref_mut() {
        c.tracks.retain(|ti, _| *ti < track_keys.len());
        for (ti, b) in fresh_tracks {
            c.tracks.insert(ti, (track_keys[ti].clone(), b));
        }
        c.buses.retain(|bi, _| *bi < p.buses.len());
    }
    // bus outputs reused when the bus's key (its state, its sidechain
    // inputs and everything that feeds it) is unchanged
    let bus_keys: Vec<Option<TrackKey>> = if keep {
        TrackCache::bus_keys(p, &track_keys, &triggers)
    } else {
        Vec::new()
    };
    let mut fresh_buses: Vec<(usize, std::sync::Arc<(Vec<f32>, Vec<f32>)>)> = Vec::new();
    crate::producer::prof("render.tracks", t_tracks);
    let t_bus = std::time::Instant::now();
    // buses: own chain + fader + balance, then into their output (another
    // bus = mixer insert routing, or the master), feeders before receivers
    let mut bus_stems = Vec::new();
    let mut bus_in: Vec<Option<(Vec<f32>, Vec<f32>)>> = bus_in.into_iter().map(Some).collect();
    for bi in bus_order(p) {
        let bus = &p.buses[bi];
        let Some((mut l, mut r)) = bus_in[bi].take() else {
            continue;
        };
        let key = bus_keys.get(bi).and_then(|k| k.as_ref());
        let hit = match (cache.as_deref(), key) {
            (Some(c), Some(k)) => c
                .buses
                .get(&bi)
                .filter(|(ck, _)| ck == k)
                .map(|(_, b)| b.clone()),
            _ => None,
        };
        match hit {
            Some(b) => {
                l.copy_from_slice(&b.0);
                r.copy_from_slice(&b.1);
            }
            None => {
                let lanes = owner_lanes(p, &bus.name);
                process_chain(&bus.effects, &lanes, &mut l, &mut r, &ctx, &tl);
                apply_fader(&mut l, &mut r, bus.volume_db, &lanes, &tl, Some(bus.pan));
                if key.is_some() {
                    fresh_buses.push((bi, std::sync::Arc::new((l.clone(), r.clone()))));
                }
            }
        }
        if !bus.mute {
            let dest = bus
                .output
                .as_deref()
                .and_then(|o| p.bus_index(o).ok())
                .filter(|d| bus_in[*d].is_some());
            let (dl, dr) = match dest {
                Some(d) => {
                    let (a, b) = bus_in[d].as_mut().unwrap();
                    (a, b)
                }
                None => (&mut ml, &mut mr),
            };
            for i in 0..total {
                dl[i] += l[i];
                dr[i] += r[i];
            }
        }
        if opts.keep_stems {
            bus_stems.push(Stem {
                name: bus.name.clone(),
                left: l,
                right: r,
                bands: None,
            });
        }
    }

    if let Some(c) = cache {
        // buses share the track cache's budget
        let slots = (budget_mb("BEATBOX_TRACK_CACHE_MB", TRACK_CACHE_MB) / buf_mb.max(1e-3)).floor()
            as usize;
        for (bi, b) in fresh_buses {
            if c.buses.contains_key(&bi) || c.tracks.len() + c.buses.len() < slots {
                if let Some(Some(k)) = bus_keys.get(bi) {
                    c.buses.insert(bi, (k.clone(), b));
                }
            }
        }
    }
    crate::producer::prof("render.buses", t_bus);
    let t_master = std::time::Instant::now();
    master_section(p, &mut ml, &mut mr, &ctx, &tl);
    crate::producer::prof("render.master", t_master);
    crate::dsp::memtrace("render end");
    Ok(Mix {
        seconds: total as f32 / SR,
        left: ml,
        right: mr,
        stems,
        track_info: track_infos,
        bus_stems,
    })
}

/// Write 16-bit stereo PCM WAV.
pub fn write_wav(path: &Path, l: &[f32], r: &[f32]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let n = l.len().min(r.len());
    let data_len = (n * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&2u16.to_le_bytes()); // stereo
    out.extend_from_slice(&(SR as u32).to_le_bytes());
    out.extend_from_slice(&((SR as u32) * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        for s in [l[i], r[i]] {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(&out)?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::project::{Note, Track};

    /// Degenerate-but-parseable project JSONs must never panic render and
    /// never emit NaN/inf: odd values from hand-edited or half-corrupt
    /// project files hit the audio path raw, so the engine has to take
    /// them in stride (clamped, ignored or erroring cleanly). Found by
    /// fuzzing examples/rt_fuzz-style mutations; 0 panics, 0 NaN outputs.
    #[test]
    fn render_degenerate_projects_never_panic_nor_nan() {
        let base = || {
            serde_json::json!({
                "name":"f","bpm":120.0,"swing":0.0,"key_root":"C","scale":"minor",
                "tracks":[{"id":"t1","name":"kick","instrument":{"type":"drum","kind":"kick"},
                    "volume_db":-1.0,"pan":0.0,"mute":false,"solo":false,"effects":[]}],
                "patterns":[{"id":"p1","name":"main","bars":1,"clips":{
                    "kick":[{"start":0.0,"len":1.0,"pitch":36,"vel":0.9,"id":"n1"}]}}],
                "arrangement":[{"pattern":"main","repeats":1}],
                "master_effects":[],"master_volume_db":0.0,"samples":[],
                "schema_version":2,"id_seq":10,"seed_log":[]
            })
        };
        let cases: Vec<fn(&mut serde_json::Value)> = vec![
            |v| v["bpm"] = 0.0.into(),
            |v| v["bpm"] = (-120.0).into(),
            |v| v["bpm"] = 1e300.into(),
            |v| v["swing"] = 5.0.into(),
            |v| v["swing"] = (-1.0).into(),
            |v| v["tracks"] = serde_json::json!([]),
            |v| v["patterns"][0]["bars"] = 0.into(),
            |v| v["patterns"][0]["clips"]["kick"][0]["len"] = (-2.0).into(),
            |v| v["patterns"][0]["clips"]["kick"][0]["start"] = 1e9.into(),
            |v| v["patterns"][0]["clips"]["kick"][0]["pitch"] = 255.into(),
            |v| v["patterns"][0]["clips"]["kick"][0]["vel"] = 1e9.into(),
            |v| v["tracks"][0]["volume_db"] = (-400.0).into(),
            |v| v["tracks"][0]["volume_db"] = 1e38.into(),
            |v| v["tracks"][0]["pan"] = 99.0.into(),
            |v| v["arrangement"] = serde_json::json!([{"pattern":"ghost","repeats":1}]),
            |v| v["arrangement"][0]["repeats"] = 0.into(),
            |v| {
                v["arrangement"] = serde_json::json!([]);
                v["patterns"] = serde_json::json!([]);
            },
            |v| v["key_root"] = "H".into(),
            |v| v["scale"] = "weird".into(),
            |v| v["tracks"][0]["effects"] = serde_json::json!([{"type":"filter","mode":"lowpass","cutoff":-100.0,"resonance":0.2}]),
            |v| {
                v["tracks"][0]["effects"] =
                    serde_json::json!([{"type":"delay","steps":0.0,"feedback":2.0,"mix":0.3}])
            },
            |v| v["tracks"][0]["effects"] = serde_json::json!([{"type":"stutter","slice_steps":0.0,"cycle_steps":8.0,"mix":1.0}]),
            |v| {
                v["tracks"][0]["effects"] =
                    serde_json::json!([{"type":"bitcrush","bits":0.0,"downsample":1,"mix":1.0}])
            },
            |v| {
                v["tracks"][0]["effects"] =
                    serde_json::json!([{"type":"pitch_shift","semitones":1e9}])
            },
            |v| {
                v["tracks"][0]["effects"] =
                    serde_json::json!([{"type":"distortion","drive":0.5,"mix":-1.0}])
            },
            |v| v["master_effects"] = serde_json::json!([{"type":"limiter","ceiling_db":20.0,"release_ms":80.0,"true_peak":true}]),
            |v| v["automation"] = serde_json::json!([{"target":"kick","param":"volume","points":[{"beat":-5.0,"value":0.0},{"beat":2.0,"value":-6.0}]}]),
            |v| v["automation"] = serde_json::json!([{"target":"kick","param":"volume","points":[{"beat":8.0,"value":0.0},{"beat":2.0,"value":-6.0}]}]),
            |v| v["automation"] = serde_json::json!([{"target":"ghost","param":"volume","points":[{"beat":0.0,"value":0.0},{"beat":2.0,"value":-6.0}]}]),
            |v| v["automation"] = serde_json::json!([{"target":"kick","param":"fx.99.nonexistent","points":[{"beat":0.0,"value":0.0},{"beat":2.0,"value":1.0}]}]),
        ];
        let bank = crate::samples::SampleBank::default();
        let opts = RenderOptions {
            tail: 0.1,
            ..Default::default()
        };
        for (i, mutate) in cases.iter().enumerate() {
            let mut v = base();
            mutate(&mut v);
            // a serde rejection is also a fine answer (typed bounds work)
            let Ok(p) = serde_json::from_value::<crate::project::Project>(v) else {
                continue;
            };
            let r =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(&p, &bank, &opts)));
            let mix = match r {
                Ok(Ok(m)) => m,
                Ok(Err(_)) => continue, // clean validation error
                Err(_) => panic!("case {i}: render panicked on degenerate project"),
            };
            assert!(
                mix.left.iter().all(|x| x.is_finite()) && mix.right.iter().all(|x| x.is_finite()),
                "case {i}: NaN/inf in render output"
            );
        }
    }

    /// The pre-fader track cache must not change a single sample: a render
    /// with faders/master moved and tracks reused equals a fresh render.
    #[test]
    fn track_cache_and_parallel_tracks_are_bit_identical() {
        let d = std::env::temp_dir().join("beatbox_render_cache_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 9}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let o = RenderOptions {
            track_stats: true,
            ..Default::default()
        };
        let mut cache = TrackCache::default();
        let a = render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
        assert!(!cache.is_empty());
        // move faders and the master the way balance_mix / master_assistant do
        let mut q = e.project.clone();
        for t in q.tracks.iter_mut() {
            t.volume_db -= 2.5;
        }
        q.master_volume_db += 1.5;
        q.master_effects.clear();
        let cached = render_cached(&q, &e.bank, &o, Some(&mut cache)).unwrap();
        let fresh = render(&q, &e.bank, &o).unwrap();
        assert_eq!(cached.left, fresh.left);
        assert_eq!(cached.right, fresh.right);
        assert_ne!(a.left, fresh.left);
        // serial (1 thread) == parallel
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let serial = pool.install(|| render(&q, &e.bank, &o)).unwrap();
        assert_eq!(serial.left, fresh.left);
        assert_eq!(serial.right, fresh.right);
        // a composition change invalidates the cache
        let mut c = q.clone();
        if let Some(pt) = c.patterns.first_mut() {
            pt.clips.clear();
        }
        let n = render_cached(&c, &e.bank, &o, Some(&mut cache)).unwrap();
        assert_eq!(n.left, render(&c, &e.bank, &o).unwrap().left);
    }

    /// Fundamental of a rendered note (YIN on a 4096-sample frame 0.25 s in).
    fn note_hz(y: &[f32]) -> f32 {
        let at = (0.25 * SR) as usize;
        crate::sc_dsp::yin_aperiodicity(&y[at..at + 4096], SR, 40.0, 2000.0, 0.2)
            .expect("voiced")
            .0
    }

    fn cents(hz: f32, want: f32) -> f32 {
        1200.0 * (hz / want).log2()
    }

    #[test]
    fn tuning_offset_shifts_pitched_tracks_only_and_zero_is_a_no_op() {
        let d = std::env::temp_dir().join("beatbox_render_tuning_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 9}),
        )
        .unwrap();
        let p0 = e.project.clone();
        // 0 is not written and schedules exactly as before
        assert!(!serde_json::to_string(&p0).unwrap().contains("tuning_cents"));
        let mut z = p0.clone();
        z.tuning_cents = 0.0;
        let bits = |ev: &Vec<Vec<Event>>| -> Vec<Vec<(usize, u32, u32, u32, Option<u32>)>> {
            ev.iter()
                .map(|t| {
                    t.iter()
                        .map(|x| {
                            (
                                x.start,
                                x.gate.to_bits(),
                                x.pitch.to_bits(),
                                x.vel.to_bits(),
                                x.slide_to.map(f32::to_bits),
                            )
                        })
                        .collect()
                })
                .collect()
        };
        let (a, _) = schedule(&p0, &RenderOptions::default());
        let (b, _) = schedule(&z, &RenderOptions::default());
        assert_eq!(bits(&a), bits(&b));
        // -44.3 c moves every pitched note by exactly that; drums stay put
        let mut t = p0.clone();
        t.tuning_cents = -44.3;
        assert!(serde_json::to_string(&t)
            .unwrap()
            .contains("\"tuning_cents\":-44.3"));
        let (c, _) = schedule(&t, &RenderOptions::default());
        let (mut moved, mut kept) = (0, 0);
        for (ti, tr) in p0.tracks.iter().enumerate() {
            for (x, y) in a[ti].iter().zip(c[ti].iter()) {
                assert_eq!(
                    (x.start, x.gate.to_bits(), x.vel.to_bits()),
                    (y.start, y.gate.to_bits(), y.vel.to_bits())
                );
                if tr.instrument.follows_tuning() {
                    assert!(
                        (y.pitch - (x.pitch - 0.443)).abs() < 1e-4,
                        "{}: {} -> {}",
                        tr.name,
                        x.pitch,
                        y.pitch
                    );
                    moved += 1;
                } else {
                    assert_eq!(x.pitch.to_bits(), y.pitch.to_bits(), "{} moved", tr.name);
                    kept += 1;
                }
            }
        }
        assert!(moved > 0 && kept > 0, "moved {moved} kept {kept}");
    }

    /// Least-squares fit of `nh` harmonics of f0 (searched over +/-3 cents
    /// around `f0`) to y; returns (best f0, residual/total in dB = THD+N).
    pub(crate) fn harmonic_fit(y: &[f32], sr: f32, f0: f32, nh: usize) -> (f32, f32) {
        let solve = |f: f64| -> f64 {
            let m = 2 * nh;
            let mut ata = vec![0.0f64; m * m];
            let mut aty = vec![0.0f64; m];
            let mut row = vec![0.0f64; m];
            for (i, &v) in y.iter().enumerate() {
                let t = i as f64 / sr as f64;
                for k in 0..nh {
                    let w = std::f64::consts::TAU * f * (k + 1) as f64 * t;
                    row[2 * k] = w.cos();
                    row[2 * k + 1] = w.sin();
                }
                for a in 0..m {
                    aty[a] += row[a] * v as f64;
                    for b in a..m {
                        ata[a * m + b] += row[a] * row[b];
                    }
                }
            }
            for a in 0..m {
                for b in 0..a {
                    ata[a * m + b] = ata[b * m + a];
                }
            }
            // Gaussian elimination
            let mut x = aty.clone();
            let mut g = ata.clone();
            for c in 0..m {
                let p = (c..m)
                    .max_by(|&i, &j| g[i * m + c].abs().total_cmp(&g[j * m + c].abs()))
                    .unwrap();
                if p != c {
                    for k in 0..m {
                        g.swap(c * m + k, p * m + k);
                    }
                    x.swap(c, p);
                }
                let d = g[c * m + c];
                if d.abs() < 1e-12 {
                    continue;
                }
                for r in 0..m {
                    if r != c {
                        let f = g[r * m + c] / d;
                        if f != 0.0 {
                            for k in c..m {
                                g[r * m + k] -= f * g[c * m + k];
                            }
                            x[r] -= f * x[c];
                        }
                    }
                }
            }
            let coef: Vec<f64> = (0..m).map(|c| x[c] / g[c * m + c]).collect();
            let (mut res, mut tot) = (0.0f64, 0.0f64);
            for (i, &v) in y.iter().enumerate() {
                let t = i as f64 / sr as f64;
                let mut s = 0.0;
                for k in 0..nh {
                    let w = std::f64::consts::TAU * f * (k + 1) as f64 * t;
                    s += coef[2 * k] * w.cos() + coef[2 * k + 1] * w.sin();
                }
                res += (v as f64 - s).powi(2);
                tot += (v as f64).powi(2);
            }
            res / tot.max(1e-30)
        };
        let mut best = (f0 as f64, f64::INFINITY);
        for step in -30..=30 {
            let f = f0 as f64 * 2f64.powf(step as f64 * 0.1 / 1200.0);
            let r = solve(f);
            if r < best.1 {
                best = (f, r);
            }
        }
        (best.0 as f32, (10.0 * best.1.max(1e-30).log10()) as f32)
    }

    /// Largest sample-to-sample step relative to the signal's peak.
    pub(crate) fn max_jump(y: &[f32]) -> f32 {
        let pk = y.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-12);
        y.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max)
            / pk
    }

    #[test]
    fn tuning_offset_resampling_is_clean_on_zones() {
        // a sustained harmonic A3 sample (root 57), 4 s, through a pitched
        // zone at tc=0 and tc=-44: f0, THD+N and the largest step
        let n = (5.0 * SR) as usize;
        let src: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / SR;
                (1..=6)
                    .map(|k| (std::f32::consts::TAU * 220.0 * k as f32 * t).sin() * 0.4 / k as f32)
                    .sum::<f32>()
            })
            .collect();
        let mut bank = SampleBank::default();
        bank.insert("harm_a3", src);
        let zone = crate::multisample::Zone {
            sample: "harm_a3".into(),
            root: 57,
            lo_note: 0,
            hi_note: 127,
            vel_lo: 0.0,
            vel_hi: 1.0,
            tune_cents: 0.0,
            gain_db: 0.0,
            rr: 0,
        };
        let ms = Instrument::Multisample(crate::multisample::MultisampleParams {
            zones: vec![zone],
            ..Default::default()
        });
        for (note, label) in [
            (57.0f32, "A3 (rate 1 vs resampled)"),
            (60.0, "C4 (both resampled)"),
        ] {
            let a = crate::instruments::render_note(&ms, note, 0.8, 4.0, &bank, 1);
            let b = crate::instruments::render_note(&ms, note - 0.44, 0.8, 4.0, &bank, 1);
            let (s0, s1) = ((0.5 * SR) as usize, (1.5 * SR) as usize);
            let f_nom = 440.0 * 2f32.powf((note - 69.0) / 12.0);
            let (fa, ta) = harmonic_fit(&a[s0..s1], SR, f_nom, 6);
            let (fb, tb) = harmonic_fit(&b[s0..s1], SR, f_nom * 2f32.powf(-44.0 / 1200.0), 6);
            let off = 1200.0 * (fb / fa).log2();
            let (ja, jb) = (
                max_jump(&a[..(4.0 * SR) as usize]),
                max_jump(&b[..(4.0 * SR) as usize]),
            );
            eprintln!("zone {label}: f0 {fa:.3} -> {fb:.3} Hz = {off:.3} c (want -44); THD+N {ta:.1} -> {tb:.1} dB; max step {ja:.4} -> {jb:.4}");
            assert!((off + 44.0).abs() < 0.5, "{label}: offset {off} c");
            // within 1 dB of tc=0, or under -70 dB outright (at A3 tc=0 is
            // rate 1, a pass-through, so its floor is the fit's own)
            assert!(
                tb <= ta + 1.0 || tb < -70.0,
                "{label}: THD+N {ta} -> {tb} dB"
            );
            assert!(jb <= ja * 1.1 + 1e-3, "{label}: max step {ja} -> {jb}");
        }
    }

    #[test]
    fn tuning_offset_lands_synth_and_pitched_samples_on_the_offset() {
        let want = 440.0 * 2f32.powf(-44.3 / 1200.0); // 428.98 Hz
        let bank_empty = SampleBank::default();
        let synth = Instrument::Synth(crate::instruments::SynthParams {
            osc2_cents: 0.0,
            osc_mix: 0.0,
            ..Default::default()
        });
        let y = crate::instruments::render_note(&synth, 69.0 - 0.443, 0.8, 1.0, &bank_empty, 1);
        let c = cents(note_hz(&y), want);
        assert!(c.abs() < 2.0, "synth A4 at -44.3 c: {c:.2} c off");
        // a pitched multisample (one zone stretched over the keyboard)
        let mut bank = SampleBank::default();
        bank.insert(
            "sine_a3",
            (0..(2.0 * SR) as usize)
                .map(|i| (std::f32::consts::TAU * 220.0 * i as f32 / SR).sin() * 0.5)
                .collect(),
        );
        let zone = crate::multisample::Zone {
            sample: "sine_a3".into(),
            root: 57,
            lo_note: 0,
            hi_note: 127,
            vel_lo: 0.0,
            vel_hi: 1.0,
            tune_cents: 0.0,
            gain_db: 0.0,
            rr: 0,
        };
        let ms = Instrument::Multisample(crate::multisample::MultisampleParams {
            zones: vec![zone.clone()],
            ..Default::default()
        });
        assert!(ms.follows_tuning());
        let y = crate::instruments::render_note(&ms, 64.0 - 0.443, 0.8, 1.0, &bank, 1);
        let c = cents(note_hz(&y), 440.0 * 2f32.powf((64.0 - 69.0 - 0.443) / 12.0));
        assert!(c.abs() < 2.0, "multisample E4 at -44.3 c: {c:.2} c off");
        // a kit (each zone one key at its own root) and drums never follow
        let kit = Instrument::Multisample(crate::multisample::MultisampleParams {
            zones: vec![
                crate::multisample::Zone {
                    lo_note: 36,
                    hi_note: 36,
                    root: 36,
                    ..zone.clone()
                },
                crate::multisample::Zone {
                    lo_note: 38,
                    hi_note: 38,
                    root: 38,
                    ..zone
                },
            ],
            ..Default::default()
        });
        assert!(!kit.follows_tuning());
        assert!(!Instrument::Drum(Default::default()).follows_tuning());
        assert!(synth.follows_tuning());
    }

    /// Adding a bus and a send (which bumps the project's id counter) keeps
    /// every track's cached buffer, and the cached render still equals a
    /// fresh one.
    #[test]
    fn track_cache_survives_new_buses_and_sends() {
        let d = std::env::temp_dir().join("beatbox_render_cache_bus_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 9}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let o = RenderOptions {
            keep_stems: true,
            ..Default::default()
        };
        let mut cache = TrackCache::default();
        render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
        let keys0: Vec<TrackKey> = (0..e.project.tracks.len())
            .map(|i| cache.tracks[&i].0.clone())
            .collect();
        let first = e.project.tracks[0].name.clone();
        e.call(
            "add_bus",
            &serde_json::json!({"name": "air_t", "preset": "none"}),
        )
        .unwrap();
        e.call("add_effect", &serde_json::json!({"track": "air_t", "type": "saturator", "params": {"mode": "exciter", "mix": 1.0}})).unwrap();
        e.call(
            "set_send",
            &serde_json::json!({"track": first, "bus": "air_t", "db": -6.0}),
        )
        .unwrap();
        let keys1 = TrackCache::keys(&e.project, &e.bank, &o);
        assert_eq!(
            keys0, keys1,
            "a new bus/send must not invalidate the tracks"
        );
        let cached = render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
        let fresh = render(&e.project, &e.bank, &o).unwrap();
        assert_eq!(cached.left, fresh.left);
        assert_eq!(cached.right, fresh.right);
    }

    /// Invalidation through the public API (engine calls; tuning_cents is
    /// set the way cover_song sets it): every mutation changes the render,
    /// and the cached render equals a fresh, uncached one byte for byte.
    /// What re-synthesises:
    /// - own effect chain (add_effect; tweak_effect: drive, wet/dry mix, EQ
    ///   band, delay time) and own non-fader automation lanes (fx.N.param,
    ///   pan; a new lane and a curve change): that track only, the other
    ///   buffers reused;
    /// - instrument params, notes (pitch, start, velocity) and pan: that
    ///   track only (no other track of this beat listens to the edited
    ///   track's note-ons; see track_cache_serves_untouched_tracks for the
    ///   sidechain and hat-choke dependencies);
    /// - tempo: every track (every position and the song length move);
    /// - tuning_cents: the tracks whose instrument follows tuning (bass,
    ///   chords, lead), not the four drum tracks;
    /// - volume (incl. near -inf, 0 and +6 dB), mute, fader lanes and their
    ///   curves, sends: no track (they act after the cached buffer); buses
    ///   are never cached, so a downstream bus or send always re-sums;
    /// - render options (tail): every track.
    #[test]
    fn track_cache_rerenders_a_track_when_its_own_params_change() {
        let d = std::env::temp_dir().join("beatbox_render_cache_own_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 5, "bars": 2}),
        )
        .unwrap();
        // a group bus with its own EQ, chords routed into it
        e.call(
            "add_bus",
            &serde_json::json!({"name": "grp", "preset": "none"}),
        )
        .unwrap();
        e.call("add_effect", &serde_json::json!({"track": "grp", "type": "parametric_eq", "params": {"bands": [{"kind": "high_shelf", "freq": 6000.0, "gain_db": -6.0, "q": 0.7}]}})).unwrap();
        e.call(
            "route_track",
            &serde_json::json!({"track": "chords", "bus": "grp"}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let o = RenderOptions {
            keep_stems: true,
            ..Default::default()
        };
        let mut cache = TrackCache::default();
        let mut last = render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
        let ti = |e: &crate::engine::Engine, n: &str| e.project.track_index(n).unwrap();
        #[derive(Clone, Debug, PartialEq)]
        enum Resynth {
            Only(usize),
            Set(Vec<usize>),
            All,
            None,
        }
        let (snare, hat, chords, lead) = (
            ti(&e, "snare"),
            ti(&e, "hat"),
            ti(&e, "chords"),
            ti(&e, "lead"),
        );
        let (bass, open_hat) = (ti(&e, "bass"), ti(&e, "open_hat"));
        let pts = |a: f32, b: f32| serde_json::json!([{"beat": 0.0, "value": a}, {"beat": 8.0, "value": b}]);
        let muts: Vec<(&str, &str, serde_json::Value, Resynth)> = vec![
            (
                "add_effect on the track's own chain",
                "add_effect",
                serde_json::json!({"track": "snare", "type": "saturator", "params": {"mode": "tube", "drive_db": 6.0}}),
                Resynth::Only(snare),
            ),
            (
                "effect param: saturator drive",
                "tweak_effect",
                serde_json::json!({"track": "snare", "index": 1, "params": {"drive_db": 18.0}}),
                Resynth::Only(snare),
            ),
            (
                "effect param: reverb wet/dry mix",
                "tweak_effect",
                serde_json::json!({"track": "chords", "index": 0, "params": {"mix": 0.6}}),
                Resynth::Only(chords),
            ),
            (
                "effect param: EQ band",
                "tweak_effect",
                serde_json::json!({"track": "hat", "index": 0, "params": {"bands": [{"kind": "bell", "freq": 6000.0, "gain_db": -6.0, "q": 1.0}]}}),
                Resynth::Only(hat),
            ),
            (
                "effect param: delay time",
                "tweak_effect",
                serde_json::json!({"track": "lead", "index": 0, "params": {"steps": 2.0}}),
                Resynth::Only(lead),
            ),
            (
                "automation: effect param lane (fx.0.mix)",
                "add_automation",
                serde_json::json!({"track": "lead", "param": "fx.0.mix", "points": pts(0.0, 0.5)}),
                Resynth::Only(lead),
            ),
            (
                "automation: pan lane",
                "add_automation",
                serde_json::json!({"track": "hat", "param": "pan", "points": pts(-0.8, 0.8)}),
                Resynth::Only(hat),
            ),
            (
                "automation curve change (fx.0.mix points)",
                "set_automation_points",
                serde_json::json!({"track": "lead", "param": "fx.0.mix", "points": pts(0.5, 0.0)}),
                Resynth::Only(lead),
            ),
            (
                "instrument param: synth cutoff/resonance",
                "tweak_instrument",
                serde_json::json!({"track": "chords", "params": {"cutoff": 900.0, "resonance": 0.5}}),
                Resynth::Only(chords),
            ),
            (
                "instrument param: 808 drive/decay",
                "tweak_instrument",
                serde_json::json!({"track": "bass", "params": {"drive": 0.6, "decay": 0.3}}),
                Resynth::Only(bass),
            ),
            (
                "note pitch (transpose)",
                "transpose",
                serde_json::json!({"track": "lead", "semitones": 2}),
                Resynth::Only(lead),
            ),
            (
                "note start time (edit_notes move)",
                "edit_notes",
                serde_json::json!({"track": "bass", "move": 1}),
                Resynth::Only(bass),
            ),
            (
                "note velocity (edit_notes)",
                "edit_notes",
                serde_json::json!({"track": "bass", "vel_scale": 0.6}),
                Resynth::Only(bass),
            ),
            (
                "track pan",
                "set_mixer",
                serde_json::json!({"track": "open_hat", "pan": -0.4}),
                Resynth::Only(open_hat),
            ),
            (
                "track gain, routed into a bus",
                "set_mixer",
                serde_json::json!({"track": "chords", "volume_db": -9.0}),
                Resynth::None,
            ),
            (
                "automation: volume lane",
                "add_automation",
                serde_json::json!({"track": "snare", "param": "volume", "points": pts(-12.0, 0.0)}),
                Resynth::None,
            ),
            (
                "automation curve change (volume points)",
                "set_automation_points",
                serde_json::json!({"track": "snare", "param": "volume", "points": pts(0.0, -12.0)}),
                Resynth::None,
            ),
            (
                "bus gain",
                "set_mixer",
                serde_json::json!({"track": "grp", "volume_db": -4.0}),
                Resynth::None,
            ),
            (
                "send into the bus (new)",
                "set_send",
                serde_json::json!({"track": "snare", "bus": "grp", "db": -6.0}),
                Resynth::None,
            ),
            (
                "send level change",
                "set_send",
                serde_json::json!({"track": "snare", "bus": "grp", "db": 0.0}),
                Resynth::None,
            ),
            (
                "track mute",
                "set_mixer",
                serde_json::json!({"track": "kick", "mute": true}),
                Resynth::None,
            ),
            (
                "track unmute",
                "set_mixer",
                serde_json::json!({"track": "kick", "mute": false}),
                Resynth::None,
            ),
            (
                "track gain near -inf (-150 dB)",
                "set_mixer",
                serde_json::json!({"track": "kick", "volume_db": -150.0}),
                Resynth::None,
            ),
            (
                "track gain 0 dB",
                "set_mixer",
                serde_json::json!({"track": "kick", "volume_db": 0.0}),
                Resynth::None,
            ),
            (
                "track gain +6 dB (headroom boost)",
                "set_mixer",
                serde_json::json!({"track": "kick", "volume_db": 6.0}),
                Resynth::None,
            ),
            (
                "tempo",
                "set_tempo",
                serde_json::json!({"bpm": 132.0}),
                Resynth::All,
            ),
            (
                "tuning_cents",
                "",
                serde_json::json!(-44.0),
                Resynth::Set({
                    let mut v = vec![bass, chords, lead];
                    v.sort();
                    v
                }),
            ),
        ];
        for (what, tool, args, want) in muts {
            let n = e.project.tracks.len();
            let p0: Vec<_> = (0..n)
                .map(|i| std::sync::Arc::as_ptr(&cache.tracks[&i].1))
                .collect();
            let k0: Vec<TrackKey> = (0..n).map(|i| cache.tracks[&i].0.clone()).collect();
            if tool.is_empty() {
                // cover_song's match_tuning sets the field directly
                e.project.tuning_cents = args.as_f64().unwrap() as f32;
            } else {
                e.call(tool, &args)
                    .unwrap_or_else(|err| panic!("{what}: {err}"));
            }
            e.bank.sync(&e.project.samples);
            let cached = render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
            let fresh = render(&e.project, &e.bank, &o).unwrap();
            assert!(
                cached.left == fresh.left && cached.right == fresh.right,
                "{what}: cached render differs from a fresh one"
            );
            assert!(
                cached.left != last.left || cached.right != last.right,
                "{what}: the render did not change"
            );
            let redone: Vec<usize> = (0..n)
                .filter(|&i| !std::ptr::eq(p0[i], std::sync::Arc::as_ptr(&cache.tracks[&i].1)))
                .collect();
            match &want {
                Resynth::Only(t) => {
                    assert_eq!(redone, vec![*t], "{what}");
                    assert_ne!(cache.tracks[t].0, k0[*t], "{what}");
                }
                Resynth::Set(s) => {
                    assert_eq!(&redone, s, "{what}");
                    assert!(s.iter().all(|t| cache.tracks[t].0 != k0[*t]), "{what}");
                }
                Resynth::All => assert_eq!(redone, (0..n).collect::<Vec<_>>(), "{what}"),
                Resynth::None => assert!(redone.is_empty(), "{what}: {redone:?}"),
            }
            println!("cache invalidation: {what} -> re-synthesised {want:?}, cached == fresh, render changed");
            last = cached;
        }
        // render options are in the key too
        let o2 = RenderOptions { tail: 3.0, ..o };
        let n = e.project.tracks.len();
        let p0: Vec<_> = (0..n)
            .map(|i| std::sync::Arc::as_ptr(&cache.tracks[&i].1))
            .collect();
        let cached = render_cached(&e.project, &e.bank, &o2, Some(&mut cache)).unwrap();
        let fresh = render(&e.project, &e.bank, &o2).unwrap();
        assert!(cached.left == fresh.left && cached.right == fresh.right);
        assert_ne!(cached.left.len(), last.left.len());
        assert!((0..n).all(|i| !std::ptr::eq(p0[i], std::sync::Arc::as_ptr(&cache.tracks[&i].1))));
        println!("cache invalidation: render option tail -> re-synthesised All, cached == fresh, render changed");
    }

    /// C7: an edit re-synthesises only the tracks whose pre-fader buffer it
    /// changes; every other track is served from the cache, and the cached
    /// render equals a fresh one byte for byte. Dependencies between tracks
    /// in this beat: the bass's sidechain listens to the kick's note-ons,
    /// and the closed hat (when unmuted) chokes the open hat.
    #[test]
    fn track_cache_serves_untouched_tracks() {
        let d = std::env::temp_dir().join("beatbox_render_cache_c7_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 5, "bars": 2}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let o = RenderOptions {
            keep_stems: true,
            ..Default::default()
        };
        let mut cache = TrackCache::default();
        let mut last = render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
        let n = e.project.tracks.len();
        assert_eq!(cache.len(), n);
        let ti = |e: &crate::engine::Engine, x: &str| e.project.track_index(x).unwrap();
        let (kick, snare, hat, open_hat) = (
            ti(&e, "kick"),
            ti(&e, "snare"),
            ti(&e, "hat"),
            ti(&e, "open_hat"),
        );
        let (bass, chords, lead) = (ti(&e, "bass"), ti(&e, "chords"), ti(&e, "lead"));
        let set = |mut v: Vec<usize>| {
            v.sort();
            v
        };
        let muts: Vec<(&str, &str, serde_json::Value, Vec<usize>)> = vec![
            (
                "notes: chords moved a step",
                "edit_notes",
                serde_json::json!({"track": "chords", "move": 1}),
                vec![chords],
            ),
            (
                "notes: lead velocity",
                "edit_notes",
                serde_json::json!({"track": "lead", "vel_scale": 0.7}),
                vec![lead],
            ),
            (
                "notes: kick moved a step (the bass ducks to the kick)",
                "edit_notes",
                serde_json::json!({"track": "kick", "move": 1}),
                set(vec![kick, bass]),
            ),
            (
                "notes: closed hat moved two steps (it chokes the open hat)",
                "edit_notes",
                serde_json::json!({"track": "hat", "move": 2}),
                set(vec![hat, open_hat]),
            ),
            (
                "instrument: lead FM index",
                "tweak_instrument",
                serde_json::json!({"track": "lead", "params": {"index": 3.0}}),
                vec![lead],
            ),
            (
                "instrument: 808 decay",
                "tweak_instrument",
                serde_json::json!({"track": "bass", "params": {"decay": 0.4}}),
                vec![bass],
            ),
            (
                "pan: snare",
                "set_mixer",
                serde_json::json!({"track": "snare", "pan": 0.3}),
                vec![snare],
            ),
            (
                "pan: chords",
                "set_mixer",
                serde_json::json!({"track": "chords", "pan": -0.2}),
                vec![chords],
            ),
            (
                "mute the closed hat (its chokes leave the open hat)",
                "set_mixer",
                serde_json::json!({"track": "hat", "mute": true}),
                vec![open_hat],
            ),
        ];
        for (what, tool, args, want) in muts {
            let p0: Vec<_> = (0..n).map(|i| cache.tracks[&i].1.clone()).collect();
            e.call(tool, &args)
                .unwrap_or_else(|err| panic!("{what}: {err}"));
            e.bank.sync(&e.project.samples);
            let cached = render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
            let fresh = render(&e.project, &e.bank, &o).unwrap();
            assert!(
                cached.left == fresh.left && cached.right == fresh.right,
                "{what}: cached render differs from a fresh one"
            );
            assert!(
                cached.left != last.left || cached.right != last.right,
                "{what}: the render did not change"
            );
            let redone: Vec<usize> = (0..n)
                .filter(|&i| !std::sync::Arc::ptr_eq(&p0[i], &cache.tracks[&i].1))
                .collect();
            assert_eq!(redone, want, "{what}");
            // every re-synthesised buffer really changed (no wasted re-render)
            for &t in &redone {
                assert!(
                    *p0[t] != *cache.tracks[&t].1,
                    "{what}: track {t} re-rendered to the same audio"
                );
            }
            println!("C7 cache: {what} -> re-rendered {} of {n} tracks {:?}, {} served from cache, cached == fresh", redone.len(), redone.iter().map(|&i| e.project.tracks[i].name.as_str()).collect::<Vec<_>>(), n - redone.len());
            last = cached;
        }
    }

    /// C7 DAG: tracks -> sends/aux buses (and bus -> bus) -> master, with
    /// sidechain edges between tracks. A ducking trigger's edit (kick
    /// notes) dirties the kick, the bass it ducks, and the buses those two
    /// feed, nothing orthogonal; a send or routing change dirties only the
    /// buses on its path; faders re-synthesise no track. Counted from the
    /// cache, and the cached render equals a fresh one byte for byte.
    /// An audio clip's ducking onsets must follow the take as it actually
    /// plays: a 2x-stretched clip hits at half rate, so its sidechain
    /// triggers land twice as far apart.
    #[test]
    fn sidechain_onsets_follow_a_stretched_clip() {
        let d =
            std::env::temp_dir().join(format!("beatbox_sidechain_stretch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d.clone());
        e.call(
            "new_project",
            &serde_json::json!({"name": "sc", "bpm": 120}),
        )
        .unwrap();
        // 2 s with four fast-decay impulses 0.5 s apart
        let mut take = vec![0.0f32; (2.0 * SR) as usize];
        for k in 0..4 {
            let at = ((0.25 + 0.5 * k as f32) * SR) as usize;
            for i in 0..(0.01 * SR) as usize {
                take[at + i] = 0.8 * (-(i as f32) / (0.002 * SR)).exp();
            }
        }
        let wav = d.join("hits.wav");
        write_wav(&wav, &take, &take).unwrap();
        e.call(
            "import_sample",
            &serde_json::json!({"path": wav.to_string_lossy(), "name": "hits"}),
        )
        .unwrap();
        e.call(
            "add_track",
            &serde_json::json!({"name": "vox", "preset": "pad"}),
        )
        .unwrap();
        e.call(
            "add_audio_clip",
            &serde_json::json!({"track": "vox", "sample": "hits", "start_beat": 0.0}),
        )
        .unwrap();
        let onsets = |e: &crate::engine::Engine| -> Vec<usize> {
            let o = RenderOptions::default();
            let (events, body) = schedule(&e.project, &o);
            let total = body + (o.tail.max(0.0) * SR) as usize;
            let (triggers, _) = sidechain_inputs(&e.project, &e.bank, &o, &events, total);
            triggers.get("vox").cloned().unwrap_or_default()
        };
        let plain = onsets(&e);
        assert_eq!(plain.len(), 4, "four impulses, got {plain:?}");
        let id = e.project.audio_clips[0].id.clone();
        e.call(
            "stretch_clip",
            &serde_json::json!({"id": id, "factor": 2.0}),
        )
        .unwrap();
        let slow = onsets(&e);
        assert_eq!(slow.len(), 4, "the stretched take keeps its hits: {slow:?}");
        for (a, b) in plain.iter().zip(slow.iter()) {
            let d = (*b as i64 - 2 * *a as i64).abs();
            assert!(
                d < 10000,
                "onset at {a} samples should land near {} at 2x, got {b}",
                2 * a
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn render_dag_sidechain_and_send_routing() {
        let d = std::env::temp_dir().join("beatbox_render_cache_dag_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 5, "bars": 2}),
        )
        .unwrap();
        for b in ["drums", "low", "verb"] {
            e.call("add_bus", &serde_json::json!({"name": b, "preset": "none"}))
                .unwrap();
        }
        e.call("add_effect", &serde_json::json!({"track": "drums", "type": "parametric_eq", "params": {"bands": [{"kind": "high_shelf", "freq": 6000.0, "gain_db": -3.0, "q": 0.7}]}})).unwrap();
        e.call(
            "add_effect",
            &serde_json::json!({"track": "verb", "type": "reverb", "params": {"mix": 1.0}}),
        )
        .unwrap();
        for t in ["kick", "snare", "hat", "open_hat"] {
            e.call(
                "route_track",
                &serde_json::json!({"track": t, "bus": "drums"}),
            )
            .unwrap();
        }
        e.call(
            "route_track",
            &serde_json::json!({"track": "bass", "bus": "low"}),
        )
        .unwrap();
        e.call(
            "set_send",
            &serde_json::json!({"track": "chords", "bus": "verb", "db": -8.0}),
        )
        .unwrap();
        e.call(
            "set_send",
            &serde_json::json!({"track": "lead", "bus": "verb", "db": -10.0}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let o = RenderOptions {
            keep_stems: true,
            ..Default::default()
        };
        let mut cache = TrackCache::default();
        let mut last = render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
        let (n, nb) = (e.project.tracks.len(), e.project.buses.len());
        assert_eq!((cache.tracks.len(), cache.buses.len()), (n, nb));
        let ti = |e: &crate::engine::Engine, x: &str| e.project.track_index(x).unwrap();
        let bx = |e: &crate::engine::Engine, x: &str| e.project.bus_index(x).unwrap();
        let (kick, bass, chords) = (ti(&e, "kick"), ti(&e, "bass"), ti(&e, "chords"));
        let (drums, low, verb) = (bx(&e, "drums"), bx(&e, "low"), bx(&e, "verb"));
        let set = |mut v: Vec<usize>| {
            v.sort();
            v
        };
        let muts: Vec<(&str, &str, serde_json::Value, Vec<usize>, Vec<usize>)> = vec![
            (
                "sidechain trigger: kick notes moved (the bass ducks to the kick)",
                "edit_notes",
                serde_json::json!({"track": "kick", "move": 1}),
                set(vec![kick, bass]),
                set(vec![drums, low]),
            ),
            (
                "send routing: new send snare -> verb",
                "set_send",
                serde_json::json!({"track": "snare", "bus": "verb", "db": -12.0}),
                vec![],
                vec![verb],
            ),
            (
                "send level: snare -> verb",
                "set_send",
                serde_json::json!({"track": "snare", "bus": "verb", "db": -4.0}),
                vec![],
                vec![verb],
            ),
            (
                "output routing: lead into the low bus (its send to verb unchanged)",
                "route_track",
                serde_json::json!({"track": "lead", "bus": "low"}),
                vec![],
                vec![low],
            ),
            (
                "track fader: kick -3 dB",
                "set_mixer",
                serde_json::json!({"track": "kick", "volume_db": -3.0}),
                vec![],
                vec![drums],
            ),
            (
                "bus fader: low -2 dB",
                "set_mixer",
                serde_json::json!({"track": "low", "volume_db": -2.0}),
                vec![],
                vec![low],
            ),
            (
                "bus effect param: drums EQ",
                "tweak_effect",
                serde_json::json!({"track": "drums", "index": 0, "params": {"bands": [{"kind": "bell", "freq": 3000.0, "gain_db": -4.0, "q": 1.0}]}}),
                vec![],
                vec![drums],
            ),
            (
                "track pan: chords (it sends to verb)",
                "set_mixer",
                serde_json::json!({"track": "chords", "pan": 0.4}),
                vec![chords],
                vec![verb],
            ),
        ];
        for (what, tool, args, want_t, want_b) in muts {
            let t0: Vec<_> = (0..n).map(|i| cache.tracks[&i].1.clone()).collect();
            let b0: Vec<_> = (0..nb).map(|i| cache.buses[&i].1.clone()).collect();
            e.call(tool, &args)
                .unwrap_or_else(|err| panic!("{what}: {err}"));
            e.bank.sync(&e.project.samples);
            let cached = render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
            let fresh = render(&e.project, &e.bank, &o).unwrap();
            assert!(
                cached.left == fresh.left && cached.right == fresh.right,
                "{what}: cached render differs from a fresh one"
            );
            assert!(
                cached.left != last.left || cached.right != last.right,
                "{what}: the render did not change"
            );
            for (a, b) in cached.bus_stems.iter().zip(fresh.bus_stems.iter()) {
                assert!(
                    a.left == b.left && a.right == b.right,
                    "{what}: bus {} differs",
                    a.name
                );
            }
            let rt: Vec<usize> = (0..n)
                .filter(|&i| !std::sync::Arc::ptr_eq(&t0[i], &cache.tracks[&i].1))
                .collect();
            let rb: Vec<usize> = (0..nb)
                .filter(|&i| !std::sync::Arc::ptr_eq(&b0[i], &cache.buses[&i].1))
                .collect();
            assert_eq!((&rt, &rb), (&want_t, &want_b), "{what}: (tracks, buses)");
            println!(
                "C7 DAG: {what} -> re-rendered tracks {:?} ({} of {n}), buses {:?} ({} of {nb}), master re-mixed, cached == fresh",
                rt.iter().map(|&i| e.project.tracks[i].name.as_str()).collect::<Vec<_>>(),
                rt.len(),
                rb.iter().map(|&i| e.project.buses[i].name.as_str()).collect::<Vec<_>>(),
                rb.len()
            );
            last = cached;
        }
    }

    /// C9: the vocal resonance tamer's bands sit in the vocal's own chain,
    /// so in the C7 DAG turning it on, or moving a band setting,
    /// re-synthesises the vocal alone and re-mixes only the buses it feeds;
    /// rendering again with unchanged settings re-renders nothing; turning
    /// it off gives back the untamed render byte for byte. Counted from the
    /// cache; cached == fresh on every step.
    #[test]
    fn vocal_resonance_tamer_re_renders_only_the_vocal_and_its_buses() {
        use crate::resonance::{band_effect, strip};
        let d = std::env::temp_dir().join("beatbox_render_cache_c9_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 5, "bars": 2}),
        )
        .unwrap();
        for b in ["drums", "vbus", "verb"] {
            e.call("add_bus", &serde_json::json!({"name": b, "preset": "none"}))
                .unwrap();
        }
        e.call(
            "add_effect",
            &serde_json::json!({"track": "verb", "type": "reverb", "params": {"mix": 1.0}}),
        )
        .unwrap();
        for t in ["kick", "snare", "hat", "open_hat"] {
            e.call(
                "route_track",
                &serde_json::json!({"track": t, "bus": "drums"}),
            )
            .unwrap();
        }
        // "lead" stands in for the vocal: its own bus plus a reverb send
        e.call(
            "route_track",
            &serde_json::json!({"track": "lead", "bus": "vbus"}),
        )
        .unwrap();
        e.call(
            "set_send",
            &serde_json::json!({"track": "lead", "bus": "verb", "db": -10.0}),
        )
        .unwrap();
        e.call(
            "set_send",
            &serde_json::json!({"track": "chords", "bus": "verb", "db": -8.0}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let o = RenderOptions {
            keep_stems: true,
            ..Default::default()
        };
        let mut cache = TrackCache::default();
        let base = render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
        let mut last = (base.left.clone(), base.right.clone());
        let (n, nb) = (e.project.tracks.len(), e.project.buses.len());
        let lead = e.project.track_index("lead").unwrap();
        let (vbus, verb) = (
            e.project.bus_index("vbus").unwrap(),
            e.project.bus_index("verb").unwrap(),
        );
        let mut tamed = vec![vbus, verb];
        tamed.sort();
        fn dyn_eq(
            e: &mut crate::engine::Engine,
            lead: usize,
            k: usize,
        ) -> &mut crate::fx::DynamicEqFx {
            match e.project.tracks[lead]
                .effects
                .iter_mut()
                .filter(|f| f.id().starts_with(crate::resonance::AUTO_RESONANCE_ID))
                .nth(k)
            {
                Some(crate::fx::Effect::DynamicEq(x)) => x,
                _ => panic!("no tamer band {k}"),
            }
        }
        let steps: Vec<(&str, u8, bool)> = vec![
            ("tamer on: 2 bands on the vocal", 0, true),
            ("tamer unchanged: render again", 1, false),
            ("tamer threshold moved", 2, true),
            ("tamer range moved", 3, true),
            ("tamer band centre moved", 4, true),
            ("tamer unchanged again", 1, false),
            ("tamer off", 5, true),
        ];
        for (what, edit, changes) in steps {
            let t0: Vec<_> = (0..n).map(|i| cache.tracks[&i].1.clone()).collect();
            let b0: Vec<_> = (0..nb).map(|i| cache.buses[&i].1.clone()).collect();
            match edit {
                0 => {
                    e.project.tracks[lead]
                        .effects
                        .push(band_effect(2500.0, -60.0, 1));
                    e.project.tracks[lead]
                        .effects
                        .push(band_effect(5000.0, -60.0, 2));
                }
                2 => dyn_eq(&mut e, lead, 0).threshold_db = -50.0,
                3 => dyn_eq(&mut e, lead, 1).range_db = -3.0,
                4 => dyn_eq(&mut e, lead, 1).freq = 4800.0,
                5 => assert_eq!(strip(&mut e.project.tracks[lead].effects), 2),
                _ => {}
            }
            let cached = render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
            let fresh = render(&e.project, &e.bank, &o).unwrap();
            assert!(
                cached.left == fresh.left && cached.right == fresh.right,
                "{what}: cached render differs from a fresh one"
            );
            assert_eq!(
                cached.left != last.0 || cached.right != last.1,
                changes,
                "{what}: did the render change"
            );
            let rt: Vec<usize> = (0..n)
                .filter(|&i| !std::sync::Arc::ptr_eq(&t0[i], &cache.tracks[&i].1))
                .collect();
            let rb: Vec<usize> = (0..nb)
                .filter(|&i| !std::sync::Arc::ptr_eq(&b0[i], &cache.buses[&i].1))
                .collect();
            let want: (Vec<usize>, Vec<usize>) = if changes {
                (vec![lead], tamed.clone())
            } else {
                (vec![], vec![])
            };
            assert_eq!(
                (rt.clone(), rb.clone()),
                want,
                "{what}: (tracks, buses) re-rendered"
            );
            println!(
                "C9 cache: {what} -> re-rendered tracks {:?} ({} of {n}), buses {:?} ({} of {nb}), cached == fresh",
                rt.iter().map(|&i| e.project.tracks[i].name.as_str()).collect::<Vec<_>>(),
                rt.len(),
                rb.iter().map(|&i| e.project.buses[i].name.as_str()).collect::<Vec<_>>(),
                rb.len()
            );
            last = (cached.left, cached.right);
        }
        let bits = |l: &[f32], r: &[f32]| {
            l.iter()
                .chain(r.iter())
                .map(|x| x.to_bits())
                .collect::<Vec<u32>>()
        };
        assert_eq!(
            bits(&last.0, &last.1),
            bits(&base.left, &base.right),
            "off == the untamed render, bit for bit"
        );
    }

    #[test]
    fn bus_routing_and_fader_lanes_re_sum_without_resynthesis() {
        let d = std::env::temp_dir().join("beatbox_render_cache_bus_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 3, "bars": 2}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let o = RenderOptions {
            keep_stems: true,
            ..Default::default()
        };
        let mut cache = TrackCache::default();
        render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
        let before: Vec<(usize, *const (Vec<f32>, Vec<f32>))> = cache
            .tracks
            .iter()
            .map(|(k, (_, b))| (*k, std::sync::Arc::as_ptr(b)))
            .collect();
        // a new bus with an EQ, every track routed into it, a fader lane
        let mut q = e.project.clone();
        let mut b = crate::project::Bus::new("beat");
        b.effects.push(serde_json::from_value(serde_json::json!({"type": "parametric_eq", "bands": [{"kind": "high_shelf", "freq": 7500.0, "gain_db": -6.0, "q": 0.7}]})).unwrap());
        q.buses.push(b);
        for t in q.tracks.iter_mut() {
            t.output = Some("beat".into());
        }
        let name = q.tracks[0].name.clone();
        q.automation.push(serde_json::from_value(serde_json::json!({"target": name, "param": "volume", "points": [{"beat": 0.0, "value": -3.0}, {"beat": 4.0, "value": 0.0}]})).unwrap());
        let cached = render_cached(&q, &e.bank, &o, Some(&mut cache)).unwrap();
        let fresh = render(&q, &e.bank, &o).unwrap();
        assert_eq!(cached.left, fresh.left);
        assert_eq!(cached.right, fresh.right);
        for (k, p) in before {
            assert!(
                std::ptr::eq(std::sync::Arc::as_ptr(&cache.tracks[&k].1), p),
                "track {k} was re-synthesised by a mixer-only change"
            );
        }
        // a track's own chain change re-synthesises that track only
        let mut c = q.clone();
        c.tracks[0].effects.push(serde_json::from_value(serde_json::json!({"type": "parametric_eq", "bands": [{"kind": "bell", "freq": 1000.0, "gain_db": -3.0, "q": 1.0}]})).unwrap());
        let ptr1 = cache.tracks.get(&1).map(|x| std::sync::Arc::as_ptr(&x.1));
        let n = render_cached(&c, &e.bank, &o, Some(&mut cache)).unwrap();
        assert_eq!(n.left, render(&c, &e.bank, &o).unwrap().left);
        if let Some(p1) = ptr1 {
            assert!(std::ptr::eq(
                std::sync::Arc::as_ptr(&cache.tracks[&1].1),
                p1
            ));
        }
    }

    #[test]
    fn retriggered_808_crossfades_without_kink() {
        // critic v15: a same-pitch 808 retrigger over a ringing note stepped
        // in at phase 0 (a low kink); it now crossfades in under the choke
        let mut p = Project::new("c", 120.0);
        let inst = crate::instruments::preset("808").unwrap();
        p.tracks.push(Track::new("bass", inst));
        p.patterns[0].clips.insert(
            "bass".into(),
            vec![Note::new(0.0, 16.0, 36, 1.0), Note::new(8.0, 8.0, 36, 1.0)],
        );
        p.master_effects.clear();
        let bank = SampleBank::default();
        let m = render(&p, &bank, &RenderOptions::default()).unwrap();
        let d2 = |a: usize, b: usize| -> f32 {
            m.left[a..b]
                .windows(3)
                .map(|w| (w[2] - 2.0 * w[1] + w[0]).abs())
                .fold(0.0, f32::max)
        };
        let at = (8.0 * p.step_secs() * SR) as usize;
        let w = (0.01 * SR) as usize;
        let first = d2(0, w);
        let retrig = d2(at - w, at + w);
        assert!(
            retrig < first,
            "retrigger kink {retrig} vs first onset {first}"
        );
    }

    #[test]
    fn declick_fades_truncated_and_offset_voices() {
        // a buffer that stops at full scale and starts at full scale
        let buf = vec![0.8f32; 4000];
        let mut out = vec![0.0f32; 10000];
        add_declicked(&mut out, 1000, &buf, None);
        let d: f32 = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(d < 0.05, "max step {d}");
        assert!(crate::ears::clicks(&out, 10).is_empty());
    }

    #[test]
    fn mono_808_chokes_without_clicks() {
        let mut p = Project::new("c", 140.0);
        let inst = crate::instruments::preset("808").unwrap();
        p.tracks.push(Track::new("bass", inst));
        // overlapping long 808s on different pitches: the old note must stop
        // (mono) and the cut must be smooth
        let notes = vec![
            Note::new(0.0, 12.0, 36, 1.0),
            Note::new(3.0, 12.0, 41, 1.0),
            Note::new(7.0, 12.0, 43, 1.0),
            Note::new(10.0, 6.0, 36, 1.0),
        ];
        p.patterns[0].clips.insert("bass".into(), notes);
        p.master_effects.clear();
        let bank = SampleBank::default();
        let m = render(&p, &bank, &RenderOptions::default()).unwrap();
        let c = crate::ears::clicks(&m.left, 50);
        assert!(c.is_empty(), "clicks at {c:?}");
        // after the choke point (step 3), the first note is gone: the signal is
        // a single sine, so its peak stays below two summed voices
        let step = p.step_secs();
        let a = ((3.5 * step) * SR) as usize;
        let b = ((6.5 * step) * SR) as usize;
        let pk = m.left[a..b].iter().fold(0.0f32, |x, y| x.max(y.abs()));
        assert!(pk < 1.2, "peak {pk}");
    }

    #[test]
    fn retriggered_kick_chokes_its_ringing_tail() {
        let mut p = Project::new("k", 80.0);
        let inst = crate::instruments::preset("kick").unwrap();
        p.tracks.push(Track::new("kick", inst));
        p.patterns[0].clips.insert(
            "kick".into(),
            vec![
                Note::new(0.0, 1.0, 60, 0.9),
                Note::new(3.0, 1.0, 60, 0.9),
                Note::new(4.0, 1.0, 60, 0.9),
            ],
        );
        p.master_effects.clear();
        let m = render(&p, &SampleBank::default(), &RenderOptions::default()).unwrap();
        let c = crate::ears::clicks(&m.left, 50);
        assert!(c.is_empty(), "clicks at {c:?}");
    }

    /// An 808 that glides into its next note continues it (legato): no
    /// punch / attack re-trigger and no phase-mismatched crossfade at the
    /// hand-off. Before, the level jumped ~+4.7 dB and dipped where the
    /// choked voice cancelled the new one.
    #[test]
    fn gliding_808_lands_legato() {
        let mut p = Project::new("glide", 144.0);
        p.patterns[0].bars = 1;
        let inst = crate::instruments::preset("808").unwrap();
        p.tracks.push(Track::new("808", inst));
        let mut a = Note::new(0.0, 6.0, 42, 0.9);
        a.slide_to = Some(45);
        let b = Note::new(6.0, 6.0, 45, 0.9);
        *p.patterns[0].notes_mut("808") = vec![a, b];
        let m = render(&p, &SampleBank::default(), &RenderOptions::default()).unwrap();
        let hand = (6.0 * p.step_secs() * SR) as usize;
        // RMS over two periods of the target pitch
        let w = (2.0 * SR / crate::dsp::midi_to_hz(45.0)) as usize;
        let rms = |c: usize| -> f32 {
            let s: f32 = m.left[c - w / 2..c + w / 2].iter().map(|x| x * x).sum();
            10.0 * (s / w as f32).max(1e-12).log10()
        };
        let before = rms(hand - (0.03 * SR) as usize);
        let lv: Vec<f32> = (0..12)
            .map(|k| rms(hand - (0.02 * SR) as usize + k * (0.005 * SR) as usize))
            .collect();
        let (lo, hi) = lv
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
        assert!(
            hi - before < 1.0,
            "re-attack bump {:.2} dB: {lv:?}",
            hi - before
        );
        assert!(
            before - lo < 1.0,
            "hand-off dip {:.2} dB: {lv:?}",
            before - lo
        );
    }
}
