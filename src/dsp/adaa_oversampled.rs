//! Oversampled ADAA: any [`Nonlinearity`] run at 1x, 2x (the default) or
//! 4x through the half-band oversampler in [`crate::oversample`].
//!
//! ADAA alone (1x) removes the aliases of harmonics far above Nyquist but is
//! weak on the ones just above it. Running it at 2x moves Nyquist up and
//! lets the oversampler's decimation filter take those out, so the two
//! combine: [`OversampledShaper::new`] gives 2x, and
//! [`OversampledShaper::with_oversampling`] picks 1x or 4x.
//!
//! What 2x and 4x buy, measured at 44.1 kHz on a 1 kHz sine against the
//! naive shaper at 1x (full table: `docs/ADAA_OVERSAMPLING.md`, produced by
//! `cargo run --release --example adaa_oversampled_bench`):
//!
//! - aliases below 20 kHz: second-order ADAA at 2x is 43 to 64 dB better
//!   (hard clip 43.7 / 43.0 / 43.9 dB at +6 / +12 / +24 dB drive), and 4x
//!   better again (hard clip ADAA-2: 70 to 75 dB);
//! - total alias power up to Nyquist: 2x and 4x both stop at about 22 to
//!   27 dB better for the hard clip, because of the transition floor
//!   described on the [`OversampleHook`] impl below. 4x does not lift that
//!   floor (it comes out 1.4 to 1.6 dB worse than 2x there); it shares the
//!   2x stage that sets it.
//!
//! Real-time safe: [`OversampledShaper::process_block`] never allocates.
//! Every buffer is reserved at construction (for [`DEFAULT_MAX_BLOCK`]
//! samples) or by [`OversampledShaper::prepare`], and a longer block is run
//! in pieces of at most that size, which gives the same output (the
//! processing does not depend on block size). Construction, `prepare` and
//! `set_oversampling` do allocate; call them off the audio thread.
//!
//! Opt-in only: nothing in the engine uses this yet, so no existing render
//! changes.

use super::adaa::{AdaaOrder, Nonlinearity, OversampleHook, Waveshaper};
use crate::oversample::Oversampler;

/// The half-band oversampler as an ADAA hook: a 2x or 4x round trip with
/// an exact integer latency (48 base-rate samples at 2x, 56 at 4x, 0 at 1x).
///
/// Transition floor. The first 2x stage (shared by 2x and 4x) passes up to
/// 0.43 fs and stops from 0.57 fs, with fs the base rate: 20.6 to 27.4 kHz
/// at 48 kHz, 19.0 to 25.1 kHz at the engine's 44.1 kHz. Harmonics the
/// shaper creates inside that band are only partly removed before
/// decimation and fold back to just under Nyquist (about 21 kHz at
/// 44.1 kHz). For heavily driven low notes they, not the shaper, set the
/// total alias power at 2x and at 4x: on a 1 kHz sine at 44.1 kHz, ADAA-2
/// at 2x is 23.7 to 27.5 dB below the naive 1x shaper (hard clip, +24 /
/// +12 / +6 dB drive) and 4x is no better (1.4 to 1.6 dB worse). Counting
/// only aliases below 20 kHz, the same 2x path is 43 to 64 dB below naive
/// 1x across the four curves. 4x is for strict alias requirements on
/// high-frequency material (see the 5 kHz table in the bench docs) and for
/// the in-band figure; it does not
/// fix the near-Nyquist floor. The half-band taps are deliberately not
/// steepened (latency and cost).
impl OversampleHook for Oversampler {
    fn factor(&self) -> usize {
        Oversampler::factor(self)
    }
    fn latency(&self) -> f64 {
        self.latency_samples() as f64
    }
    fn run(&mut self, block: &mut [f32], f: &mut dyn FnMut(&mut [f32])) {
        self.process(block, |hi| f(hi))
    }
}

/// Rate an [`OversampledShaper`] runs its shaper at. 2x is the default;
/// 4x is for strict alias requirements (see the floor note on the
/// [`OversampleHook`] impl for [`Oversampler`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Oversampling {
    /// Base rate: the shaper alone, no oversampler latency.
    X1,
    /// 2x, one half-band stage (48 base-rate samples of round trip).
    #[default]
    X2,
    /// 4x, two cascaded half-band stages (56 base-rate samples).
    X4,
}

