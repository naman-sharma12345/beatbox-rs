//! Antiderivative anti-aliasing (ADAA) for memoryless waveshapers: a hard
//! clipper, `tanh`, a cubic and a C2 quintic soft clipper, each at first
//! and second order, plus a small hook for running any of them inside an oversampled
//! block.
//!
//! Written from the published math (no library code was used):
//! - First order: J. D. Parker, V. Zavalishin, E. Le Bivic, "Reducing the
//!   aliasing of nonlinear waveshaping using continuous-time convolution",
//!   Proc. DAFx-16, 2016.
//! - Second order: S. Bilbao, F. Esqueda, J. D. Parker, V. Välimäki,
//!   "Antiderivative antialiasing for memoryless nonlinearities", IEEE
//!   Signal Processing Letters 24(7), 2017.
//!
//! How it works. A naive shaper computes `y[n] = f(x[n])`, and every
//! harmonic `f` creates above Nyquist folds back as an alias. ADAA treats
//! the input as a continuous, piecewise-linear signal and averages `f`
//! over each segment instead of sampling it at the end points. With `F1`
//! the antiderivative of `f` and `F2` the antiderivative of `F1`:
//!
//! - first order: `y[n] = (F1(x[n]) - F1(x[n-1])) / (x[n] - x[n-1])`,
//!   which is `f` averaged over the segment (half a sample of delay);
//! - second order: `y[n] = 2 / (x[n] - x[n-2]) * (D(x[n], x[n-1]) -
//!   D(x[n-1], x[n-2]))` with `D(a, b) = (F2(a) - F2(b)) / (a - b)`
//!   (one sample of delay).
//!
//! Both are divided differences, so they lose precision (and eventually
//! divide by zero) as neighbouring inputs get close, so every quotient has
//! a guard that switches to its exact limit:
//! - first order, `|x[n] - x[n-1]| < ADAA_EPS` (1e-5): `f` at the segment
//!   midpoint;
//! - second order, an adjacent step under `ADAA_EPS`: that step's `D` takes
//!   its limit `F1` at the midpoint, inside the same formula (not a switch
//!   to first order, which would jump half a sample in delay);
//! - second order, the wide step `|x[n] - x[n-2]| < ADAA2_WIDE_EPS` (2e-5):
//!   the `x[n] == x[n-2]` limit `2 / (c - b) * (F1(c) - D(c, b))`, with
//!   `c` the mean of `x[n]` and `x[n-2]` and `b = x[n-1]`;
//! - both adjacent steps collapsed (a DC hold): `f` at the mean of the
//!   three inputs, which is exactly `f(x)` on a held value. Everything runs in `f64`, and each shape
//!   gives closed-form divided differences on its polynomial pieces (and on
//!   `tanh`'s asymptotic part), so the large values of `F2` at high drive
//!   never cancel against each other.
//!
//! The shapes are odd functions and the formulas preserve that exactly:
//! `-x` in gives bit-for-bit `-y` out, and a constant input gives `f(c)`.
//!
//! Opt-in only: nothing in the engine uses this yet, so no existing render
//! changes.

/// Gap between neighbouring (drive-scaled) inputs below which the divided
/// differences fall back to their limits.
pub const ADAA_EPS: f64 = 1e-5;

/// Second order's wide-step guard: `|x[n] - x[n-2]|` below this takes the
/// `x[n] == x[n-2]` limit.
pub const ADAA2_WIDE_EPS: f64 = 2.0 * ADAA_EPS;

/// A memoryless nonlinearity with its first two antiderivatives, both
/// pinned to 0 at `x = 0`.
pub trait Nonlinearity {
    /// The shaping curve.
    fn f(&self, x: f64) -> f64;
    /// `F1`, with `F1' = f` and `F1(0) = 0`.
    fn f1(&self, x: f64) -> f64;
    /// `F2`, with `F2' = F1` and `F2(0) = 0`.
    fn f2(&self, x: f64) -> f64;
    /// `(F1(a) - F1(b)) / (a - b)` for `a != b`. Shapes override it with a
    /// closed form where the plain quotient would cancel.
    fn dd1(&self, a: f64, b: f64) -> f64 {
        (self.f1(a) - self.f1(b)) / (a - b)
    }
    /// `(F2(a) - F2(b)) / (a - b)` for `a != b`, overridden like [`Self::dd1`].
    fn dd2(&self, a: f64, b: f64) -> f64 {
        (self.f2(a) - self.f2(b)) / (a - b)
    }
    /// A per-point value [`Self::dd1_aux`] can reuse (for `tanh`,
    /// `ln(1 + e^(-2|x|))`). The streaming shaper computes it once per
    /// sample and hands it back on the next sample as the old point's.
    /// Default: unused.
    fn aux1(&self, _x: f64) -> f64 {
        0.0
    }
    /// [`Self::dd1`] given `aux1(a)` and `aux1(b)`; must equal `dd1(a, b)`
    /// bit for bit.
    fn dd1_aux(&self, a: f64, _aux_a: f64, b: f64, _aux_b: f64) -> f64 {
        self.dd1(a, b)
    }
    /// The same for second order: a per-point value [`Self::dd2_aux`] can
    /// reuse (for `tanh`, the non-polynomial part of `F2`).
    fn aux2(&self, _x: f64) -> f64 {
        0.0
    }
    /// [`Self::dd2`] given `aux2(a)` and `aux2(b)`; must equal `dd2(a, b)`
    /// bit for bit.
    fn dd2_aux(&self, a: f64, _aux_a: f64, b: f64, _aux_b: f64) -> f64 {
        self.dd2(a, b)
    }
}

/// `(a^(n+1) - b^(n+1)) / (a - b)`, the sum of `a^(n-k) b^k` for `k` in `0..=n`.
fn hsum(a: f64, b: f64, n: u32) -> f64 {
    // Horner in a: s = s * a + b^k for k = 0..=n.
    let mut s = 0.0;
    let mut bk = 1.0;
    for _ in 0..=n {
        s = s * a + bk;
        bk *= b;
    }
    s
}

