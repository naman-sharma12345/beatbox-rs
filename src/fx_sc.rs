//! Insert effects hand-ported from SoundCraft: Mod Delay (modulated
//! feedback delay with a low-pass in the loop), Maximizer (lookahead
//! brickwall limiter with drive), Lo-Fi (bit crusher + sample-and-hold
//! decimator with hiss) and Rectifier (half/full-wave, octave-up harmonics).
//!
//! Adapted from SoundCraft (https://github.com/storytold/soundcraft):
//! `crates/dsp/src/plugins/modulation.rs` (`ModDelay`),
//! `crates/dsp/src/plugins/dynamics.rs` (`Maximizer`),
//! `crates/dsp/src/plugins/harmonic.rs` (`LoFi`, `Rectifier`) and
//! `crates/dsp/src/util.rs` (`DelayLine`, `DcBlocker`, `Rng`).
//! at commit 7414719. Copyright (c) 2026 ArtCraft Team and the SoundCraft
//! contributors, MIT OR Apache-2.0 (used
//! here under MIT). Changes: edition 2024 -> 2021, stateful block plugins
//! rewritten as whole-buffer stereo functions with static parameters (no
//! smoothing), parameters in beatbox units (0..1 mixes, dB, ms). See
//! THIRD_PARTY_NOTICES.md.

use crate::dsp::{db_to_gain, SR};
use crate::fx::{LoFiFx, MaximizerFx, ModDelayFx, RectifierFx};
use std::f32::consts::TAU;

#[inline]
fn flush(x: f32) -> f32 {
    if x.is_finite() && x.abs() > 1.0e-25 {
        x
    } else {
        0.0
    }
}

#[inline]
fn fin(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

fn ms_to_samples(ms: f32) -> f32 {
    let s = ms * 0.001 * SR;
    if s.is_finite() {
        s.max(0.0)
    } else {
        0.0
    }
}

/// Power-of-two ring buffer delay line.
struct Line {
    buf: Vec<f32>,
    mask: usize,
    pos: usize,
}

impl Line {
    fn new(max_delay: usize) -> Self {
        let len = max_delay.saturating_add(4).clamp(4, 1 << 24).next_power_of_two();
        Line { buf: vec![0.0; len], mask: len - 1, pos: 0 }
    }
    #[inline]
    fn push(&mut self, x: f32) {
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) & self.mask;
    }
    /// The sample written `d` pushes ago (1 = the last one).
    #[inline]
    fn tap(&self, d: usize) -> f32 {
        self.buf[self.pos.wrapping_sub(d) & self.mask]
    }
    fn max_delay(&self) -> f32 {
        self.buf.len().saturating_sub(3) as f32
    }
    /// Hermite-interpolated read `d` samples back.
    #[inline]
    fn read_cubic(&self, d: f32) -> f32 {
        let d = if d.is_finite() { d.clamp(2.0, (self.max_delay() - 1.0).max(2.0)) } else { 2.0 };
        let i = d as usize;
        let f = d - i as f32;
        let (xm1, x0, x1, x2) = (self.tap(i - 1), self.tap(i), self.tap(i + 1), self.tap(i + 2));
        let c1 = 0.5 * (x1 - xm1);
        let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
        let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
        ((c3 * f + c2) * f + c1) * f + x0
    }
}

const MAX_DELAY_MS: f32 = 2000.0;
const MOD_DEPTH_MS: f32 = 5.0;

/// Feedback delay in milliseconds with LFO modulation of the delay time
/// (right channel a quarter cycle ahead) and a one-pole low-pass inside
/// the feedback loop, so repeats darken.
/// `offset`: the buffer's place in the song (samples); the modulation LFO
/// starts where a whole-song render's would be there.
pub fn mod_delay(p: &ModDelayFx, l: &mut [f32], r: &mut [f32], offset: usize) {
    let time = p.time_ms.clamp(1.0, MAX_DELAY_MS);
    let fb = p.feedback.clamp(-0.99, 0.99);
    let mix = p.mix.clamp(0.0, 1.0);
    let depth = p.mod_depth.clamp(0.0, 1.0) * MOD_DEPTH_MS;
    let inc = (p.mod_rate_hz.clamp(0.01, 10.0) / SR).min(0.5);
    let a = (-TAU * p.lpf_hz.clamp(500.0, 20000.0) / SR).exp();
    let max = ms_to_samples(MAX_DELAY_MS + 2.0 * MOD_DEPTH_MS) as usize + 8;
    let mut lines = [Line::new(max), Line::new(max)];
    let mut lp = [0.0f32; 2];
    let mut phase = 0.0f32;
    // the same f32 accumulation a whole-song render runs up to `offset`
    for _ in 0..offset {
        phase += inc;
        if phase >= 1.0 {
            phase -= 1.0;
        }
    }
    let n = l.len().min(r.len());
    for i in 0..n {
        phase += inc;
        if phase >= 1.0 {
            phase -= 1.0;
        }
        for c in 0..2 {
            let x = if c == 0 { &mut l[i] } else { &mut r[i] };
            let dry = fin(*x);
            let ph = if c == 1 { (phase + 0.25).fract() } else { phase };
            let modv = depth * 0.5 * (1.0 + (ph * TAU).sin());
            let y = lines[c].read_cubic(ms_to_samples(time + modv));
            lp[c] = flush(y + (lp[c] - y) * a);
            lines[c].push(flush(dry + fb * lp[c]));
            *x = dry * (1.0 - mix) + lp[c] * mix;
        }
    }
}