impl Oversampling {
    /// The rate multiple: 1, 2 or 4.
    pub fn factor(self) -> usize {
        match self {
            Oversampling::X1 => 1,
            Oversampling::X2 => 2,
            Oversampling::X4 => 4,
        }
    }

    /// Group delay, in base-rate samples, of a shaper with ADAA `order` run
    /// at this rate: the oversampler's round trip plus the order's own delay
    /// (0, 0.5 or 1 high-rate samples) divided by the factor. Exact, and the
    /// same as [`OversampledShaper::latency`]: 0 / 0.5 / 1 at 1x,
    /// 48 / 48.25 / 48.5 at 2x, 56 / 56.125 / 56.25 at 4x (naive / first /
    /// second order).
    pub fn latency(self, order: AdaaOrder) -> f64 {
        let ws = Waveshaper::new(super::adaa::HardClip, order, 1.0);
        ws.latency_with(&Oversampler::new(self.factor()))
    }
}

/// Block size, in base-rate samples, an [`OversampledShaper`] reserves for
/// at construction. Longer blocks still work without allocating; they are
/// processed in pieces of this size.
pub const DEFAULT_MAX_BLOCK: usize = 2048;

/// A mono waveshaper with optional ADAA, run inside its own oversampler.
/// One per channel. Defaults to 2x. Processing never allocates (see the
/// module docs).
#[derive(Clone, Debug)]
pub struct OversampledShaper<N: Nonlinearity> {
    /// The shaper, running at the high rate. Its `drive`, `order` and
    /// `out_gain` may change between blocks (an order change changes
    /// [`Self::latency`]); after changing `shape`, call [`Self::reset`].
    pub shaper: Waveshaper<N>,
    os: Oversampler,
    mode: Oversampling,
    max_block: usize,
}

impl<N: Nonlinearity> OversampledShaper<N> {
    /// `shape` with ADAA `order` and input `drive`, at 2x.
    pub fn new(shape: N, order: AdaaOrder, drive: f64) -> Self {
        Self::with_oversampling(shape, order, drive, Oversampling::X2)
    }

    /// Same as [`Self::new`] at the given rate.
    pub fn with_oversampling(shape: N, order: AdaaOrder, drive: f64, mode: Oversampling) -> Self {
        let mut os = Oversampler::new(mode.factor());
        os.prepare(DEFAULT_MAX_BLOCK);
        OversampledShaper { shaper: Waveshaper::new(shape, order, drive), os, mode, max_block: DEFAULT_MAX_BLOCK }
    }

    /// Reserves for blocks of up to `max_block` base-rate samples (at least
    /// 1), the size [`Self::process_block`] then works in. Allocates; call
    /// it off the audio thread. Output does not change.
    pub fn prepare(&mut self, max_block: usize) {
        self.max_block = max_block.max(1);
        self.os.prepare(self.max_block);
    }

    /// The block size processing works in (see [`Self::prepare`]).
    pub fn max_block(&self) -> usize {
        self.max_block
    }

    pub fn oversampling(&self) -> Oversampling {
        self.mode
    }

    /// Changes the rate. A change clears the oversampler and the shaper's
    /// history (both live at the old rate) and allocates the new
    /// oversampler, so call it off the audio thread; setting the current
    /// rate is a no-op.
    pub fn set_oversampling(&mut self, mode: Oversampling) {
        if mode != self.mode {
            self.mode = mode;
            self.os = Oversampler::new(mode.factor());
            self.os.prepare(self.max_block);
            self.shaper.reset();
        }
    }

    /// Clears the oversampler and the shaper history (as if the input had
    /// been 0).
    pub fn reset(&mut self) {
        self.os.reset();
        self.shaper.reset();
    }

    /// Exact group delay in base-rate samples, for delay compensation of a
    /// dry path: see [`Oversampling::latency`]. Fractional except for the
    /// naive order and for second order at 1x.
    pub fn latency(&self) -> f64 {
        self.shaper.latency_with(&self.os)
    }

    /// Shapes `block` in place at the chosen rate. Any block size; the
    /// output does not depend on how the stream is split into blocks. Never
    /// allocates: blocks longer than [`Self::max_block`] run in pieces.
    pub fn process_block(&mut self, block: &mut [f32]) {
        for piece in block.chunks_mut(self.max_block) {
            self.shaper.process_with(&mut self.os, piece);
        }
    }
}

