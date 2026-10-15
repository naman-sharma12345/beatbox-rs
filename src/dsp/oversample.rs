//! Clean-room 2x / 4x half-band polyphase oversampler, and an opt-in
//! wrapper that runs the ADAA waveshapers of [`super::adaa`] inside it.
//!
//! Written from textbook design only. No third-party oversampler code,
//! coefficients or design notes were read or used. References:
//! - T. W. Parks, J. H. McClellan, "Chebyshev approximation for nonrecursive
//!   digital filters with linear phase", IEEE Trans. Circuit Theory 19(2),
//!   1972 (the Remez exchange used by [`design_halfband`]);
//! - A. V. Oppenheim, R. W. Schafer, "Discrete-Time Signal Processing",
//!   ch. 4 (rate change, polyphase) and ch. 7 (optimum FIR design);
//! - R. E. Crochiere, L. R. Rabiner, "Multirate Digital Signal Processing",
//!   1983 (half-band filters, multistage decimation and interpolation);
//! - f. j. harris, "Multirate Signal Processing for Communication Systems",
//!   2004 (half-band polyphase structure, cascaded 2x stages).
//!
//! # Design
//!
//! A half-band low-pass `h` of length `4K - 1` has `h[c] = 1/2` at its
//! centre `c = 2K - 1`, and every other tap an even distance from the
//! centre is exactly zero. Its response is
//! `H(w) = 1/2 + sum_{j=1..K} 2 g_j cos((2j - 1) w)` with `g_j = h[c +- (2j - 1)]`,
//! so `H(w) + H(pi - w) = 1`: a minimax fit of the cosine sum to `1/2` on
//! the passband `[0, wp]` automatically gives the mirrored stopband
//! `[pi - wp, pi]` with the same ripple `delta`. [`design_halfband`] runs
//! the Remez exchange on that one-band problem (Haar basis
//! `cos((2j - 1) w)`, `K + 1` alternation points, the `(K + 1)`-square
//! system solved directly).
//!
//! Two fixed stages, chosen here from first principles:
//!
//! | stage | K | taps (nonzero) | pass edge | stop edge | ripple / rejection |
//! |---|---|---|---|---|---|
//! | 1 (1x <-> 2x) | 56 | 223 (113) | 0.475 fs | 0.525 fs | about +-0.0002 dB / -93 dB |
//! | 2 (2x <-> 4x) | 7 | 27 (15) | 0.54 fs | 1.46 fs | far below stage 1 / -110 dB |
//!
//! (`fs` is the base rate.) Stage 1 sets the passband: flat to 20.9 kHz at
//! 44.1 kHz. A half-band is always -6 dB at the base Nyquist, so anything
//! the shaper leaves in its transition band (0.475..0.525 fs) folds into
//! 0.475..0.5 fs, above 20.9 kHz at 44.1 kHz; the narrow transition keeps
//! that leak out of the audio band. Stage 2 runs at 4x and only has to
//! reject what would fold onto stage 1's pass and transition bands, so its
//! own transition is wide and it is short; its stopband starts where its
//! fold-over lands inside stage 1's stopband, so in the 4x path every alias
//! that reaches the audio band is attenuated by stage 2's full rejection.
//!
//! Polyphase: interpolation needs one `2K`-tap dot product per input
//! sample (the other output phase is the centre tap, a pure delay), and
//! decimation one `2K`-tap dot product per output sample plus the centre
//! tap, so the zero taps are never multiplied.
//!
//! Latency (round trip, base-rate samples, exact and whole): `2K1 - 1` = 111
//! at 2x; at 4x stage 2 adds `(2K2 - 1) / 2` and a one-sample delay at the
//! 2x rate adds `1/2`, so `2K1 - 1 + K2` = 118. The whole round trip with
//! nothing in the middle is linear phase about that delay.
//!
//! Measured (my measurement, `measure_alias_table` / `measure_cpu` below):
//! alias suppression against the naive 1x clipper, worst case over the
//! hard / tanh / cubic / quintic curves, 1, 5, 10 and 15 kHz tones at
//! 44.1 and 48 kHz, driven +12 and +20 dB:
//!
//! | | 1x | 2x | 4x |
//! |---|---|---|---|
//! | ADAA2, tones | -14.6 dB | 29.4 dB | 53.0 dB |
//! | ADAA1, tones | 4.6 dB | 19.3 dB | 34.2 dB |
//! | ADAA2, sweep (full / to 20 kHz) | 16.3 / 16.9 dB | 36.3 / 38.8 dB | 38.2 / 61.8 dB |
//!
//! (ADAA2's own sinc-squared averaging dims a 15 kHz fundamental at 1x,
//! hence the negative figure there.) First order at 2x stays near 20 dB
//! on 10-15 kHz tones because the shaper itself aliases at the 2x rate,
//! so second order is the default at both factors, and a first-order
//! request at 2x is routed to second order outside the low-CPU draft mode
//! ([`route_order`]). Cost per base-rate sample, ADAA2 tanh:
//! about 31 ns at 1x, 101 ns at 2x, 193 ns at 4x.
//!
//! Opt-in only: nothing in the engine uses this, so no existing render
//! changes.

use super::adaa::{AdaaOrder, Nonlinearity, OversampleHook, Waveshaper};
use std::sync::OnceLock;

/// One half-band stage's design parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HalfBandSpec {
    /// Number of nonzero taps on each side of the centre; length `4k - 1`.
    pub k: usize,
    /// Passband edge in cycles per sample at the stage's high rate
    /// (below 0.25; the stopband starts at `0.5 - pass_edge`).
    pub pass_edge: f64,
}

/// Stage 1, between the base rate and 2x.
pub const STAGE1: HalfBandSpec = HalfBandSpec { k: 56, pass_edge: 0.2375 };
/// Stage 2, between 2x and 4x.
pub const STAGE2: HalfBandSpec = HalfBandSpec { k: 7, pass_edge: 0.135 };

