//! DSP ported from SoundCraft: an 8-line FDN reverb, a soft-knee lookahead
//! compressor gain computer with sidechain high-pass and parallel mix, YIN
//! pitch detection with note segmentation, spectral-flux transient detection
//! and strip-silence ranges.
//!
//! Adapted from SoundCraft (https://github.com/storytold/soundcraft):
//! `crates/dsp/src/plugins/reverb.rs`, `crates/dsp/src/plugins/dynamics.rs`
//! (`compress_gr`, `ballistic`, `Compressor::process`),
//! `crates/dsp/src/pitch_detect.rs` and `crates/dsp/src/offline.rs`
//! (`detect_transients`, `non_silent_ranges`).
//! Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors,
//! MIT OR Apache-2.0 (used here under MIT). Changes: edition 2024 -> 2021,
//! stateful block plugins rewritten as whole-buffer stereo functions (the
//! FDN is a stateful block processor again, with f64 state: `Fdn`),
//! parameter smoothing dropped (parameters are static per render), rustfft
//! replaced by `analysis::fft`. See THIRD_PARTY_NOTICES.md.

use crate::dsp::{db_to_gain, gain_to_db, Biquad, BiquadKind, SR};
use std::f32::consts::{FRAC_1_SQRT_2, TAU};

fn ms(ms: f32) -> f32 {
    ms * 0.001 * SR
}

// ---------------------------------------------------------------- delay line

#[derive(Default, Clone)]
struct DelayLine {
    buf: Vec<f64>,
    mask: usize,
    pos: usize,
}

impl DelayLine {
    fn with_max(max_delay: usize) -> Self {
        let len = max_delay
            .saturating_add(4)
            .clamp(4, 1 << 26)
            .next_power_of_two();
        DelayLine {
            buf: vec![0.0; len],
            mask: len - 1,
            pos: 0,
        }
    }
    #[inline]
    fn push(&mut self, x: f64) {
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) & self.mask;
    }
    #[inline]
    fn tap(&self, d: usize) -> f64 {
        self.buf[self.pos.wrapping_sub(d) & self.mask]
    }
    #[inline]
    fn read(&self, d: f64) -> f64 {
        let max = (self.buf.len() - 3) as f64;
        let d = if d.is_finite() {
            d.clamp(1.0, max)
        } else {
            1.0
        };
        let i = d as usize;
        let f = d - i as f64;
        let a = self.tap(i);
        let b = self.tap(i + 1);
        a + (b - a) * f
    }
}

// ---------------------------------------------------------------- FDN reverb

const LINES: usize = 8;
const DIFFUSERS: usize = 4;
const SIZE_MIN: f32 = 0.35;
const SIZE_MAX: f32 = 1.6;

pub struct FdnConfig {
    pub delays_ms: [f32; LINES],
    pub diffusers_ms: [f32; DIFFUSERS],
    pub mod_ms: f32,
    pub mod_hz: f32,
}

pub const ROOM: FdnConfig = FdnConfig {
    delays_ms: [23.13, 27.71, 31.87, 36.29, 41.33, 45.67, 51.09, 56.93],
    diffusers_ms: [4.71, 3.59, 12.73, 9.31],
    mod_ms: 0.25,
    mod_hz: 0.7,
};

pub const PLATE: FdnConfig = FdnConfig {
    delays_ms: [13.71, 17.93, 21.07, 24.73, 28.31, 32.89, 36.13, 40.31],
    diffusers_ms: [3.13, 4.27, 7.93, 11.29],
    mod_ms: 0.4,
    mod_hz: 1.1,
};

/// Beatbox addition: longer, sparser lines for a concert-hall size.
pub const HALL: FdnConfig = FdnConfig {
    delays_ms: [43.07, 51.61, 59.93, 67.17, 76.31, 83.89, 94.03, 103.97],
    diffusers_ms: [6.37, 4.91, 17.21, 13.07],
    mod_ms: 0.5,
    mod_hz: 0.45,
};

#[derive(Clone, Copy, Debug)]
pub struct FdnParams {
    /// RT60 in seconds.
    pub decay_s: f32,
    /// 0..1 scales every line between 0.35x and 1.6x.
    pub size: f32,
    pub damping_hz: f32,
    /// 0..1 allpass diffusion.
    pub diffusion: f32,
    pub predelay_ms: f32,
    pub width: f32,
    pub low_cut_hz: f32,
    pub mix: f32,
}