const LOOKAHEAD_MS: f32 = 1.5;

/// Lookahead brickwall limiter: the input is driven by `-threshold_db`
/// and never exceeds `ceiling_db`. Gain is a min-hold over the lookahead
/// window followed by an equal-length moving average, so it ramps down
/// before each peak reaches the output. Adds 1.5 ms of latency, which is
/// compensated here (the output is shifted back) so the insert stays in time.
/// Returns the largest gain reduction in dB.
pub fn maximizer(p: &MaximizerFx, l: &mut [f32], r: &mut [f32]) -> f32 {
    let n = l.len().min(r.len());
    if n == 0 {
        return 0.0;
    }
    let drive = db_to_gain(-p.threshold_db.clamp(-30.0, 0.0));
    let ceil = db_to_gain(p.ceiling_db.clamp(-30.0, 0.0));
    let rn = ms_to_samples(p.release_ms.clamp(1.0, 1000.0));
    let rel = if rn < 1.0e-3 { 0.0 } else { (-1.0 / rn).exp() };
    let la = (ms_to_samples(LOOKAHEAD_MS).round() as usize).max(1);
    let mut dl = [Line::new(la + 2), Line::new(la + 2)];
    let mut req = vec![1.0f32; la];
    let mut smooth = vec![1.0f32; la];
    let (mut idx, mut sum, mut held, mut min_gain) = (0usize, la as f64, 1.0f32, 1.0f32);
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    // run la - 1 extra frames of silence to flush the lookahead
    for k in 0..n + la - 1 {
        let (xl, xr) = if k < n { (fin(l[k]) * drive, fin(r[k]) * drive) } else { (0.0, 0.0) };
        dl[0].push(xl);
        dl[1].push(xr);
        let peak = xl.abs().max(xr.abs());
        req[idx] = if peak > ceil { ceil / peak } else { 1.0 };
        let hold = req.iter().copied().fold(1.0f32, f32::min);
        held = if hold < held { hold } else { hold + (held - hold) * rel };
        sum += f64::from(held) - f64::from(smooth[idx]);
        smooth[idx] = held;
        idx = (idx + 1) % la;
        if idx == 0 {
            sum = smooth.iter().map(|&v| f64::from(v)).sum();
        }
        let g = ((sum / la as f64) as f32).clamp(0.0, 1.0);
        min_gain = min_gain.min(g);
        if k + 1 >= la {
            let o = k + 1 - la;
            out_l[o] = (dl[0].tap(la) * g).clamp(-ceil, ceil);
            out_r[o] = (dl[1].tap(la) * g).clamp(-ceil, ceil);
        }
    }
    l[..n].copy_from_slice(&out_l);
    r[..n].copy_from_slice(&out_r);
    -20.0 * min_gain.max(1.0e-6).log10()
}

/// Xorshift32 noise, deterministic.
struct Rng(u32);
impl Rng {
    fn bipolar(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
    }
}

/// Bit crusher and sample-and-hold decimator with optional hiss.
pub fn lofi(p: &LoFiFx, l: &mut [f32], r: &mut [f32]) {
    let step = 2f32.powf(1.0 - p.bits.clamp(1.0, 24.0));
    let inc = (p.sample_rate_hz.clamp(500.0, 48000.0) / SR).min(1.0);
    let nz = {
        let v = p.noise.clamp(0.0, 1.0);
        v * v * 0.1
    };
    let mix = p.mix.clamp(0.0, 1.0);
    let mut rng = Rng(77);
    let mut phase = 1.0f32;
    let mut held = [0.0f32; 2];
    let n = l.len().min(r.len());
    for i in 0..n {
        phase += inc;
        let grab = phase >= 1.0;
        if grab {
            phase -= 1.0;
        }
        for c in 0..2 {
            let x = if c == 0 { &mut l[i] } else { &mut r[i] };
            let dry = fin(*x);
            if grab {
                let noisy = dry + if nz > 0.0 { rng.bipolar() * nz } else { 0.0 };
                held[c] = (noisy / step).round() * step;
            }
            *x = dry * (1.0 - mix) + held[c] * mix;
        }
    }
}