/// A designed half-band filter.
#[derive(Clone, Debug)]
pub struct HalfBandDesign {
    pub spec: HalfBandSpec,
    /// `g_1..g_K`: the taps at odd distance `2j - 1` from the centre.
    pub taps: Vec<f64>,
    /// Equiripple deviation: passband `|H - 1| <= delta`, stopband `|H| <= delta`.
    pub delta: f64,
    /// Remez iterations to convergence.
    pub iterations: usize,
}

impl HalfBandDesign {
    /// The full impulse response, length `4K - 1`, centre `1/2`.
    pub fn impulse_response(&self) -> Vec<f64> {
        let k = self.spec.k;
        let c = 2 * k - 1;
        let mut h = vec![0.0; 4 * k - 1];
        h[c] = 0.5;
        for (j, &g) in self.taps.iter().enumerate() {
            let d = 2 * j + 1;
            h[c + d] = g;
            h[c - d] = g;
        }
        h
    }

    /// `|H|` at `f` cycles per sample (zero phase, so this is `H` itself).
    pub fn response(&self, f: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * f;
        0.5 + self.taps.iter().enumerate().map(|(j, &g)| 2.0 * g * ((2 * j + 1) as f64 * w).cos()).sum::<f64>()
    }
}

/// Solves `a x = b` in place (Gaussian elimination, partial pivoting).
fn solve(a: &mut [f64], b: &mut [f64], n: usize) {
    for col in 0..n {
        let piv = (col..n).max_by(|&i, &j| a[i * n + col].abs().total_cmp(&a[j * n + col].abs())).unwrap_or(col);
        if piv != col {
            for k in 0..n {
                a.swap(col * n + k, piv * n + k);
            }
            b.swap(col, piv);
        }
        let d = a[col * n + col];
        for r in col + 1..n {
            let m = a[r * n + col] / d;
            if m != 0.0 {
                for k in col..n {
                    a[r * n + k] -= m * a[col * n + k];
                }
                b[r] -= m * b[col];
            }
        }
    }
    for r in (0..n).rev() {
        let mut s = b[r];
        for k in r + 1..n {
            s -= a[r * n + k] * b[k];
        }
        b[r] = s / a[r * n + r];
    }
}

/// Equiripple half-band design by the Remez exchange (Parks & McClellan
/// 1972), on the equivalent one-band problem described in the module docs.
/// Deterministic: same `spec`, same taps, on every run.
pub fn design_halfband(spec: HalfBandSpec) -> HalfBandDesign {
    let k = spec.k;
    assert!(k >= 1 && spec.pass_edge > 0.0 && spec.pass_edge < 0.25, "bad half-band spec {spec:?}");
    let wp = 2.0 * std::f64::consts::PI * spec.pass_edge;
    let ng = 32 * k + 1;
    let grid: Vec<f64> = (0..ng).map(|i| wp * i as f64 / (ng - 1) as f64).collect();
    let basis = |w: f64, j: usize| ((2 * j + 1) as f64 * w).cos();
    let mut ext: Vec<usize> = (0..=k).map(|i| ((i * (ng - 1)) as f64 / k as f64).round() as usize).collect();
    let n = k + 1;
    let mut c = vec![0.0; k];
    let mut delta = 0.0;
    let mut iterations = 0;
    let mut err = vec![0.0; ng];
    for it in 1..=100 {
        iterations = it;
        let mut a = vec![0.0; n * n];
        let mut b = vec![0.5; n];
        for (r, &gi) in ext.iter().enumerate() {
            for j in 0..k {
                a[r * n + j] = basis(grid[gi], j);
            }
            a[r * n + k] = if r % 2 == 0 { 1.0 } else { -1.0 };
        }
        solve(&mut a, &mut b, n);
        c.copy_from_slice(&b[..k]);
        delta = b[k].abs();
        for (e, &w) in err.iter_mut().zip(&grid) {
            *e = (0..k).map(|j| c[j] * basis(w, j)).sum::<f64>() - 0.5;
        }
        // Local extrema of the error (end points included), then keep one
        // per run of equal sign, the largest.
        let mut alt: Vec<usize> = Vec::with_capacity(2 * n);
        for i in 0..ng {
            let e = err[i];
            let l = if i > 0 { err[i - 1] } else { f64::NAN };
            let r = if i + 1 < ng { err[i + 1] } else { f64::NAN };
            let is_ext = if e >= 0.0 { !(l > e) && !(r > e) } else { !(l < e) && !(r < e) };
            if !is_ext {
                continue;
            }
            match alt.last() {
                Some(&p) if (err[p] >= 0.0) == (e >= 0.0) => {
                    if e.abs() > err[p].abs() {
                        *alt.last_mut().unwrap() = i;
                    }
                }
                _ => alt.push(i),
            }
        }
        while alt.len() > n {
            if err[alt[0]].abs() < err[alt[alt.len() - 1]].abs() {
                alt.remove(0);
            } else {
                alt.pop();
            }
        }
        if alt.len() < n || alt == ext {
            break;
        }
        ext = alt;
    }
    // Taps: A(w) = sum c_j cos((2j-1)w) = sum 2 g_j cos(...), so g_j = c_j / 2.
    HalfBandDesign { spec, taps: c.iter().map(|v| 0.5 * v).collect(), delta, iterations }
}

/// Stage 1's design, computed once per process.
pub fn stage1_design() -> &'static HalfBandDesign {
    static D: OnceLock<HalfBandDesign> = OnceLock::new();
    D.get_or_init(|| design_halfband(STAGE1))
}

/// Stage 2's design, computed once per process.
pub fn stage2_design() -> &'static HalfBandDesign {
    static D: OnceLock<HalfBandDesign> = OnceLock::new();
    D.get_or_init(|| design_halfband(STAGE2))
}