/// Which saturated tail of a unit clipper both points sit on: `Some(1.0)`
/// when both are `> 1`, `Some(-1.0)` when both are `< -1`.
fn same_tail(a: f64, b: f64) -> Option<f64> {
    if a > 1.0 && b > 1.0 {
        Some(1.0)
    } else if a < -1.0 && b < -1.0 {
        Some(-1.0)
    } else {
        None
    }
}

fn in_core(a: f64, b: f64) -> bool {
    a.abs() <= 1.0 && b.abs() <= 1.0
}

/// Hard clipper at +-1: `f(x) = clamp(x, -1, 1)`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HardClip;

impl Nonlinearity for HardClip {
    fn f(&self, x: f64) -> f64 {
        x.clamp(-1.0, 1.0)
    }
    fn f1(&self, x: f64) -> f64 {
        let a = x.abs();
        if a <= 1.0 {
            0.5 * x * x
        } else {
            a - 0.5
        }
    }
    fn f2(&self, x: f64) -> f64 {
        let a = x.abs();
        if a <= 1.0 {
            x * x * x / 6.0
        } else {
            (0.5 * a * a - 0.5 * a + 1.0 / 6.0).copysign(x)
        }
    }
    fn dd1(&self, a: f64, b: f64) -> f64 {
        if let Some(s) = same_tail(a, b) {
            s
        } else if in_core(a, b) {
            0.5 * (a + b)
        } else {
            (self.f1(a) - self.f1(b)) / (a - b)
        }
    }
    fn dd2(&self, a: f64, b: f64) -> f64 {
        if let Some(s) = same_tail(a, b) {
            s * 0.5 * (a + b) - 0.5
        } else if in_core(a, b) {
            hsum(a, b, 2) / 6.0
        } else {
            (self.f2(a) - self.f2(b)) / (a - b)
        }
    }
}

/// C2 quintic soft clipper: `f(x) = (15x - 10x^3 + 3x^5) / 8` on `[-1, 1]`
/// and `sign(x)` outside. Slope 15/8 at 0, and slope and curvature both
/// reach 0 at +-1, so it joins the flat tails smoothly.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SoftClip5;

impl Nonlinearity for SoftClip5 {
    fn f(&self, x: f64) -> f64 {
        if x.abs() >= 1.0 {
            1.0f64.copysign(x)
        } else {
            let x2 = x * x;
            x * (15.0 - x2 * (10.0 - 3.0 * x2)) / 8.0
        }
    }
    fn f1(&self, x: f64) -> f64 {
        let a = x.abs();
        if a <= 1.0 {
            let x2 = x * x;
            x2 * (7.5 - x2 * (2.5 - 0.5 * x2)) / 8.0
        } else {
            a - 0.3125
        }
    }
    fn f2(&self, x: f64) -> f64 {
        let a = x.abs();
        if a <= 1.0 {
            let x2 = x * x;
            x * x2 * (2.5 - x2 * (0.5 - x2 / 14.0)) / 8.0
        } else {
            (0.5 * a * a - 0.3125 * a + 1.0 / 14.0).copysign(x)
        }
    }
    fn dd1(&self, a: f64, b: f64) -> f64 {
        if let Some(s) = same_tail(a, b) {
            s
        } else if in_core(a, b) {
            (7.5 * hsum(a, b, 1) - 2.5 * hsum(a, b, 3) + 0.5 * hsum(a, b, 5)) / 8.0
        } else {
            (self.f1(a) - self.f1(b)) / (a - b)
        }
    }
    fn dd2(&self, a: f64, b: f64) -> f64 {
        if let Some(s) = same_tail(a, b) {
            s * 0.5 * (a + b) - 0.3125
        } else if in_core(a, b) {
            (2.5 * hsum(a, b, 2) - 0.5 * hsum(a, b, 4) + hsum(a, b, 6) / 14.0) / 8.0
        } else {
            (self.f2(a) - self.f2(b)) / (a - b)
        }
    }
}

/// Cubic soft clipper: `f(x) = x - x^3/3` on `[-1, 1]`, held at `+-2/3`
/// outside (slope 1 at 0, slope 0 at +-1, so C1). Algebraic throughout:
/// `F1 = x^2/2 - x^4/12` and `F2 = x^3/6 - x^5/60` in the core, quadratic
/// and linear tails outside. Scale `out_gain` by 1.5 for a unit ceiling.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SoftClip3;

impl Nonlinearity for SoftClip3 {
    fn f(&self, x: f64) -> f64 {
        if x.abs() >= 1.0 {
            (2.0f64 / 3.0).copysign(x)
        } else {
            x - x * x * x / 3.0
        }
    }
    fn f1(&self, x: f64) -> f64 {
        let a = x.abs();
        if a <= 1.0 {
            let x2 = x * x;
            x2 * (0.5 - x2 / 12.0)
        } else {
            2.0 / 3.0 * a - 0.25
        }
    }
    fn f2(&self, x: f64) -> f64 {
        let a = x.abs();
        if a <= 1.0 {
            let x2 = x * x;
            x * x2 * (1.0 / 6.0 - x2 / 60.0)
        } else {
            (a * a / 3.0 - 0.25 * a + 1.0 / 15.0).copysign(x)
        }
    }
    fn dd1(&self, a: f64, b: f64) -> f64 {
        if let Some(s) = same_tail(a, b) {
            s * 2.0 / 3.0
        } else if in_core(a, b) {
            0.5 * hsum(a, b, 1) - hsum(a, b, 3) / 12.0
        } else {
            (self.f1(a) - self.f1(b)) / (a - b)
        }
    }
    fn dd2(&self, a: f64, b: f64) -> f64 {
        if let Some(s) = same_tail(a, b) {
            s * (a + b) / 3.0 - 0.25
        } else if in_core(a, b) {
            hsum(a, b, 2) / 6.0 - hsum(a, b, 4) / 60.0
        } else {
            (self.f2(a) - self.f2(b)) / (a - b)
        }
    }
}

