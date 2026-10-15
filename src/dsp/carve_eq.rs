//! Sidechain dynamic-EQ carving: a key signal (a vocal, say) ducks one
//! frequency band of a target (a pad, a guitar bus) only while the key has
//! energy in that band, so the two stop fighting there and the rest of the
//! target is untouched.
//!
//! Clean-room, standard dynamic-EQ construction:
//! - band of interest [low_hz, high_hz]; its centre is the geometric mean
//!   and Q = centre / bandwidth;
//! - key detector: the key through two 2nd-order bandpass sections on that band (or a
//!   separate key band), squared, a 10 ms RMS follower, in dB;
//! - gain computer: cut = clamp((level - threshold) * (1 - 1/ratio), 0, max_cut);
//! - attack / release one-pole smoothing of the cut (dB);
//! - target: an RBJ biquad with gain -cut, refreshed every 16 samples:
//!   a peaking bell at the centre (`Bell`), or a shelf (S = 1) with its
//!   corner at high_hz (`LowShelf`) or low_hz (`HighShelf`).
//! - phase: minimum-phase IIR (poles and zeros inside the unit circle,
//!   checked in the tests); the group delay at the band centre is reported
//!   by `group_delay_samples` and by the `carve_frequencies` action. At 0 dB the filter is skipped, so the
//!   target passes bit for bit while the key is quiet.
//!
//! Real-time: fixed state, no allocation in `process`, no latency.

use std::f64::consts::PI;

/// The shape of the cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarveMode {
    /// Peaking bell at the band's geometric centre, Q = centre / bandwidth.
    Bell,
    /// Low shelf with its corner at `high_hz` (cuts the band and below).
    LowShelf,
    /// High shelf with its corner at `low_hz` (cuts the band and above).
    HighShelf,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CarveParams {
    pub mode: CarveMode,
    pub low_hz: f32,
    pub high_hz: f32,
    /// Key band (defaults to the carve band when None).
    pub key_low_hz: Option<f32>,
    pub key_high_hz: Option<f32>,
    pub threshold_db: f32,
    pub ratio: f32,
    pub max_cut_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
}

