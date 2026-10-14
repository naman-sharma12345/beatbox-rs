//! Half-band polyphase FIR oversampling (2x, and 4x as two cascaded 2x
//! stages), for running nonlinear processing at a higher rate.
//!
//! Adapted from Signalsmith Audio's DSP library
//! (https://github.com/Signalsmith-Audio/dsp, `rates.h`: `Oversampler2xFIR`,
//! `fillKaiserSinc`; `windows.h`: `Kaiser::withBandwidth`, `Kaiser::fill`),
//! Copyright (c) 2021 Geraint Luff / Signalsmith Audio Ltd., MIT License.
//! Changes: ported to Rust, one mono channel per instance with streaming
//! history instead of a fixed max block, a cascade for 4x, a second-stage
//! kernel designed for the narrower band it sees, `bessel_i0` summed to a
//! relative 1e-12 instead of an absolute 1e-4, and tests. See
//! THIRD_PARTY_NOTICES.md.
//!
//! How it works: the 2x upsampler copies each input sample to the even
//! output slot (delayed by `half_latency` input samples) and fills the odd
//! slot with a half-sample interpolation, a Kaiser-windowed sinc of
//! `2 * half_latency` taps. That is a half-band filter in polyphase form: the
//! even phase is a single unit tap, so only half the taps are ever computed.
//! The downsampler is the transpose: the average of the delayed even sample
//! and the same half-sample filter run over the odd samples.
//!
//! Opt-in only: nothing in the engine uses this yet, so no existing render
//! changes. Latency is an integer number of base-rate (1x) samples, reported
//! by [`Oversampler::latency_samples`] (round trip, up then down) for host
//! delay compensation.

use std::f64::consts::PI;

/// Default first-stage design: 24 half-latency (48-tap half-sample kernel),
/// passband edge 0.43 x base rate (18.96 kHz at 44.1 kHz), stop edge 0.57.
pub const STAGE1_HALF_LATENCY: usize = 24;
pub const STAGE1_PASS: f64 = 0.43;
/// Second stage of the 4x cascade runs at 2x and only needs to keep what
/// stage 1 passes, so its pass edge sits at stage 1's stop edge
/// ((1 - 0.43) / 2 of its own input rate). Folded content then lands inside
/// stage 1's stop band.
pub const STAGE2_HALF_LATENCY: usize = 8;
pub const STAGE2_PASS: f64 = (1.0 - STAGE1_PASS) * 0.5;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..200 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-12 {
            break;
        }
    }
    sum
}

/// Signalsmith `Kaiser::bandwidthToBeta` (non-heuristic).
fn kaiser_beta_for_bandwidth(bandwidth: f64) -> f64 {
    let bw = bandwidth.max(2.0);
    (bw * bw * 0.25 - 1.0).sqrt() * PI
}

/// Signalsmith `fillKaiserSinc(data, length, passFreq, stopFreq)`: a
/// Kaiser-windowed sinc lowpass of `length` taps, frequencies relative to the
/// sample rate the taps are spaced at.
pub fn kaiser_sinc(length: usize, pass_freq: f64, stop_freq: f64) -> Vec<f64> {
    if length == 0 {
        return Vec::new();
    }
    let n = length as f64;
    let mut bandwidth = (stop_freq - pass_freq) * n;
    bandwidth += 1.25 / bandwidth;
    let beta = kaiser_beta_for_bandwidth(bandwidth);
    let inv_b0 = 1.0 / bessel_i0(beta);
    let centre = (n - 1.0) * 0.5;
    let sinc_scale = PI * (pass_freq + stop_freq);
    let amp_scale = pass_freq + stop_freq;
    (0..length)
        .map(|i| {
            let r = (2.0 * i as f64 + 1.0) / n - 1.0;
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) * inv_b0;
            let px = (i as f64 - centre) * sinc_scale;
            let sinc = if px.abs() > 1e-6 { px.sin() * amp_scale / px } else { amp_scale };
            w * sinc
        })
        .collect()
}

