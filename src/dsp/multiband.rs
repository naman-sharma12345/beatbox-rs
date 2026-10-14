//! Multiband dynamics: an LR4 crossover tree (`super::crossover`) with a
//! feed-forward compressor per band.
//!
//! The compressor follows Giannoulis, Massberg & Reiss, "Digital Dynamic
//! Range Compressor Design - A Tutorial and Analysis" (JAES 60(6), 2012),
//! clean-room from the paper's equations:
//! - level in dB: x_G = 20 log10 |x| (peak, channels linked by max),
//! - soft-knee gain computer with threshold T, ratio R, knee width W:
//!   y_G = x_G                                   if 2(x_G - T) < -W
//!   y_G = x_G + (1/R - 1)(x_G - T + W/2)^2 / 2W  if |2(x_G - T)| <= W
//!   y_G = T + (x_G - T) / R                     if 2(x_G - T) > W
//! - the gain reduction x_L = x_G - y_G (dB, >= 0) smoothed by the paper's
//!   log-domain "smooth decoupled" peak detector:
//!   y_1 = max(x_L, a_R y_1 + (1 - a_R) x_L); y_L = a_A y_L + (1 - a_A) y_1,
//!   a = exp(-1 / (tau fs)),
//! - gain = 10^((M - y_L) / 20) with makeup M.
//!
//! Per band: threshold, ratio, attack, release, knee, makeup, solo, bypass.
//! A band that never crosses into the knee keeps gain exactly 1.0, so with
//! every threshold above the signal the output is the crossover's allpass
//! sum of the input (unity magnitude), bit for bit. No latency; no
//! allocation in `process`.

use super::crossover::{Crossover, MAX_BANDS};

/// One band's compressor settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandParams {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
    pub makeup_db: f32,
    /// Hear only the solo'd bands (any band solo'd mutes the others).
    pub solo: bool,
    /// Pass the band with unity gain (still phase-matched to the others).
    pub bypass: bool,
}

impl Default for BandParams {
    fn default() -> Self {
        BandParams { threshold_db: -18.0, ratio: 2.0, attack_ms: 10.0, release_ms: 150.0, knee_db: 6.0, makeup_db: 0.0, solo: false, bypass: false }
    }
}

/// The Giannoulis-Massberg-Reiss static curve: gain reduction in dB (>= 0)
/// for a level `x_g` in dB.
#[inline]
pub fn gain_reduction_db(x_g: f32, t: f32, r: f32, w: f32) -> f32 {
    let r = r.max(1.0);
    let over2 = 2.0 * (x_g - t);
    let y_g = if over2 < -w {
        x_g
    } else if w > 0.0 && over2.abs() <= w {
        let d = x_g - t + w / 2.0;
        x_g + (1.0 / r - 1.0) * d * d / (2.0 * w)
    } else {
        t + (x_g - t) / r
    };
    (x_g - y_g).max(0.0)
}

#[derive(Clone, Copy, Debug)]
struct BandState {
    a_att: f32,
    a_rel: f32,
    y1: f32,
    yl: f32,
    makeup: f32,
}

/// Streaming multiband compressor for up to 8 linked channels.
pub struct MultibandDynamics {
    channels: usize,
    bands: usize,
    params: [BandParams; MAX_BANDS],
    state: [BandState; MAX_BANDS],
    xo: Vec<Crossover>,
    sr: f32,
    max_gr: [f32; MAX_BANDS],
}

fn coef(ms: f32, sr: f32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * sr)).exp()
    }
}

impl MultibandDynamics {
    /// Allocates (one crossover per channel); call it off the audio thread.
    /// `crossovers.len() + 1` bands; missing band params take the defaults.
    pub fn new(sr: f32, channels: usize, crossovers: &[f32], bands: &[BandParams]) -> Self {
        let channels = channels.clamp(1, 8);
        let xo0 = Crossover::new(crossovers, sr);
        let nb = xo0.bands();
        let mut m = MultibandDynamics {
            channels,
            bands: nb,
            params: [BandParams::default(); MAX_BANDS],
            state: [BandState { a_att: 0.0, a_rel: 0.0, y1: 0.0, yl: 0.0, makeup: 1.0 }; MAX_BANDS],
            xo: vec![xo0; channels],
            sr,
            max_gr: [0.0; MAX_BANDS],
        };
        for b in 0..nb {
            m.set_band(b, bands.get(b).copied().unwrap_or_default());
        }
        m
    }

    pub fn bands(&self) -> usize {
        self.bands
    }

    /// Samples of delay: none (IIR crossover, no lookahead).
    pub fn latency(&self) -> usize {
        0
    }

    /// Change one band's settings (no allocation; safe on the audio thread).
    pub fn set_band(&mut self, b: usize, p: BandParams) {
        if b >= self.bands {
            return;
        }
        self.params[b] = p;
        let s = &mut self.state[b];
        s.a_att = coef(p.attack_ms, self.sr);
        s.a_rel = coef(p.release_ms, self.sr);
        s.makeup = if p.makeup_db == 0.0 { 1.0 } else { 10f32.powf(p.makeup_db / 20.0) };
    }

