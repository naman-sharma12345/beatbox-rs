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

/// Two-sample PolyBLEP saw (the pre-mipmap oscillator), kept for reference
/// and for the alias test that proves the mipmapped tables beat it.
pub fn saw_polyblep(phase: f32, dt: f32) -> f32 {
    2.0 * phase - 1.0 - poly_blep(phase, dt)
}

const MIP_SIZE: usize = 2048;
/// Bands per octave of the mipmapped oscillator tables (from 20 Hz).
const MIP_PER_OCT: f32 = 3.0;
const MIP_BANDS: usize = 32;
/// Highest partial a table may hold: inaudible above this, and it keeps a
/// margin under Nyquist for the top note of every band.
const MIP_TOP_HZ: f32 = 19_000.0;

/// Band-limited single-cycle tables for saw, square and triangle, one per
/// third of an octave: each holds only the harmonics that stay below
/// `MIP_TOP_HZ` for the highest pitch of its band, so nothing folds back.
fn mip_tables() -> &'static [Vec<f32>; 3] {
    static T: std::sync::OnceLock<[Vec<f32>; 3]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut out: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        let stride = MIP_SIZE + 1; // +1 guard sample for interpolation
        for t in out.iter_mut() {
            *t = vec![0.0; stride * MIP_BANDS];
        }
        for b in 0..MIP_BANDS {
            let f_hi = 20.0 * 2f32.powf((b as f32 + 1.0) / MIP_PER_OCT);
            let nh = ((MIP_TOP_HZ / f_hi) as usize).clamp(1, MIP_SIZE / 2 - 1);
            for i in 0..=MIP_SIZE {
                let x = i as f64 / MIP_SIZE as f64;
                let (mut saw, mut sq, mut tri) = (0.0f64, 0.0f64, 0.0f64);
                for h in 1..=nh {
                    let hf = h as f64;
                    let w = 2.0 * std::f64::consts::PI * hf * x;
                    saw -= w.sin() / hf;
                    if h % 2 == 1 {
                        sq += w.sin() / hf;
                        tri += w.cos() / (hf * hf);
                    }
                }
                let at = b * stride + i;
                out[0][at] = (saw * 2.0 / std::f64::consts::PI) as f32;
                out[1][at] = (sq * 4.0 / std::f64::consts::PI) as f32;
                out[2][at] = (tri * 8.0 / (std::f64::consts::PI * std::f64::consts::PI)) as f32;
            }
        }
        out
    })
}

fn mip_read(which: usize, phase: f32, dt: f32) -> f32 {
    let f = dt.abs() * SR;
    let b = if f <= 20.0 {
        0
    } else {
        ((MIP_PER_OCT * (f / 20.0).log2()) as usize).min(MIP_BANDS - 1)
    };
    let t = &mip_tables()[which];
    let p = phase.rem_euclid(1.0) * MIP_SIZE as f32;
    let i = (p as usize).min(MIP_SIZE - 1);
    let fr = p - i as f32;
    let base = b * (MIP_SIZE + 1) + i;
    t[base] + (t[base + 1] - t[base]) * fr
}