/// `sum a[i] * b[i]` with eight partial sums, so the loop vectorizes.
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let ca = a.chunks_exact(8);
    let cb = b.chunks_exact(8);
    let (ra, rb) = (ca.remainder(), cb.remainder());
    let mut acc = [0.0f32; 8];
    for (x, y) in ca.zip(cb) {
        for l in 0..8 {
            acc[l] += x[l] * y[l];
        }
    }
    let mut tail = 0.0f32;
    for (x, y) in ra.iter().zip(rb) {
        tail += x * y;
    }
    ((acc[0] + acc[4]) + (acc[1] + acc[5])) + ((acc[2] + acc[6]) + (acc[3] + acc[7])) + tail
}

/// A window of the last `n` samples, oldest first, without copying: every
/// sample is written twice, `n` apart, so `buf[pos + 1..=pos + n]` is
/// always contiguous.
#[derive(Clone, Debug)]
struct History {
    buf: Box<[f32]>,
    n: usize,
    pos: usize,
}

impl History {
    fn new(n: usize) -> Self {
        Self { buf: vec![0.0; 2 * n].into_boxed_slice(), n, pos: n - 1 }
    }
    #[inline]
    fn push(&mut self, x: f32) -> &[f32] {
        self.pos += 1;
        if self.pos == self.n {
            self.pos = 0;
        }
        self.buf[self.pos] = x;
        self.buf[self.pos + self.n] = x;
        &self.buf[self.pos + 1..=self.pos + self.n]
    }
    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.pos = self.n - 1;
    }
}

/// One half-band 2x stage: a polyphase interpolator and a polyphase
/// decimator, each with its own state.
#[derive(Clone, Debug)]
struct Stage {
    k: usize,
    /// `2K` taps over a window `x[n - 2K + 1..=n]`: `g_{K-i}` for `i < K`,
    /// `g_{i-K+1}` for `i >= K` (the two symmetric halves of the odd phase).
    kern: Box<[f32]>,
    up: History,
    down_even: History,
    /// Delay line of length `K` for the decimator's centre tap.
    down_odd: Box<[f32]>,
    odd_pos: usize,
}

impl Stage {
    fn new(d: &HalfBandDesign) -> Self {
        let k = d.spec.k;
        let kern: Vec<f32> = (0..2 * k).map(|i| if i < k { d.taps[k - 1 - i] } else { d.taps[i - k] } as f32).collect();
        Self {
            k,
            kern: kern.into_boxed_slice(),
            up: History::new(2 * k),
            down_even: History::new(2 * k),
            down_odd: vec![0.0; k].into_boxed_slice(),
            odd_pos: 0,
        }
    }

    fn reset(&mut self) {
        self.up.clear();
        self.down_even.clear();
        self.down_odd.fill(0.0);
        self.odd_pos = 0;
    }

    /// 2x up: `out.len() == 2 * input.len()`. Gain 2 on the filter, so the
    /// passband is unity. Delay `2K - 1` samples at the high rate.
    fn upsample(&mut self, input: &[f32], out: &mut [f32]) {
        let k = self.k;
        for (x, o) in input.iter().zip(out.chunks_exact_mut(2)) {
            let w = self.up.push(*x);
            o[0] = 2.0 * dot(&self.kern, w);
            o[1] = w[k];
        }
    }

    /// 2x down: `out.len() * 2 == input.len()`. Delay `2K - 1` samples at
    /// the high rate.
    fn downsample(&mut self, input: &[f32], out: &mut [f32]) {
        for (p, o) in input.chunks_exact(2).zip(out.iter_mut()) {
            let centre = self.down_odd[self.odd_pos];
            self.down_odd[self.odd_pos] = p[1];
            self.odd_pos += 1;
            if self.odd_pos == self.k {
                self.odd_pos = 0;
            }
            let w = self.down_even.push(p[0]);
            *o = 0.5 * centre + dot(&self.kern, w);
        }
    }
}

/// The oversampling factor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OversampleFactor {
    X2,
    X4,
}

impl OversampleFactor {
    pub fn ratio(self) -> usize {
        match self {
            OversampleFactor::X2 => 2,
            OversampleFactor::X4 => 4,
        }
    }
}

/// Default internal block (base-rate samples); longer blocks are processed
/// in pieces of this size.
pub const DEFAULT_MAX_BLOCK: usize = 512;

/// The 2x / 4x half-band oversampler, one channel. Implements
/// [`OversampleHook`], so [`Waveshaper::process_with`] runs a shaper inside
/// it. All buffers are allocated in the constructor; processing never
/// allocates.
#[derive(Clone, Debug)]
pub struct HalfBandOversampler {
    factor: OversampleFactor,
    s1: Stage,
    s2: Option<Stage>,
    /// One-sample delay at the 2x rate in the 4x down path, which makes the
    /// 4x round trip a whole number of base samples.
    pad: f32,
    max_block: usize,
    mid: Box<[f32]>,
    hi: Box<[f32]>,
}

impl HalfBandOversampler {
    pub fn new(factor: OversampleFactor) -> Self {
        Self::with_max_block(factor, DEFAULT_MAX_BLOCK)
    }

    pub fn with_max_block(factor: OversampleFactor, max_block: usize) -> Self {
        let max_block = max_block.max(1);
        let s2 = match factor {
            OversampleFactor::X2 => None,
            OversampleFactor::X4 => Some(Stage::new(stage2_design())),
        };
        let mid_len = if s2.is_some() { 2 * max_block } else { 0 };
        Self {
            factor,
            s1: Stage::new(stage1_design()),
            s2,
            pad: 0.0,
            max_block,
            mid: vec![0.0; mid_len].into_boxed_slice(),
            hi: vec![0.0; factor.ratio() * max_block].into_boxed_slice(),
        }
    }

    pub fn oversample_factor(&self) -> OversampleFactor {
        self.factor
    }

    /// Round-trip latency in whole base-rate samples: 111 at 2x, 118 at 4x.
    pub fn latency_samples(&self) -> usize {
        let l1 = 2 * self.s1.k - 1;
        match &self.s2 {
            None => l1,
            Some(s2) => l1 + s2.k,
        }
    }