/// Counts heap allocations per thread in this crate's unit-test binary, so
/// a test can prove a code path never allocates. Test builds only.
#[cfg(test)]
pub(crate) mod alloc_count {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static COUNT: Cell<u64> = const { Cell::new(0) };
    }

    struct Counting;

    fn bump() {
        let _ = COUNT.try_with(|c| c.set(c.get() + 1));
    }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            bump();
            System.alloc(layout)
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            bump();
            System.alloc_zeroed(layout)
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            bump();
            System.realloc(ptr, layout, new_size)
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            System.dealloc(ptr, layout)
        }
    }

    #[global_allocator]
    static ALLOC: Counting = Counting;

    /// Allocations (including reallocations) made on this thread so far.
    pub(crate) fn on_this_thread() -> u64 {
        COUNT.with(|c| c.get())
    }
}

#[cfg(test)]
mod tests {
    use super::super::adaa::{HardClip, SoftClip3, SoftClip5, Tanh};
    use super::*;
    use std::f64::consts::PI;

    const FS: f64 = 44_100.0;
    const MODES: [Oversampling; 3] = [Oversampling::X1, Oversampling::X2, Oversampling::X4];
    const ORDERS: [AdaaOrder; 3] = [AdaaOrder::Naive, AdaaOrder::First, AdaaOrder::Second];
    const NAMES: [&str; 4] = ["hard", "tanh", "soft3", "soft5"];

    /// Runs `input` through a fresh shaper, in blocks of `block`.
    fn run(name: &str, order: AdaaOrder, mode: Oversampling, drive: f64, input: &[f32], block: usize) -> Vec<f32> {
        fn go<N: Nonlinearity>(mut s: OversampledShaper<N>, input: &[f32], block: usize) -> Vec<f32> {
            let mut buf = input.to_vec();
            buf.chunks_mut(block).for_each(|c| s.process_block(c));
            buf
        }
        match name {
            "hard" => go(OversampledShaper::with_oversampling(HardClip, order, drive, mode), input, block),
            "tanh" => go(OversampledShaper::with_oversampling(Tanh, order, drive, mode), input, block),
            "soft3" => go(OversampledShaper::with_oversampling(SoftClip3, order, drive, mode), input, block),
            _ => go(OversampledShaper::with_oversampling(SoftClip5, order, drive, mode), input, block),
        }
    }