struct Allpass {
    buf: Vec<f64>,
    pos: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Allpass {
            buf: vec![0.0; len.max(1)],
            pos: 0,
        }
    }
    #[inline]
    fn process(&mut self, x: f64, g: f64) -> f64 {
        let d = self.buf[self.pos];
        let v = x + g * d;
        self.buf[self.pos] = flush64(v);
        self.pos += 1;
        if self.pos >= self.buf.len() {
            self.pos = 0;
        }
        d - g * v
    }
}

#[inline]
fn flush64(x: f64) -> f64 {
    if x.is_finite() && x.abs() > 1.0e-25 {
        x
    } else {
        0.0
    }
}

#[inline]
fn ms64(ms: f64) -> f64 {
    ms * 0.001 * SR as f64
}

/// RBJ low cut (Q 1/sqrt 2) in f64, transposed direct form II.
#[derive(Clone, Copy, Debug)]
struct LowCut64 {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl LowCut64 {
    fn new(freq: f64) -> Self {
        let f = freq.clamp(10.0, SR as f64 * 0.49);
        let w = std::f64::consts::TAU * f / SR as f64;
        let (sw, cw) = (w.sin(), w.cos());
        let alpha = sw / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        LowCut64 {
            b0: (1.0 + cw) / 2.0 / a0,
            b1: -(1.0 + cw) / a0,
            b2: (1.0 + cw) / 2.0 / a0,
            a1: -2.0 * cw / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }
    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        if !self.z1.is_finite() || !self.z2.is_finite() {
            self.z1 = 0.0;
            self.z2 = 0.0;
        }
        y
    }
}

#[inline]
fn hadamard8(v: &mut [f64; LINES]) {
    let mut h = 1;
    while h < LINES {
        let mut i = 0;
        while i < LINES {
            for j in i..i + h {
                let (a, b) = (v[j], v[j + h]);
                v[j] = a + b;
                v[j + h] = a - b;
            }
            i += h * 2;
        }
        h *= 2;
    }
    let s = 1.0 / (LINES as f64).sqrt();
    v.iter_mut().for_each(|x| *x *= s);
}

/// 8-line feedback-delay-network reverb as a stateful stereo processor:
/// pre-delay, 4 allpass diffusers per side, Hadamard mixing, per-line RT60
/// gains, in-loop HF damping, slow delay modulation, low cut and width.
///
/// State and arithmetic are f64 (input and output stay f32). In f32 the
/// loop's rounding depends on everything it has ever heard, so a render of
/// (history + edit) minus a render of the edit alone did not cancel the
/// history exactly: issue #1, the ~2e-6 rms live-splice residual. In f64
/// that residual sits below one f32 ulp of the output.
///
/// `process` carries every piece of state between calls, so a buffer
/// processed in blocks of any size is bit-identical to one call.
pub struct Fdn {
    pre: [DelayLine; 2],
    pre_n: f64,
    diff: [Vec<Allpass>; 2],
    g_diff: f64,
    lines: Vec<DelayLine>,
    base: [f64; LINES],
    gains: [f64; LINES],
    mod_depth: f64,
    damp: f64,
    lp: [f64; LINES],
    lfo: f64,
    lfo_inc: f64,
    lowcut: [LowCut64; 2],
    mix: f64,
    width: f64,
}

impl Fdn {
    /// `offset`: the first processed sample's place in the song; the
    /// delay-line modulation LFO starts where a whole-song render's would be.
    pub fn new(cfg: &FdnConfig, p: &FdnParams, offset: usize) -> Self {
        let size = (SIZE_MIN + (SIZE_MAX - SIZE_MIN) * p.size.clamp(0.0, 1.0)) as f64;
        let rt60 = p.decay_s.clamp(0.05, 30.0) as f64;
        let mk_pre = || DelayLine::with_max(ms(250.0) as usize + 4);
        let diff = [0usize, 1].map(|side| {
            let spread = if side == 0 { 1.0 } else { 1.071 };
            cfg.diffusers_ms
                .iter()
                .map(|m| Allpass::new(ms64(*m as f64 * spread) as usize))
                .collect::<Vec<_>>()
        });
        let mod_depth = ms64(cfg.mod_ms as f64);
        let lines = cfg
            .delays_ms
            .iter()
            .map(|m| {
                DelayLine::with_max((ms64(*m as f64 * SIZE_MAX as f64) + mod_depth + 4.0) as usize)
            })
            .collect();
        let base = cfg.delays_ms.map(|m| ms64(m as f64 * size));
        let gains = cfg.delays_ms.map(|m| {
            10f64
                .powf(-3.0 * (m as f64 * size * 0.001) / rt60)
                .min(0.9999)
        });
        let lfo_inc = cfg.mod_hz as f64 / SR as f64;
        // the same f64 accumulation a whole-song render runs up to `offset`
        let mut lfo = 0.0f64;
        for _ in 0..offset {
            lfo += lfo_inc;
            if lfo >= 1.0 {
                lfo -= 1.0;
            }
        }
        let lc = p.low_cut_hz.clamp(10.0, 2000.0) as f64;
        Fdn {
            pre: [mk_pre(), mk_pre()],
            pre_n: ms64(p.predelay_ms.clamp(0.0, 250.0) as f64),
            diff,
            g_diff: 0.7 * p.diffusion.clamp(0.0, 1.0) as f64,
            lines,
            base,
            gains,
            mod_depth,
            damp: (-std::f64::consts::TAU * p.damping_hz.clamp(500.0, 20000.0) as f64 / SR as f64)
                .exp(),
            lp: [0.0; LINES],
            lfo,
            lfo_inc,
            lowcut: [LowCut64::new(lc), LowCut64::new(lc)],
            mix: p.mix.clamp(0.0, 1.0) as f64,
            width: p.width.clamp(0.0, 2.0) as f64,
        }
    }