/// `tanh` saturator. `F1(x) = ln cosh x`, computed without `cosh` (so it
/// cannot overflow at any `x`): `ln(1 + 2 sinh^2(x/2))` below `|x| = 0.25`,
/// `|x| - ln 2 + t(|x|)` with `t(u) = ln(1 + e^(-2u))` up to
/// [`TANH_ASYMPTOTE`] (15), and the bare asymptote `|x| - ln 2` past it. The
/// step at 15 is `e^-30` (~1e-13), which a first-order quotient over the
/// smallest gap turns into < 1e-8.
/// `F2(x) = sign(x) * (x^2/2 - |x| ln 2 + R(|x|))` with
/// `R(u) = pi^2/24 + Li2(-e^(-2u)) / 2`, which runs from 0 at `u = 0` to
/// `pi^2/24` as `u` grows (and a Taylor series near 0). `R` is evaluated
/// from `t` as well, so the streaming shaper computes one `exp` and one
/// `ln_1p` per sample (see [`Nonlinearity::aux1`] and
/// [`Nonlinearity::aux2`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tanh;

const LN2: f64 = std::f64::consts::LN_2;
const PI2_24: f64 = std::f64::consts::PI * std::f64::consts::PI / 24.0;
/// Below this `|x|`, `tanh`'s `F2` uses its Taylor series.
const TANH_SERIES: f64 = 0.125;

/// Past this `|x|`, `ln cosh x` is taken as its asymptote `|x| - ln 2`
/// (the dropped `ln(1 + e^(-2|x|))` is under 1e-13 there).
pub const TANH_ASYMPTOTE: f64 = 15.0;

/// Past this `|x|`, `t(u) = ln(1 + e^(-2u))` (< 5e-18) is taken as 0 in
/// `R`, where it is far below the resolution of `pi^2/24`.
const TANH_T_CUTOFF: f64 = 20.0;

/// `t(u) = ln(1 + e^(-2u))` for `u >= 0`, the part of `ln cosh` above its
/// asymptote; 0 past [`TANH_T_CUTOFF`].
fn tanh_t(u: f64) -> f64 {
    if u > TANH_T_CUTOFF {
        0.0
    } else {
        (-2.0 * u).exp().ln_1p()
    }
}

/// `t` as `F1` uses it: 0 past [`TANH_ASYMPTOTE`].
fn f1_tail(u: f64, t: f64) -> f64 {
    if u > TANH_ASYMPTOTE {
        0.0
    } else {
        t
    }
}

/// `Li2(z)` from `w = -ln(1 - z)`, by the Bernoulli series
/// `sum B_n w^(n+1) / (n+1)!`; for `z` in `[-1, 0]`, `|w| <= ln 2` and 9
/// terms reach ~1e-17.
fn li2_w(w: f64) -> f64 {
    // B_n / (n + 1)! for n = 2, 4, 6, ..., 16.
    const C: [f64; 8] = [
        1.0 / 36.0,
        -1.0 / 3600.0,
        1.0 / 211_680.0,
        -1.0 / 10_886_400.0,
        1.0 / 526_901_760.0,
        -691.0 / 2730.0 / 6_227_020_800.0,
        7.0 / 6.0 / 1_307_674_368_000.0,
        -3617.0 / 510.0 / 355_687_428_096_000.0,
    ];
    // Estrin's scheme in y = w^2 (shorter dependency chain than Horner).
    let y = w * w;
    let y2 = y * y;
    let y4 = y2 * y2;
    let lo = (C[0] + C[1] * y) + y2 * (C[2] + C[3] * y);
    let hi = (C[4] + C[5] * y) + y2 * (C[6] + C[7] * y);
    let odd = lo + y4 * hi;
    // w - w^2/4 + w^3 (1/36 - w^2/3600 + ...)
    w - 0.25 * y + w * y * odd
}

/// `R(u) = pi^2/24 + Li2(-e^(-2u)) / 2`, from `t = t(u)`: with
/// `z = -e^(-2u)`, `w = -ln(1 - z) = -t`.
fn tanh_r(t: f64) -> f64 {
    PI2_24 + 0.5 * li2_w(-t)
}

/// Below this `|x|`, `F1` uses `ln(1 + 2 sinh^2(x/2))`, which keeps full
/// relative precision near 0.
const TANH_SINH: f64 = 0.25;

/// `F1` of `tanh` from `t = t(|x|)`.
fn tanh_f1(x: f64, t: f64) -> f64 {
    let u = x.abs();
    if u < TANH_SINH {
        let s = (0.5 * u).sinh();
        (2.0 * s * s).ln_1p()
    } else {
        u - LN2 + f1_tail(u, t)
    }
}

/// `F2` of `tanh` from `r = R(|x|)`.
fn tanh_f2(x: f64, r: f64) -> f64 {
    let u = x.abs();
    if u < TANH_SERIES {
        // Term by term integral of ln cosh x = x^2/2 - x^4/12 + x^6/45
        // - 17x^8/2520 + 31x^10/14175 - 691x^12/935550 + ...; at
        // |x| < 0.125 the first omitted term is under 1e-20.
        const C: [f64; 6] = [
            1.0 / 6.0,
            -1.0 / 60.0,
            1.0 / 315.0,
            -17.0 / 22_680.0,
            31.0 / 155_925.0,
            -691.0 / 12_162_150.0,
        ];
        let x2 = x * x;
        let mut p = 0.0;
        for c in C.iter().rev() {
            p = p * x2 + c;
        }
        x * x2 * p
    } else {
        (0.5 * u * u - u * LN2 + r).copysign(x)
    }
}