    /// Largest gain reduction per band (dB) since the last call; resets it.
    pub fn take_max_gain_reduction_db(&mut self) -> [f32; MAX_BANDS] {
        std::mem::replace(&mut self.max_gr, [0.0; MAX_BANDS])
    }

    pub fn reset(&mut self) {
        for x in self.xo.iter_mut() {
            x.reset();
        }
        for s in self.state.iter_mut() {
            s.y1 = 0.0;
            s.yl = 0.0;
        }
        self.max_gr = [0.0; MAX_BANDS];
    }

    /// Process linked channels in place (missing channels are silence,
    /// extra ones untouched).
    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        let nch = chans.len().min(self.channels);
        let n = chans[..nch].iter().map(|c| c.len()).min().unwrap_or(0);
        let nb = self.bands;
        let any_solo = self.params[..nb].iter().any(|p| p.solo);
        let mut split = [[0.0f64; MAX_BANDS]; 8];
        for i in 0..n {
            for c in 0..self.channels {
                let x = if c < nch { chans[c][i] as f64 } else { 0.0 };
                self.xo[c].process(x, &mut split[c]);
            }
            let mut out = [0.0f64; 8];
            for b in 0..nb {
                let p = self.params[b];
                if any_solo && !p.solo {
                    continue;
                }
                let g = if p.bypass {
                    1.0f64
                } else {
                    let peak = (0..self.channels).fold(0.0f64, |m, c| m.max(split[c][b].abs())) as f32;
                    let x_g = if peak > 1e-12 { 20.0 * peak.log10() } else { -240.0 };
                    let x_l = gain_reduction_db(x_g, p.threshold_db, p.ratio, p.knee_db);
                    let s = &mut self.state[b];
                    s.y1 = x_l.max(s.a_rel * s.y1 + (1.0 - s.a_rel) * x_l);
                    s.yl = s.a_att * s.yl + (1.0 - s.a_att) * s.y1;
                    if s.yl > self.max_gr[b] {
                        self.max_gr[b] = s.yl;
                    }
                    let g = if s.yl == 0.0 { 1.0 } else { 10f32.powf(-s.yl / 20.0) };
                    (g * s.makeup) as f64
                };
                for c in 0..self.channels {
                    out[c] += split[c][b] * g;
                }
            }
            for (c, ch) in chans.iter_mut().enumerate().take(nch) {
                ch[i] = out[c] as f32;
            }
        }
    }
}

/// Offline helper: run whole buffers, returns the max gain reduction per band.
pub fn process_offline(chans: &mut [Vec<f32>], sr: f32, crossovers: &[f32], bands: &[BandParams]) -> Vec<f32> {
    let mut m = MultibandDynamics::new(sr, chans.len(), crossovers, bands);
    {
        let mut refs: Vec<&mut [f32]> = chans.iter_mut().map(|c| c.as_mut_slice()).collect();
        m.process(&mut refs);
    }
    m.take_max_gain_reduction_db()[..m.bands()].to_vec()
}

#[cfg(test)]
mod tests {
    use super::super::crossover::tests::band_irs;
    use super::super::crossover::Biquad;
    use super::*;
    use std::f32::consts::PI;

    const SR: f32 = 44_100.0;
    const XO: [f32; 2] = [150.0, 2500.0];