/// One mono 2x half-band stage (Signalsmith `Oversampler2xFIR`).
#[derive(Clone, Debug)]
pub struct HalfBand2x {
    half_latency: usize,
    kernel: Vec<f32>,
    up_hist: Vec<f32>,
    down_hist: Vec<f32>,
    scratch: Vec<f32>,
}

impl HalfBand2x {
    /// `half_latency` >= 1; `pass_freq` in (0, 0.5) relative to the lower rate
    /// (the stop edge is mirrored at `1 - pass_freq`).
    pub fn new(half_latency: usize, pass_freq: f64) -> Self {
        assert!(half_latency >= 1, "half_latency must be at least 1");
        assert!(pass_freq > 0.0 && pass_freq < 0.5, "pass_freq must be in (0, 0.5)");
        let k = 2 * half_latency;
        let mut kernel: Vec<f32> = kaiser_sinc(k, pass_freq, 1.0 - pass_freq).into_iter().map(|v| v as f32).collect();
        // The design is symmetric in exact arithmetic; mirror the first half so
        // the taps are bit-for-bit symmetric (strictly linear phase) whatever
        // the last-bit rounding of the window and sinc evaluation.
        for i in 0..k / 2 {
            kernel[k - 1 - i] = kernel[i];
        }
        HalfBand2x { half_latency, kernel, up_hist: vec![0.0; k], down_hist: vec![0.0; 2 * k], scratch: Vec::new() }
    }

    /// Round-trip latency (up then down) in lower-rate samples; equal to the
    /// upsample latency in higher-rate samples.
    pub fn latency(&self) -> usize {
        2 * self.half_latency
    }

    /// Reserves scratch for blocks of up to `max_lower` lower-rate samples
    /// (`up` input / `down` output), so `up` and `down` on such blocks never
    /// allocate. Output is unaffected.
    pub fn prepare(&mut self, max_lower: usize) {
        let need = 2 * self.kernel.len() + 2 * max_lower;
        self.scratch.reserve(need.saturating_sub(self.scratch.len()));
    }

    pub fn reset(&mut self) {
        self.up_hist.iter_mut().for_each(|v| *v = 0.0);
        self.down_hist.iter_mut().for_each(|v| *v = 0.0);
    }

    /// `out.len()` must be `2 * input.len()`.
    pub fn up(&mut self, input: &[f32], out: &mut [f32]) {
        assert_eq!(out.len(), 2 * input.len(), "up: output must be twice the input length");
        let k = self.kernel.len();
        let h = self.half_latency;
        let n = input.len();
        let buf = &mut self.scratch;
        buf.clear();
        buf.extend_from_slice(&self.up_hist);
        buf.extend_from_slice(input);
        for i in 0..n {
            out[2 * i] = buf[i + h];
            let window = &buf[i + 1..i + 1 + k];
            let mut sum = 0.0f32;
            for (x, c) in window.iter().zip(&self.kernel) {
                sum += x * c;
            }
            out[2 * i + 1] = sum;
        }
        self.up_hist.copy_from_slice(&buf[n..n + k]);
    }

    /// `input.len()` must be `2 * out.len()`.
    pub fn down(&mut self, input: &[f32], out: &mut [f32]) {
        assert_eq!(input.len(), 2 * out.len(), "down: input must be twice the output length");
        let k = self.kernel.len();
        let n = out.len();
        let buf = &mut self.scratch;
        buf.clear();
        buf.extend_from_slice(&self.down_hist);
        buf.extend_from_slice(input);
        for (i, o) in out.iter_mut().enumerate() {
            let v1 = buf[2 * i + k];
            let mut sum = 0.0f32;
            for (j, c) in self.kernel.iter().enumerate() {
                sum += buf[2 * (i + j) + 1] * c;
            }
            *o = (v1 + sum) * 0.5;
        }
        self.down_hist.copy_from_slice(&buf[2 * n..2 * n + 2 * k]);
    }
}