    /// Process one block in place (any length; blocks may differ in size).
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        let (mix, width) = (self.mix, self.width);
        for i in 0..n {
            let mut ins = [
                if l[i].is_finite() { l[i] as f64 } else { 0.0 },
                if r[i].is_finite() { r[i] as f64 } else { 0.0 },
            ];
            for (side, x) in ins.iter_mut().enumerate() {
                self.pre[side].push(*x);
                let mut v = if self.pre_n < 1.0 {
                    *x
                } else {
                    self.pre[side].read(self.pre_n)
                };
                for ap in self.diff[side].iter_mut() {
                    v = ap.process(v, self.g_diff);
                }
                *x = v;
            }
            self.lfo += self.lfo_inc;
            if self.lfo >= 1.0 {
                self.lfo -= 1.0;
            }
            let (s, c) = (self.lfo * std::f64::consts::TAU).sin_cos();
            let mods = [0.0, s, 0.0, c, 0.0, -s, 0.0, -c];
            let mut o = [0.0f64; LINES];
            for k in 0..LINES {
                let raw = self.lines[k].read(self.base[k] + self.mod_depth * (1.0 + mods[k]));
                self.lp[k] = flush64(raw + (self.lp[k] - raw) * self.damp);
                o[k] = raw;
            }
            let mut fb: [f64; LINES] = std::array::from_fn(|k| self.lp[k] * self.gains[k]);
            hadamard8(&mut fb);
            for (k, (line, f)) in self.lines.iter_mut().zip(fb).enumerate() {
                let inj = if k % 2 == 0 { ins[0] } else { ins[1] };
                line.push(flush64(f + inj * 0.5));
            }
            let wl = self.lowcut[0].process(0.4 * (o[0] - o[2] + o[4] - o[6]));
            let wr = self.lowcut[1].process(0.4 * (o[1] - o[3] + o[5] - o[7]));
            let mid = 0.5 * (wl + wr);
            let side = 0.5 * (wl - wr) * width;
            let (wl, wr) = (mid + side, mid - side);
            let (dl, dr) = (
                if l[i].is_finite() { l[i] as f64 } else { 0.0 },
                if r[i].is_finite() { r[i] as f64 } else { 0.0 },
            );
            l[i] = (dl * (1.0 - mix) + wl * mix) as f32;
            r[i] = (dr * (1.0 - mix) + wr * mix) as f32;
        }
    }
}

/// The FDN over a whole stereo buffer: one `Fdn::process` call.
/// `offset`: the buffer's first sample's place in the song.
pub fn fdn_reverb(cfg: &FdnConfig, p: &FdnParams, l: &mut [f32], r: &mut [f32], offset: usize) {
    Fdn::new(cfg, p, offset).process(l, r);
}

// ---------------------------------------------------------------- compressor

/// Gain reduction (dB, >= 0) for a level with a quadratic soft knee.
pub fn compress_gr(level_db: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    let over = level_db - threshold;
    if knee > 1e-3 && 2.0 * over.abs() <= knee {
        let t = over + knee / 2.0;
        slope * t * t / (2.0 * knee)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    }
}

#[inline]
fn ballistic(state: f32, target: f32, att: f32, rel: f32) -> f32 {
    let c = if target > state { att } else { rel };
    target + (state - target) * c
}

