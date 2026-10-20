//! Linkwitz-Riley crossovers (LR4, 24 dB/oct), clean-room from the
//! textbook construction (Linkwitz & Riley; Zölzer, DAFX): each LR4 lowpass
//! and highpass is a cascade of two identical 2nd-order Butterworth
//! sections (Q = 1/sqrt 2, bilinear transform prewarped at the crossover).
//! LP4 + HP4 is then the 2nd-order allpass with the same pole pair, and
//! LP4 and HP4 are in phase at every frequency.
//!
//! N bands from N - 1 ascending crossover frequencies, as a tree: band i is
//! LP4(f_i) after the highpasses of every lower crossover, and it then runs
//! through the allpass of every higher crossover f_j (j > i), so all bands
//! carry the same phase and their sum is
//! AP(f_0) AP(f_1) ... AP(f_{N-2}) x: unity magnitude at every frequency.
//!
//! Real-time contract: fixed-size state, f64 arithmetic, no allocation and
//! no latency (IIR, zero lookahead).

use std::f64::consts::{PI, SQRT_2};

/// Most crossover points (bands = crossovers + 1).
pub const MAX_CROSSOVERS: usize = 4;
pub const MAX_BANDS: usize = MAX_CROSSOVERS + 1;

/// A transposed direct form II biquad in f64.
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn from(b: [f64; 3], a: [f64; 3]) -> Self {
        Biquad {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            a1: a[1] / a[0],
            a2: a[2] / a[0],
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn k(f: f64, sr: f64) -> f64 {
        (PI * f / sr).tan()
    }

    /// 2nd-order Butterworth lowpass at `f`.
    pub fn butter_lp(f: f64, sr: f64) -> Self {
        let k = Self::k(f, sr);
        let k2 = k * k;
        Self::from(
            [k2, 2.0 * k2, k2],
            [
                1.0 + SQRT_2 * k + k2,
                2.0 * (k2 - 1.0),
                1.0 - SQRT_2 * k + k2,
            ],
        )
    }

    /// 2nd-order Butterworth highpass at `f`.
    pub fn butter_hp(f: f64, sr: f64) -> Self {
        let k = Self::k(f, sr);
        let k2 = k * k;
        Self::from(
            [1.0, -2.0, 1.0],
            [
                1.0 + SQRT_2 * k + k2,
                2.0 * (k2 - 1.0),
                1.0 - SQRT_2 * k + k2,
            ],
        )
    }

    /// 2nd-order allpass with the Butterworth pole pair at `f` (= LP4 + HP4).
    pub fn butter_ap(f: f64, sr: f64) -> Self {
        let k = Self::k(f, sr);
        let k2 = k * k;
        let (d0, d1, d2) = (
            1.0 + SQRT_2 * k + k2,
            2.0 * (k2 - 1.0),
            1.0 - SQRT_2 * k + k2,
        );
        Self::from([d2, d1, d0], [d0, d1, d2])
    }

    #[inline]
    pub fn run(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// One LR4 split: lowpass and highpass, each two Butterworth sections.
#[derive(Clone, Copy, Debug, Default)]
struct Lr4 {
    lp: [Biquad; 2],
    hp: [Biquad; 2],
}

impl Lr4 {
    fn new(f: f64, sr: f64) -> Self {
        let (l, h) = (Biquad::butter_lp(f, sr), Biquad::butter_hp(f, sr));
        Lr4 {
            lp: [l, l],
            hp: [h, h],
        }
    }
    #[inline]
    fn split(&mut self, x: f64) -> (f64, f64) {
        let l0 = self.lp[0].run(x);
        let lo = self.lp[1].run(l0);
        let h0 = self.hp[0].run(x);
        let hi = self.hp[1].run(h0);
        (lo, hi)
    }
}

/// One channel of an N-band LR4 crossover tree with allpass phase
/// compensation (see the module docs). `Copy`, fixed size.
#[derive(Clone, Copy, Debug)]
pub struct Crossover {
    n_x: usize,
    split: [Lr4; MAX_CROSSOVERS],
    // comp[i][j]: the allpass of crossover j applied to band i (j > i)
    comp: [[Biquad; MAX_CROSSOVERS]; MAX_CROSSOVERS],
}

impl Crossover {
    /// `freqs`: ascending crossover frequencies, 1..=MAX_CROSSOVERS of them
    /// (extra ones are ignored; frequencies are clamped to 10 Hz .. 0.45 sr
    /// and forced ascending).
    pub fn new(freqs: &[f32], sr: f32) -> Self {
        let sr = sr as f64;
        let n_x = freqs.len().clamp(1, MAX_CROSSOVERS);
        let mut f = [0.0f64; MAX_CROSSOVERS];
        let mut prev = 0.0f64;
        for i in 0..n_x {
            let v = (freqs.get(i).copied().unwrap_or(1000.0) as f64)
                .clamp(10.0, 0.45 * sr)
                .max(prev * 1.0001);
            f[i] = v;
            prev = v;
        }
        let mut split = [Lr4::default(); MAX_CROSSOVERS];
        let mut comp = [[Biquad::default(); MAX_CROSSOVERS]; MAX_CROSSOVERS];
        for i in 0..n_x {
            split[i] = Lr4::new(f[i], sr);
            for j in i + 1..n_x {
                comp[i][j] = Biquad::butter_ap(f[j], sr);
            }
        }
        Crossover { n_x, split, comp }
    }

    pub fn bands(&self) -> usize {
        self.n_x + 1
    }

    pub fn reset(&mut self) {
        for s in self.split.iter_mut() {
            for b in s.lp.iter_mut().chain(s.hp.iter_mut()) {
                b.reset();
            }
        }
        for row in self.comp.iter_mut() {
            for b in row.iter_mut() {
                b.reset();
            }
        }
    }

    /// Split one sample into `bands()` bands (low to high) in `out`.
    #[inline]
    pub fn process(&mut self, x: f64, out: &mut [f64; MAX_BANDS]) {
        let mut rest = x;
        for i in 0..self.n_x {
            let (lo, hi) = self.split[i].split(rest);
            let mut b = lo;
            for j in i + 1..self.n_x {
                b = self.comp[i][j].run(b);
            }
            out[i] = b;
            rest = hi;
        }
        out[self.n_x] = rest;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Impulse responses of every band (`len` samples each).
    pub fn band_irs(freqs: &[f32], sr: f32, len: usize) -> Vec<Vec<f64>> {
        let mut x = Crossover::new(freqs, sr);
        let nb = x.bands();
        let mut irs = vec![vec![0.0f64; len]; nb];
        let mut o = [0.0f64; MAX_BANDS];
        for n in 0..len {
            x.process(if n == 0 { 1.0 } else { 0.0 }, &mut o);
            for b in 0..nb {
                irs[b][n] = o[b];
            }
        }
        irs
    }

    /// Complex DFT of `h` at frequency `f` (direct sum, f64).
    pub fn dft(h: &[f64], f: f64, sr: f64) -> (f64, f64) {
        let w = -2.0 * PI * f / sr;
        let (mut re, mut im) = (0.0, 0.0);
        for (n, v) in h.iter().enumerate() {
            re += v * (w * n as f64).cos();
            im += v * (w * n as f64).sin();
        }
        (re, im)
    }

    fn log_freqs(n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| 20.0 * (20000.0f64 / 20.0).powf(i as f64 / (n - 1) as f64))
            .collect()
    }

    #[test]
    fn lr4_bands_sum_to_a_flat_allpass() {
        let mut lines = Vec::new();
        for (freqs, sr) in [
            (vec![150.0f32, 2500.0], 44_100.0f32),
            (vec![150.0, 2500.0], 48_000.0),
            (vec![80.0, 400.0, 3000.0, 9000.0], 44_100.0),
            (vec![1000.0], 96_000.0),
        ] {
            let irs = band_irs(&freqs, sr, 1 << 16);
            let sum: Vec<f64> = (0..irs[0].len())
                .map(|n| irs.iter().map(|b| b[n]).sum())
                .collect();
            let mut dev = 0.0f64;
            for f in log_freqs(400) {
                let (re, im) = dft(&sum, f, sr as f64);
                dev = dev.max((10.0 * (re * re + im * im).log10()).abs());
            }
            lines.push(format!("{freqs:?} @ {sr}: max |sum| deviation {dev:.2e} dB over 20 Hz..20 kHz (400 log points)"));
            assert!(dev < 0.01, "{freqs:?} {sr}: {dev} dB");
        }
        eprintln!("LR4_FLATNESS\n{}", lines.join("\n"));
    }

    #[test]
    fn lr4_slopes_are_24_db_per_octave_and_minus_6_at_the_crossover() {
        let sr = 48_000.0f32;
        let irs = band_irs(&[1000.0], sr, 1 << 15);
        let mag = |h: &[f64], f: f64| {
            let (re, im) = dft(h, f, sr as f64);
            10.0 * (re * re + im * im).log10()
        };
        assert!(
            (mag(&irs[0], 1000.0) + 6.02).abs() < 0.05,
            "{}",
            mag(&irs[0], 1000.0)
        );
        assert!(
            (mag(&irs[1], 1000.0) + 6.02).abs() < 0.05,
            "{}",
            mag(&irs[1], 1000.0)
        );
        // two octaves above / below: ~ -48 dB (24 dB/oct asymptote, bilinear-warped a hair)
        let lo4 = mag(&irs[0], 4000.0);
        let hi4 = mag(&irs[1], 250.0);
        assert!(lo4 < -46.0 && lo4 > -50.0, "{lo4}");
        assert!(hi4 < -46.0 && hi4 > -50.0, "{hi4}");
    }

    #[test]
    fn adjacent_bands_are_phase_aligned() {
        let sr = 44_100.0f32;
        let freqs = [150.0f32, 2500.0];
        let irs = band_irs(&freqs, sr, 1 << 16);
        let mut worst = 0.0f64;
        for b in 0..irs.len() - 1 {
            for f in log_freqs(300) {
                let (ar, ai) = dft(&irs[b], f, sr as f64);
                let (br, bi) = dft(&irs[b + 1], f, sr as f64);
                let (ma, mb) = ((ar * ar + ai * ai).sqrt(), (br * br + bi * bi).sqrt());
                // only where both bands carry real energy (above -30 dB)
                if ma < 0.0316 || mb < 0.0316 {
                    continue;
                }
                // phase of a * conj(b)
                let d = (ai * br - ar * bi).atan2(ar * br + ai * bi).to_degrees();
                worst = worst.max(d.abs());
            }
        }
        eprintln!("LR4_PHASE worst adjacent-band phase difference {worst:.2e} deg");
        assert!(worst < 0.1, "{worst} deg");
    }
}