    fn noise(n: usize, amp: f32, seed: u32) -> Vec<f32> {
        let mut s = seed.max(1);
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                amp * ((s as f32 / u32::MAX as f32) * 2.0 - 1.0)
            })
            .collect()
    }

    #[test]
    fn the_static_curve_matches_the_paper() {
        // below the knee: no reduction; above: (x - T)(1 - 1/R); at T: (1 - 1/R) W / 8
        assert_eq!(gain_reduction_db(-40.0, -20.0, 4.0, 6.0), 0.0);
        assert!((gain_reduction_db(-8.0, -20.0, 4.0, 6.0) - 9.0).abs() < 1e-5);
        let at_t = gain_reduction_db(-20.0, -20.0, 4.0, 6.0);
        assert!((at_t - (1.0 - 0.25) * 9.0 / 12.0).abs() < 1e-5, "{at_t}");
        // the curve is continuous at both knee edges
        for x in [-23.0f32, -17.0] {
            let (a, b) = (gain_reduction_db(x - 1e-3, -20.0, 4.0, 6.0), gain_reduction_db(x + 1e-3, -20.0, 4.0, 6.0));
            // 2e-3 dB apart at slope <= 1 - 1/R: a jump would be far larger
            assert!((a - b).abs() < 2.5e-3, "{x}: {a} {b}");
        }
        // hard knee
        assert!((gain_reduction_db(-10.0, -20.0, 2.0, 0.0) - 5.0).abs() < 1e-5);
    }

    #[test]
    fn null_test_thresholds_at_0_dbfs_give_the_allpass_sum() {
        let x = noise(30000, 0.25, 9); // -12 dBFS peak: every band stays under the knee
        let bands = [BandParams { threshold_db: 0.0, ..Default::default() }; 3];
        let mut ch = vec![x.clone(), x.clone()];
        let gr = process_offline(&mut ch, SR, &XO, &bands);
        assert!(gr.iter().all(|g| *g == 0.0), "{gr:?}");
        // reference: the input through AP(150) then AP(2500), computed independently
        let (mut a, mut b) = (Biquad::butter_ap(150.0, SR as f64), Biquad::butter_ap(2500.0, SR as f64));
        let refy: Vec<f64> = x.iter().map(|v| b.run(a.run(*v as f64))).collect();
        let err = ch[0].iter().zip(&refy).fold(0.0f64, |m, (y, r)| m.max((*y as f64 - r).abs()));
        let rms = (refy.iter().map(|v| v * v).sum::<f64>() / refy.len() as f64).sqrt();
        let null_db = 20.0 * (err / rms).log10();
        eprintln!("MULTIBAND_NULL max |out - allpass(in)| = {err:.2e} ({null_db:.1} dB re signal RMS)");
        assert!(null_db < -120.0, "{null_db}");
        assert_eq!(ch[0], ch[1]);
    }

    #[test]
    fn a_loud_low_band_is_compressed_and_the_others_are_not() {
        // 60 Hz at -3 dBFS plus 1 kHz and 8 kHz at -30 dBFS
        let n = 44100;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / SR;
                0.7 * (2.0 * PI * 60.0 * t).sin() + 0.03 * (2.0 * PI * 1000.0 * t).sin() + 0.03 * (2.0 * PI * 8000.0 * t).sin()
            })
            .collect();
        let p = BandParams { threshold_db: -20.0, ratio: 4.0, attack_ms: 5.0, release_ms: 100.0, knee_db: 0.0, ..Default::default() };
        let mut ch = vec![x.clone()];
        let gr = process_offline(&mut ch, SR, &XO, &[p; 3]);
        eprintln!("MULTIBAND_GR low/mid/high {:.2} / {:.2} / {:.2} dB", gr[0], gr[1], gr[2]);
        // low band peaks ~ -3 dB: (17 dB over) * 0.75 = 12.8 dB of reduction
        assert!(gr[0] > 11.0 && gr[0] < 14.0, "{gr:?}");
        assert!(gr[1] < 0.5 && gr[2] < 0.5, "{gr:?}");
    }

    #[test]
    fn solo_and_bypass_do_what_they_say() {
        let x = noise(20000, 0.5, 4);
        let irs = band_irs(&XO, SR, 20000);
        // solo the mid band with bypass on everything: output = mid band of the input
        let mut bands = [BandParams { bypass: true, ..Default::default() }; 3];
        bands[1].solo = true;
        let mut ch = vec![x.clone()];
        process_offline(&mut ch, SR, &XO, &bands);
        let mid: Vec<f64> = (0..x.len()).map(|n| (0..=n).map(|k| irs[1][k] * x[n - k] as f64).sum()).take(2000).collect();
        let err = mid.iter().zip(&ch[0]).fold(0.0f64, |m, (r, y)| m.max((r - *y as f64).abs()));
        assert!(err < 1e-5, "{err}");
        // bypass on every band with a crushing threshold: still the allpass sum, no reduction
        let bands = [BandParams { threshold_db: -60.0, ratio: 20.0, bypass: true, ..Default::default() }; 3];
        let mut ch = vec![x.clone()];
        let gr = process_offline(&mut ch, SR, &XO, &bands);
        assert!(gr.iter().all(|g| *g == 0.0));
        let tot = ch[0].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
        assert!((10.0 * tot.log10()).abs() < 0.1, "{tot}");
    }

    #[test]
    fn attack_and_release_follow_their_time_constants() {
        // a step from silence into a hot low tone, then back to quiet
        let n = 44100;
        let x: Vec<f32> = (0..n).map(|i| if (4410..22050).contains(&i) { 0.9 * (2.0 * PI * 80.0 * i as f32 / SR).sin() } else { 0.0 }).collect();
        let p = BandParams { threshold_db: -30.0, ratio: 10.0, attack_ms: 20.0, release_ms: 200.0, knee_db: 0.0, ..Default::default() };
        let mut m = MultibandDynamics::new(SR, 1, &XO, &[p; 3]);
        let mut y = x.clone();
        // measure the low band's gain reduction trace through the state after each block of 441
        let mut trace = Vec::new();
        for blk in y.chunks_mut(441) {
            m.process(&mut [blk]);
            trace.push(m.state[0].yl);
        }
        let full = trace[30..49].iter().cloned().fold(0.0f32, f32::max);
        // after 20 ms (2 blocks) of attack the reduction is near 1 - 1/e of its steady value or more
        let a20 = trace[11];
        assert!(a20 > 0.5 * full && a20 < full, "attack {a20} of {full}");
        // 200 ms after the tone stops it has fallen to about 1/e
        let r200 = trace[49 + 20];
        assert!(r200 < 0.5 * full && r200 > 0.2 * full, "release {r200} of {full}");
    }
}