/// A mono oversampler at 1x (pass-through), 2x or 4x (two cascaded 2x
/// half-band stages). Use one per channel.
#[derive(Clone, Debug)]
pub struct Oversampler {
    factor: usize,
    stages: Vec<HalfBand2x>,
    hi: Vec<f32>,
    mid: Vec<f32>,
}

impl Oversampler {
    /// `factor` is 1, 2 or 4.
    pub fn new(factor: usize) -> Self {
        let stages = match factor {
            1 => vec![],
            2 => vec![HalfBand2x::new(STAGE1_HALF_LATENCY, STAGE1_PASS)],
            4 => vec![
                HalfBand2x::new(STAGE1_HALF_LATENCY, STAGE1_PASS),
                HalfBand2x::new(STAGE2_HALF_LATENCY, STAGE2_PASS),
            ],
            _ => panic!("oversampling factor must be 1, 2 or 4, got {factor}"),
        };
        Oversampler { factor, stages, hi: Vec::new(), mid: Vec::new() }
    }

    pub fn factor(&self) -> usize {
        self.factor
    }

    /// Round-trip latency (up, then down) in base-rate (1x) samples, for
    /// host delay compensation: 0 at 1x, 48 at 2x, 56 at 4x. Each direction
    /// is half of it (the upsampled signal is `factor * latency / 2`
    /// high-rate samples late).
    pub fn latency_samples(&self) -> usize {
        // Stage i runs with a lower rate of 2^i x base, so its round trip in
        // base samples is its own latency / 2^i (exact for the designs here).
        self.stages.iter().enumerate().map(|(i, s)| s.latency() >> i).sum()
    }

    pub fn reset(&mut self) {
        self.stages.iter_mut().for_each(HalfBand2x::reset);
    }

    /// Reserves every internal buffer for blocks of up to `max_block`
    /// base-rate samples, so [`Self::up`], [`Self::down`] and
    /// [`Self::process`] on such blocks never touch the heap. Without it the
    /// buffers grow on first use. Output is unaffected.
    pub fn prepare(&mut self, max_block: usize) {
        self.hi.reserve((self.factor * max_block).saturating_sub(self.hi.len()));
        if self.stages.len() > 1 {
            self.mid.reserve((2 * max_block).saturating_sub(self.mid.len()));
        }
        for (i, s) in self.stages.iter_mut().enumerate() {
            s.prepare(max_block << i);
        }
    }

    /// Upsamples `input` and returns the `factor * input.len()` high-rate
    /// samples (valid until the next call).
    pub fn up(&mut self, input: &[f32]) -> &mut [f32] {
        let n = input.len();
        self.hi.resize(n * self.factor, 0.0);
        match self.stages.len() {
            0 => self.hi.copy_from_slice(input),
            1 => self.stages[0].up(input, &mut self.hi),
            _ => {
                self.mid.resize(2 * n, 0.0);
                self.stages[0].up(input, &mut self.mid);
                self.stages[1].up(&self.mid, &mut self.hi);
            }
        }
        &mut self.hi
    }

    /// Downsamples `input` (`factor * out.len()` high-rate samples) into `out`.
    pub fn down(&mut self, input: &[f32], out: &mut [f32]) {
        assert_eq!(input.len(), out.len() * self.factor, "down: input must be factor x output length");
        match self.stages.len() {
            0 => out.copy_from_slice(input),
            1 => self.stages[0].down(input, out),
            _ => {
                self.mid.resize(2 * out.len(), 0.0);
                self.stages[1].down(input, &mut self.mid);
                self.stages[0].down(&self.mid, out);
            }
        }
    }

