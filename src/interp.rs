//! Fractional-sample interpolation shared by delay lines, the sampler and
//! granular voices: 4-point cubic Hermite (Catmull-Rom) and linear, plus a
//! fractional delay line built on them.
//!
//! The Hermite kernel is adapted from Signalsmith Audio's DSP library
//! (https://github.com/Signalsmith-Audio/dsp, `delay.h`: `InterpolatorCubic`),
//! Copyright (c) 2021 Geraint Luff / Signalsmith Audio Ltd., MIT License.
//! See THIRD_PARTY_NOTICES.md. Changes: ported to Rust, an integer position
//! returns the stored sample untouched (bit-exact), delay-line and
//! buffer-read wrappers, tests.
//!
//! Opt-in only: nothing in the engine uses this yet, so no existing render
//! changes. `read_linear` keeps today's `a + (b - a) * f` formula exactly, so
//! a caller can switch interpolators without touching its linear path.

/// Cubic Hermite (Catmull-Rom) through `p1` (t = 0) and `p2` (t = 1), with
/// neighbours `p0` before and `p3` after. Signalsmith `InterpolatorCubic`.
#[inline]
pub fn hermite4(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let cb = p2 - p1;
    let k1 = (p2 - p0) * 0.5;
    let k3 = k1 + (p3 - p1) * 0.5 - cb * 2.0;
    let k2 = cb - k3 - k1;
    p1 + t * (k1 + t * (k2 + t * k3))
}