    fn noise(n: usize, seed: u64) -> Vec<f32> {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                ((s >> 11) as f64 / (1u64 << 52) as f64 - 1.0) as f32
            })
            .collect()
    }

    /// In-place radix-2 FFT in f64 (the f32 one in `analysis` is too coarse
    /// for the -100 dB figures checked here).
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
            let ang = -2.0 * PI / len as f64;
            for k in 0..len / 2 {
                let (wi, wr) = (ang * k as f64).sin_cos();
                for start in (0..n).step_by(len) {
                    let (a, b) = (start + k, start + k + len / 2);
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

    /// (total alias power, alias power below 20 kHz), both in dB relative
    /// to the fundamental at `bin`; harmonics of `bin` are not aliases.
    fn alias_db(y: &[f32], bin: usize) -> (f64, f64) {
        let n = y.len();
        let mut re: Vec<f64> = y.iter().map(|v| *v as f64).collect();
        let mut im = vec![0.0; n];
        fft64(&mut re, &mut im);
        let p = |k: usize| re[k] * re[k] + im[k] * im[k];
        let (mut tot, mut aud) = (0.0, 0.0);
        for k in (1..=n / 2).filter(|k| k % bin != 0) {
            tot += p(k);
            if k as f64 * FS / n as f64 <= 20_000.0 {
                aud += p(k);
            }
        }
        (10.0 * (tot / p(bin)).log10(), 10.0 * (aud / p(bin)).log10())
    }

    #[test]
    fn default_is_2x_and_latency_is_exact_and_consistent() {
        let s = OversampledShaper::new(Tanh, AdaaOrder::Second, 2.0);
        assert_eq!(s.oversampling(), Oversampling::X2);
        assert_eq!(Oversampling::default(), Oversampling::X2);
        assert_eq!(s.latency(), 48.5);
        let want = [[0.0, 0.5, 1.0], [48.0, 48.25, 48.5], [56.0, 56.125, 56.25]];
        for (m, mode) in MODES.iter().enumerate() {
            for (o, order) in ORDERS.iter().enumerate() {
                let s = OversampledShaper::with_oversampling(HardClip, *order, 1.0, *mode);
                assert_eq!(s.latency(), want[m][o], "{mode:?} {order:?}");
                assert_eq!(mode.latency(*order), want[m][o]);
                assert_eq!(s.latency(), s.shaper.latency_with(&Oversampler::new(mode.factor())));
            }
        }
    }

    #[test]
    fn group_delay_matches_latency_at_2x_and_4x() {
        // Hard clip at a level that stays inside |u| < 1 is exactly linear,
        // so the chain is linear phase. Phase delay of exact-bin sines
        // (whole cycles in the window), read by projection, must equal
        // latency() at every passband frequency.
        let n = 1 << 14;
        let warm = 1024;
        for mode in [Oversampling::X1, Oversampling::X2, Oversampling::X4] {
            for order in ORDERS {
                for bin in [37usize, 149, 1013, 4093] {
                    let w = 2.0 * PI * bin as f64 / n as f64;
                    let x: Vec<f32> = (0..n + warm).map(|i| (0.4 * (w * i as f64).sin()) as f32).collect();
                    let y = run("hard", order, mode, 0.5, &x, 333);
                    let (mut s, mut c) = (0.0, 0.0);
                    for i in warm..n + warm {
                        s += y[i] as f64 * (w * i as f64).sin();
                        c += y[i] as f64 * (w * i as f64).cos();
                    }
                    let delay = (-c).atan2(s) / w;
                    let want = OversampledShaper::with_oversampling(HardClip, order, 0.5, mode).latency();
                    // Phase is mod 2 pi: unwrap toward the expected value.
                    let period = 2.0 * PI / w;
                    let delay = delay + ((want - delay) / period).round() * period;
                    assert!((delay - want).abs() < 2e-3, "{mode:?} {order:?} bin {bin}: measured {delay:.5}, latency() {want}");
                }
            }
        }
    }

    #[test]
    fn dc_symmetry_and_finite_through_the_oversampled_path() {
        let x = noise(6000, 11);
        for mode in MODES {
            for order in ORDERS {
                for name in NAMES {
                    // Exact odd symmetry: the filters are linear and the shapes
                    // odd. Compared as values: an exact cancellation in the
                    // filter sums gives +0.0 for both signs of input.
                    let a = run(name, order, mode, 8.0, &x, 512);
                    let neg: Vec<f32> = x.iter().map(|v| -v).collect();
                    let b = run(name, order, mode, 8.0, &neg, 512);
                    assert!(a.iter().zip(&b).all(|(p, q)| *p == -*q), "{name} {mode:?} {order:?} not odd");
                    // Silence in, exact silence out.
                    assert!(run(name, order, mode, 8.0, &[0.0; 700], 256).iter().all(|v| *v == 0.0));
                    // DC holds settle to f(drive * c) (half-band DC gain is 1
                    // to within its ripple).
                    for c in [0.5f32, 1.0, 2.0, -0.3] {
                        let y = run(name, order, mode, 1.0, &vec![c; 1024], 128);
                        let want = match name {
                            "hard" => HardClip.f(c as f64),
                            "tanh" => Tanh.f(c as f64),
                            "soft3" => SoftClip3.f(c as f64),
                            _ => SoftClip5.f(c as f64),
                        };
                        assert!(y[200..].iter().all(|v| (*v as f64 - want).abs() < 5e-4), "{name} {mode:?} {order:?} DC {c}: {} vs {want}", y[1023]);
                    }
                    // Nothing non-finite: hard drive, steps, holds, huge and tiny inputs.
                    let mut wild: Vec<f32> = x.iter().map(|v| v * 3.0).collect();
                    wild.extend(std::iter::repeat(1.0e3).take(300));
                    wild.extend(std::iter::repeat(-1.0e3).take(300));
                    wild.extend((0..300).map(|i| if i % 2 == 0 { 1.0e-30 } else { -1.0e-30 }));
                    wild.extend(std::iter::repeat(0.75).take(300));
                    for drive in [1.0, 16.0, 1000.0] {
                        let y = run(name, order, mode, drive, &wild, 512);
                        assert!(y.iter().all(|v| v.is_finite()), "{name} {mode:?} {order:?} drive {drive}");
                    }
                }
            }
        }
    }

    #[test]
    fn output_does_not_depend_on_block_size_and_set_oversampling_resets() {
        let x = noise(5000, 5);
        for mode in MODES {
            let a = run("tanh", AdaaOrder::Second, mode, 4.0, &x, 512);
            let b = run("tanh", AdaaOrder::Second, mode, 4.0, &x, 37);
            assert!(a.iter().zip(&b).all(|(p, q)| p.to_bits() == q.to_bits()), "{mode:?}");
        }
        // Switching away and back starts clean, like a fresh 2x shaper.
        let mut s = OversampledShaper::new(Tanh, AdaaOrder::Second, 4.0);
        let mut junk = x.clone();
        s.process_block(&mut junk);
        s.set_oversampling(Oversampling::X4);
        assert_eq!(s.latency(), 56.25);
        s.set_oversampling(Oversampling::X2);
        let mut y = x.clone();
        s.process_block(&mut y);
        let fresh = run("tanh", AdaaOrder::Second, Oversampling::X2, 4.0, &x, x.len());
        assert!(y.iter().zip(&fresh).all(|(p, q)| p.to_bits() == q.to_bits()));
    }

    #[test]
    fn one_x_is_the_plain_shaper_bit_for_bit() {
        let x = noise(3000, 2);
        for order in ORDERS {
            let mut plain = x.clone();
            Waveshaper::new(SoftClip5, order, 5.0).process_block(&mut plain);
            let os = run("soft5", order, Oversampling::X1, 5.0, &x, 512);
            assert!(plain.iter().zip(&os).all(|(p, q)| p.to_bits() == q.to_bits()), "{order:?}");
        }
    }

    /// Runs `input` as one `process_block` call through a fresh shaper
    /// prepared for `prep` (`None`: the default `DEFAULT_MAX_BLOCK`).
    fn run_prepared(name: &str, order: AdaaOrder, mode: Oversampling, drive: f64, input: &[f32], prep: Option<usize>) -> Vec<f32> {
        fn go<N: Nonlinearity>(mut s: OversampledShaper<N>, input: &[f32], prep: Option<usize>) -> Vec<f32> {
            if let Some(m) = prep {
                s.prepare(m);
                assert_eq!(s.max_block(), m);
            }
            let mut buf = input.to_vec();
            s.process_block(&mut buf);
            buf
        }
        match name {
            "hard" => go(OversampledShaper::with_oversampling(HardClip, order, drive, mode), input, prep),
            "tanh" => go(OversampledShaper::with_oversampling(Tanh, order, drive, mode), input, prep),
            "soft3" => go(OversampledShaper::with_oversampling(SoftClip3, order, drive, mode), input, prep),
            _ => go(OversampledShaper::with_oversampling(SoftClip5, order, drive, mode), input, prep),
        }
    }

    #[test]
    fn processing_never_allocates() {
        // Counted by the test allocator on this thread: after construction,
        // prepare and set_oversampling (all outside the counted region),
        // process_block makes no heap allocation for any curve, rate or
        // order, for blocks shorter than, equal to and longer than
        // max_block, and across reset().
        use super::alloc_count::on_this_thread;
        let x = noise(9000, 21);
        // The counter is live: an oversampler that was never prepared grows
        // its buffers on first use.
        let mut raw = Oversampler::new(2);
        let (mut probe, before) = (x[..64].to_vec(), on_this_thread());
        raw.process(&mut probe, |_| {});
        assert!(on_this_thread() > before, "allocation counter saw nothing");

        fn check<N: Nonlinearity>(name: &str, mk: impl Fn(AdaaOrder, Oversampling) -> OversampledShaper<N>, x: &[f32]) {
            let other = |m: Oversampling| if m == Oversampling::X4 { Oversampling::X1 } else { Oversampling::X4 };
            for mode in MODES {
                for order in ORDERS {
                    // Setup, not counted: default capacity; a smaller
                    // prepare; and a rate round trip away and back.
                    let mut a = mk(order, mode);
                    let mut b = mk(order, mode);
                    b.prepare(256);
                    let mut c = mk(order, mode);
                    c.set_oversampling(other(mode));
                    c.set_oversampling(mode);
                    let mut buf = x.to_vec();
                    let before = on_this_thread();
                    for (lo, hi) in [(0, 1), (1, 38), (38, 550), (550, 2598), (2598, 9000)] {
                        a.process_block(&mut buf[lo..hi]);
                        b.process_block(&mut buf[lo..hi]);
                        c.process_block(&mut buf[lo..hi]);
                    }
                    a.reset();
                    a.process_block(&mut buf[..DEFAULT_MAX_BLOCK]);
                    let made = on_this_thread() - before;
                    assert_eq!(made, 0, "{name} {mode:?} {order:?}: {made} allocations while processing");
                    assert!(buf.iter().all(|v| v.is_finite()), "{name} {mode:?} {order:?}");
                }
            }
        }
        check("hard", |o, m| OversampledShaper::with_oversampling(HardClip, o, 6.0, m), &x);
        check("tanh", |o, m| OversampledShaper::with_oversampling(Tanh, o, 6.0, m), &x);
        check("soft3", |o, m| OversampledShaper::with_oversampling(SoftClip3, o, 6.0, m), &x);
        check("soft5", |o, m| OversampledShaper::with_oversampling(SoftClip5, o, 6.0, m), &x);
    }

    #[test]
    fn prepare_and_piecewise_processing_do_not_change_output() {
        // Reference: the whole 7000-sample signal as ONE piece (prepared for
        // its full length). Compared bit for bit with the default capacity
        // (2048: pieces of 2048) and prepare(100) (pieces of 100), both fed
        // a block longer than their capacity, for every rate and ADAA order.
        let x = noise(7000, 8);
        for mode in MODES {
            for order in [AdaaOrder::First, AdaaOrder::Second] {
                for name in ["soft3", "tanh"] {
                    let whole = run_prepared(name, order, mode, 5.0, &x, Some(x.len()));
                    for prep in [None, Some(100)] {
                        let y = run_prepared(name, order, mode, 5.0, &x, prep);
                        assert!(y.iter().zip(&whole).all(|(p, q)| p.to_bits() == q.to_bits()), "{name} {mode:?} {order:?} prep {prep:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn alias_figures_match_the_bench() {
        // 1 kHz (bin 1487 of 65536 at 44.1 kHz), unit sine, blocks of 512,
        // 8192 samples of warm-up: the numbers in docs/ADAA_OVERSAMPLING.md.
        // (curve, drive dB, mode, order, total dB, below-20-kHz dB)
        let cases: [(&str, f64, Oversampling, AdaaOrder, f64, f64); 7] = [
            ("hard", 24.0, Oversampling::X1, AdaaOrder::Naive, -25.1, -26.5),
            ("hard", 24.0, Oversampling::X1, AdaaOrder::Second, -36.6, -40.1),
            ("hard", 24.0, Oversampling::X2, AdaaOrder::Second, -48.8, -70.5),
            ("hard", 24.0, Oversampling::X4, AdaaOrder::Second, -47.3, -101.2),
            ("tanh", 24.0, Oversampling::X2, AdaaOrder::Second, -52.1, -95.0),
            ("soft3", 12.0, Oversampling::X2, AdaaOrder::First, -66.1, -88.1),
            ("soft5", 24.0, Oversampling::X2, AdaaOrder::Second, -46.8, -72.8),
        ];
        let (n, warm, bin) = (1usize << 16, 8192usize, 1487usize);
        let x: Vec<f32> = (0..n + warm).map(|i| (2.0 * PI * bin as f64 * i as f64 / n as f64).sin() as f32).collect();
        for (name, drive_db, mode, order, tot, aud) in cases {
            let drive = 10f64.powf(drive_db / 20.0);
            let y = run(name, order, mode, drive, &x, 512);
            let (t, a) = alias_db(&y[warm..], bin);
            assert!((t - tot).abs() < 0.3 && (a - aud).abs() < 0.3, "{name} +{drive_db} {mode:?} {order:?}: total {t:.2} (bench {tot}), <20k {a:.2} (bench {aud})");
        }
    }
}
