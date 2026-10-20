//! DC guard for the tape-stop effects: `stutter` in `tape_stop` mode and
//! `gross_beat` with the `tape_stop` / `tape_stop_end` time presets.
//!
//! A tape stop reads the source ever more slowly, so every period stretches
//! and, as the speed reaches zero, the read freezes on whatever sample value
//! it stopped at: the wet signal drifts into infrasonic content and DC (a
//! sine stopped mid-swing holds that value), heard as a thump. Producers
//! measured a 0.12 FS DC blip on a real render.
//!
//! Design: a 2-pole (12 dB/oct) Butterworth high-pass, fc = 20 Hz, Q = 1/sqrt 2,
//! on the effect's wet read, before its gain envelope, and only on the ramp.
//! - 20 Hz, the top of the 15-20 Hz band: the most infrasonic rejection the
//!   band allows (worst stop-tail DC in the unit test 0.142 FS unguarded,
//!   0.0122 at 18 Hz, 0.0100 at 20 Hz), while the passband cost stays small:
//!   -0.24 dB at E1 (41.2 Hz), -0.45 dB at C#1 (34.6 Hz). It only ever acts
//!   inside a tape stop, where those notes are falling through it anyway.
//! - Q = 1/sqrt 2 (maximally flat): no resonant peak to boost the stop exactly
//!   where it passes through 20 Hz, and no ringing on the final freeze.
//! - Before the gain envelope: the envelope still closes the ramp at exactly
//!   zero, so the filter leaves no tail to cut at the next cycle.
//! - Warm start: at each ramp the state is run over 0.25 s of the source at
//!   unity speed (what was playing into the stop). A cold start on running
//!   audio is itself a low-frequency blip (up to 0.24 FS on a 0.5 FS 41 Hz
//!   sine). The poles decay as exp(-w0 t / 2Q): 2e-10 after 0.25 s.
//! - f64 state and coefficients (poles at radius 0.998 lose precision in f32);
//!   outputs under 1e-20 flush to zero, so a stop held at rate 0 settles to an
//!   exact 0 instead of a denormal tail.
//! - No state outlives one pass of the effect: a bypassed effect is not run,
//!   and each ramp (each re-trigger, every cycle) starts from the warm-up.
//!
//! Coefficients: R. Bristow-Johnson, "Cookbook formulae for audio EQ biquad
//! filter coefficients" (HPF), direct form I. Clean-room; no code adapted.

use super::SR;

/// Corner frequency of the tape-stop DC guard (Hz).
pub const TAPE_DC_HZ: f64 = 20.0;
/// Butterworth Q of the guard.
pub const TAPE_DC_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// Outputs below this are flushed to zero (no denormal stalls at rate 0).
pub const FLUSH: f64 = 1e-20;

/// Warm-up length (samples of source at unity speed) before each ramp.
pub fn preroll() -> usize {
    (0.25 * SR) as usize
}

