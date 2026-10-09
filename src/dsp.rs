//! Low-level DSP building blocks: RNG, envelopes, oscillators, filters.

use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

/// Engine sample rate. Everything renders at 44.1 kHz.
pub const SR: f32 = 44_100.0;

pub fn midi_to_hz(m: f32) -> f32 {
    440.0 * 2f32.powf((m - 69.0) / 12.0)
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn gain_to_db(g: f32) -> f32 {
    if g <= 1e-9 {
        -180.0
    } else {
        20.0 * g.log10()
    }
}

/// Small, fast, deterministic xorshift RNG so renders are reproducible.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407)
            | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Uniform in [0, 1).
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.f32()
    }
    /// Uniform in [-1, 1).
    pub fn bipolar(&mut self) -> f32 {
        self.f32() * 2.0 - 1.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
    pub fn chance(&mut self, p: f32) -> bool {
        self.f32() < p
    }
    /// Pick an index with the given weights.
    pub fn weighted(&mut self, weights: &[f32]) -> usize {
        let total: f32 = weights.iter().sum();
        if total <= 0.0 {
            return 0;
        }
        let mut r = self.f32() * total;
        for (i, w) in weights.iter().enumerate() {
            if r < *w {
                return i;
            }
            r -= w;
        }
        weights.len() - 1
    }
}

/// Attack / decay / sustain / release envelope. Times in seconds, sustain 0..1.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Adsr {
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
}

impl Adsr {
    pub const fn new(attack: f32, decay: f32, sustain: f32, release: f32) -> Self {
        Adsr {
            attack,
            decay,
            sustain,
            release,
        }
    }

    fn ads(&self, t: f32) -> f32 {
        let a = self.attack.max(0.0005);
        let d = self.decay.max(0.0005);
        if t < a {
            t / a
        } else if t < a + d {
            let x = (t - a) / d;
            // exponential-ish decay curve
            self.sustain + (1.0 - self.sustain) * (1.0 - x).powf(2.0)
        } else {
            self.sustain
        }
    }

    /// Envelope level at time `t` for a note held for `gate` seconds.
    pub fn level(&self, t: f32, gate: f32) -> f32 {
        if t < gate {
            self.ads(t)
        } else {
            let l = self.ads(gate);
            let rt = t - gate;
            let r = self.release.max(0.001);
            if rt >= r {
                0.0
            } else {
                l * (1.0 - rt / r).powf(2.0)
            }
        }
    }

    pub fn total(&self, gate: f32) -> f32 {
        gate + self.release.max(0.001)
    }
}

impl Default for Adsr {
    fn default() -> Self {
        Adsr::new(0.005, 0.2, 0.7, 0.2)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Wave {
    Sine,
    #[default]
    Saw,
    Square,
    Triangle,
    Noise,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    #[default]
    Lowpass,
    Highpass,
    Bandpass,
    Notch,
}

fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

/// Band-limited oscillator sample. `phase` in [0,1), `dt` = freq / SR.
pub fn osc(wave: Wave, phase: f32, dt: f32, rng: &mut Rng) -> f32 {
    match wave {
        Wave::Sine => (2.0 * PI * phase).sin(),
        Wave::Saw => 2.0 * phase - 1.0 - poly_blep(phase, dt),
        Wave::Square => {
            let v = if phase < 0.5 { 1.0 } else { -1.0 };
            v + poly_blep(phase, dt) - poly_blep((phase + 0.5) % 1.0, dt)
        }
        Wave::Triangle => 4.0 * (phase - 0.5).abs() - 1.0,
        Wave::Noise => rng.bipolar(),
    }
}

/// Topology-preserving state variable filter (Zavalishin / Cytomic).
#[derive(Clone, Copy, Debug, Default)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
}

impl Svf {
    /// `res` 0..1 maps to Q 0.5..10.
    pub fn process(&mut self, x: f32, cutoff: f32, res: f32, mode: FilterMode) -> f32 {
        let fc = cutoff.clamp(20.0, SR * 0.45);
        let g = (PI * fc / SR).tan();
        let q = 0.5 + res.clamp(0.0, 1.0) * 9.5;
        let k = 1.0 / q;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let v3 = x - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        if !self.ic1.is_finite() || !self.ic2.is_finite() {
            self.ic1 = 0.0;
            self.ic2 = 0.0;
        }
        match mode {
            FilterMode::Lowpass => v2,
            FilterMode::Bandpass => v1,
            FilterMode::Highpass => x - k * v1 - v2,
            FilterMode::Notch => x - k * v1,
        }
    }
}

/// One-pole DC blocker.
#[derive(Clone, Copy, Debug, Default)]
pub struct DcBlock {
    x1: f32,
    y1: f32,
}

impl DcBlock {
    pub fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + 0.995 * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}

/// Linear-interpolated read from a buffer at a fractional index.
pub fn lerp_read(buf: &[f32], pos: f32) -> f32 {
    if pos < 0.0 {
        return 0.0;
    }
    let i = pos as usize;
    if i + 1 >= buf.len() {
        return buf.get(i).copied().unwrap_or(0.0);
    }
    let f = pos - i as f32;
    buf[i] * (1.0 - f) + buf[i + 1] * f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi_a4_is_440() {
        assert!((midi_to_hz(69.0) - 440.0).abs() < 1e-3);
        assert!((midi_to_hz(81.0) - 880.0).abs() < 1e-2);
    }

    #[test]
    fn adsr_shape() {
        let e = Adsr::new(0.1, 0.1, 0.5, 0.2);
        assert!(e.level(0.05, 1.0) > 0.4 && e.level(0.05, 1.0) < 0.6);
        assert!((e.level(0.5, 1.0) - 0.5).abs() < 1e-4);
        assert_eq!(e.level(1.3, 1.0), 0.0);
    }

    #[test]
    fn rng_is_deterministic() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let v = a.f32();
        assert!((0.0..1.0).contains(&v));
    }
}