impl Nonlinearity for Tanh {
    fn f(&self, x: f64) -> f64 {
        x.tanh()
    }
    fn f1(&self, x: f64) -> f64 {
        tanh_f1(x, self.aux1(x))
    }
    fn f2(&self, x: f64) -> f64 {
        tanh_f2(x, self.aux2(x))
    }
    fn dd1(&self, a: f64, b: f64) -> f64 {
        self.dd1_aux(a, self.aux1(a), b, self.aux1(b))
    }
    fn dd2(&self, a: f64, b: f64) -> f64 {
        self.dd2_aux(a, self.aux2(a), b, self.aux2(b))
    }
    fn aux1(&self, x: f64) -> f64 {
        let u = x.abs();
        if u > TANH_ASYMPTOTE {
            0.0
        } else {
            tanh_t(u)
        }
    }
    fn dd1_aux(&self, a: f64, ta: f64, b: f64, tb: f64) -> f64 {
        if a.abs() >= 1.0 && b.abs() >= 1.0 && (a > 0.0) == (b > 0.0) {
            // Same side: F1 = |x| - ln 2 + t(|x|), so the |x| part is exact.
            let s = 1.0f64.copysign(a);
            s + (f1_tail(a.abs(), ta) - f1_tail(b.abs(), tb)) / (a - b)
        } else {
            (tanh_f1(a, ta) - tanh_f1(b, tb)) / (a - b)
        }
    }
    fn aux2(&self, x: f64) -> f64 {
        let u = x.abs();
        if u < TANH_SERIES {
            0.0
        } else {
            tanh_r(tanh_t(u))
        }
    }
    fn dd2_aux(&self, a: f64, ra: f64, b: f64, rb: f64) -> f64 {
        if a.abs() >= TANH_SERIES && b.abs() >= TANH_SERIES && (a > 0.0) == (b > 0.0) {
            // Same side: the quadratic part's divided difference is exact,
            // only the small, bounded R(u) is differenced numerically.
            let s = 1.0f64.copysign(a);
            s * 0.5 * (a + b) - LN2 + s * (ra - rb) / (a - b)
        } else {
            (tanh_f2(a, ra) - tanh_f2(b, rb)) / (a - b)
        }
    }
}

/// First-order ADAA output for the segment from `x1` (previous input) to
/// `x0` (current input): `f` averaged over the segment.
pub fn adaa1<N: Nonlinearity + ?Sized>(nl: &N, x0: f64, x1: f64) -> f64 {
    if (x0 - x1).abs() < ADAA_EPS {
        nl.f(0.5 * (x0 + x1))
    } else {
        nl.dd1(x0, x1)
    }
}

/// `D(a, b) = (F2(a) - F2(b)) / (a - b)`, with its `a == b` limit `F1`.
fn d2<N: Nonlinearity + ?Sized>(nl: &N, a: f64, b: f64) -> f64 {
    if (a - b).abs() < ADAA_EPS {
        nl.f1(0.5 * (a + b))
    } else {
        nl.dd2(a, b)
    }
}

/// Second-order ADAA output from the current input `x0` and the two before
/// it, `x1` and `x2`.
pub fn adaa2<N: Nonlinearity + ?Sized>(nl: &N, x0: f64, x1: f64, x2: f64) -> f64 {
    let d = x0 - x2;
    if d.abs() < ADAA2_WIDE_EPS {
        // Limit x0 -> x2 (the formula is symmetric in x0 and x2, so their
        // mean is a second-order accurate stand-in).
        let c = 0.5 * (x0 + x2);
        let dc = c - x1;
        // Both adjacent steps under ADAA_EPS imply |dc| < ADAA_EPS.
        if dc.abs() < ADAA_EPS {
            nl.f(c + (x1 - c) / 3.0)
        } else {
            2.0 / dc * (nl.f1(c) - d2(nl, c, x1))
        }
    } else {
        2.0 * (d2(nl, x0, x1) - d2(nl, x1, x2)) / d
    }
}

/// Anti-aliasing order of a [`Waveshaper`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AdaaOrder {
    /// Plain `f(drive * x)`, no anti-aliasing and no delay.
    #[default]
    Naive,
    /// First-order ADAA: half a sample of delay.
    First,
    /// Second-order ADAA: one sample of delay.
    Second,
}

/// A streaming mono waveshaper: `out_gain * f(drive * x)` with optional ADAA.
/// Use one per channel. State is kept in `f64`.
#[derive(Clone, Debug)]
pub struct Waveshaper<N: Nonlinearity> {
    pub shape: N,
    pub order: AdaaOrder,
    pub drive: f64,
    pub out_gain: f64,
    x1: f64,
    x2: f64,
    /// `D(x1, x2)` from the previous sample, reused by second order while
    /// `d_valid` (cleared whenever another order ran).
    d_prev: f64,
    d_valid: bool,
    /// `shape.aux1(x1)` or `shape.aux2(x1)`, for the order in `a_order`
    /// (`Naive` when not valid).
    a1: f64,
    a_order: AdaaOrder,
}

impl<N: Nonlinearity> Waveshaper<N> {
    /// `order` and `drive` may change between samples (the history is kept
    /// in the driven domain); after changing `shape`, call [`Self::reset`].
    pub fn new(shape: N, order: AdaaOrder, drive: f64) -> Self {
        Waveshaper {
            shape,
            order,
            drive,
            out_gain: 1.0,
            x1: 0.0,
            x2: 0.0,
            d_prev: 0.0,
            d_valid: false,
            a1: 0.0,
            a_order: AdaaOrder::Naive,
        }
    }

