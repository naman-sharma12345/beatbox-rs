//! Modulation effects from FL's rack: Frequency Shifter, Ring Modulator and
//! Stereo Shaper. Whole-buffer stereo processors like the rest of the rack.
//!
//! * frequency shifter: every partial moves by the same number of Hz (not a
//!   pitch shift), via a 90-degree IIR allpass pair (an analytic signal)
//!   times a complex oscillator. Small shifts (0.5-5 Hz) give a slow
//!   phasing swirl; big ones make metallic, inharmonic FX and risers.
//! * ring modulator: the input times a carrier, so a partial at f becomes
//!   f-c and f+c: bells, robots, sci-fi, a dirtier 808.
//! * stereo shaper: a 2x2 L/R matrix plus a delay and a phase flip on one
//!   side, with presets (mono, swap, wide, pseudo_stereo, side_only, ...).

use crate::dsp::SR;
use crate::fx::{FreqShiftFx, RingModFx, StereoShaperFx};
use std::f32::consts::PI;

/// One 2nd-order allpass section of the Hilbert pair: y = a^2 (x + y[-2]) - x[-2].
#[derive(Clone, Copy, Default)]
struct Ap2 {
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Ap2 {
    fn new(a: f32) -> Self {
        Ap2 { a2: a * a, ..Default::default() }
    }
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.a2 * (x + self.y2) - self.x2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Two allpass chains whose outputs stay ~90 degrees apart from ~20 Hz to
/// ~20 kHz (Niemitalo's coefficients): (in-phase, quadrature).
struct Hilbert {
    a: [Ap2; 4],
    b: [Ap2; 4],
    delay: f32,
}

impl Hilbert {
    fn new() -> Self {
        let ca = [0.692_387_8, 0.936_065_43, 0.988_229_5, 0.998_748_85];
        let cb = [0.402_192_12, 0.856_171_1, 0.972_290_95, 0.995_288_5];
        Hilbert { a: ca.map(Ap2::new), b: cb.map(Ap2::new), delay: 0.0 }
    }
    #[inline]
    fn process(&mut self, x: f32) -> (f32, f32) {
        let mut i = x;
        for s in self.a.iter_mut() {
            i = s.process(i);
        }
        let mut q = x;
        for s in self.b.iter_mut() {
            q = s.process(q);
        }
        // the in-phase path runs one sample late
        let re = self.delay;
        self.delay = i;
        (re, q)
    }
}

pub fn check_freq_shift(p: &FreqShiftFx) -> Result<(), String> {
    if !(-5000.0..=5000.0).contains(&p.shift_hz) {
        return Err(format!("shift_hz {} outside -5000..5000", p.shift_hz));
    }
    Ok(())
}

pub fn freq_shift(p: &FreqShiftFx, l: &mut [f32], r: &mut [f32]) {
    let mix = p.mix.clamp(0.0, 1.0);
    let fb = p.feedback.clamp(0.0, 0.9);
    // the right side shifts by shift + stereo_hz: the two sides drift apart
    for (ci, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
        let f = p.shift_hz + if ci == 1 { p.stereo_hz } else { 0.0 };
        let w = 2.0 * PI * f / SR;
        let mut h = Hilbert::new();
        let mut last = 0.0f32;
        let (mut c, mut s) = (1.0f32, 0.0f32);
        let (cw, sw) = (w.cos(), w.sin());
        for (i, x) in ch.iter_mut().enumerate() {
            let (re, im) = h.process(*x + last * fb);
            // upper sideband for a positive shift (sign checked by the test)
            let y = re * c + im * s;
            last = y;
            *x = *x * (1.0 - mix) + y * mix;
            // rotate the oscillator; renormalise now and then
            let nc = c * cw - s * sw;
            s = s * cw + c * sw;
            c = nc;
            if i % 4096 == 0 {
                let m = (c * c + s * s).sqrt().max(1e-9);
                c /= m;
                s /= m;
            }
        }
    }
}

pub fn check_ring_mod(p: &RingModFx) -> Result<(), String> {
    if !["sine", "triangle", "square", "saw"].contains(&p.shape.as_str()) {
        return Err(format!("shape '{}' (sine, triangle, square, saw)", p.shape));
    }
    if !(0.1..=10000.0).contains(&p.freq_hz) {
        return Err(format!("freq_hz {} outside 0.1..10000", p.freq_hz));
    }
    Ok(())
}

fn carrier(shape: &str, ph: f32) -> f32 {
    // ph in [0, 1)
    match shape {
        "triangle" => 1.0 - 4.0 * (ph - 0.5).abs(),
        "square" => {
            // softened edges so the square does not click
            (((2.0 * PI * ph).sin()) * 6.0).tanh()
        }
        "saw" => 2.0 * ph - 1.0,
        _ => (2.0 * PI * ph).sin(),
    }
}

pub fn ring_mod(p: &RingModFx, l: &mut [f32], r: &mut [f32]) {
    let mix = p.mix.clamp(0.0, 1.0);
    let inc = p.freq_hz / SR;
    for (ci, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
        let mut ph = if ci == 1 { p.stereo_phase.rem_euclid(1.0) } else { 0.0 };
        // a slow LFO on the carrier frequency (lfo_hz, lfo_depth in semitones)
        for (i, x) in ch.iter_mut().enumerate() {
            let semis = if p.lfo_hz > 0.0 { p.lfo_depth * (2.0 * PI * p.lfo_hz * i as f32 / SR).sin() } else { 0.0 };
            let step = inc * 2f32.powf(semis / 12.0);
            let c = carrier(&p.shape, ph);
            *x = *x * (1.0 - mix) + *x * c * mix;
            ph += step;
            if ph >= 1.0 {
                ph -= ph.floor();
            }
        }
    }
}

/// (ll, rl, lr, rr, delay side, invert side) for a preset; ll = left from
/// left, rl = left from right, lr = right from left, rr = right from right.
fn preset(name: &str) -> Option<(f32, f32, f32, f32)> {
    Some(match name {
        "none" | "custom" => return None,
        "mono" => (0.5, 0.5, 0.5, 0.5),
        "swap" => (0.0, 1.0, 1.0, 0.0),
        "left_only" => (1.0, 0.0, 1.0, 0.0),
        "right_only" => (0.0, 1.0, 0.0, 1.0),
        // M/S matrices written out as L/R gains
        "wide" => (1.35, -0.35, -0.35, 1.35),
        "narrow" => (0.75, 0.25, 0.25, 0.75),
        "side_only" => (0.5, -0.5, -0.5, 0.5),
        "pseudo_stereo" => (1.0, 0.0, 0.0, 1.0),
        _ => return None,
    })
}

pub const STEREO_PRESETS: &[&str] = &["custom", "mono", "swap", "left_only", "right_only", "wide", "narrow", "side_only", "pseudo_stereo"];

pub fn check_stereo_shaper(p: &StereoShaperFx) -> Result<(), String> {
    if !STEREO_PRESETS.contains(&p.preset.as_str()) && p.preset != "none" {
        return Err(format!("preset '{}' ({})", p.preset, STEREO_PRESETS.join(", ")));
    }
    if !(0.0..=50.0).contains(&p.delay_ms) {
        return Err(format!("delay_ms {} outside 0..50", p.delay_ms));
    }
    Ok(())
}

pub fn stereo_shaper(p: &StereoShaperFx, l: &mut [f32], r: &mut [f32]) {
    let (ll, rl, lr, rr) = preset(&p.preset).unwrap_or((p.left_from_left, p.left_from_right, p.right_from_left, p.right_from_right));
    let mix = p.mix.clamp(0.0, 1.0);
    // pseudo_stereo: a mono source gets a short delay on the right, and the
    // delayed side is phase-flipped against the low end staying mono
    let pseudo = p.preset == "pseudo_stereo";
    let delay_ms = if pseudo && p.delay_ms == 0.0 { 12.0 } else { p.delay_ms };
    let d = (delay_ms.clamp(0.0, 50.0) * 0.001 * SR) as usize;
    let mut buf = vec![0.0f32; d + 1];
    let mut w = 0usize;
    let flip_l = if p.invert_left { -1.0 } else { 1.0 };
    let flip_r = if p.invert_right { -1.0 } else { 1.0 };
    let delay_right = p.delay_side >= 0.0;
    for i in 0..l.len() {
        let (x_l, x_r) = (l[i], r[i]);
        let mut yl = (ll * x_l + rl * x_r) * flip_l;
        let mut yr = (lr * x_l + rr * x_r) * flip_r;
        if d > 0 {
            let target = if delay_right { yr } else { yl };
            let out = buf[w];
            buf[w] = target;
            w = (w + 1) % buf.len();
            let delayed = if pseudo {
                // the mid passes straight; only the delayed difference widens
                let m = 0.5 * (yl + yr);
                m + 0.5 * (out - m) * 1.2
            } else {
                out
            };
            if delay_right {
                yr = delayed;
            } else {
                yl = delayed;
            }
        }
        l[i] = x_l * (1.0 - mix) + yl * mix;
        r[i] = x_r * (1.0 - mix) + yr * mix;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * PI * f * i as f32 / SR).sin() * 0.5).collect()
    }