/// One channel of the guard: RBJ high-pass, direct form I, f64.
#[derive(Clone, Copy, Debug)]
pub struct TapeDcGuard {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Default for TapeDcGuard {
    fn default() -> Self {
        Self::new(TAPE_DC_HZ, TAPE_DC_Q)
    }
}

impl TapeDcGuard {
    /// RBJ HPF: w0 = 2 pi fc / fs, alpha = sin w0 / 2Q,
    /// b = [(1+cos w0)/2, -(1+cos w0), (1+cos w0)/2], a = [1+alpha, -2 cos w0, 1-alpha].
    pub fn new(fc: f64, q: f64) -> Self {
        let w0 = std::f64::consts::TAU * fc / SR as f64;
        let (sn, cs) = w0.sin_cos();
        let alpha = sn / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b0 = (1.0 + cs) * 0.5 / a0;
        TapeDcGuard {
            b0,
            b1: -2.0 * b0,
            b2: b0,
            a1: -2.0 * cs / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    /// Clear the state (as if the input had been silent).
    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    /// The filter memory [x1, x2, y1, y2] (for checks: never denormal).
    pub fn state(&self) -> [f64; 4] {
        [self.x1, self.x2, self.y1, self.y2]
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let mut y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        // a stop frozen on a held value decays toward zero for as long as it
        // holds: flush below 1e-20 (far under f32's smallest step at any
        // audible level) so the state never goes denormal; with b0+b1+b2 = 0
        // a held input then keeps the output at exactly 0
        if y.abs() < FLUSH {
            y = 0.0;
        }
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Magnitude response of the guard, measured on steady sines.
    fn gain_db(f: f32) -> f32 {
        let mut g = TapeDcGuard::default();
        let n = (SR * 2.0) as usize;
        let mut acc = 0.0f64;
        let mut ref_acc = 0.0f64;
        for i in 0..n {
            let x = (std::f32::consts::TAU * f * i as f32 / SR).sin();
            let y = g.process(x);
            if i >= n / 2 {
                acc += (y as f64).powi(2);
                ref_acc += (x as f64).powi(2);
            }
        }
        (10.0 * (acc / ref_acc).log10()) as f32
    }

    #[test]
    fn tape_dc_guard_is_a_20_hz_butterworth_high_pass() {
        let at = |f: f32| gain_db(f);
        let (g10, g20, g41, g100) = (at(10.0), at(20.0), at(41.2), at(100.0));
        eprintln!("tape dc guard: 10 Hz {g10:.2} dB, 20 Hz {g20:.2} dB, 41.2 Hz {g41:.2} dB, 100 Hz {g100:.3} dB");
        assert!((g20 + 3.01).abs() < 0.05, "-3 dB at fc: {g20}");
        assert!((g10 + 12.3).abs() < 0.2, "12 dB/oct below fc: {g10}");
        assert!((g41 + 0.24).abs() < 0.05, "E1 passes: {g41}");
        assert!(g100.abs() < 0.02, "flat above: {g100}");
        // DC is removed: a step settles to zero
        let mut g = TapeDcGuard::default();
        let mut y = 0.0;
        for _ in 0..preroll() {
            y = g.process(0.5);
        }
        assert!(y.abs() < 1e-6, "step residue {y}");
    }

    /// Rate 0 held: a 55 Hz sine stops dead and the tape holds that value for
    /// 10 s. The output stays finite and bounded, decays, settles to exactly 0,
    /// and the state is never denormal on the way.
    #[test]
    fn tape_dc_guard_is_stable_at_rate_zero() {
        let mut g = TapeDcGuard::default();
        let stop = (0.2 * SR) as usize + 37;
        let src = |i: usize| (std::f32::consts::TAU * 55.0 * i.min(stop) as f32 / SR).sin() * 0.5;
        let held = src(stop);
        let n = (10.0 * SR) as usize;
        let mut peak = 0.0f32;
        let mut at_1s = 0.0f32;
        let mut settled_at = None;
        for i in 0..n {
            let y = g.process(src(i));
            assert!(y.is_finite());
            for v in [g.x1, g.x2, g.y1, g.y2] {
                assert!(v == 0.0 || v.is_normal(), "denormal state {v:e} at {i}");
            }
            peak = peak.max(y.abs());
            if i == stop + SR as usize {
                at_1s = y.abs();
            }
            if y == 0.0 && settled_at.is_none() && i > stop {
                settled_at = Some(i);
            }
        }
        let settled = settled_at.map(|i| (i - stop) as f32 / SR);
        eprintln!("rate 0 held at {held:.3}: peak {peak:.3}, |y| 1 s into the hold {at_1s:e}, exactly 0 after {settled:?} s");
        assert!(peak <= 1.0, "bounded: {peak}");
        assert!(at_1s < 1e-6, "decays: {at_1s}");
        let s = settled.expect("settles to exactly 0");
        assert!(s < 1.0, "settles within 1 s: {s}");
        assert_eq!(g.y1, 0.0);
    }
}