/// Band-limited oscillator sample. `phase` in [0,1), `dt` = freq / SR.
/// Saw, square and triangle read mipmapped band-limited tables (alias
/// energy around -60 dB even at the top of the keyboard, where the old
/// two-sample PolyBLEP sat near -27 dB).
pub fn osc(wave: Wave, phase: f32, dt: f32, rng: &mut Rng) -> f32 {
    match wave {
        Wave::Sine => (2.0 * PI * phase).sin(),
        Wave::Saw => mip_read(0, phase, dt),
        Wave::Square => mip_read(1, phase, dt),
        Wave::Triangle => mip_read(2, phase, dt),
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

/// RBJ-cookbook biquad (transposed direct form II).
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiquadKind {
    Bell,
    LowShelf,
    HighShelf,
    LowCut,
    HighCut,
    Notch,
    Bandpass,
}

impl Biquad {
    pub fn new(kind: BiquadKind, freq: f32, q: f32, gain_db: f32) -> Self {
        let f = freq.clamp(10.0, SR * 0.49);
        let w = 2.0 * PI * f / SR;
        let (sw, cw) = (w.sin(), w.cos());
        let q = q.clamp(0.1, 30.0);
        let alpha = sw / (2.0 * q);
        let a = 10f32.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BiquadKind::Bell => (
                1.0 + alpha * a,
                -2.0 * cw,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cw,
                1.0 - alpha / a,
            ),
            BiquadKind::LowShelf | BiquadKind::HighShelf => {
                let sa = 2.0 * a.sqrt() * alpha;
                if kind == BiquadKind::LowShelf {
                    (
                        a * ((a + 1.0) - (a - 1.0) * cw + sa),
                        2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
                        a * ((a + 1.0) - (a - 1.0) * cw - sa),
                        (a + 1.0) + (a - 1.0) * cw + sa,
                        -2.0 * ((a - 1.0) + (a + 1.0) * cw),
                        (a + 1.0) + (a - 1.0) * cw - sa,
                    )
                } else {
                    (
                        a * ((a + 1.0) + (a - 1.0) * cw + sa),
                        -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
                        a * ((a + 1.0) + (a - 1.0) * cw - sa),
                        (a + 1.0) - (a - 1.0) * cw + sa,
                        2.0 * ((a - 1.0) - (a + 1.0) * cw),
                        (a + 1.0) - (a - 1.0) * cw - sa,
                    )
                }
            }
            BiquadKind::LowCut => (
                (1.0 + cw) / 2.0,
                -(1.0 + cw),
                (1.0 + cw) / 2.0,
                1.0 + alpha,
                -2.0 * cw,
                1.0 - alpha,
            ),
            BiquadKind::HighCut => (
                (1.0 - cw) / 2.0,
                1.0 - cw,
                (1.0 - cw) / 2.0,
                1.0 + alpha,
                -2.0 * cw,
                1.0 - alpha,
            ),
            BiquadKind::Notch => (1.0, -2.0 * cw, 1.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            // constant 0 dB peak gain band-pass
            BiquadKind::Bandpass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
        };
        Biquad {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        if !self.z1.is_finite() || !self.z2.is_finite() {
            self.z1 = 0.0;
            self.z2 = 0.0;
        }
        y
    }

    /// Magnitude response in dB at `freq`.
    pub fn response_db(&self, freq: f32) -> f32 {
        let w = 2.0 * PI * freq / SR;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di).max(1e-20)).log10()
    }
}

/// Radix-2 complex FFT with precomputed twiddles and bit-reversal, for
/// repeated transforms of one size (convolution, wavetables, analysis).
pub struct Fft {
    n: usize,
    rev: Vec<usize>,
    cos: Vec<f32>,
    sin: Vec<f32>,
}

impl Fft {
    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 2);
        let bits = n.trailing_zeros();
        let rev = (0..n)
            .map(|i| i.reverse_bits() >> (usize::BITS - bits))
            .collect();
        let cos = (0..n / 2)
            .map(|k| (2.0 * std::f64::consts::PI * k as f64 / n as f64).cos() as f32)
            .collect();
        let sin = (0..n / 2)
            .map(|k| -(2.0 * std::f64::consts::PI * k as f64 / n as f64).sin() as f32)
            .collect();
        Fft { n, rev, cos, sin }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Forward transform in place (inverse = conjugate trick, see `inverse`).
    pub fn forward(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i];
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let (wr, wi) = (self.cos[k * step], self.sin[k * step]);
                    let a = start + k;
                    let b = a + half;
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }

    /// Inverse transform in place, scaled by 1/n.
    pub fn inverse(&self, re: &mut [f32], im: &mut [f32]) {
        for v in im.iter_mut() {
            *v = -*v;
        }
        self.forward(re, im);
        let s = 1.0 / self.n as f32;
        for (r, i) in re.iter_mut().zip(im.iter_mut()) {
            *r *= s;
            *i = -*i * s;
        }
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
    fn fft_roundtrip_and_biquad_response() {
        let f = Fft::new(64);
        let orig: Vec<f32> = (0..64).map(|i| (i as f32 * 0.3).sin()).collect();
        let mut re = orig.clone();
        let mut im = vec![0.0; 64];
        f.forward(&mut re, &mut im);
        f.inverse(&mut re, &mut im);
        for (a, b) in re.iter().zip(orig.iter()) {
            assert!((a - b).abs() < 1e-4);
        }
        let b = Biquad::new(BiquadKind::Bell, 1000.0, 1.0, 6.0);
        assert!((b.response_db(1000.0) - 6.0).abs() < 0.1);
        assert!(b.response_db(50.0).abs() < 0.5);
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