    /// Clears the input history (as if the input had been 0).
    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.d_valid = false;
        self.a_order = AdaaOrder::Naive;
    }

    /// Group delay the ADAA order adds, in samples at the rate it runs at:
    /// 0, 0.5 or 1.
    pub fn delay_samples(&self) -> f64 {
        match self.order {
            AdaaOrder::Naive => 0.0,
            AdaaOrder::First => 0.5,
            AdaaOrder::Second => 1.0,
        }
    }

    /// One sample. Same arithmetic as [`adaa1`] / [`adaa2`] (bit for bit),
    /// with the per-point `aux` and second order's `D(x1, x2)` carried over
    /// from the previous sample instead of recomputed.
    #[inline]
    pub fn process_sample(&mut self, x: f32) -> f32 {
        let u = self.drive * x as f64;
        if self.order == AdaaOrder::Naive {
            self.d_valid = false;
            self.a_order = AdaaOrder::Naive;
            self.x2 = self.x1;
            self.x1 = u;
            return (self.shape.f(u) * self.out_gain) as f32;
        }
        let first = self.order == AdaaOrder::First;
        if self.a_order != self.order {
            self.a1 = if first {
                self.shape.aux1(self.x1)
            } else {
                self.shape.aux2(self.x1)
            };
            self.a_order = self.order;
        }
        let a0 = if first {
            self.shape.aux1(u)
        } else {
            self.shape.aux2(u)
        };
        let near = (u - self.x1).abs() < ADAA_EPS;
        let y = if first {
            self.d_valid = false;
            if near {
                self.shape.f(0.5 * (u + self.x1))
            } else {
                self.shape.dd1_aux(u, a0, self.x1, self.a1)
            }
        } else {
            if !self.d_valid {
                self.d_prev = d2(&self.shape, self.x1, self.x2);
                self.d_valid = true;
            }
            let d_new = if near {
                self.shape.f1(0.5 * (u + self.x1))
            } else {
                self.shape.dd2_aux(u, a0, self.x1, self.a1)
            };
            let d = u - self.x2;
            let y = if d.abs() < ADAA2_WIDE_EPS {
                adaa2(&self.shape, u, self.x1, self.x2)
            } else {
                2.0 * (d_new - self.d_prev) / d
            };
            self.d_prev = d_new;
            y
        };
        self.x2 = self.x1;
        self.x1 = u;
        self.a1 = a0;
        (y * self.out_gain) as f32
    }

    /// Shapes `block` in place.
    pub fn process_block(&mut self, block: &mut [f32]) {
        for s in block.iter_mut() {
            *s = self.process_sample(*s);
        }
    }

    /// Shapes `block` in place inside `hook`'s rate change (see
    /// [`OversampleHook`]). The shaper's history lives at the high rate, so
    /// keep one hook and one shaper together per channel, and reset both
    /// when the factor changes.
    pub fn process_with<H: OversampleHook + ?Sized>(&mut self, hook: &mut H, block: &mut [f32]) {
        hook.run(block, &mut |hi: &mut [f32]| self.process_block(hi));
    }

    /// Total group delay of [`Self::process_with`] through `hook`, in
    /// base-rate samples: the hook's round trip plus this shaper's delay at
    /// the high rate (`delay_samples() / factor`). Fractional for first
    /// order (0.5 at 1x, 0.25 at 2x); a sample-accurate integer
    /// compensation needs the second order at 1x (1.0) or a factor that
    /// makes the sum whole.
    pub fn latency_with<H: OversampleHook + ?Sized>(&self, hook: &H) -> f64 {
        hook.latency() + self.delay_samples() / hook.factor() as f64
    }
}

/// Runs block processing at a higher sample rate: up-sample `block`, call
/// `f` on the high-rate samples, down-sample back into `block`. The shaper
/// does not care how; any oversampler can implement this, and
/// [`NoOversampling`] is the 1x pass-through.
pub trait OversampleHook {
    /// Rate multiple `f` runs at (1 for pass-through).
    fn factor(&self) -> usize;
    /// Exact group delay of the round trip (up, then down) in base-rate
    /// samples, with `f` the identity: what a mixer's delay compensation
    /// must add to the dry path. 0 for [`NoOversampling`]. For a shaper
    /// inside, add its own delay at the high rate with
    /// [`Waveshaper::latency_with`].
    fn latency(&self) -> f64;
    /// Up, `f(high_rate_samples)`, down, in place on `block`.
    fn run(&mut self, block: &mut [f32], f: &mut dyn FnMut(&mut [f32]));
}

/// 1x: runs `f` directly on the block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoOversampling;