fn time_coef(ms_: f32) -> f32 {
    let n = ms(ms_);
    if n < 1.0e-3 {
        0.0
    } else {
        (-1.0 / n).exp()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CompParams {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
    pub makeup_db: f32,
    /// Sidechain high-pass (Hz); 0 = off.
    pub sc_hpf_hz: f32,
    pub lookahead_ms: f32,
    /// Parallel mix 0..1.
    pub mix: f32,
}

/// Feed-forward stereo-linked compressor with soft knee, lookahead, a
/// sidechain high-pass and parallel mix. Offline, so the lookahead is
/// applied by reading the detector ahead (no latency shift). Returns the max
/// gain reduction in dB.
pub fn compress(p: &CompParams, l: &mut [f32], r: &mut [f32]) -> f32 {
    let n = l.len().min(r.len());
    if n == 0 {
        return 0.0;
    }
    let att = time_coef(p.attack_ms.max(0.01));
    let rel = time_coef(p.release_ms.max(1.0));
    let la = ms(p.lookahead_ms.clamp(0.0, 10.0)).round() as usize;
    let mut sc = if p.sc_hpf_hz > 0.0 {
        Some([
            Biquad::new(BiquadKind::LowCut, p.sc_hpf_hz, FRAC_1_SQRT_2, 0.0),
            Biquad::new(BiquadKind::LowCut, p.sc_hpf_hz, FRAC_1_SQRT_2, 0.0),
        ])
    } else {
        None
    };
    let makeup = db_to_gain(p.makeup_db);
    let mix = p.mix.clamp(0.0, 1.0);
    let mut gains = vec![1.0f32; n];
    let mut gr = 0.0f32;
    let mut max_gr = 0.0f32;
    for i in 0..n {
        let (a, b) = match sc.as_mut() {
            Some(f) => (f[0].process(l[i]), f[1].process(r[i])),
            None => (l[i], r[i]),
        };
        let level = a.abs().max(b.abs());
        let target = compress_gr(
            gain_to_db(level),
            p.threshold_db,
            p.ratio,
            p.knee_db.max(0.0),
        );
        gr = ballistic(gr, target, att, rel);
        if !gr.is_finite() {
            gr = 0.0;
        }
        max_gr = max_gr.max(gr);
        gains[i] = db_to_gain(-gr);
    }
    for i in 0..n {
        let g = gains[(i + la).min(n - 1)] * makeup;
        let k = (1.0 - mix) + g * mix;
        l[i] *= k;
        r[i] *= k;
    }
    max_gr
}

// ---------------------------------------------------------------- YIN

/// A detected note: start and end in samples, MIDI pitch, velocity estimate.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct DetectedNote {
    pub start: usize,
    pub end: usize,
    pub pitch: u8,
    pub velocity: u8,
}

/// Fundamental of one frame (YIN, de Cheveigne & Kawahara 2002), or None when unvoiced.
pub fn yin(
    frame: &[f32],
    sample_rate: f32,
    min_hz: f32,
    max_hz: f32,
    threshold: f32,
) -> Option<f32> {
    if !(sample_rate.is_finite() && sample_rate > 0.0) || frame.len() < 64 {
        return None;
    }
    let max_tau = ((sample_rate / min_hz.max(20.0)) as usize).min(frame.len() / 2);
    let min_tau = ((sample_rate / max_hz.max(min_hz + 1.0)) as usize).max(2);
    if max_tau <= min_tau + 2 {
        return None;
    }
    let w = frame.len() - max_tau;
    let mut d = vec![0.0f32; max_tau + 1];
    for (tau, dv) in d.iter_mut().enumerate().skip(1) {
        let mut sum = 0.0f32;
        for j in 0..w {
            let diff = frame[j] - frame[j + tau];
            sum += diff * diff;
        }
        *dv = sum;
    }
    let mut cmnd = vec![1.0f32; max_tau + 1];
    let mut running = 0.0f32;
    for tau in 1..=max_tau {
        running += d[tau];
        cmnd[tau] = if running > 0.0 {
            d[tau] * tau as f32 / running
        } else {
            1.0
        };
    }
    let mut tau = min_tau;
    while tau < max_tau {
        if cmnd[tau] < threshold {
            while tau + 1 < max_tau && cmnd[tau + 1] < cmnd[tau] {
                tau += 1;
            }
            let (a, b, c) = (cmnd[tau - 1], cmnd[tau], cmnd[tau + 1]);
            let denom = a - 2.0 * b + c;
            let shift = if denom.abs() > 1e-9 {
                0.5 * (a - c) / denom
            } else {
                0.0
            };
            let t = tau as f32 + shift.clamp(-1.0, 1.0);
            return (t > 0.0).then_some(sample_rate / t);
        }
        tau += 1;
    }
    None
}

/// YIN with aperiodicity and a sharper period estimate: (fundamental Hz,
/// aperiodicity), or None when unvoiced.
///
/// Same steps as `yin` (difference function, cumulative-mean-normalised
/// difference (CMNDF), the first lag under the absolute `threshold`, then
/// down to that dip's local minimum). Two differences: the aperiodicity
/// (the CMNDF at the chosen lag: 0 = perfectly periodic, near 1 = noise) is
/// returned for voicing gates, and the sub-sample period comes from a
/// parabola through the raw difference function around the dip rather than
/// through the CMNDF. The CMNDF's running-mean normalisation skews its
/// parabola, which costs `yin` up to ~1.8 cents at 440-880 Hz at 11.025 kHz;
/// here pure sines read within 0.5 cent from 82 Hz to 1 kHz at 11.025 and
/// 44.1 kHz. `yin` is left as it was so existing analyses stay byte-identical.
pub fn yin_aperiodicity(
    frame: &[f32],
    sample_rate: f32,
    min_hz: f32,
    max_hz: f32,
    threshold: f32,
) -> Option<(f32, f32)> {
    if !(sample_rate.is_finite() && sample_rate > 0.0) || frame.len() < 64 {
        return None;
    }
    let max_tau = ((sample_rate / min_hz.max(20.0)) as usize).min(frame.len() / 2);
    let min_tau = ((sample_rate / max_hz.max(min_hz + 1.0)) as usize).max(2);
    if max_tau <= min_tau + 2 {
        return None;
    }
    let w = frame.len() - max_tau;
    let mut d = vec![0.0f32; max_tau + 1];
    for (tau, dv) in d.iter_mut().enumerate().skip(1) {
        let mut sum = 0.0f32;
        for j in 0..w {
            let diff = frame[j] - frame[j + tau];
            sum += diff * diff;
        }
        *dv = sum;
    }
    let mut cmnd = vec![1.0f32; max_tau + 1];
    let mut running = 0.0f32;
    for tau in 1..=max_tau {
        running += d[tau];
        cmnd[tau] = if running > 0.0 {
            d[tau] * tau as f32 / running
        } else {
            1.0
        };
    }
    let mut tau = min_tau;
    while tau < max_tau {
        if cmnd[tau] < threshold {
            while tau + 1 < max_tau && cmnd[tau + 1] < cmnd[tau] {
                tau += 1;
            }
            let (a, b, c) = (d[tau - 1], d[tau], d[tau + 1]);
            let denom = a - 2.0 * b + c;
            let shift = if denom.abs() > 1e-12 {
                0.5 * (a - c) / denom
            } else {
                0.0
            };
            let t = tau as f32 + shift.clamp(-1.0, 1.0);
            return (t > 0.0).then_some((sample_rate / t, cmnd[tau]));
        }
        tau += 1;
    }
    None
}

/// Convert mono audio to notes: YIN per frame, runs of the same semitone of
/// at least 3 frames become notes.
pub fn audio_to_notes(audio: &[f32], sample_rate: f32) -> Vec<DetectedNote> {
    let sr = if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        SR
    };
    let win = ((sr * 0.046) as usize).clamp(256, 8192);
    let hop = win / 4;
    let min_frames = 3;
    let mut frames: Vec<(usize, Option<u8>, f32)> = Vec::new();
    let mut pos = 0usize;
    while pos + win <= audio.len() && frames.len() < 2_000_000 {
        let frame = &audio[pos..pos + win];
        let rms = (frame.iter().map(|x| x * x).sum::<f32>() / win as f32).sqrt();
        let pitch = if rms > 0.01 {
            yin(frame, sr, 50.0, 1600.0, 0.15)
                .map(|f| (69.0 + 12.0 * (f / 440.0).log2()).round().clamp(0.0, 127.0) as u8)
        } else {
            None
        };
        frames.push((pos, pitch, rms));
        pos += hop;
    }
    let mut notes = Vec::new();
    let mut i = 0;
    while i < frames.len() {
        let (start, Some(p), _) = frames[i] else {
            i += 1;
            continue;
        };
        let mut j = i;
        let mut peak = 0.0f32;
        while let Some((_, Some(q), rms)) = frames.get(j).copied() {
            if q != p {
                break;
            }
            peak = peak.max(rms);
            j += 1;
        }
        if j - i >= min_frames {
            let end = frames.get(j).map_or(audio.len(), |f| f.0);
            let velocity = (40.0 + 87.0 * (peak / 0.5).min(1.0)) as u8;
            notes.push(DetectedNote {
                start,
                end,
                pitch: p,
                velocity: velocity.clamp(1, 127),
            });
        }
        i = j.max(i + 1);
    }
    notes
}