    pub fn reset(&mut self) {
        self.s1.reset();
        if let Some(s2) = &mut self.s2 {
            s2.reset();
        }
        self.pad = 0.0;
    }

    /// Up, `f`, down on one piece no longer than `max_block`.
    fn run_piece(&mut self, piece: &mut [f32], f: &mut dyn FnMut(&mut [f32])) {
        let n = piece.len();
        let r = self.factor.ratio();
        let hi = &mut self.hi[..r * n];
        match &mut self.s2 {
            None => {
                self.s1.upsample(piece, hi);
                f(hi);
                self.s1.downsample(hi, piece);
            }
            Some(s2) => {
                let mid = &mut self.mid[..2 * n];
                self.s1.upsample(piece, mid);
                s2.upsample(mid, hi);
                f(hi);
                s2.downsample(hi, mid);
                for v in mid.iter_mut() {
                    let d = self.pad;
                    self.pad = *v;
                    *v = d;
                }
                self.s1.downsample(mid, piece);
            }
        }
    }
}

impl OversampleHook for HalfBandOversampler {
    fn factor(&self) -> usize {
        self.factor.ratio()
    }
    fn latency(&self) -> f64 {
        self.latency_samples() as f64
    }
    fn run(&mut self, block: &mut [f32], f: &mut dyn FnMut(&mut [f32])) {
        let m = self.max_block;
        for piece in block.chunks_mut(m) {
            self.run_piece(piece, f);
        }
    }
}

/// The ADAA order the oversampled path actually runs for a requested
/// `order` at `factor`. Second order is the default at both 2x and 4x.
/// First order at 2x leaves 10-15 kHz tones only about 20 dB below the
/// naive clipper (the shaper aliases at the 2x rate itself, my
/// measurement), short of the 24 dB floor, so outside draft mode a
/// first-order request at 2x is routed to second order. First order runs
/// as asked at 4x, and at either factor in draft mode (the low-CPU
/// preview). `Naive` is never changed.
pub fn route_order(order: AdaaOrder, factor: OversampleFactor, draft: bool) -> AdaaOrder {
    match (order, factor, draft) {
        (AdaaOrder::First, OversampleFactor::X2, false) => AdaaOrder::Second,
        _ => order,
    }
}

/// An ADAA waveshaper running inside the half-band oversampler: opt-in
/// anti-aliased saturation. One per channel.
///
/// Second-order ADAA is the default at 2x and 4x ([`Self::new`]). First
/// order is available at 4x through [`Self::with_order`], and at either
/// factor through [`Self::draft`], the low-CPU mode; a first-order request
/// at 2x outside draft mode runs second order (see [`route_order`]).
#[derive(Clone, Debug)]
pub struct AdaaOversampled<N: Nonlinearity> {
    shaper: Waveshaper<N>,
    os: HalfBandOversampler,
    draft: bool,
}

impl<N: Nonlinearity> AdaaOversampled<N> {
    /// The default: second-order ADAA at `factor`.
    pub fn new(shape: N, drive: f64, factor: OversampleFactor) -> Self {
        Self::with_order(shape, AdaaOrder::Second, drive, factor)
    }

    /// `order` at `factor`, routed by [`route_order`] (first order at 2x
    /// runs second order).
    pub fn with_order(shape: N, order: AdaaOrder, drive: f64, factor: OversampleFactor) -> Self {
        let order = route_order(order, factor, false);
        Self { shaper: Waveshaper::new(shape, order, drive), os: HalfBandOversampler::new(factor), draft: false }
    }

    /// Low-CPU draft mode: first-order ADAA at `factor`, 2x included
    /// (about 20% cheaper than second order with `tanh`, and less clean
    /// at 2x on high tones).
    pub fn draft(shape: N, drive: f64, factor: OversampleFactor) -> Self {
        Self { shaper: Waveshaper::new(shape, AdaaOrder::First, drive), os: HalfBandOversampler::new(factor), draft: true }
    }

    /// Shapes `block` in place.
    pub fn process_block(&mut self, block: &mut [f32]) {
        self.shaper.process_with(&mut self.os, block);
    }

    /// Total delay in base-rate samples: the oversampler's round trip plus
    /// the shaper's own delay at the high rate (first order 0.5, second 1.0
    /// high-rate samples).
    pub fn latency(&self) -> f64 {
        self.shaper.latency_with(&self.os)
    }

    pub fn reset(&mut self) {
        self.shaper.reset();
        self.os.reset();
    }

    /// The order that runs (after routing).
    pub fn order(&self) -> AdaaOrder {
        self.shaper.order
    }

    pub fn is_draft(&self) -> bool {
        self.draft
    }

    /// Changes the order between blocks, routed like the constructor (the
    /// draft flag is kept).
    pub fn set_order(&mut self, order: AdaaOrder) {
        self.shaper.order = route_order(order, self.os.oversample_factor(), self.draft);
    }

    /// Input gain into the curve (may change between blocks).
    pub fn set_drive(&mut self, drive: f64) {
        self.shaper.drive = drive;
    }

    /// Output gain after the curve.
    pub fn set_out_gain(&mut self, gain: f64) {
        self.shaper.out_gain = gain;
    }