impl OversampleHook for NoOversampling {
    fn factor(&self) -> usize {
        1
    }
    fn latency(&self) -> f64 {
        0.0
    }
    fn run(&mut self, block: &mut [f32], f: &mut dyn FnMut(&mut [f32])) {
        f(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::fft;

    /// Dilogarithm `Li2(z)` for `z` in `[-1, 0]`.
    fn li2_neg(z: f64) -> f64 {
        li2_w(-(-z).ln_1p())
    }

    const FS: f64 = 44_100.0;
    const NAMES: [&str; 4] = ["hard", "tanh", "soft3", "soft5"];
    const ORDERS: [AdaaOrder; 3] = [AdaaOrder::Naive, AdaaOrder::First, AdaaOrder::Second];

    fn shape(name: &str) -> Box<dyn Nonlinearity> {
        match name {
            "hard" => Box::new(HardClip),
            "tanh" => Box::new(Tanh),
            "soft3" => Box::new(SoftClip3),
            _ => Box::new(SoftClip5),
        }
    }

    /// Runs `buf` through a fresh `Waveshaper` of the named shape.
    fn run(name: &str, order: AdaaOrder, drive: f64, input: &[f32]) -> Vec<f32> {
        let mut buf = input.to_vec();
        match name {
            "hard" => Waveshaper::new(HardClip, order, drive).process_block(&mut buf),
            "tanh" => Waveshaper::new(Tanh, order, drive).process_block(&mut buf),
            "soft3" => Waveshaper::new(SoftClip3, order, drive).process_block(&mut buf),
            _ => Waveshaper::new(SoftClip5, order, drive).process_block(&mut buf),
        }
        buf
    }

    /// Deterministic noise in [-1, 1).
    fn noise(n: usize, seed: u64) -> Vec<f64> {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 11) as f64 / (1u64 << 52) as f64 - 1.0
            })
            .collect()
    }

    #[test]
    fn antiderivatives_differentiate_back() {
        // F1' = f and F2' = F1 by central differences, across the kinks,
        // tanh's internal switches (|x| = 0.125, 0.25, 15) and far out.
        let h = 1e-6;
        let xs = [
            -700.0, -40.0, -15.0, -10.0, -3.0, -0.25, -1.5, -1.0, -0.999, -0.5, -0.1, 0.0, 0.05,
            0.125, 0.13, 0.3, 0.99, 1.0, 1.001, 2.0, 7.5, 10.0, 40.0, 700.0,
        ];
        for name in NAMES {
            let nl = shape(name);
            for &x in &xs {
                let d1 = (nl.f1(x + h) - nl.f1(x - h)) / (2.0 * h);
                let d2 = (nl.f2(x + h) - nl.f2(x - h)) / (2.0 * h);
                let tol1 = 1e-6 * (1.0 + x.abs());
                let tol2 = 1e-7 * (1.0 + x * x);
                assert!(
                    (d1 - nl.f(x)).abs() < tol1,
                    "{name} F1' at {x}: {d1} vs {}",
                    nl.f(x)
                );
                assert!(
                    (d2 - nl.f1(x)).abs() < tol2,
                    "{name} F2' at {x}: {d2} vs {}",
                    nl.f1(x)
                );
            }
            assert_eq!(nl.f1(0.0), 0.0, "{name}");
            assert_eq!(nl.f2(0.0), 0.0, "{name}");
            for &x in &[1e30, -1e30, 3.4e38] {
                assert!(
                    nl.f(x).is_finite() && nl.f1(x).is_finite() && nl.f2(x).is_finite(),
                    "{name} at {x}"
                );
            }
        }
        // ln cosh never calls cosh: exact asymptote where cosh itself overflows.
        assert!((Tanh.f1(1000.0) - (1000.0 - LN2)).abs() < 1e-12);
        assert!(1000f64.cosh().is_infinite());
        // The f32 range: cosh overflows f32 past ~89, F1 stays finite and exact.
        assert!((90f32).cosh().is_infinite());
        assert!((Tanh.f1(90.0) - (90.0 - LN2)).abs() < 1e-12);
        // Each formula switch is continuous to the last bit or two.
        for &(x, tol) in &[(TANH_SERIES, 1e-16), (TANH_SINH, 2e-16), (10.0, 1e-14)] {
            let below = x.next_down();
            let d1 = Tanh.f1(x) - Tanh.f1(below) - Tanh.f(x) * (x - below);
            let d2 = Tanh.f2(x) - Tanh.f2(below) - Tanh.f1(x) * (x - below);
            assert!(
                d1.abs() <= tol && d2.abs() <= tol,
                "tanh switch at {x}: {d1:e} {d2:e}"
            );
        }
        // Below the asymptote switch the e^(-2|x|) tail is kept (it is
        // e^-20 at 10); past it F1 is the bare asymptote, and the step
        // across the switch is e^-30 (~9.4e-14).
        assert!((Tanh.f1(10.0) - (10.0 - LN2) - (-20f64).exp()).abs() < 1e-15);
        let (at, past) = (TANH_ASYMPTOTE, TANH_ASYMPTOTE.next_up());
        assert_eq!(Tanh.f1(past), past - LN2);
        let step1 = Tanh.f1(past) - Tanh.f1(at) - Tanh.f(at) * (past - at);
        assert!(
            step1.abs() < 1e-13,
            "F1 step at the asymptote switch: {step1:e}"
        );
        let step2 = Tanh.f2(past) - Tanh.f2(at) - Tanh.f1(at) * (past - at);
        assert!(
            step2.abs() < 3e-14,
            "F2 step at the asymptote switch: {step2:e}"
        );
        // First-order output across the switch over the smallest gap.
        let y = adaa1(&Tanh, at + 0.6 * ADAA_EPS, at - 0.6 * ADAA_EPS);
        assert!((y - at.tanh()).abs() < 1e-8, "{y}");
        // Li2(-1) = -pi^2/12.
        assert!((li2_neg(-1.0) + std::f64::consts::PI.powi(2) / 12.0).abs() < 1e-15);
    }

    #[test]
    fn closed_form_divided_differences_match_quotients() {
        // On well-separated points, the closed-form divided differences agree
        // with the plain quotients.
        let pts = [
            -9.0, -2.0, -1.2, -0.7, -0.2, 0.15, 0.6, 0.95, 1.3, 4.0, 11.0,
        ];
        for name in NAMES {
            let nl = shape(name);
            for &a in &pts {
                for &b in &pts {
                    if a == b {
                        continue;
                    }
                    let q1 = (nl.f1(a) - nl.f1(b)) / (a - b);
                    let q2 = (nl.f2(a) - nl.f2(b)) / (a - b);
                    assert!((nl.dd1(a, b) - q1).abs() < 1e-12, "{name} dd1({a},{b})");
                    assert!(
                        (nl.dd2(a, b) - q2).abs() < 1e-11 * (1.0 + q2.abs()),
                        "{name} dd2({a},{b})"
                    );
                }
            }
        }
    }

    #[test]
    fn fallback_branches_agree_with_the_formula_at_the_threshold() {
        // Just under ADAA_EPS, the limit used as the fallback and the full
        // divided-difference formula evaluated at the same inputs must agree:
        // below f32 resolution for first order, under -110 dB for second
        // order (which divides twice).
        let centres = [
            0.0, 0.3, 0.999_99, 1.0, 1.000_01, -1.0, 2.5, -12.0, 0.124_999, 0.125, 9.999_99, 10.0,
            14.999_99, 15.0, -15.000_01, 60.0,
        ];
        let gap = 0.99 * ADAA_EPS;
        for name in NAMES {
            let nl = &*shape(name);
            for &c in &centres {
                for &k in &[1.0, -1.0] {
                    let x0 = c + k * gap;
                    let fb = adaa1(nl, x0, c);
                    let full = nl.dd1(x0, c);
                    // 1e-9 everywhere except across tanh's asymptote switch
                    // at 15, whose e^-30 step in F1 over a 1e-5 gap is ~1e-8.
                    let tol = if name == "tanh" && (c.abs() - TANH_ASYMPTOTE).abs() < 1e-3 {
                        2e-8
                    } else {
                        1e-9
                    };
                    assert!(
                        (fb - full).abs() < tol,
                        "{name} adaa1 at {c}: {fb} vs {full}"
                    );
                    for &step in &[0.5 * ADAA_EPS, 3e-5, 1e-3, 0.2] {
                        // Wide-step case (x0 ~ x2), x1 a step away.
                        let x0w = c + k * 0.99 * ADAA2_WIDE_EPS;
                        let x1 = c + step;
                        let fb = adaa2(nl, x0w, x1, c);
                        let full = 2.0 * (d2(nl, x0w, x1) - d2(nl, x1, c)) / (x0w - c);
                        assert!(
                            (fb - full).abs() < 3e-6,
                            "{name} adaa2 x0~x2 at {c} step {step}: {fb} vs {full}"
                        );
                        assert!(adaa2(nl, x0, x1, c).is_finite());
                        // Adjacent case (x0 ~ x1), x2 a step away.
                        let x2 = c - step - 2.0 * ADAA_EPS;
                        let fb = d2(nl, x0, c);
                        let full = nl.dd2(x0, c);
                        assert!(
                            (fb - full).abs() < 1e-10,
                            "{name} D x0~x1 at {c}: {fb} vs {full}"
                        );
                        assert!(adaa2(nl, x0, c, x2).is_finite());
                    }
                }
                // All three equal: exactly f(c).
                assert_eq!(adaa1(nl, c, c), nl.f(c), "{name}");
                assert_eq!(adaa2(nl, c, c, c), nl.f(c), "{name}");
            }
        }
    }

    #[test]
    fn second_order_matches_exact_closed_form() {
        // Hard clipper with the main formula: exactly +-1 in the saturated
        // tails and (x0 + x1 + x2) / 3 in the core, even at high drive where
        // F2 is large.
        for &(x0, x1, x2) in &[
            (5.0, 5.000_02, 5.000_05),
            (-12.0, -12.000_1, -11.999_95),
            (0.2, 0.200_03, 0.199_98),
            (300.0, 300.001, 300.002),
        ] {
            let y = adaa2(&HardClip, x0, x1, x2);
            let want = if x0 > 1.0 {
                1.0
            } else if x0 < -1.0 {
                -1.0
            } else {
                (x0 + x1 + x2) / 3.0
            };
            assert!((y - want).abs() < 1e-9, "({x0},{x1},{x2}): {y} vs {want}");
        }
    }

    #[test]
    fn dc_holds_and_steps_stay_finite() {
        // DC, a held value, a step, a held step and alternating tiny wiggles
        // around large values: every output finite and in range, at every
        // order and drive.
        let mut x = Vec::new();
        x.extend(std::iter::repeat_n(0.0f32, 32));
        x.extend(std::iter::repeat_n(0.25f32, 64));
        x.extend(std::iter::repeat_n(-0.9f32, 64));
        x.extend([1.0, 1.0, -1.0, -1.0, 1.0, 0.5, 0.5, 0.5]);
        x.extend((0..64).map(|i| 0.5 + if i % 2 == 0 { 1e-7 } else { -1e-7 }));
        x.extend((0..64).map(|i| -0.7 + i as f32 * 1e-8));
        x.extend(std::iter::repeat_n(f32::MIN_POSITIVE, 16));
        for name in NAMES {
            for order in ORDERS {
                // Constant DC of 0, 0.5, 1 and 2 for 1024 samples each:
                // finite throughout, exactly f(drive * v) once held.
                for v in [0.0f32, 0.5, 1.0, 2.0] {
                    let y = run(name, order, 3.0, &[v; 1024]);
                    assert!(y.iter().all(|s| s.is_finite()), "{name} {order:?} DC {v}");
                    let want = shape(name).f(3.0 * v as f64) as f32;
                    assert!(
                        y[2..].iter().all(|s| *s == want),
                        "{name} {order:?} DC {v}: {} vs {want}",
                        y[1023]
                    );
                }
                for drive in [1.0, 4.0, 63.0, 1e6] {
                    let y = run(name, order, drive, &x);
                    assert!(
                        y.iter().all(|v| v.is_finite() && v.abs() <= 1.0 + 1e-6),
                        "{name} {order:?} drive {drive}"
                    );
                    // Held values settle to exactly f(drive * v).
                    let nl = shape(name);
                    let want = nl.f(drive * 0.25f32 as f64) as f32;
                    assert_eq!(y[32 + 63], want, "{name} {order:?} drive {drive}");
                }
            }
        }
    }

    #[test]
    fn odd_symmetry_and_dc() {
        let x: Vec<f32> = noise(4096, 3).iter().map(|v| (*v * 1.7) as f32).collect();
        let neg: Vec<f32> = x.iter().map(|v| -v).collect();
        for name in NAMES {
            for order in ORDERS {
                let (a, b) = (run(name, order, 6.0, &x), run(name, order, 6.0, &neg));
                for (p, q) in a.iter().zip(&b) {
                    assert_eq!(
                        p.to_bits(),
                        (-q).to_bits(),
                        "{name} {order:?} not exactly odd"
                    );
                }
                assert!(
                    run(name, order, 6.0, &[0.0; 64]).iter().all(|v| *v == 0.0),
                    "{name} {order:?}"
                );
            }
        }
    }

    #[test]
    fn agrees_with_naive_at_low_frequency() {
        // 50 Hz at drive 3: ADAA output matches the naive shaper at the same
        // (delay-compensated) point. The error is the segment average's
        // curvature term (~gap^2/24 * f''), and only at the corners for the
        // hard clipper (~gap/8).
        let n = 8820;
        let u: Vec<f64> = (0..n)
            .map(|i| 3.0 * (2.0 * std::f64::consts::PI * 50.0 * i as f64 / FS).sin())
            .collect();
        for name in NAMES {
            let nl = shape(name);
            let tol = if name == "hard" { 3e-3 } else { 2e-4 };
            let (mut e1, mut e2) = (0.0f64, 0.0f64);
            for i in 2..n {
                e1 = e1.max((adaa1(&*nl, u[i], u[i - 1]) - nl.f(0.5 * (u[i] + u[i - 1]))).abs());
                e2 = e2.max((adaa2(&*nl, u[i], u[i - 1], u[i - 2]) - nl.f(u[i - 1])).abs());
            }
            assert!(e1 < tol && e2 < tol, "{name}: first {e1:e}, second {e2:e}");
        }
    }

    #[test]
    fn streaming_matches_the_free_functions_and_block_size_does_not_matter() {
        let x: Vec<f32> = noise(3000, 9).iter().map(|v| (*v * 0.8) as f32).collect();
        let whole = run("tanh", AdaaOrder::Second, 8.0, &x);
        // The streaming shaper reuses D(x1, x2); the free function recomputes it.
        let (mut x1, mut x2) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            let u = 8.0 * *v as f64;
            assert_eq!(whole[i], adaa2(&Tanh, u, x1, x2) as f32, "sample {i}");
            (x2, x1) = (x1, u);
        }
        let first = run("tanh", AdaaOrder::First, 8.0, &x);
        let mut x1 = 0.0;
        for (i, v) in x.iter().enumerate() {
            let u = 8.0 * *v as f64;
            assert_eq!(
                first[i],
                adaa1(&Tanh, u, x1) as f32,
                "first order, sample {i}"
            );
            x1 = u;
        }
        // Odd block sizes through the 1x hook give bit-identical output.
        let mut ws = Waveshaper::new(Tanh, AdaaOrder::Second, 8.0);
        let mut hook = NoOversampling;
        assert_eq!((hook.factor(), hook.latency()), (1, 0.0));
        assert_eq!(ws.latency_with(&hook), 1.0);
        ws.order = AdaaOrder::First;
        assert_eq!(ws.latency_with(&Repeat2x { hi: Vec::new() }), 0.25);
        ws.order = AdaaOrder::Second;
        let mut chunked = x.clone();
        for c in chunked.chunks_mut(37) {
            ws.process_with(&mut hook, c);
        }
        assert!(whole
            .iter()
            .zip(&chunked)
            .all(|(a, b)| a.to_bits() == b.to_bits()));
        // Switching order mid-stream picks the second-order history back up.
        let mut ws = Waveshaper::new(Tanh, AdaaOrder::Second, 8.0);
        let mut sw = x.clone();
        ws.process_block(&mut sw[..100]);
        ws.order = AdaaOrder::First;
        ws.process_block(&mut sw[100..200]);
        ws.order = AdaaOrder::Second;
        ws.process_block(&mut sw[200..]);
        assert!(whole[200..]
            .iter()
            .zip(&sw[200..])
            .all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    /// A toy 2x hook for the tests: sample repeat up, pair average down.
    /// (Not an anti-aliasing oversampler; it only checks the plumbing.)
    struct Repeat2x {
        hi: Vec<f32>,
    }

    impl OversampleHook for Repeat2x {
        fn factor(&self) -> usize {
            2
        }
        fn latency(&self) -> f64 {
            0.0
        }
        fn run(&mut self, block: &mut [f32], f: &mut dyn FnMut(&mut [f32])) {
            self.hi.clear();
            self.hi.extend(block.iter().flat_map(|v| [*v, *v]));
            f(&mut self.hi);
            for (o, p) in block.iter_mut().zip(self.hi.chunks(2)) {
                *o = 0.5 * (p[0] + p[1]);
            }
        }
    }

    #[test]
    fn shaper_runs_at_the_hook_rate_with_state_across_blocks() {
        let x: Vec<f32> = (0..2048)
            .map(|i| (0.9 * (i as f64 * 0.01).sin()) as f32)
            .collect();
        let go = |chunk: usize| -> Vec<f32> {
            let mut ws = Waveshaper::new(HardClip, AdaaOrder::First, 4.0);
            let mut hook = Repeat2x { hi: Vec::new() };
            let mut buf = x.clone();
            for c in buf.chunks_mut(chunk) {
                ws.process_with(&mut hook, c);
            }
            buf
        };
        let (a, b) = (go(2048), go(61));
        assert!(
            a.iter().zip(&b).all(|(p, q)| p.to_bits() == q.to_bits()),
            "state lost at block edges"
        );
        // The shaper saw 2x the samples (every second one a zero-gap segment,
        // the fallback), and the result tracks the naive clipper.
        for i in 1..2048 {
            let want = (4.0 * x[i] as f64).clamp(-1.0, 1.0);
            assert!((a[i] as f64 - want).abs() < 0.05, "{i}: {} vs {want}", a[i]);
        }
    }

    /// Alias power relative to the fundamental, in dB, for a sine on an
    /// exact FFT bin (odd bin, so no aliased harmonic lands on a real one).
    fn alias_db(y: &[f32], bin: usize) -> f64 {
        let n = y.len();
        let mut re = y.to_vec();
        let mut im = vec![0.0f32; n];
        fft(&mut re, &mut im);
        let p = |k: usize| re[k] as f64 * re[k] as f64 + im[k] as f64 * im[k] as f64;
        let alias: f64 = (1..=n / 2).filter(|k| k % bin != 0).map(p).sum();
        10.0 * (alias / p(bin)).log10()
    }

    #[test]
    fn adaa_reduces_aliasing_of_driven_sines() {
        // 1x, 16k-point FFT, after a warm-up. Regression guards set with
        // margin under the measured values (1 kHz +12 dB: first order
        // 4.7..8.6 dB, second 9.5..15.2 dB better than naive; 5 kHz +20 dB:
        // 7.6..8.4 and 14.7..15.5 dB).
        let n = 16_384;
        let warm = 1024;
        for &(bin, drive, g1, g2) in &[(373usize, 4.0, 3.0, 7.0), (1857, 10.0, 6.0, 12.0)] {
            let x: Vec<f32> = (0..n + warm)
                .map(|i| {
                    (2.0 * std::f64::consts::PI * bin as f64 * i as f64 / n as f64).sin() as f32
                })
                .collect();
            for name in NAMES {
                let db: Vec<f64> = ORDERS
                    .iter()
                    .map(|o| alias_db(&run(name, *o, drive, &x)[warm..], bin))
                    .collect();
                eprintln!("alias bin {bin} drive {drive} {name}: naive {:.1} dB, first {:.1} dB, second {:.1} dB", db[0], db[1], db[2]);
                assert!(
                    db[1] < db[0] - g1,
                    "{name}: first order {:.1} vs naive {:.1}",
                    db[1],
                    db[0]
                );
                assert!(
                    db[2] < db[0] - g2,
                    "{name}: second order {:.1} vs naive {:.1}",
                    db[2],
                    db[0]
                );
            }
        }
    }
}