/// Half- or full-wave rectifier (adds octave-up harmonics) with a 10 Hz
/// DC blocker on the wet path.
pub fn rectifier(p: &RectifierFx, l: &mut [f32], r: &mut [f32]) {
    let full = !p.mode.eq_ignore_ascii_case("half");
    let mix = p.mix.clamp(0.0, 1.0);
    let out = db_to_gain(p.output_db.clamp(-24.0, 12.0));
    let rc = (-TAU * 10.0 / SR).exp();
    for ch in [l, r] {
        let (mut x1, mut y1) = (0.0f32, 0.0f32);
        for s in ch.iter_mut() {
            let dry = fin(*s);
            let w = if full { dry.abs() } else { dry.max(0.0) };
            let y = w - x1 + rc * y1;
            x1 = w;
            y1 = flush(y);
            *s = (dry * (1.0 - mix) + y1 * mix) * out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f32, secs: f32, amp: f32) -> Vec<f32> {
        (0..(secs * SR) as usize).map(|i| amp * (TAU * f * i as f32 / SR).sin()).collect()
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    #[test]
    fn maximizer_holds_the_ceiling_and_stays_in_time() {
        let mut l = sine(220.0, 1.0, 1.6);
        let mut r = l.clone();
        let p = MaximizerFx { threshold_db: -6.0, ceiling_db: -1.0, ..Default::default() };
        let gr = maximizer(&p, &mut l, &mut r);
        assert!(peak(&l) <= db_to_gain(-1.0) + 1e-4, "peak {}", peak(&l));
        assert!(gr > 6.0, "gr {gr}");
        // no latency: a click at 0.5 s stays at 0.5 s
        let mut c = vec![0.0f32; SR as usize];
        c[22050] = 0.5;
        let mut c2 = c.clone();
        maximizer(&MaximizerFx::default(), &mut c, &mut c2);
        let at = c.iter().enumerate().max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap()).unwrap().0;
        assert_eq!(at, 22050);
    }

    #[test]
    fn mod_delay_repeats_and_darkens() {
        let mut l = vec![0.0f32; SR as usize];
        l[0] = 1.0;
        let mut r = l.clone();
        let p = ModDelayFx { time_ms: 100.0, feedback: 0.5, mix: 1.0, mod_depth: 0.0, ..Default::default() };
        mod_delay(&p, &mut l, &mut r, 0);
        let d = ms_to_samples(100.0) as usize;
        let e1: f32 = l[d - 5..d + 20].iter().map(|v| v.abs()).sum();
        let e2: f32 = l[2 * d - 5..2 * d + 40].iter().map(|v| v.abs()).sum();
        assert!(e1 > 0.3 && e2 > 0.05 && e2 < e1, "{e1} {e2}");
        assert!(l[..d - 10].iter().all(|v| v.abs() < 1e-6));
    }

    #[test]
    fn lofi_quantizes_and_rectifier_doubles_frequency() {
        let mut l = sine(100.0, 0.5, 0.8);
        let mut r = l.clone();
        lofi(&LoFiFx { bits: 3.0, sample_rate_hz: 8000.0, ..Default::default() }, &mut l, &mut r);
        let mut levels: Vec<i32> = l.iter().map(|v| (v * 1000.0).round() as i32).collect();
        levels.sort_unstable();
        levels.dedup();
        assert!(levels.len() <= 9, "{} levels", levels.len());
        // full-wave rectified 100 Hz has its fundamental at 200 Hz
        let mut a = sine(100.0, 1.0, 0.5);
        let mut b = a.clone();
        rectifier(&RectifierFx::default(), &mut a, &mut b);
        let corr = |f: f32| {
            let (mut c, mut s) = (0.0f32, 0.0f32);
            for (i, v) in a[(0.2 * SR) as usize..].iter().enumerate() {
                let ph = TAU * f * i as f32 / SR;
                c += v * ph.cos();
                s += v * ph.sin();
            }
            (c * c + s * s).sqrt()
        };
        assert!(corr(200.0) > 10.0 * corr(100.0), "{} vs {}", corr(200.0), corr(100.0));
    }
}
