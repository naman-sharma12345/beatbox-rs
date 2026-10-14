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
    fn integer_delay_settings_match_the_existing_ring_delay_bit_for_bit() {
        // The engine's existing integer delay (fx.rs `Effect::Delay`): a ring
        // of exactly d slots, read slot i % d, then write the input there.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let x: Vec<f32> = (0..20_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                ((seed >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
            })
            .collect();
        for d in [1usize, 2, 7, 100, 1000, 4410] {
            let mut ring = vec![0.0f32; d];
            let mut dl = FracDelay::with_max(d + 8);
            for (i, &v) in x.iter().enumerate() {
                let k = i % d;
                let want = ring[k];
                ring[k] = v;
                dl.push(v);
                for interp in [Interp::Hermite, Interp::Linear] {
                    assert_eq!(dl.read(d as f64, interp).to_bits(), want.to_bits(), "d = {d}, {interp:?}, sample {i}");
                }
            }
        }
    }

    #[test]
    fn swept_delay_has_no_clicks_or_zipper_noise() {
        // A 1 kHz sine at -6 dBFS read through a delay that glides 10 -> 410
        // samples over 2 s (400 integer crossings), then wobbles fast
        // (+/- 40 samples at 5 Hz).
        let n = 2 * 44_100 + 44_100;
        let amp = 0.5f64;
        let w = 2.0 * PI * 1000.0 / FS;
        let delay = |k: usize| -> f64 {
            let t = k as f64 / FS;
            if t < 2.0 {
                10.0 + 200.0 * t
            } else {
                410.0 - 40.0 * (2.0 * PI * 5.0 * (t - 2.0)).sin()
            }
        };
        let run = |read: &dyn Fn(&FracDelay, f64) -> f32| -> (f64, f64, f64) {
            let mut dl = FracDelay::with_max(512);
            let (mut res_peak, mut d2_out, mut d2_ideal) = (0.0f64, 0.0f64, 0.0f64);
            let (mut y1, mut y2, mut i1, mut i2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
            for k in 0..n {
                dl.push((amp * (w * k as f64).sin()) as f32);
                let d = delay(k);
                let y = read(&dl, d) as f64;
                let ideal = amp * (w * (k as f64 - d)).sin();
                if k > 1024 {
                    res_peak = res_peak.max((y - ideal).abs());
                    d2_out = d2_out.max((y - 2.0 * y1 + y2).abs());
                    d2_ideal = d2_ideal.max((ideal - 2.0 * i1 + i2).abs());
                }
                (y2, y1, i2, i1) = (y1, y, i1, ideal);
            }
            (res_peak, d2_out, d2_ideal)
        };
        let (h_res, h_d2, ideal_d2) = run(&|dl, d| dl.read_hermite(d));
        let (l_res, l_d2, _) = run(&|dl, d| dl.read_linear(d));
        let (s_res, s_d2, _) = run(&|dl, d| dl.tap(d.round() as usize));
        let db = |v: f64| 20.0 * (v / amp).log10();
        eprintln!(
            "swept 1 kHz: residual peak hermite {:.1} dB, linear {:.1} dB, stepped integer {:.1} dB (re signal); \
             max 2nd difference hermite {h_d2:.5}, linear {l_d2:.5}, stepped {s_d2:.5}, ideal {ideal_d2:.5}",
            db(h_res),
            db(l_res),
            db(s_res)
        );
        // No click: the output never bends harder than the ideal glide does.
        assert!(h_d2 <= ideal_d2 * 1.01, "hermite 2nd difference {h_d2} vs ideal {ideal_d2}");
        // No zipper: the residual stays 60 dB under the signal, where
        // integer-stepped delay (which zippers) is far louder.
        assert!(db(h_res) < -60.0, "hermite residual {}", db(h_res));
        assert!(s_res > 30.0 * h_res, "stepped {s_res} should be far worse than hermite {h_res}");

        // Continuity across integer crossings on a frozen buffer: stepping
        // the delay by 1e-4 never jumps more at an integer than between them.
        let mut dl = FracDelay::with_max(64);
        for k in 0..64 {
            dl.push((amp * (w * k as f64).sin()) as f32);
        }
        let (mut jump_at_int, mut jump_between) = (0.0f64, 0.0f64);
        let mut prev = dl.read_hermite(5.0) as f64;
        for s in 1..=40_000 {
            let d = 5.0 + s as f64 * 1e-4;
            let y = dl.read_hermite(d) as f64;
            let j = (y - prev).abs();
            let near_int = (d - d.round()).abs() < 1.5e-4;
            if near_int {
                jump_at_int = jump_at_int.max(j);
            } else {
                jump_between = jump_between.max(j);
            }
            prev = y;
        }
        eprintln!("frozen buffer, delay 5..9 in 1e-4 steps: max step at integer crossings {jump_at_int:.2e}, elsewhere {jump_between:.2e}");
        assert!(jump_at_int <= jump_between * 1.5 + 1e-6, "discontinuity at an integer crossing");
    }

    /// Reference: the same delayed read done on the plain history array with
    /// `read_at` (forward-time stencil), independent of the ring indexing.
    fn reference(x: &[f32], k: usize, d: f64) -> f32 {
        read_at(x, k as f64 - d, Interp::Hermite)
    }

    #[test]
    fn boundary_cases_d1_max_delay_and_ring_wrap_have_no_fencepost_errors() {
        let x: Vec<f32> = (0..1000).map(|i| (0.37 * i as f64).sin() as f32 * 0.8 + 0.1).collect();
        let mut dl = FracDelay::with_max(60);
        let len = dl.buf.len(); // 64: wraps every 64 pushes
        let max = dl.max_delay();
        assert_eq!(len, 64);
        assert!(max >= 60.0 && max == (len - 3) as f64);
        let mut checked = 0usize;
        let mut wraps_seen = 0usize;
        for (k, &v) in x.iter().enumerate() {
            dl.push(v);
            if dl.pos == 0 {
                wraps_seen += 1;
            }
            if k < len + 4 {
                continue; // buffer not yet full of real history
            }
            // d = 1: the stencil [-1, 0, 1, 2] reaches tap(0), the newest sample.
            // max: the stencil's far end is tap(len - 1), the oldest slot.
            for d in [1.0, 1.001, 1.25, 1.5, 1.999, 2.0, max - 1.75, max - 1.0, max - 0.5, max - 0.001, max] {
                let got = dl.read_hermite(d);
                let want = reference(&x, k, d);
                assert!((got - want).abs() <= 2e-6, "k {k} (ring pos {}), d {d}: {got} vs {want}", dl.pos);
                checked += 1;
            }
            // Integer delays across the whole range are the stored sample,
            // including d = 1 and d = max at every ring position.
            for d in [1usize, 2, len / 2, len - 4, len - 3] {
                assert_eq!(dl.read_hermite(d as f64).to_bits(), x[k - d].to_bits(), "k {k}, d {d}");
                assert_eq!(dl.read_linear(d as f64).to_bits(), x[k - d].to_bits(), "k {k}, d {d}");
            }
            // Past the end clamps to max, never reads a wrapped (newer) slot.
            assert_eq!(dl.read_hermite(max + 10.0).to_bits(), dl.read_hermite(max).to_bits());
            assert_eq!(dl.read_hermite(0.25).to_bits(), dl.read_hermite(1.0).to_bits(), "below 1 clamps to 1");
        }
        assert!(wraps_seen >= 10, "ring wrapped {wraps_seen} times");
        eprintln!("boundary reads checked: {checked} (d = 1 .. max = {max}, {wraps_seen} ring wraps), all within 2e-6 of the array reference");
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