/// Linear interpolation, the formula the engine uses today.
#[inline]
pub fn linear(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Which interpolator a reader uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Interp {
    /// Two-point linear (today's behaviour).
    #[default]
    Linear,
    /// Four-point cubic Hermite.
    Hermite,
}

/// Reads `src` at fractional position `pos` (samples), treating everything
/// outside the buffer as silence. Integer positions return the sample itself.
#[inline]
pub fn read_at(src: &[f32], pos: f64, interp: Interp) -> f32 {
    if !(pos >= 0.0) {
        return 0.0;
    }
    let i = pos as usize;
    let t = (pos - i as f64) as f32;
    let at = |k: isize| -> f32 {
        if k < 0 {
            0.0
        } else {
            src.get(k as usize).copied().unwrap_or(0.0)
        }
    };
    let ii = i as isize;
    if t == 0.0 {
        return at(ii);
    }
    match interp {
        Interp::Linear => linear(at(ii), at(ii + 1), t),
        Interp::Hermite => hermite4(at(ii - 1), at(ii), at(ii + 1), at(ii + 2), t),
    }
}

/// A power-of-two ring buffer delay line with fractional reads.
/// `tap(0)` is the most recently pushed sample.
#[derive(Clone, Debug)]
pub struct FracDelay {
    buf: Vec<f32>,
    mask: usize,
    pos: usize,
}

impl FracDelay {
    /// Room for delays up to `max_delay` samples (plus the Hermite neighbours).
    pub fn with_max(max_delay: usize) -> Self {
        let len = max_delay.saturating_add(4).clamp(4, 1 << 26).next_power_of_two();
        FracDelay { buf: vec![0.0; len], mask: len - 1, pos: 0 }
    }

    /// Largest delay a fractional read accepts.
    pub fn max_delay(&self) -> f64 {
        (self.buf.len() - 3) as f64
    }

    pub fn reset(&mut self) {
        self.buf.iter_mut().for_each(|v| *v = 0.0);
        self.pos = 0;
    }

    #[inline]
    pub fn push(&mut self, x: f32) {
        self.pos = (self.pos + 1) & self.mask;
        self.buf[self.pos] = x;
    }

    /// The sample pushed `d` pushes ago (`d = 0` is the latest).
    #[inline]
    pub fn tap(&self, d: usize) -> f32 {
        self.buf[self.pos.wrapping_sub(d) & self.mask]
    }

    #[inline]
    fn split(&self, delay: f64, min: f64) -> (usize, f32) {
        let d = if delay.is_finite() { delay.clamp(min, self.max_delay()) } else { min };
        let i = d as usize;
        (i, (d - i as f64) as f32)
    }

    /// Linear read at `delay` samples (clamped to 0..=max_delay).
    #[inline]
    pub fn read_linear(&self, delay: f64) -> f32 {
        let (i, f) = self.split(delay, 0.0);
        linear(self.tap(i), self.tap(i + 1), f)
    }

    /// Hermite read at `delay` samples (clamped to 1..=max_delay, since the
    /// cubic needs one newer neighbour). Integer delays are exact copies.
    #[inline]
    pub fn read_hermite(&self, delay: f64) -> f32 {
        let (i, f) = self.split(delay, 1.0);
        if f == 0.0 {
            return self.tap(i);
        }
        // Catmull-Rom is symmetric in time, so walking back from tap(i)
        // towards tap(i + 1) with fraction f is the same curve.
        hermite4(self.tap(i - 1), self.tap(i), self.tap(i + 1), self.tap(i + 2), f)
    }

    #[inline]
    pub fn read(&self, delay: f64, interp: Interp) -> f32 {
        match interp {
            Interp::Linear => self.read_linear(delay),
            Interp::Hermite => self.read_hermite(delay),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    const FS: f64 = 44_100.0;

    fn sine(freq: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * PI * freq * i as f64 / FS).sin() as f32).collect()
    }

    /// Phase (radians) and amplitude of `y` at `freq`, Hann-windowed projection.
    fn phasor(y: &[f64], freq: f64) -> (f64, f64) {
        let n = y.len() as f64;
        let w = 2.0 * PI * freq / FS;
        let (mut re, mut im, mut wsum) = (0.0, 0.0, 0.0);
        for (k, v) in y.iter().enumerate() {
            let win = 0.5 - 0.5 * (2.0 * PI * k as f64 / n).cos();
            re += v * win * (w * k as f64).cos();
            im -= v * win * (w * k as f64).sin();
            wsum += win;
        }
        (im.atan2(re), 2.0 * (re * re + im * im).sqrt() / wsum)
    }

    #[test]
    fn constant_fractional_delay_of_1k_is_accurate_to_a_hundredth_of_a_sample() {
        let x = sine(1000.0, 16384);
        let w = 2.0 * PI * 1000.0 / FS;
        let (mut worst_d, mut worst_db) = (0.0f64, 0.0f64);
        for step in 0..=40 {
            let delay = 10.0 + step as f64 * 0.025; // 10.000 .. 11.000
            let mut dl = FracDelay::with_max(64);
            let mut out = Vec::with_capacity(x.len());
            let mut ref_ = Vec::with_capacity(x.len());
            for &s in &x {
                dl.push(s);
                out.push(dl.read_hermite(delay) as f64);
                ref_.push(dl.tap(0) as f64);
            }
            let (po, ao) = phasor(&out[2048..], 1000.0);
            let (pr, ar) = phasor(&ref_[2048..], 1000.0);
            let mut dphi = pr - po;
            while dphi < 0.0 {
                dphi += 2.0 * PI;
            }
            let measured = dphi / w; // < 1 period, and delay*w < 2*pi here
            worst_d = worst_d.max((measured - delay).abs());
            worst_db = worst_db.max((20.0 * (ao / ar).log10()).abs());
        }
        eprintln!("hermite constant delay at 1 kHz: worst delay error {worst_d:.5} samples, worst magnitude error {worst_db:.5} dB");
        assert!(worst_d < 0.01, "delay error {worst_d}");
        assert!(worst_db < 0.05, "magnitude error {worst_db}");
    }

    /// Error power (dB re signal) of a chorus-style swept read of a sine,
    /// against the ideal continuously delayed sine.
    fn sweep_noise_db(freq: f64, interp: Interp) -> f64 {
        let n = 1 << 16;
        let x = sine(freq, n);
        let mut dl = FracDelay::with_max(256);
        let (mut e, mut s) = (0.0f64, 0.0f64);
        for (k, &v) in x.iter().enumerate() {
            dl.push(v);
            // 20 +/- 10 samples (0.45 +/- 0.23 ms) at 0.7 Hz, chorus-like.
            let d = 20.0 + 10.0 * (2.0 * PI * 0.7 * k as f64 / FS).sin();
            let y = dl.read(d, interp) as f64;
            if k >= 1024 {
                let ideal = (2.0 * PI * freq * (k as f64 - d) / FS).sin();
                e += (y - ideal) * (y - ideal);
                s += ideal * ideal;
            }
        }
        10.0 * (e / s).log10()
    }

    #[test]
    fn swept_delay_modulation_noise_is_20_db_below_linear_at_1k() {
        let mut lines = Vec::new();
        for f in [1000.0, 5000.0, 10000.0] {
            let lin = sweep_noise_db(f, Interp::Linear);
            let her = sweep_noise_db(f, Interp::Hermite);
            lines.push((f, lin, her));
            eprintln!("swept-delay noise at {f:.0} Hz: linear {lin:.1} dB, hermite {her:.1} dB, improvement {:.1} dB", lin - her);
        }
        let (_, lin1k, her1k) = lines[0];
        assert!(lin1k - her1k >= 20.0, "1 kHz improvement {}", lin1k - her1k);
        // Above that the 4-point cubic still wins, by less (measured, not a spec).
        for (f, lin, her) in &lines[1..] {
            assert!(lin - her >= 5.0, "{f} Hz: linear {lin} hermite {her}");
        }
    }

    #[test]
    fn integer_delays_are_bit_exact_copies() {
        let x = sine(1234.5, 4000);
        let mut dl = FracDelay::with_max(64);
        for (k, &v) in x.iter().enumerate() {
            dl.push(v);
            for d in 1..=40usize {
                let want = if k >= d { x[k - d] } else { 0.0 };
                assert_eq!(dl.read_hermite(d as f64).to_bits(), want.to_bits());
                assert_eq!(dl.read_linear(d as f64).to_bits(), want.to_bits());
            }
        }
        for p in [0usize, 1, 17, 3999] {
            assert_eq!(read_at(&x, p as f64, Interp::Hermite).to_bits(), x[p].to_bits());
        }
    }

    #[test]
    fn linear_reads_match_the_engines_existing_formula_bit_for_bit() {
        // The same `a + (b - a) * f` the current delay/sampler code uses.
        let x = sine(777.0, 3000);
        let mut dl = FracDelay::with_max(64);
        for (k, &v) in x.iter().enumerate() {
            dl.push(v);
            if k < 40 {
                continue;
            }
            for d in [3.25f64, 7.5, 12.875, 30.1] {
                let i = d as usize;
                let f = (d - i as f64) as f32;
                let (a, b) = (x[k - i], x[k - i - 1]);
                assert_eq!(dl.read_linear(d).to_bits(), (a + (b - a) * f).to_bits());
            }
            let pos = k as f64 - 9.375;
            let i = pos as usize;
            let f = (pos - i as f64) as f32;
            assert_eq!(read_at(&x, pos, Interp::Linear).to_bits(), (x[i] + (x[i + 1] - x[i]) * f).to_bits());
        }
    }

    #[test]
    fn hermite_reproduces_quadratics_and_buffer_edges_read_as_silence() {
        // Catmull-Rom is exact for quadratics; check a quadratic ramp.
        let q: Vec<f32> = (0..32).map(|i| 0.01 * (i as f32) * (i as f32) - 0.2 * i as f32).collect();
        for k in 1..28 {
            for t in [0.1f32, 0.5, 0.9] {
                let p = k as f32 + t;
                let want = 0.01 * p * p - 0.2 * p;
                let got = read_at(&q, (k as f64) + t as f64, Interp::Hermite);
                assert!((got - want).abs() < 1e-5, "{k}+{t}: {got} vs {want}");
            }
        }
        assert_eq!(read_at(&q, -0.5, Interp::Hermite), 0.0);
        assert_eq!(read_at(&q, 40.0, Interp::Hermite), 0.0);
        assert_eq!(read_at(&q, f64::NAN, Interp::Linear), 0.0);
        let dl = FracDelay::with_max(8);
        assert!(dl.read_hermite(f64::INFINITY).abs() == 0.0 && dl.read_hermite(-3.0) == 0.0);
    }
}