    /// Runs `f` on `block` at the oversampled rate, in place:
    /// up, `f(high_rate_samples)`, down. Output is delayed by
    /// `latency_samples()` base-rate samples.
    pub fn process<F: FnMut(&mut [f32])>(&mut self, block: &mut [f32], mut f: F) {
        self.up(block);
        let mut hi = std::mem::take(&mut self.hi);
        f(&mut hi);
        self.down(&hi, block);
        self.hi = hi;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 44_100.0;

    fn db(x: f64) -> f64 {
        20.0 * x.max(1e-300).log10()
    }

    /// |DTFT| of `h` at `f_hz`, with `h` sampled at `rate_hz`.
    fn dtft_mag(h: &[f64], f_hz: f64, rate_hz: f64) -> f64 {
        let w = 2.0 * PI * f_hz / rate_hz;
        let (mut re, mut im) = (0.0, 0.0);
        for (n, v) in h.iter().enumerate() {
            re += v * (w * n as f64).cos();
            im -= v * (w * n as f64).sin();
        }
        (re * re + im * im).sqrt()
    }

    /// Upsampler impulse response at the high rate, scaled so the passband
    /// amplitude gain is 1 (zero-stuffing divides amplitude by the factor).
    fn up_response(factor: usize) -> Vec<f64> {
        let mut os = Oversampler::new(factor);
        let mut x = vec![0.0f32; 256];
        x[0] = 1.0;
        os.up(&x).iter().map(|&v| v as f64 / factor as f64).collect()
    }

    /// Equivalent anti-alias filter of the downsampler, g[n] in
    /// y[m] = sum g[n] x[factor*m - n], recovered with one impulse per phase.
    fn down_response(factor: usize) -> Vec<f64> {
        let len = 256;
        let mut g = vec![0.0f64; len * factor];
        let s = 4; // impulse placed after a few zero blocks
        for p in 0..factor {
            let mut os = Oversampler::new(factor);
            let mut x = vec![0.0f32; len * factor];
            x[factor * s + p] = 1.0;
            let mut y = vec![0.0f32; len];
            os.down(&x, &mut y);
            for (m, &v) in y.iter().enumerate().skip(s) {
                let n = factor * (m - s);
                if n >= p {
                    g[n - p] = v as f64;
                } else {
                    assert!(v.abs() < 1e-12, "non-causal tap");
                }
            }
        }
        g
    }

    fn freqs(lo: f64, hi: f64, n: usize) -> impl Iterator<Item = f64> {
        (0..=n).map(move |i| lo + (hi - lo) * i as f64 / n as f64)
    }

    #[test]
    fn passband_is_flat_within_a_tenth_of_a_db_to_18k() {
        for factor in [2usize, 4] {
            let rate = FS * factor as f64;
            let up = up_response(factor);
            let down = down_response(factor);
            let (mut worst_up, mut worst_down, mut worst_rt) = (0.0f64, 0.0f64, 0.0f64);
            for f in freqs(0.0, 18_000.0, 720) {
                let gu = dtft_mag(&up, f, rate);
                let gd = dtft_mag(&down, f, rate);
                worst_up = worst_up.max(db(gu).abs());
                worst_down = worst_down.max(db(gd).abs());
                worst_rt = worst_rt.max(db(gu * gd).abs());
            }
            eprintln!("{factor}x passband 0-18 kHz worst |dev|: up {worst_up:.6} dB, down {worst_down:.6} dB, round trip {worst_rt:.6} dB");
            // Spec: +/-0.1 dB. Regression guard (measured 0.00016 dB worst on
            // x86), with headroom for x86/ARM or SIMD drift: 0.0005 dB.
            assert!(worst_up <= 0.0005 && worst_down <= 0.0005 && worst_rt <= 0.0005, "{factor}x passband");
        }
    }

    #[test]
    fn stopband_is_at_least_90_db_down() {
        for factor in [2usize, 4] {
            let rate = FS * factor as f64;
            let up = up_response(factor);
            let down = down_response(factor);
            // Everything from the stop edge (0.57 x 44.1k = 25.1 kHz, where the
            // first image of 18.96 kHz content lands) to the high-rate Nyquist.
            let lo = (1.0 - STAGE1_PASS) * FS;
            let (mut worst_up, mut worst_down) = (-400.0f64, -400.0f64);
            for f in freqs(lo, rate / 2.0, 4000) {
                worst_up = worst_up.max(db(dtft_mag(&up, f, rate)));
                worst_down = worst_down.max(db(dtft_mag(&down, f, rate)));
            }
            eprintln!("{factor}x stopband {:.0} Hz..{:.0} Hz worst: up (image) {worst_up:.1} dB, down (alias) {worst_down:.1} dB", lo, rate / 2.0);
            // Spec: -90 dB. Regression guard (measured -103.1 dB on x86): -100 dB.
            assert!(worst_up <= -100.0 && worst_down <= -100.0, "{factor}x stopband");
        }
    }

    /// Deterministic multitone, 24 partials between 20 Hz and 18 kHz.
    fn multitone(n: usize) -> Vec<f32> {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let parts: Vec<(f64, f64)> = (0..24).map(|_| (20.0 + rnd() * 17_980.0, rnd() * 2.0 * PI)).collect();
        (0..n)
            .map(|i| {
                let t = i as f64 / FS;
                (parts.iter().map(|(f, ph)| (2.0 * PI * f * t + ph).sin()).sum::<f64>() * 0.03) as f32
            })
            .collect()
    }

    fn round_trip(factor: usize, x: &[f32], block: usize) -> Vec<f32> {
        let mut os = Oversampler::new(factor);
        let mut y = x.to_vec();
        for chunk in y.chunks_mut(block) {
            os.process(chunk, |_| {});
        }
        y
    }

    fn null_db(x: &[f32], y: &[f32], lag: usize, skip: usize) -> f64 {
        let (mut e, mut s) = (0.0f64, 0.0f64);
        for i in skip..x.len() - lag {
            let d = y[i + lag] as f64 - x[i] as f64;
            e += d * d;
            s += x[i] as f64 * x[i] as f64;
        }
        10.0 * (e / s).log10()
    }

    #[test]
    fn up_down_round_trip_nulls_below_minus_90_db_at_the_reported_latency() {
        let x = multitone(1 << 15);
        for factor in [2usize, 4] {
            let y = round_trip(factor, &x, 512);
            let l = Oversampler::new(factor).latency_samples();
            let at = null_db(&x, &y, l, 4096);
            let off_lo = null_db(&x, &y, l - 1, 4096);
            let off_hi = null_db(&x, &y, l + 1, 4096);
            eprintln!("{factor}x round-trip null at latency {l}: {at:.1} dB (at {}: {off_lo:.1} dB, at {}: {off_hi:.1} dB)", l - 1, l + 1);
            // Spec: -90 dB. Regression guards (measured -110.9 dB at 2x and
            // -101.1 dB at 4x on x86): -105 dB at 2x, -98 dB at 4x.
            let guard = if factor == 2 { -105.0 } else { -98.0 };
            assert!(at <= guard, "{factor}x null {at} (guard {guard})");
            assert!(off_lo > -20.0 && off_hi > -20.0, "only the reported latency should null");
        }
    }

    #[test]
    fn reported_latency_matches_the_impulse_and_the_design() {
        assert_eq!(Oversampler::new(1).latency_samples(), 0);
        assert_eq!(Oversampler::new(2).latency_samples(), 2 * STAGE1_HALF_LATENCY);
        assert_eq!(Oversampler::new(4).latency_samples(), 2 * STAGE1_HALF_LATENCY + STAGE2_HALF_LATENCY);
        for factor in [1usize, 2, 4] {
            let l = Oversampler::new(factor).latency_samples();
            let mut x = vec![0.0f32; 1024];
            x[100] = 1.0;
            let y = round_trip(factor, &x, 1024);
            let peak = y.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
            assert_eq!(peak, 100 + l, "{factor}x round-trip peak");
            // An impulse is full-band, so the band edge trims its peak a little
            // (about 0.976); the pass-through is exact.
            let floor = if factor == 1 { 1.0 } else { 0.95 };
            assert!(y[peak] >= floor && y[peak] <= 1.0, "{factor}x round-trip peak height {}", y[peak]);
            // Each direction is half the round trip: the upsampled impulse
            // peaks factor * l / 2 high-rate samples late.
            let mut os = Oversampler::new(factor);
            let hi = os.up(&x).to_vec();
            let hp = hi.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
            assert_eq!(hp, factor * 100 + factor * l / 2, "{factor}x up peak");
        }
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        let x = multitone(5000);
        for factor in [2usize, 4] {
            let a = round_trip(factor, &x, 5000);
            let b = round_trip(factor, &x, 1);
            let c = round_trip(factor, &x, 37);
            assert_eq!(a, b, "{factor}x block 1");
            assert_eq!(a, c, "{factor}x block 37");
        }
    }

    #[test]
    fn one_x_is_a_bit_exact_pass_through_and_reset_clears_state() {
        let x = multitone(777);
        assert_eq!(round_trip(1, &x, 64), x);
        let mut os = Oversampler::new(4);
        let mut a = x.clone();
        os.process(&mut a, |_| {});
        os.reset();
        let mut b = x.clone();
        os.process(&mut b, |_| {});
        assert_eq!(a, b);
    }

    #[test]
    fn oversampled_tanh_aliases_far_less_than_1x() {
        // 5 kHz sine at -6 dBFS into tanh(4x): the 7th harmonic (35 kHz)
        // folds to 9.1 kHz at 1x. Oversampling removes it.
        let n = 1 << 14;
        let f0 = 5_000.0;
        let x: Vec<f32> = (0..n).map(|i| (0.5 * (2.0 * PI * f0 * i as f64 / FS).sin()) as f32).collect();
        let shape = |v: &mut [f32]| v.iter_mut().for_each(|s| *s = (4.0 * *s).tanh());
        let mut plain = x.clone();
        shape(&mut plain);
        let mut os = Oversampler::new(4);
        let mut hq = x.clone();
        for c in hq.chunks_mut(256) {
            os.process(c, shape);
        }
        let tail = |v: &[f32]| v[4096..].iter().map(|&s| s as f64).collect::<Vec<_>>();
        let alias = FS - 7.0 * f0; // 35 kHz folds to 44.1k - 35k = 9.1 kHz
        let rel = |v: &[f64]| {
            let w: Vec<f64> = v.iter().enumerate().map(|(i, s)| s * (0.5 - 0.5 * (2.0 * PI * i as f64 / v.len() as f64).cos())).collect();
            db(dtft_mag(&w, alias, FS) / dtft_mag(&w, f0, FS))
        };
        let (a1, a4) = (rel(&tail(&plain)), rel(&tail(&hq)));
        eprintln!("tanh alias at {alias:.0} Hz: 1x {a1:.1} dB, 4x {a4:.1} dB");
        assert!(a4 < a1 - 30.0, "4x {a4} vs 1x {a1}");
    }

    #[test]
    fn unit_impulse_up_passthrough_down_peaks_exactly_at_latency_samples() {
        // Input [1.0, 0.0, 0.0, ...] at the base rate, up -> untouched -> down,
        // one block; the peak index is in base-rate frames.
        for (factor, want) in [(1usize, 0usize), (2, 48), (4, 56)] {
            let mut os = Oversampler::new(factor);
            assert_eq!(os.latency_samples(), want, "{factor}x latency_samples (base-rate samples)");
            let mut x = vec![0.0f32; 512];
            x[0] = 1.0;
            let hi = os.up(&x).to_vec();
            let mut y = vec![0.0f32; 512];
            os.down(&hi, &mut y);
            let peak = y.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
            eprintln!("{factor}x unit impulse: output peak at base-rate index {peak} (latency_samples {}), height {:.6}", os.latency_samples(), y[peak]);
            assert_eq!(peak, os.latency_samples(), "{factor}x peak index");
            // Nothing arrives before the reported latency except the filter's
            // own pre-ring, which is symmetric around the peak.
            for k in 0..peak {
                assert!((y[k] - y[2 * peak - k]).abs() <= 1e-6, "{factor}x round trip not symmetric at {k}");
            }
        }
    }

    #[test]
    fn fir_coefficients_are_symmetric_linear_phase_and_strict_half_band() {
        for (h, pass) in [(STAGE1_HALF_LATENCY, STAGE1_PASS), (STAGE2_HALF_LATENCY, STAGE2_PASS)] {
            // The raw f64 design, before mirroring: how far rounding leaves it
            // from exact symmetry.
            let raw = kaiser_sinc(2 * h, pass, 1.0 - pass);
            let raw_asym = (0..raw.len()).map(|i| (raw[i] - raw[raw.len() - 1 - i]).abs()).fold(0.0f64, f64::max);
            let raw_f32_asym = (0..raw.len())
                .filter(|&i| (raw[i] as f32).to_bits() != (raw[raw.len() - 1 - i] as f32).to_bits())
                .count();
            // The whole 2x interpolation filter, read off the stage's own
            // upsampler: an impulse at input 0 gives h on high-rate indices
            // 1 ..= 4h-1, so N = 4h - 1 taps centred on index 2h.
            let mut st = HalfBand2x::new(h, pass);
            let mut x = vec![0.0f32; 4 * h];
            x[0] = 1.0;
            let mut ir = vec![0.0f32; 8 * h];
            st.up(&x, &mut ir);
            assert_eq!(ir[0], 0.0);
            assert!(ir[4 * h..].iter().all(|&v| v == 0.0), "taps beyond N");
            let taps = &ir[1..4 * h];
            let n = taps.len();
            let centre = n / 2;
            for i in 0..n {
                assert_eq!(taps[i].to_bits(), taps[n - 1 - i].to_bits(), "half-latency {h}: h[{i}] != h[{}]", n - 1 - i);
                let dist = i.abs_diff(centre);
                if dist == 0 {
                    assert_eq!(taps[i], 1.0, "centre tap (0.5 in unity-gain half-band scaling)");
                } else if dist % 2 == 0 {
                    assert_eq!(taps[i].to_bits(), 0.0f32.to_bits(), "half-latency {h}: h[{i}] should be a zero tap");
                } else {
                    assert!(taps[i] != 0.0, "half-latency {h}: h[{i}] odd tap is zero");
                }
            }
            let zeros = taps.iter().filter(|&&v| v == 0.0).count();
            eprintln!(
                "half-latency {h}: N = {n} taps, h[n] == h[N-1-n] bit-exact, {zeros} zero taps (every other one), centre h[{centre}] = 1.0; \
                 raw f64 design asymmetry {raw_asym:.1e} ({raw_f32_asym} of {} f32 taps differ from their mirror before mirroring)",
                raw.len()
            );
        }
        // The 4x cascade is a convolution of two symmetric half-band filters:
        // symmetric to float rounding, but not itself a half-band (no zero taps).
        let mut os = Oversampler::new(4);
        let mut x = vec![0.0f32; 256];
        x[0] = 1.0;
        let ir = os.up(&x).to_vec();
        let c = 4 * os.latency_samples() / 2;
        let worst = (0..=c).map(|n| (ir[c - n] - ir[c + n]).abs()).fold(0.0f32, f32::max);
        eprintln!("4x cascade impulse response: centre {c} (high-rate), worst asymmetry {worst:.1e}");
        assert!(worst <= 1e-6, "4x asymmetry {worst}");
    }

    #[test]
    #[should_panic]
    fn three_x_is_refused() {
        let _ = Oversampler::new(3);
    }
}