    pub fn oversampler(&self) -> &HalfBandOversampler {
        &self.os
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::adaa::{HardClip, NoOversampling, SoftClip3, SoftClip5, Tanh};

    const NAMES: [&str; 4] = ["hard", "tanh", "soft3", "soft5"];

    /// In-place radix-2 FFT in `f64` (test-only measurement precision).
    fn fft64(re: &mut [f64], im: &mut [f64]) {
        let n = re.len();
        let mut j = 0;
        for i in 1..n {
            let mut bit = n >> 1;
            while j & bit != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j |= bit;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let ang = -2.0 * std::f64::consts::PI / len as f64;
            for s in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let (wr, wi) = ((ang * k as f64).cos(), (ang * k as f64).sin());
                    let (a, b) = (s + k, s + k + len / 2);
                    let (xr, xi) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                    re[b] = re[a] - xr;
                    im[b] = im[a] - xi;
                    re[a] += xr;
                    im[a] += xi;
                }
            }
            len <<= 1;
        }
    }

    fn power_spectrum(y: &[f32]) -> Vec<f64> {
        let mut re: Vec<f64> = y.iter().map(|&v| v as f64).collect();
        let mut im = vec![0.0; y.len()];
        fft64(&mut re, &mut im);
        (0..=y.len() / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect()
    }

    /// Alias power relative to the fundamental (dB) for a sine on odd FFT bin
    /// `bin` of the last `n` samples: every bin that is not a harmonic
    /// (a multiple of `bin`), up to `f_max_hz` (or Nyquist).
    pub(super) fn alias_db(y: &[f32], n: usize, bin: usize, fs: f64, f_max_hz: Option<f64>) -> f64 {
        let p = power_spectrum(&y[y.len() - n..]);
        let top = f_max_hz.map_or(n / 2, |f| ((f * n as f64 / fs).floor() as usize).min(n / 2));
        let alias: f64 = (1..=top).filter(|k| k % bin != 0).map(|k| p[k]).sum();
        10.0 * (alias / p[bin]).log10()
    }

    /// Runs `input` through the named curve at `order`, at 1x (`factor` None)
    /// or inside a fresh oversampler.
    pub(super) fn shape(name: &str, order: AdaaOrder, drive: f64, factor: Option<OversampleFactor>, input: &[f32]) -> Vec<f32> {
        fn go<N: Nonlinearity>(nl: N, order: AdaaOrder, drive: f64, factor: Option<OversampleFactor>, buf: &mut [f32]) {
            match factor {
                None => Waveshaper::new(nl, order, drive).process_block(buf),
                // measurement wants the order as asked: draft for first order
                // (2x routing would turn it into second), with_order otherwise
                Some(f) if order == AdaaOrder::First => AdaaOversampled::draft(nl, drive, f).process_block(buf),
                Some(f) => AdaaOversampled::with_order(nl, order, drive, f).process_block(buf),
            }
        }
        let mut buf = input.to_vec();
        match name {
            "hard" => go(HardClip, order, drive, factor, &mut buf),
            "tanh" => go(Tanh, order, drive, factor, &mut buf),
            "soft3" => go(SoftClip3, order, drive, factor, &mut buf),
            _ => go(SoftClip5, order, drive, factor, &mut buf),
        }
        buf
    }

    /// A unit sine on the odd FFT bin nearest `f_hz`, `n + warm` samples long.
    pub(super) fn tone(f_hz: f64, fs: f64, n: usize, warm: usize) -> (Vec<f32>, usize) {
        let mut bin = (f_hz * n as f64 / fs).round() as usize;
        if bin % 2 == 0 {
            bin += 1;
        }
        let x = (0..n + warm).map(|i| (2.0 * std::f64::consts::PI * (bin as f64) * (i % n) as f64 / n as f64).sin() as f32).collect();
        (x, bin)
    }

    #[test]
    fn stage_designs_are_equiripple_and_meet_their_specs() {
        for (spec, min_rej_db) in [(STAGE1, 93.0), (STAGE2, 108.0)] {
            let d = design_halfband(spec);
            assert_eq!(d.taps.len(), spec.k);
            let lim = 10f64.powf(-min_rej_db / 20.0);
            assert!(d.delta < lim, "{spec:?}: delta {:.3e}", d.delta);
            // dense check of both bands, and the alternation count
            let m = 200 * spec.k;
            let (mut worst_pass, mut worst_stop) = (0.0f64, 0.0f64);
            let mut signs = Vec::new();
            for i in 0..=m {
                let f = spec.pass_edge * i as f64 / m as f64;
                let e = d.response(f) - 1.0;
                worst_pass = worst_pass.max(e.abs());
                worst_stop = worst_stop.max(d.response(0.5 - f).abs());
                if e.abs() > 0.999 * d.delta {
                    let s = e > 0.0;
                    if signs.last() != Some(&s) {
                        signs.push(s);
                    }
                }
            }
            assert!(worst_pass <= d.delta * 1.01, "{spec:?}: pass {worst_pass:.3e} vs delta {:.3e}", d.delta);
            assert!(worst_stop <= d.delta * 1.01, "{spec:?}: stop {worst_stop:.3e}");
            assert!(signs.len() > spec.k, "{spec:?}: {} alternations, want {}", signs.len(), spec.k + 1);
            // half-band identity: exactly -6 dB at a quarter of the rate
            assert!((d.response(0.25) - 0.5).abs() < 1e-12);
            // the impulse response has the zeros a half-band must have
            let h = d.impulse_response();
            let c = 2 * spec.k - 1;
            assert_eq!(h.len(), 4 * spec.k - 1);
            for (i, v) in h.iter().enumerate() {
                if i != c && (i as isize - c as isize) % 2 == 0 {
                    assert_eq!(*v, 0.0);
                }
            }
            // deterministic
            assert_eq!(design_halfband(spec).taps, d.taps);
            eprintln!("{spec:?}: taps {} (nonzero {}), delta {:.3e} ({:.1} dB, pass ripple +-{:.5} dB), {} iterations", h.len(), 2 * spec.k + 1, d.delta, 20.0 * d.delta.log10(), 20.0 * (1.0 + d.delta).log10(), d.iterations);
        }
    }

    /// Impulse response of the round trip with nothing in the middle.
    fn round_trip_ir(f: OversampleFactor, len: usize, max_block: usize) -> Vec<f32> {
        let mut os = HalfBandOversampler::with_max_block(f, max_block);
        let mut x = vec![0.0f32; len];
        x[0] = 1.0;
        os.run(&mut x, &mut |_hi: &mut [f32]| {});
        x
    }

    #[test]
    fn round_trip_is_flat_linear_phase_and_delayed_by_the_reported_latency() {
        for (f, lat) in [(OversampleFactor::X2, 111usize), (OversampleFactor::X4, 118)] {
            let os = HalfBandOversampler::new(f);
            assert_eq!(os.latency_samples(), lat);
            assert_eq!(os.latency(), lat as f64);
            let n = 4096;
            let ir = round_trip_ir(f, n, 333);
            // block size does not matter
            assert_eq!(ir, round_trip_ir(f, n, 1));
            assert_eq!(ir, round_trip_ir(f, n, DEFAULT_MAX_BLOCK));
            let peak = (0..n).max_by(|&a, &b| ir[a].abs().total_cmp(&ir[b].abs())).unwrap();
            assert_eq!(peak, lat, "{f:?}: peak at {peak}");
            for i in 1..=lat {
                assert!((ir[lat + i] - ir[lat - i]).abs() < 2e-7, "{f:?}: not symmetric at +-{i}");
            }
            assert!(ir[2 * lat + 1..].iter().all(|v| v.abs() < 1e-7), "{f:?}: tail past 2x latency");
            // magnitude: flat to 20 kHz at 44.1 kHz (0.4535 fs) and to 0.47 fs
            let p = power_spectrum(&ir);
            let mut worst = 0.0f64;
            for (k, pk) in p.iter().enumerate().take(((0.47 * n as f64) as usize) + 1) {
                let db = 10.0 * pk.log10();
                worst = worst.max(db.abs());
                assert!(db.abs() < 0.002, "{f:?}: {db:.5} dB at {:.4} fs", k as f64 / n as f64);
            }
            eprintln!("{f:?}: latency {lat}, passband (0..0.47 fs) deviation {worst:.5} dB");
            // the shaper's own delay rides on top, at the high rate
            let r = f.ratio() as f64;
            let a1 = AdaaOversampled::draft(Tanh, 2.0, f);
            let a2 = AdaaOversampled::new(Tanh, 2.0, f);
            assert_eq!(a1.latency(), lat as f64 + 0.5 / r);
            assert_eq!(a2.latency(), lat as f64 + 1.0 / r);
        }
    }

    #[test]
    fn oversampled_adaa_meets_the_alias_targets() {
        // Targets: 2x >= 24 dB and 4x >= 30 dB less aliasing than the naive
        // 1x clipper, full band and audio band, and 4x below 2x in the audio
        // band, for every curve, tone and rate. Second-order ADAA, driven
        // +12 dB. My measurement (`measure_alias_table`, 2^15 points): minima
        // 29.4 dB at 2x and 53.0 dB at 4x. Where 4x and 2x both sit at the
        // f32 floor (alias under -130 dBc) the comparison is noise, so it is
        // skipped there. Above 20 kHz both share stage 1's transition band,
        // so the full band is held to the floors only.
        let n = 1 << 14;
        let warm = 4096;
        for fs in [44_100.0, 48_000.0] {
            for name in NAMES {
                for f in [1000.0, 5000.0, 10_000.0, 15_000.0] {
                    let (x, bin) = tone(f, fs, n, warm);
                    let naive = shape(name, AdaaOrder::Naive, 4.0, None, &x);
                    let (nf, na) = (alias_db(&naive, n, bin, fs, None), alias_db(&naive, n, bin, fs, Some(20_000.0)));
                    let lv = |c| {
                        let y = shape(name, AdaaOrder::Second, 4.0, Some(c), &x);
                        (alias_db(&y, n, bin, fs, None), alias_db(&y, n, bin, fs, Some(20_000.0)))
                    };
                    let ((y2f, y2a), (y4f, y4a)) = (lv(OversampleFactor::X2), lv(OversampleFactor::X4));
                    let (f2, a2, f4, a4) = (nf - y2f, na - y2a, nf - y4f, na - y4a);
                    eprintln!("{fs} {name} {f}: 2x {f2:.1}/{a2:.1} dB, 4x {f4:.1}/{a4:.1} dB (full/audio); 4x audio alias {y4a:.1} dBc");
                    assert!(f2 >= 26.0 && a2 >= 26.0, "{fs} {name} {f}: 2x {f2:.1}/{a2:.1}");
                    assert!(f4 >= 40.0 && a4 >= 40.0, "{fs} {name} {f}: 4x {f4:.1}/{a4:.1}");
                    assert!(a4 > a2 || y4a < -130.0, "{fs} {name} {f}: 4x {a4:.1} not above 2x {a2:.1} (audio band, 4x at {y4a:.1} dBc)");
                }
            }
        }
    }

    #[test]
    fn small_signals_pass_unchanged_apart_from_the_delay() {
        // The linear chain: an in-band sine through the oversampler with the
        // identity inside comes back as the input delayed by the latency.
        for f in [OversampleFactor::X2, OversampleFactor::X4] {
            let mut os = HalfBandOversampler::new(f);
            let lat = os.latency_samples();
            let fs = 44_100.0;
            let x: Vec<f32> = (0..8192).map(|i| (0.3 * (2.0 * std::f64::consts::PI * 997.0 * i as f64 / fs).sin()) as f32).collect();
            let mut y = x.clone();
            os.run(&mut y, &mut |_hi: &mut [f32]| {});
            let err = (lat + 300..8192).map(|i| (y[i] - x[i - lat]).abs()).fold(0.0f32, f32::max);
            assert!(err < 2e-5, "{f:?}: {err}");
        }
    }

    #[test]
    fn second_order_is_the_default_and_first_order_at_2x_is_routed() {
        use AdaaOrder::*;
        use OversampleFactor::*;
        // the routing table
        assert_eq!(route_order(First, X2, false), Second);
        assert_eq!(route_order(First, X4, false), First);
        assert_eq!(route_order(First, X2, true), First);
        assert_eq!(route_order(First, X4, true), First);
        for f in [X2, X4] {
            for d in [false, true] {
                assert_eq!(route_order(Second, f, d), Second);
                assert_eq!(route_order(Naive, f, d), Naive);
            }
        }
        // constructors
        for f in [X2, X4] {
            let a = AdaaOversampled::new(Tanh, 4.0, f);
            assert_eq!((a.order(), a.is_draft()), (Second, false));
            let d = AdaaOversampled::draft(Tanh, 4.0, f);
            assert_eq!((d.order(), d.is_draft()), (First, true));
        }
        let mut r = AdaaOversampled::with_order(HardClip, First, 4.0, X2);
        assert_eq!(r.order(), Second);
        assert_eq!(r.latency(), 111.0 + 0.5);
        assert_eq!(AdaaOversampled::with_order(HardClip, First, 4.0, X4).order(), First);
        r.set_order(First);
        assert_eq!(r.order(), Second);
        let mut d = AdaaOversampled::draft(HardClip, 4.0, X2);
        d.set_order(Second);
        assert_eq!(d.order(), Second);
        d.set_order(First);
        assert_eq!(d.order(), First);
        // Case 1: ADAA1 requested at 2x runs as ADAA2 (renders exactly the
        // 2x default, and differs from a real first-order 2x render).
        let (x, _) = tone(10_000.0, 44_100.0, 4096, 0);
        let render = |a: &mut AdaaOversampled<SoftClip3>| {
            let mut y = x.clone();
            a.process_block(&mut y);
            y
        };
        let asked1_2x = render(&mut AdaaOversampled::with_order(SoftClip3, First, 4.0, X2));
        assert_eq!(asked1_2x, render(&mut AdaaOversampled::new(SoftClip3, 4.0, X2)));
        assert_ne!(asked1_2x, render(&mut AdaaOversampled::draft(SoftClip3, 4.0, X2)));
        // Case 2: ADAA1 requested at 4x stays ADAA1, untouched: same order,
        // same latency, and bit-identical to the plain first-order shaper in
        // the same 4x oversampler.
        let mut a4 = AdaaOversampled::with_order(SoftClip3, First, 4.0, X4);
        assert_eq!((a4.order(), a4.is_draft()), (First, false));
        assert_eq!(a4.latency(), 118.0 + 0.5 / 4.0);
        let asked1_4x = render(&mut a4);
        let mut plain = x.clone();
        Waveshaper::new(SoftClip3, First, 4.0).process_with(&mut HalfBandOversampler::new(X4), &mut plain);
        assert_eq!(asked1_4x, plain);
        assert_ne!(asked1_4x, render(&mut AdaaOversampled::new(SoftClip3, 4.0, X4)));
    }

    /// FNV-1a over the bytes of `v`.
    fn fnv(v: &[f32]) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for x in v {
            for b in x.to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        h
    }

    #[test]
    fn default_path_is_unchanged() {
        // The 1x shapers (what exists on develop) are untouched: their
        // output on a fixed signal hashes to the value develop 511785f
        // produces, and running them through `NoOversampling` is bit
        // identical. Nothing in the engine calls this module, so every
        // default render is byte-identical (checked by the golden gates).
        let x: Vec<f32> = (0..4096).map(|i| (1.7 * (i as f64 * 0.0731).sin() + 0.4 * (i as f64 * 0.91).sin()) as f32).collect();
        let mut all = Vec::new();
        for name in NAMES {
            for order in [AdaaOrder::Naive, AdaaOrder::First, AdaaOrder::Second] {
                let y = shape(name, order, 3.0, None, &x);
                let mut z = x.clone();
                match name {
                    "hard" => Waveshaper::new(HardClip, order, 3.0).process_with(&mut NoOversampling, &mut z),
                    "tanh" => Waveshaper::new(Tanh, order, 3.0).process_with(&mut NoOversampling, &mut z),
                    "soft3" => Waveshaper::new(SoftClip3, order, 3.0).process_with(&mut NoOversampling, &mut z),
                    _ => Waveshaper::new(SoftClip5, order, 3.0).process_with(&mut NoOversampling, &mut z),
                }
                assert_eq!(y, z);
                all.extend_from_slice(&y);
            }
        }
        assert_eq!(fnv(&all), DEVELOP_1X_HASH, "1x shaper output changed: {:#018x}", fnv(&all));
    }


    /// Exponential sweep `f1 -> f2` over `secs`, unit amplitude, and its
    /// instantaneous frequency at sample `i`.
    fn sweep(f1: f64, f2: f64, secs: f64, fs: f64) -> (Vec<f32>, impl Fn(f64) -> f64) {
        let n = (secs * fs) as usize;
        let l = secs / (f2 / f1).ln();
        let x = (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                (2.0 * std::f64::consts::PI * f1 * l * ((t / l).exp() - 1.0)).sin() as f32
            })
            .collect();
        (x, move |i: f64| f1 * (i / fs / l).exp())
    }

    /// Sweep alias ratio (dB): Blackman-Harris STFT (4096, hop 2048); in each
    /// frame whose fundamental is 500 Hz..f_stop, every bin farther than 8
    /// bins from the band any harmonic sweeps through is alias. Summed alias
    /// energy over summed fundamental energy, up to `f_max_hz`.
    fn sweep_alias_db(y: &[f32], inst: &dyn Fn(f64) -> f64, fs: f64, lat: f64, f_stop: f64, f_max_hz: f64) -> f64 {
        let w = 4096;
        let win: Vec<f64> = (0..w)
            .map(|i| {
                let a = 2.0 * std::f64::consts::PI * i as f64 / w as f64;
                0.35875 - 0.48829 * a.cos() + 0.14128 * (2.0 * a).cos() - 0.01168 * (3.0 * a).cos()
            })
            .collect();
        let bw = fs / w as f64;
        let top = ((f_max_hz / bw).floor() as usize).min(w / 2);
        let (mut al, mut fu) = (0.0, 0.0);
        let mut s = 0;
        while s + w <= y.len() {
            let fa = inst(s as f64 - lat);
            let fb = inst((s + w) as f64 - lat);
            if fa >= 500.0 && fb <= f_stop {
                let mut re: Vec<f64> = (0..w).map(|i| y[s + i] as f64 * win[i]).collect();
                let mut im = vec![0.0; w];
                fft64(&mut re, &mut im);
                let mut harm = vec![false; w / 2 + 1];
                let mut k = 1.0;
                while k * fa <= fs / 2.0 {
                    let lo = ((k * fa / bw).floor() as isize - 8).max(0) as usize;
                    let hi = ((k * fb / bw).ceil() as usize + 8).min(w / 2);
                    for h in harm.iter_mut().take(hi + 1).skip(lo) {
                        *h = true;
                    }
                    if k == 1.0 {
                        fu += (lo..=hi).map(|b| re[b] * re[b] + im[b] * im[b]).sum::<f64>();
                    }
                    k += 1.0;
                }
                al += (2..=top).filter(|&b| !harm[b]).map(|b| re[b] * re[b] + im[b] * im[b]).sum::<f64>();
            }
            s += w / 2;
        }
        10.0 * (al / fu).log10()
    }

    /// The measurement table in the report. Run with
    /// `cargo test --release --lib measure_alias_table -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn measure_alias_table() {
        let n = 1 << 15;
        let cfgs: [(&str, Option<OversampleFactor>); 3] = [("1x", None), ("2x", Some(OversampleFactor::X2)), ("4x", Some(OversampleFactor::X4))];
        println!("TABLE kind,fs,curve,drive_db,order,signal,naive_full_db,naive_aud_db,cfg,supp_full_db,supp_aud_db");
        for fs in [44_100.0, 48_000.0] {
            for drive in [4.0, 10.0] {
                let ddb = 20.0 * f64::log10(drive);
                for name in NAMES {
                    for order in [AdaaOrder::First, AdaaOrder::Second] {
                        for f in [1000.0, 5000.0, 10_000.0, 15_000.0] {
                            let (x, bin) = tone(f, fs, n, 4096);
                            let naive = shape(name, AdaaOrder::Naive, drive, None, &x);
                            let nf = alias_db(&naive, n, bin, fs, None);
                            let na = alias_db(&naive, n, bin, fs, Some(20_000.0));
                            for (cn, c) in cfgs {
                                let y = shape(name, order, drive, c, &x);
                                println!("TABLE tone,{fs},{name},{ddb:.0},{order:?},{f},{nf:.2},{na:.2},{cn},{:.2},{:.2}", nf - alias_db(&y, n, bin, fs, None), na - alias_db(&y, n, bin, fs, Some(20_000.0)));
                            }
                        }
                        // sweep 200 Hz .. 18 kHz over 8 s
                        let f_stop = 18_000.0;
                        let (x, inst) = sweep(200.0, f_stop, 8.0, fs);
                        let naive = shape(name, AdaaOrder::Naive, drive, None, &x);
                        let nf = sweep_alias_db(&naive, &inst, fs, 0.0, f_stop, fs / 2.0);
                        let na = sweep_alias_db(&naive, &inst, fs, 0.0, f_stop, 20_000.0);
                        for (cn, c) in cfgs {
                            let y = shape(name, order, drive, c, &x);
                            let lat = match c {
                                None => 0.0,
                                Some(f) => HalfBandOversampler::new(f).latency(),
                            };
                            let yf = sweep_alias_db(&y, &inst, fs, lat, f_stop, fs / 2.0);
                            let ya = sweep_alias_db(&y, &inst, fs, lat, f_stop, 20_000.0);
                            println!("TABLE sweep,{fs},{name},{ddb:.0},{order:?},sweep,{nf:.2},{na:.2},{cn},{:.2},{:.2}", nf - yf, na - ya);
                        }
                    }
                }
            }
        }
    }

    /// CPU cost per base-rate sample. Run with
    /// `cargo test --release --lib measure_cpu -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn measure_cpu() {
        let fs = 48_000.0;
        let x: Vec<f32> = (0..(fs as usize) * 2).map(|i| (0.9 * (2.0 * std::f64::consts::PI * 441.0 * i as f64 / fs).sin() + 0.3 * (i as f64 * 0.37).sin()) as f32).collect();
        let time = |f: &mut dyn FnMut(&mut [f32])| {
            let mut best = f64::MAX;
            for _ in 0..7 {
                let mut b = x.clone();
                let t = std::time::Instant::now();
                for chunk in b.chunks_mut(256) {
                    f(chunk);
                }
                best = best.min(t.elapsed().as_secs_f64());
                std::hint::black_box(&b);
            }
            best * 1e9 / x.len() as f64
        };
        for f in [OversampleFactor::X2, OversampleFactor::X4] {
            let mut os = HalfBandOversampler::new(f);
            let ns = time(&mut |c: &mut [f32]| os.run(c, &mut |_h: &mut [f32]| {}));
            println!("CPU oversampler_only,{f:?},{ns:.1}");
        }
        macro_rules! bench {
            ($nl:expr, $name:expr) => {
                for order in [AdaaOrder::Naive, AdaaOrder::First, AdaaOrder::Second] {
                    let mut ws = Waveshaper::new($nl, order, 4.0);
                    let base = time(&mut |c: &mut [f32]| ws.process_block(c));
                    println!("CPU {},{order:?},1x,{base:.1}", $name);
                    for f in [OversampleFactor::X2, OversampleFactor::X4] {
                        let mut a = if order == AdaaOrder::First { AdaaOversampled::draft($nl, 4.0, f) } else { AdaaOversampled::with_order($nl, order, 4.0, f) };
                        let ns = time(&mut |c: &mut [f32]| a.process_block(c));
                        println!("CPU {},{order:?},{f:?},{ns:.1}", $name);
                    }
                }
            };
        }
        bench!(HardClip, "hard");
        bench!(Tanh, "tanh");
        bench!(SoftClip3, "soft3");
        bench!(SoftClip5, "soft5");
    }

    /// `fnv` of `default_path_is_unchanged`'s 1x outputs on develop 511785f.
    const DEVELOP_1X_HASH: u64 = 0x5bc4_d736_cef8_8fa3;
}