    /// Goertzel power at f over x.
    fn power(x: &[f32], f: f32) -> f32 {
        let w = 2.0 * PI * f / SR;
        let c = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &v in x {
            let s = v + c * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        (s1 * s1 + s2 * s2 - c * s1 * s2) / (x.len() as f32).powi(2)
    }

    #[test]
    fn freq_shift_moves_every_partial_by_hz() {
        let n = 44100;
        let mut l = tone(1000.0, n);
        let mut r = l.clone();
        let p = FreqShiftFx { shift_hz: 150.0, ..Default::default() };
        freq_shift(&p, &mut l, &mut r);
        let tail = &l[4410..];
        let up = power(tail, 1150.0);
        let down = power(tail, 850.0);
        let orig = power(tail, 1000.0);
        assert!(up > 40.0 * down, "upper sideband wins: {up} vs {down}");
        assert!(up > 40.0 * orig, "the original partial is gone: {up} vs {orig}");
        // negative shift goes down
        let mut l2 = tone(1000.0, n);
        let mut r2 = l2.clone();
        freq_shift(&FreqShiftFx { shift_hz: -150.0, ..Default::default() }, &mut l2, &mut r2);
        assert!(power(&l2[4410..], 850.0) > 40.0 * power(&l2[4410..], 1150.0));
        assert!(l.iter().chain(l2.iter()).all(|x| x.is_finite() && x.abs() < 1.5));
    }

    #[test]
    fn ring_mod_makes_sum_and_difference() {
        let n = 44100;
        let mut l = tone(1000.0, n);
        let mut r = l.clone();
        ring_mod(&RingModFx { freq_hz: 200.0, ..Default::default() }, &mut l, &mut r);
        let (lo, hi, mid) = (power(&l, 800.0), power(&l, 1200.0), power(&l, 1000.0));
        assert!(lo > 100.0 * mid && hi > 100.0 * mid, "{lo} {hi} {mid}");
        assert!(check_ring_mod(&RingModFx { shape: "wobble".into(), ..Default::default() }).is_err());
    }

    #[test]
    fn stereo_shaper_presets() {
        let mut l = vec![1.0f32; 100];
        let mut r = vec![0.0f32; 100];
        stereo_shaper(&StereoShaperFx { preset: "swap".into(), ..Default::default() }, &mut l, &mut r);
        assert_eq!((l[50], r[50]), (0.0, 1.0));
        let mut l = vec![1.0f32; 100];
        let mut r = vec![0.0f32; 100];
        stereo_shaper(&StereoShaperFx { preset: "mono".into(), ..Default::default() }, &mut l, &mut r);
        assert_eq!(l[50], r[50]);
        // pseudo stereo widens a mono source: the sides differ after the delay
        let mut l = tone(700.0, 4410);
        let mut r = l.clone();
        stereo_shaper(&StereoShaperFx { preset: "pseudo_stereo".into(), ..Default::default() }, &mut l, &mut r);
        let side: f32 = l.iter().zip(&r).skip(1000).map(|(a, b)| (a - b).abs()).sum();
        assert!(side > 10.0, "{side}");
        // defaults are a clean pass-through
        let mut l = tone(300.0, 1000);
        let mut r = tone(500.0, 1000);
        let (l0, r0) = (l.clone(), r.clone());
        stereo_shaper(&StereoShaperFx::default(), &mut l, &mut r);
        assert_eq!((l, r), (l0, r0));
        assert!(check_stereo_shaper(&StereoShaperFx { preset: "huge".into(), ..Default::default() }).is_err());
    }
}