impl Default for CarveParams {
    fn default() -> Self {
        CarveParams { mode: CarveMode::Bell, low_hz: 150.0, high_hz: 400.0, key_low_hz: None, key_high_hz: None, threshold_db: -40.0, ratio: 4.0, max_cut_db: 6.0, attack_ms: 10.0, release_ms: 150.0 }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Bq {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Bq {
    #[inline]
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
    fn set(&mut self, b: [f64; 3], a: [f64; 3]) {
        self.b = [b[0] / a[0], b[1] / a[0], b[2] / a[0]];
        self.a = [a[1] / a[0], a[2] / a[0]];
    }
}

fn centre_q(lo: f32, hi: f32, sr: f64) -> (f64, f64) {
    let lo = (lo as f64).clamp(10.0, 0.45 * sr);
    let hi = (hi as f64).clamp(lo * 1.01, 0.49 * sr);
    let c = (lo * hi).sqrt();
    (c, c / (hi - lo))
}

/// RBJ constant-peak bandpass coefficients.
fn bandpass(c: f64, q: f64, sr: f64) -> ([f64; 3], [f64; 3]) {
    let w = 2.0 * PI * c / sr;
    let al = w.sin() / (2.0 * q);
    ([al, 0.0, -al], [1.0 + al, -2.0 * w.cos(), 1.0 - al])
}

/// RBJ peaking EQ coefficients (gain in dB).
fn peaking(c: f64, q: f64, gain_db: f64, sr: f64) -> ([f64; 3], [f64; 3]) {
    let a = 10f64.powf(gain_db / 40.0);
    let w = 2.0 * PI * c / sr;
    let al = w.sin() / (2.0 * q);
    let cw = w.cos();
    ([1.0 + al * a, -2.0 * cw, 1.0 - al * a], [1.0 + al / a, -2.0 * cw, 1.0 - al / a])
}

/// RBJ shelf coefficients (slope S = 1), corner `f`, gain in dB.
fn shelf(high: bool, f: f64, gain_db: f64, sr: f64) -> ([f64; 3], [f64; 3]) {
    let a = 10f64.powf(gain_db / 40.0);
    let w = 2.0 * PI * f / sr;
    let (cw, al) = (w.cos(), w.sin() / std::f64::consts::SQRT_2);
    let sa = 2.0 * a.sqrt() * al;
    if high {
        (
            [a * ((a + 1.0) + (a - 1.0) * cw + sa), -2.0 * a * ((a - 1.0) + (a + 1.0) * cw), a * ((a + 1.0) + (a - 1.0) * cw - sa)],
            [(a + 1.0) - (a - 1.0) * cw + sa, 2.0 * ((a - 1.0) - (a + 1.0) * cw), (a + 1.0) - (a - 1.0) * cw - sa],
        )
    } else {
        (
            [a * ((a + 1.0) - (a - 1.0) * cw + sa), 2.0 * a * ((a - 1.0) - (a + 1.0) * cw), a * ((a + 1.0) - (a - 1.0) * cw - sa)],
            [(a + 1.0) + (a - 1.0) * cw + sa, -2.0 * ((a - 1.0) + (a + 1.0) * cw), (a + 1.0) + (a - 1.0) * cw - sa],
        )
    }
}

/// The target filter of a carve of `cut_db` (>= 0) for these params.
pub fn carve_coeffs(p: &CarveParams, cut_db: f64, sr: f64) -> ([f64; 3], [f64; 3]) {
    let (c, q) = centre_q(p.low_hz, p.high_hz, sr);
    match p.mode {
        CarveMode::Bell => peaking(c, q, -cut_db, sr),
        CarveMode::LowShelf => shelf(false, (p.high_hz as f64).clamp(10.0, 0.49 * sr), -cut_db, sr),
        CarveMode::HighShelf => shelf(true, (p.low_hz as f64).clamp(10.0, 0.49 * sr), -cut_db, sr),
    }
}

/// Where the cut is centred: the bell's centre, or the shelf's corner (Hz).
pub fn carve_focus_hz(p: &CarveParams, sr: f64) -> f64 {
    match p.mode {
        CarveMode::Bell => centre_q(p.low_hz, p.high_hz, sr).0,
        CarveMode::LowShelf => p.high_hz as f64,
        CarveMode::HighShelf => p.low_hz as f64,
    }
}

/// Group delay (samples) of a biquad at `f`, from the phase slope (+-0.5 Hz).
pub fn group_delay_samples(b: [f64; 3], a: [f64; 3], f: f64, sr: f64) -> f64 {
    let ph = |f: f64| {
        let w = 2.0 * PI * f / sr;
        let (re_n, im_n) = (b[0] + b[1] * w.cos() + b[2] * (2.0 * w).cos(), -(b[1] * w.sin() + b[2] * (2.0 * w).sin()));
        let (re_d, im_d) = (a[0] + a[1] * w.cos() + a[2] * (2.0 * w).cos(), -(a[1] * w.sin() + a[2] * (2.0 * w).sin()));
        im_n.atan2(re_n) - im_d.atan2(re_d)
    };
    let (f1, f2) = (f - 0.5, f + 0.5);
    let mut d = ph(f2) - ph(f1);
    while d > PI {
        d -= 2.0 * PI;
    }
    while d < -PI {
        d += 2.0 * PI;
    }
    -d / (2.0 * PI * (f2 - f1) / sr)
}

/// Phase (degrees) of a biquad at `f`.
pub fn phase_deg(b: [f64; 3], a: [f64; 3], f: f64, sr: f64) -> f64 {
    let w = 2.0 * PI * f / sr;
    let (re_n, im_n) = (b[0] + b[1] * w.cos() + b[2] * (2.0 * w).cos(), -(b[1] * w.sin() + b[2] * (2.0 * w).sin()));
    let (re_d, im_d) = (a[0] + a[1] * w.cos() + a[2] * (2.0 * w).cos(), -(a[1] * w.sin() + a[2] * (2.0 * w).sin()));
    (im_n.atan2(re_n) - im_d.atan2(re_d)).to_degrees()
}

/// Magnitude (dB) of a biquad at `f`.
pub fn magnitude_db(b: [f64; 3], a: [f64; 3], f: f64, sr: f64) -> f64 {
    let w = 2.0 * PI * f / sr;
    let (re_n, im_n) = (b[0] + b[1] * w.cos() + b[2] * (2.0 * w).cos(), -(b[1] * w.sin() + b[2] * (2.0 * w).sin()));
    let (re_d, im_d) = (a[0] + a[1] * w.cos() + a[2] * (2.0 * w).cos(), -(a[1] * w.sin() + a[2] * (2.0 * w).sin()));
    10.0 * ((re_n * re_n + im_n * im_n) / (re_d * re_d + im_d * im_d)).log10()
}

/// Both roots of z^2 + c1 z + c2 (normalized by c0) inside the unit circle.
pub fn roots_inside(c: [f64; 3]) -> bool {
    let (c1, c2) = (c[1] / c[0], c[2] / c[0]);
    // Jury / Schur-Cohn test for a quadratic
    c2.abs() < 1.0 && c1.abs() < 1.0 + c2
}

const REFRESH: usize = 16;

/// Streaming key-driven band carve for up to 8 target channels.
pub struct Carver {
    sr: f64,
    p: CarveParams,
    key_bp: [Bq; 2],
    eq: [Bq; 8],
    rms_c: f64,
    att: f64,
    rel: f64,
    pow: f64,
    cut: f64,
    applied: f64,
    count: usize,
    max_cut: f64,
    ducked_samples: u64,
}

impl Carver {
    pub fn new(sr: f32, p: CarveParams) -> Self {
        let sr = sr as f64;
        let (kc, kq) = centre_q(p.key_low_hz.unwrap_or(p.low_hz), p.key_high_hz.unwrap_or(p.high_hz), sr);
        // two identical bandpass sections: 4th order, so a key well outside the band stays out
        let mut key_bp = [Bq::default(); 2];
        let (b, a) = bandpass(kc, kq, sr);
        key_bp[0].set(b, a);
        key_bp[1].set(b, a);
        let c = |ms: f32| if ms <= 0.0 { 0.0 } else { (-1.0 / (ms as f64 * 0.001 * sr)).exp() };
        Carver {
            sr,
            p,
            key_bp,
            eq: [Bq::default(); 8],
            rms_c: c(10.0),
            att: c(p.attack_ms),
            rel: c(p.release_ms),
            pow: 0.0,
            cut: 0.0,
            applied: 0.0,
            count: 0,
            max_cut: 0.0,
            ducked_samples: 0,
        }
    }

    pub fn latency(&self) -> usize {
        0
    }

    /// Largest cut applied (dB) and the number of samples with any cut.
    pub fn stats(&self) -> (f32, u64) {
        (self.max_cut as f32, self.ducked_samples)
    }

    /// Carve `target` channels in place, driven by `key` (mono, aligned).
    /// Processes min(key length, target length) frames.
    pub fn process(&mut self, key: &[f32], target: &mut [&mut [f32]]) {
        let nch = target.len().min(8);
        let n = target[..nch].iter().map(|c| c.len()).min().unwrap_or(0).min(key.len());
        let p = self.p;
        for i in 0..n {
            let k0 = self.key_bp[0].run(key[i] as f64);
            let k = self.key_bp[1].run(k0);
            self.pow = self.rms_c * self.pow + (1.0 - self.rms_c) * k * k;
            let lvl = if self.pow > 1e-24 { 10.0 * self.pow.log10() + 3.0103 } else { -240.0 };
            let want = ((lvl - p.threshold_db as f64) * (1.0 - 1.0 / (p.ratio as f64).max(1.0))).clamp(0.0, p.max_cut_db.max(0.0) as f64);
            let co = if want > self.cut { self.att } else { self.rel };
            self.cut = co * self.cut + (1.0 - co) * want;
            if self.cut < 1e-4 && want == 0.0 {
                self.cut = 0.0;
            }
            if self.count == 0 {
                if self.cut != self.applied {
                    self.applied = self.cut;
                    if self.applied > 0.0 {
                        let (b, a) = carve_coeffs(&self.p, self.applied, self.sr);
                        for e in self.eq.iter_mut() {
                            e.set(b, a);
                        }
                    }
                }
                self.count = REFRESH;
            }
            self.count -= 1;
            if self.applied > 0.0 {
                self.ducked_samples += 1;
                if self.applied > self.max_cut {
                    self.max_cut = self.applied;
                }
                for (c, ch) in target.iter_mut().enumerate().take(nch) {
                    ch[i] = self.eq[c].run(ch[i] as f64) as f32;
                }
            } else {
                // flat: pass untouched, keep the filter state at rest
                for e in self.eq.iter_mut() {
                    e.z = [0.0; 2];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI as P;

    const SR: f32 = 44_100.0;

    fn tone(f: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * P * f * i as f32 / SR).sin()).collect()
    }

    fn band_level_db(x: &[f32], f: f32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (2.0 * P * i as f32 / x.len() as f32).cos();
            let ph = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / SR as f64;
            re += (v * w) as f64 * ph.cos();
            im += (v * w) as f64 * ph.sin();
        }
        10.0 * ((re * re + im * im) as f32).log10()
    }

    #[test]
    fn a_quiet_key_leaves_the_target_bit_identical() {
        let x: Vec<f32> = tone(250.0, 0.5, 20000).iter().zip(tone(3000.0, 0.3, 20000)).map(|(a, b)| a + b).collect();
        let key = vec![0.0f32; 20000];
        let mut y = x.clone();
        let mut c = Carver::new(SR, CarveParams::default());
        c.process(&key, &mut [&mut y]);
        assert_eq!(c.stats(), (0.0, 0));
        assert!(x.iter().zip(&y).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn a_key_in_the_band_carves_only_that_band() {
        let n = 44100;
        // target: 250 Hz (in the 150-400 band) and 4 kHz (out of it), both -12 dBFS
        let x: Vec<f32> = tone(250.0, 0.25, n).iter().zip(tone(4000.0, 0.25, n)).map(|(a, b)| a + b).collect();
        // key: a 220 Hz "voice" at -12 dBFS, all the way through
        let key = tone(220.0, 0.25, n);
        let p = CarveParams { max_cut_db: 6.0, ..Default::default() };
        let mut y = x.clone();
        let mut c = Carver::new(SR, p);
        c.process(&key, &mut [&mut y]);
        let (lo_in, lo_out) = (band_level_db(&x[11025..], 250.0), band_level_db(&y[11025..], 250.0));
        let (hi_in, hi_out) = (band_level_db(&x[11025..], 4000.0), band_level_db(&y[11025..], 4000.0));
        eprintln!("CARVE in-band 250 Hz: {:.2} dB, out-of-band 4 kHz: {:.2} dB, max cut {:.2} dB", lo_out - lo_in, hi_out - hi_in, c.stats().0);
        assert!((lo_out - lo_in + 6.0).abs() < 0.6, "{}", lo_out - lo_in);
        assert!((hi_out - hi_in).abs() < 0.3, "{}", hi_out - hi_in);
        assert!((c.stats().0 - 6.0).abs() < 0.01);
    }

    #[test]
    fn a_key_outside_the_band_does_not_carve() {
        let n = 22050;
        let x = tone(250.0, 0.25, n);
        let key = tone(3000.0, 0.5, n); // presence, not mud
        let mut y = x.clone();
        let mut c = Carver::new(SR, CarveParams::default());
        c.process(&key, &mut [&mut y]);
        let d = band_level_db(&y[5000..], 250.0) - band_level_db(&x[5000..], 250.0);
        assert!(d.abs() < 0.5 && c.stats().0 < 1.0, "{d} {:?}", c.stats());
    }

    #[test]
    fn the_cut_releases_after_the_key_stops() {
        let n = 44100;
        let x = tone(300.0, 0.25, n);
        let key: Vec<f32> = tone(300.0, 0.3, n).into_iter().enumerate().map(|(i, v)| if i < 11025 { v } else { 0.0 }).collect();
        let mut y = x.clone();
        let mut c = Carver::new(SR, CarveParams { release_ms: 100.0, ..Default::default() });
        c.process(&key, &mut [&mut y]);
        // well after the release the band is back to (nearly) full level
        let d = band_level_db(&y[33000..], 300.0) - band_level_db(&x[33000..], 300.0);
        assert!(d.abs() < 0.2, "{d}");
    }

    #[test]
    fn shelf_modes_cut_their_side_only() {
        let n = 44100;
        let key = tone(220.0, 0.25, n);
        // low shelf, corner at 400 Hz: an 80 Hz tone drops ~6 dB, 4 kHz stays
        let x: Vec<f32> = tone(80.0, 0.25, n).iter().zip(tone(4000.0, 0.25, n)).map(|(a, b)| a + b).collect();
        let mut y = x.clone();
        let mut c = Carver::new(SR, CarveParams { mode: CarveMode::LowShelf, ..Default::default() });
        c.process(&key, &mut [&mut y]);
        let lo = band_level_db(&y[11025..], 80.0) - band_level_db(&x[11025..], 80.0);
        let hi = band_level_db(&y[11025..], 4000.0) - band_level_db(&x[11025..], 4000.0);
        eprintln!("CARVE low shelf: 80 Hz {lo:.2} dB, 4 kHz {hi:.2} dB");
        assert!((lo + 6.0).abs() < 0.6 && hi.abs() < 0.3, "{lo} {hi}");
        // high shelf on the presence band (corner 2 kHz), keyed by a 3 kHz voice: 8 kHz drops, 200 Hz stays
        let key = tone(3000.0, 0.25, n);
        let x: Vec<f32> = tone(200.0, 0.25, n).iter().zip(tone(8000.0, 0.25, n)).map(|(a, b)| a + b).collect();
        let mut y = x.clone();
        let p = CarveParams { mode: CarveMode::HighShelf, low_hz: 2000.0, high_hz: 4000.0, ..Default::default() };
        let mut c = Carver::new(SR, p);
        c.process(&key, &mut [&mut y]);
        let lo = band_level_db(&y[11025..], 200.0) - band_level_db(&x[11025..], 200.0);
        let hi = band_level_db(&y[11025..], 8000.0) - band_level_db(&x[11025..], 8000.0);
        eprintln!("CARVE high shelf: 200 Hz {lo:.2} dB, 8 kHz {hi:.2} dB");
        assert!((hi + 6.0).abs() < 0.6 && lo.abs() < 0.3, "{lo} {hi}");
    }

    #[test]
    fn every_carve_filter_is_minimum_phase_and_its_group_delay_is_reported() {
        let sr = SR as f64;
        let mut lines = Vec::new();
        for (mode, lo, hi) in [(CarveMode::Bell, 150.0f32, 400.0f32), (CarveMode::Bell, 2000.0, 4000.0), (CarveMode::LowShelf, 150.0, 400.0), (CarveMode::HighShelf, 2000.0, 4000.0)] {
            let p = CarveParams { mode, low_hz: lo, high_hz: hi, ..Default::default() };
            for cut in [3.0f64, 6.0, 12.0] {
                let (b, a) = carve_coeffs(&p, cut, sr);
                assert!(roots_inside(a) && roots_inside(b), "{mode:?} {cut}: not minimum phase");
                let f = carve_focus_hz(&p, sr);
                let gd = group_delay_samples(b, a, f, sr);
                assert!(gd.is_finite());
                if cut == 6.0 {
                    lines.push(format!("{mode:?} {lo}-{hi} Hz, 6 dB cut: group delay at {f:.0} Hz = {gd:+.2} samples ({:+.3} ms)", gd / sr * 1000.0));
                }
            }
        }
        // the numeric group delay matches the impulse response's phase slope (a sanity check of the helper)
        let p = CarveParams::default();
        let (b, a) = carve_coeffs(&p, 6.0, sr);
        let mut q = Bq::default();
        q.set(b, a);
        let ir: Vec<f64> = (0..1 << 15).map(|n| q.run(if n == 0 { 1.0 } else { 0.0 })).collect();
        let f = carve_focus_hz(&p, sr);
        let ph = |f: f64| {
            let w = -2.0 * PI * f / sr;
            let (re, im) = ir.iter().enumerate().fold((0.0, 0.0), |(r, i), (n, v)| (r + v * (w * n as f64).cos(), i + v * (w * n as f64).sin()));
            im.atan2(re)
        };
        let gd_ir = -(ph(f + 0.5) - ph(f - 0.5)) / (2.0 * PI / sr);
        let gd = group_delay_samples(b, a, f, sr);
        assert!((gd - gd_ir).abs() < 0.05, "{gd} vs {gd_ir}");
        eprintln!("CARVE_PHASE minimum-phase IIR\n{}", lines.join("\n"));
    }

    #[test]
    fn a_key_below_threshold_nulls_bit_exact_against_the_dry_signal() {
        // the key sounds in the band, but at -70 dBFS: under the -40 dB threshold, so no cut at all
        let n = 30000;
        let x: Vec<f32> = tone(250.0, 0.5, n).iter().zip(tone(9000.0, 0.2, n)).map(|(a, b)| a + b).collect();
        let key = tone(250.0, 0.000_316, n);
        for mode in [CarveMode::Bell, CarveMode::LowShelf, CarveMode::HighShelf] {
            let mut y = x.clone();
            let mut c = Carver::new(SR, CarveParams { mode, ..Default::default() });
            c.process(&key, &mut [&mut y]);
            let resid = x.iter().zip(&y).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
            eprintln!("CARVE_NULL {mode:?}: max |dry - out| = {resid:e} (bit-exact: {})", resid == 0.0);
            assert!(x.iter().zip(&y).all(|(a, b)| a.to_bits() == b.to_bits()), "{mode:?}");
        }
    }

    /// Analog prototypes (RBJ cookbook s-domain forms, unwarped frequency axis).
    fn analog_db(p: &CarveParams, cut: f64, f: f64, sr: f64) -> f64 {
        let a = 10f64.powf(-cut / 40.0);
        let (num, den): ((f64, f64), (f64, f64)) = match p.mode {
            CarveMode::Bell => {
                let (c, q) = centre_q(p.low_hz, p.high_hz, sr);
                let w = f / c;
                ((1.0 - w * w, w * a / q), (1.0 - w * w, w / (a * q)))
            }
            CarveMode::LowShelf => {
                let w = f / p.high_hz as f64;
                let s2 = std::f64::consts::SQRT_2;
                // A (s^2 + sqrt(A)/Q s + A) / (A s^2 + sqrt(A)/Q s + 1), Q = 1/sqrt2
                ((a * (a - w * w), a * a.sqrt() * s2 * w), (1.0 - a * w * w, a.sqrt() * s2 * w))
            }
            CarveMode::HighShelf => {
                let w = f / p.low_hz as f64;
                let s2 = std::f64::consts::SQRT_2;
                // A (A s^2 + sqrt(A)/Q s + 1) / (s^2 + sqrt(A)/Q s + A)
                ((a * (1.0 - a * w * w), a * a.sqrt() * s2 * w), (a - w * w, a.sqrt() * s2 * w))
            }
        };
        10.0 * ((num.0 * num.0 + num.1 * num.1) / (den.0 * den.0 + den.1 * den.1)).log10()
    }

    #[test]
    fn response_near_nyquist_tracks_the_analog_prototype_and_phase_is_reported() {
        let sr = SR as f64;
        let mut lines = Vec::new();
        for (mode, lo, hi) in [(CarveMode::Bell, 150.0f32, 400.0f32), (CarveMode::Bell, 2000.0, 4000.0), (CarveMode::LowShelf, 150.0, 400.0), (CarveMode::HighShelf, 2000.0, 4000.0)] {
            let p = CarveParams { mode, low_hz: lo, high_hz: hi, max_cut_db: 12.0, ..Default::default() };
            let cut = p.max_cut_db as f64;
            let (b, a) = carve_coeffs(&p, cut, sr);
            let err16 = magnitude_db(b, a, 16000.0, sr) - analog_db(&p, cut, 16000.0, sr);
            let f = carve_focus_hz(&p, sr);
            let err_focus = magnitude_db(b, a, f, sr) - analog_db(&p, cut, f, sr);
            let ph = phase_deg(b, a, f, sr);
            let gd = group_delay_samples(b, a, f, sr);
            lines.push(format!(
                "{mode:?} {lo}-{hi} Hz at max cut {cut} dB: |H| error vs analog {err16:+.3} dB at 16 kHz, {err_focus:+.4} dB at {f:.0} Hz; phase {ph:+.2} deg, group delay {gd:+.2} samples ({:+.3} ms) at {f:.0} Hz",
                gd / sr * 1000.0
            ));
            assert!(err_focus.abs() < 0.01, "{mode:?}: {err_focus}");
            assert!(err16.abs() < 1.0, "{mode:?}: {err16}");
        }
        eprintln!("CARVE_RESPONSE\n{}", lines.join("\n"));
    }

    #[test]
    fn a_dynamic_key_sweep_is_smooth_and_its_group_delay_stays_bounded() {
        // the key ramps on and off (5 ms ramps, 100 ms on / 100 ms off), fast attack/release, 12 dB max cut:
        // the cut (dB) is one-pole smoothed and the coefficients are refreshed every 16 samples
        let n = 88_200;
        let sr = SR as f64;
        let x: Vec<f32> = tone(250.0, 0.25, n).iter().zip(tone(4000.0, 0.25, n)).map(|(a, b)| a + b).collect();
        let key: Vec<f32> = tone(220.0, 0.5, n)
            .into_iter()
            .enumerate()
            .map(|(i, v)| {
                let t = i % 8820;
                let g = if t < 4410 { (t as f32 / 220.5).min(1.0) } else { (1.0 - (t - 4410) as f32 / 220.5).max(0.0) };
                v * g
            })
            .collect();
        let p = CarveParams { max_cut_db: 12.0, attack_ms: 5.0, release_ms: 50.0, ..Default::default() };
        let f = carve_focus_hz(&p, sr);
        let mut y = x.clone();
        let mut c = Carver::new(SR, p);
        let (mut cuts, mut gds) = (Vec::new(), Vec::new());
        for (kb, yb) in key.chunks(REFRESH).zip(y.chunks_mut(REFRESH)) {
            c.process(kb, &mut [yb]);
            cuts.push(c.applied);
            let (b, a) = carve_coeffs(&p, c.applied, sr);
            gds.push(group_delay_samples(b, a, f, sr));
        }
        // static reference: the same filter at a fixed max cut over the whole signal
        let (b, a) = carve_coeffs(&p, 12.0, sr);
        let mut q = Bq::default();
        q.set(b, a);
        let s: Vec<f32> = x.iter().map(|v| q.run(*v as f64) as f32).collect();
        let step = |v: &[f32]| v.windows(2).skip(1000).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
        let curv = |v: &[f32]| v.windows(3).skip(1000).fold(0.0f32, |m, w| m.max((w[2] - 2.0 * w[1] + w[0]).abs()));
        let (st_dry, st_static, st_dyn) = (step(&x), step(&s), step(&y));
        let (cv_dry, cv_static, cv_dyn) = (curv(&x), curv(&s), curv(&y));
        let gd_static = group_delay_samples(b, a, f, sr);
        let gd_max = gds.iter().fold(0.0f64, |m, g| m.max(g.abs()));
        let dgd = gds.windows(2).fold(0.0f64, |m, w| m.max((w[1] - w[0]).abs()));
        let dcut = cuts.windows(2).fold(0.0f64, |m, w| m.max((w[1] - w[0]).abs()));
        let cut_max = cuts.iter().cloned().fold(0.0, f64::max);
        let cut_min_late = cuts[cuts.len() / 2..].iter().cloned().fold(f64::MAX, f64::min);
        eprintln!(
            "CARVE_SWEEP cut range {cut_min_late:.3}..{cut_max:.3} dB, max cut change per 16-sample refresh {dcut:.3} dB\n\
             max |y[n]-y[n-1]|: dry {st_dry:.5}, static 12 dB {st_static:.5}, dynamic {st_dyn:.5} (dynamic/static {:.4})\n\
             max |2nd difference|: dry {cv_dry:.5}, static {cv_static:.5}, dynamic {cv_dyn:.5}\n\
             group delay at {f:.0} Hz: static max cut {gd_static:+.3} samples, dynamic max |gd| {gd_max:.3}, max change per refresh {dgd:.4} samples",
            st_dyn / st_static
        );
        assert!(cut_max > 11.9 && cut_min_late < cut_max - 3.0, "{cut_min_late} {cut_max}");
        assert!(st_dyn <= st_dry.max(st_static) * 1.01, "{st_dyn} vs {st_dry} / {st_static}");
        assert!(cv_dyn <= cv_dry.max(cv_static) * 1.05, "{cv_dyn} vs {cv_dry} / {cv_static}");
        assert!(gds.iter().all(|g| g.is_finite()) && gd_max <= gd_static.abs() * 1.001 + 1e-9, "{gd_max} {gd_static}");
        assert!(dgd < 0.25 * gd_static.abs(), "{dgd} vs {gd_static}");
    }
}