// ---------------------------------------------------------------- transients

/// Spectral-flux onsets with an adaptive (local mean x mult + delta)
/// threshold; `sensitivity` 0..1. Returns sample positions.
pub fn detect_transients(mono: &[f32], sensitivity: f32) -> Vec<usize> {
    let len = mono.len();
    let sens = if sensitivity.is_finite() {
        sensitivity.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let n = ((SR * 0.023) as usize).clamp(64, 8192).next_power_of_two();
    let hop = n / 4;
    if len < n {
        return Vec::new();
    }
    let window: Vec<f32> = (0..n)
        .map(|i| 0.5 - 0.5 * (TAU * i as f32 / n as f32).cos())
        .collect();
    let frames = (len - n) / hop + 1;
    let mut prev = vec![0.0f32; n / 2 + 1];
    let mut flux = Vec::with_capacity(frames);
    let (mut re, mut im) = (vec![0.0f32; n], vec![0.0f32; n]);
    for m in 0..frames {
        let start = m * hop;
        for i in 0..n {
            re[i] = mono[start + i] * window[i];
            im[i] = 0.0;
        }
        crate::analysis::fft(&mut re, &mut im);
        let mut f = 0.0;
        for (k, p) in prev.iter_mut().enumerate() {
            let mag = (1.0 + 100.0 * (re[k] * re[k] + im[k] * im[k]).sqrt()).ln();
            f += (mag - *p).max(0.0);
            *p = mag;
        }
        flux.push(if m == 0 { 0.0 } else { f });
    }
    let max_flux = flux.iter().copied().fold(0.0f32, f32::max);
    if max_flux <= 1e-6 {
        return Vec::new();
    }
    let w = ((0.1 * SR) as usize / hop).max(2);
    let mult = 1.0 + 2.5 * (1.0 - sens);
    let delta = max_flux * (0.02 + 0.25 * (1.0 - sens));
    let min_gap = ((0.05 * SR) as usize / hop).max(1);
    let mut out: Vec<usize> = Vec::new();
    let mut last: Option<usize> = None;
    for m in 1..flux.len() {
        let cur = flux[m];
        let lo = m.saturating_sub(w);
        let hi = (m + w + 1).min(flux.len());
        let local = flux[lo..hi].iter().sum::<f32>() / (hi - lo).max(1) as f32;
        let prev_v = flux[m - 1];
        let next_v = flux.get(m + 1).copied().unwrap_or(0.0);
        if cur > local * mult + delta
            && cur >= prev_v
            && cur > next_v
            && last.is_none_or(|l| m - l >= min_gap)
        {
            last = Some(m);
            let start = m * hop;
            let seg = &mono[start..(start + n).min(len)];
            let pk = seg.iter().fold(0.0f32, |a, v| a.max(v.abs()));
            let off = seg
                .iter()
                .position(|v| v.abs() >= 0.25 * pk)
                .unwrap_or(n / 2);
            let pos = start + off;
            if out.last().is_none_or(|&p| pos > p) {
                out.push(pos);
            }
        }
    }
    out
}

/// Strip silence: (start, end) ranges whose peak exceeds `threshold_db`;
/// gaps shorter than `min_gap` samples do not split; padded and merged.
pub fn non_silent_ranges(
    x: &[f32],
    threshold_db: f32,
    min_gap: usize,
    pad_before: usize,
    pad_after: usize,
) -> Vec<(usize, usize)> {
    let len = x.len();
    let thr = db_to_gain(if threshold_db.is_nan() {
        -48.0
    } else {
        threshold_db
    });
    let mut raw: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    let mut last_loud = 0usize;
    for (i, v) in x.iter().enumerate() {
        if !(v.is_finite() && v.abs() > thr) {
            continue;
        }
        match start {
            Some(s) if i - last_loud > min_gap.max(1) => {
                raw.push((s, last_loud + 1));
                start = Some(i);
            }
            None => start = Some(i),
            _ => {}
        }
        last_loud = i;
    }
    if let Some(s) = start {
        raw.push((s, last_loud + 1));
    }
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(raw.len());
    for (s, e) in raw {
        let s = s.saturating_sub(pad_before);
        let e = e.saturating_add(pad_after).min(len);
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f32, secs: f32) -> Vec<f32> {
        (0..(secs * SR) as usize)
            .map(|i| (i as f32 * f * TAU / SR).sin() * 0.5)
            .collect()
    }

    fn level_db(x: &[f32], at_s: f32) -> f32 {
        let s = (at_s * SR) as usize;
        let w = (0.05 * SR) as usize;
        let e: f32 = x[s..s + w].iter().map(|v| v * v).sum::<f32>() / w as f32;
        10.0 * e.max(1e-20).log10()
    }

    #[test]
    fn yin_aperiodicity_reads_sines_within_a_cent() {
        for (sr, win) in [(11_025.0f32, 441usize), (44_100.0, 2048)] {
            for f0 in [82.41f32, 110.0, 196.0, 261.63, 440.0, 659.26, 880.0, 1046.5] {
                let f: Vec<f32> = (0..win)
                    .map(|i| (TAU * f0 * i as f32 / sr + 0.3).sin())
                    .collect();
                let (hz, ap) = yin_aperiodicity(&f, sr, 65.0, 1100.0, 0.15)
                    .unwrap_or_else(|| panic!("{f0} Hz at {sr}: unvoiced"));
                let cents = 1200.0 * (hz / f0).log2();
                assert!(cents.abs() < 1.0, "{f0} Hz at {sr}: {cents:.3} cents");
                assert!(ap < 0.05, "{f0} Hz at {sr}: aperiodicity {ap}");
            }
        }
    }

    #[test]
    fn yin_aperiodicity_no_octave_error_on_a_harmonic_rich_tone() {
        // weak fundamental, nine strong harmonics (1/k): the classic octave trap
        let sr = 11_025.0f32;
        for f0 in [110.0f32, 220.0, 330.0] {
            let f: Vec<f32> = (0..441)
                .map(|i| {
                    let t = i as f32 / sr;
                    (1..=10)
                        .filter(|k| f0 * *k as f32 <= 5000.0)
                        .map(|k| {
                            (if k == 1 { 0.3 } else { 1.0 / k as f32 })
                                * (TAU * f0 * k as f32 * t).sin()
                        })
                        .sum::<f32>()
                })
                .collect();
            let (hz, _) = yin_aperiodicity(&f, sr, 65.0, 1100.0, 0.15).expect("voiced");
            let cents = 1200.0 * (hz / f0).log2();
            assert!(
                cents.abs() < 5.0,
                "{f0} Hz harmonic tone read {hz} Hz ({cents:.1} cents)"
            );
        }
    }

    #[test]
    fn yin_aperiodicity_noise_is_unvoiced() {
        let mut seed = 99u32;
        for _ in 0..20 {
            let f: Vec<f32> = (0..441)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (seed >> 8) as f32 / 8388608.0 - 1.0
                })
                .collect();
            assert!(
                yin_aperiodicity(&f, 11_025.0, 65.0, 1100.0, 0.15).is_none(),
                "white noise read as voiced"
            );
        }
    }

    #[test]
    fn fdn_blocks_of_any_size_match_one_call() {
        // a burst, a tone and silence, through the hall with pre-delay, from a song offset
        let n = 3 * SR as usize;
        let src: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / SR;
                if i < 2000 {
                    ((i * 7919) % 101) as f32 / 50.0 - 1.0
                } else if t < 1.5 {
                    0.3 * (TAU * 330.0 * t).sin()
                } else {
                    0.0
                }
            })
            .collect();
        let p = FdnParams {
            decay_s: 3.0,
            size: 0.6,
            damping_hz: 9000.0,
            diffusion: 0.7,
            predelay_ms: 30.0,
            width: 1.2,
            low_cut_hz: 120.0,
            mix: 0.35,
        };
        let off = 123_457;
        let (mut wl, mut wr) = (
            src.clone(),
            src.iter().map(|x| -0.8 * x).collect::<Vec<f32>>(),
        );
        let (sl, sr) = (wl.clone(), wr.clone());
        fdn_reverb(&HALL, &p, &mut wl, &mut wr, off);
        for bs in [1usize, 7, 64, 127, 512] {
            let (mut l, mut r) = (sl.clone(), sr.clone());
            let mut f = Fdn::new(&HALL, &p, off);
            let mut i = 0;
            while i < n {
                let j = (i + bs).min(n);
                f.process(&mut l[i..j], &mut r[i..j]);
                i = j;
            }
            assert!(
                l == wl && r == wr,
                "block size {bs}: not bit-identical to one call"
            );
        }
    }

    #[test]
    fn fdn_decay_follows_rt60() {
        for decay in [0.8f32, 2.0] {
            let n = (SR * (decay + 1.0)) as usize;
            let mut l = vec![0.0f32; n];
            l[0] = 1.0;
            let mut r = l.clone();
            let p = FdnParams {
                decay_s: decay,
                size: 0.5,
                damping_hz: 20000.0,
                diffusion: 0.75,
                predelay_ms: 0.0,
                width: 1.0,
                low_cut_hz: 20.0,
                mix: 1.0,
            };
            fdn_reverb(&ROOM, &p, &mut l, &mut r, 0);
            let drop = level_db(&l, 0.2) - level_db(&l, 0.2 + decay / 2.0);
            assert!(
                (drop - 30.0).abs() < 8.0,
                "decay {decay}: {drop} dB over half RT60"
            );
            assert!(l.iter().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn compressor_static_curve_knee_and_sidechain_hpf() {
        assert_eq!(compress_gr(-30.0, -20.0, 4.0, 0.0), 0.0);
        assert!((compress_gr(-10.0, -20.0, 4.0, 0.0) - 7.5).abs() < 1e-4);
        let at = compress_gr(-20.0, -20.0, 4.0, 6.0);
        assert!(at > 0.0 && at < 1.0, "{at}");
        assert!((compress_gr(-10.0, -20.0, 4.0, 6.0) - 7.5).abs() < 1e-4);
        let p = CompParams {
            threshold_db: -16.0,
            ratio: 4.0,
            attack_ms: 1.0,
            release_ms: 50.0,
            knee_db: 0.0,
            makeup_db: 0.0,
            sc_hpf_hz: 0.0,
            lookahead_ms: 5.0,
            mix: 1.0,
        };
        let mut l = tone(1000.0, 1.0);
        let mut r = l.clone();
        let gr = compress(&p, &mut l, &mut r);
        let expect = compress_gr(gain_to_db(0.5), -16.0, 4.0, 0.0);
        assert!((gr - expect).abs() < 0.6, "gr {gr} vs {expect}");
        let tail_pk = l[30000..40000].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((gain_to_db(tail_pk) - (gain_to_db(0.5) - expect)).abs() < 0.7);
        let (mut a, mut b) = (tone(40.0, 0.5), tone(40.0, 0.5));
        let plain = compress(&p, &mut a, &mut b);
        let (mut a, mut b) = (tone(40.0, 0.5), tone(40.0, 0.5));
        let hp = compress(
            &CompParams {
                sc_hpf_hz: 300.0,
                ..p
            },
            &mut a,
            &mut b,
        );
        assert!(hp < plain - 3.0, "hpf {hp} vs {plain}");
    }

    #[test]
    fn yin_error_is_small() {
        for f in [41.2f32, 55.0, 110.0, 261.63, 440.0, 987.77] {
            let t = tone(f, 0.2);
            let est = yin(&t[..4096], SR, 30.0, 1600.0, 0.15).unwrap();
            let cents = 1200.0 * (est / f).log2();
            assert!(cents.abs() < 5.0, "{f} Hz -> {est} ({cents} cents)");
        }
        let mut a = tone(261.63, 0.4);
        a.extend(vec![0.0; 4800]);
        a.extend(tone(392.0, 0.4));
        let pitches: Vec<u8> = audio_to_notes(&a, SR).iter().map(|x| x.pitch).collect();
        assert_eq!(pitches, vec![60, 67]);
        assert!(audio_to_notes(&vec![0.0; 44100], SR).is_empty());
    }

    #[test]
    fn transients_and_strip_silence() {
        let mut x = vec![0.0f32; (SR * 2.0) as usize];
        let mut rng = crate::dsp::Rng::new(3);
        for k in 0..4 {
            let s = k * (SR * 0.5) as usize + 1000;
            for j in 0..3000 {
                x[s + j] = rng.bipolar() * (-(j as f32) / 400.0).exp() * 0.7;
            }
        }
        let t = detect_transients(&x, 0.5);
        assert_eq!(t.len(), 4, "{t:?}");
        for (k, p) in t.iter().enumerate() {
            assert!((*p as i64 - (k * 22050 + 1000) as i64).abs() < 300, "{t:?}");
        }
        let r = non_silent_ranges(&x, -40.0, 100, 10, 10);
        assert_eq!(r.len(), 4, "{r:?}");
    }
}
