//! Character effects from FL's rack: a vinyl / tape simulator (FL's Vintage
//! Chorus-and-crackle territory, iZotope Vinyl style) and a drawn-curve
//! Waveshaper (FL Fruity WaveShaper). Whole-buffer stereo processors.
//!
//! * vinyl: crackle (sparse, band-limited pops at `crackle` per second),
//!   surface hiss, wow (slow pitch drift) and flutter (fast pitch wobble)
//!   from a modulated delay line, an `age` knob that narrows the band like
//!   a worn record, and `mono` that folds the sides in. The lo-fi staple.
//! * waveshaper: the transfer curve is drawn as points 'x:y, x:y' (0..1,
//!   mirrored for the negative half when `symmetric`), or picked from
//!   presets; drive_db pushes the input into the curve.

use crate::dsp::{Biquad, BiquadKind, Rng, SR};
use crate::fx::{VinylFx, WaveshaperFx};
use std::f32::consts::PI;

pub fn check_vinyl(p: &VinylFx) -> Result<(), String> {
    if !(0.0..=200.0).contains(&p.crackle) {
        return Err(format!(
            "crackle {} outside 0..200 pops per second",
            p.crackle
        ));
    }
    if !(0.0..=50.0).contains(&p.wow_cents) || !(0.0..=30.0).contains(&p.flutter_cents) {
        return Err("wow_cents 0..50, flutter_cents 0..30".into());
    }
    if !(0.0..=1.0).contains(&p.age) || !(0.0..=1.0).contains(&p.mono) {
        return Err("age and mono are 0..1".into());
    }
    Ok(())
}

/// Delay (samples) whose sine modulation at `rate` Hz gives a peak pitch
/// deviation of `cents`: ratio - 1 = 2 pi f D / SR.
fn depth_samples(cents: f32, rate: f32) -> f32 {
    if cents <= 0.0 || rate <= 0.0 {
        return 0.0;
    }
    let dev = (2f32).powf(cents / 1200.0) - 1.0;
    dev * SR / (2.0 * PI * rate)
}

pub fn vinyl(p: &VinylFx, l: &mut [f32], r: &mut [f32]) {
    vinyl_at(p, l, r, 0)
}

/// Streaming core of `vinyl_at`: the wow/flutter delay line, the worn-record
/// band filters, the mono fold and the record's noise floor (crackle pops
/// and hiss), one stereo sample at a time. Pre-warmed by `offset` samples:
/// the delay slot counts from the song's start and the noise (which does not
/// depend on the audio) is run through the first `offset` samples, so a
/// stream started at a song offset matches the whole-song pass bit-for-bit.
pub struct VinylCore {
    bl: Vec<f32>,
    br: Vec<f32>,
    size: usize,
    w: usize,
    i: usize,
    base: f32,
    dw: f32,
    df: f32,
    wow_rate: f32,
    flut_rate: f32,
    ph0: f32,
    age: f32,
    f: [Biquad; 4],
    mono: f32,
    mix: f32,
    rng: Rng,
    pop_p: f32,
    pop_gain: f32,
    hiss_gain: f32,
    pop_env: f32,
    pop_amp: f32,
    pop_side: f32,
    pop_decay: f32,
    pop_bp: Biquad,
    hiss_l: Biquad,
    hiss_r: Biquad,
}

impl VinylCore {
    pub fn new(p: &VinylFx, offset: usize) -> Self {
        let mix = p.mix.clamp(0.0, 1.0);
        let wow_rate = p.wow_hz.clamp(0.05, 4.0);
        let flut_rate = p.flutter_hz.clamp(2.0, 20.0);
        let dw = depth_samples(p.wow_cents.clamp(0.0, 50.0), wow_rate);
        let df = depth_samples(p.flutter_cents.clamp(0.0, 30.0), flut_rate);
        let base = dw + df + 4.0;
        let size = (2.0 * base) as usize + 8;
        let age = p.age.clamp(0.0, 1.0);
        // a worn record: the band closes in from both ends
        let lo = 20.0 + 160.0 * age;
        let hi = 18000.0 - 12500.0 * age;
        let mut rng = Rng::new(p.seed as u64 ^ 0x5EED_u64);
        let ph0 = rng.f32();
        let mut c = VinylCore {
            bl: vec![0.0f32; size],
            br: vec![0.0f32; size],
            size,
            w: offset % size,
            i: offset,
            base,
            dw,
            df,
            wow_rate,
            flut_rate,
            ph0,
            age,
            f: [
                Biquad::new(BiquadKind::LowCut, lo, 0.707, 0.0),
                Biquad::new(BiquadKind::HighCut, hi, 0.707, 0.0),
                Biquad::new(BiquadKind::LowCut, lo, 0.707, 0.0),
                Biquad::new(BiquadKind::HighCut, hi, 0.707, 0.0),
            ],
            mono: p.mono.clamp(0.0, 1.0),
            mix,
            rng,
            // crackle: a pop is a short decaying noise burst through a band-pass
            pop_p: p.crackle.clamp(0.0, 200.0) / SR,
            pop_gain: crate::dsp::db_to_gain(p.crackle_db),
            hiss_gain: if p.hiss_db <= -90.0 {
                0.0
            } else {
                crate::dsp::db_to_gain(p.hiss_db)
            },
            pop_env: 0.0,
            pop_amp: 0.0,
            pop_side: 0.0,
            pop_decay: (-1.0 / (0.0008 * SR)).exp(),
            pop_bp: Biquad::new(BiquadKind::Bandpass, 2500.0, 0.8, 0.0),
            hiss_l: Biquad::new(BiquadKind::HighCut, 9000.0, 0.707, 0.0),
            hiss_r: Biquad::new(BiquadKind::HighCut, 9000.0, 0.707, 0.0),
        };
        for _ in 0..offset {
            c.noise();
        }
        c
    }

    /// The noise floor of the record for one sample: (pop l, pop r, hiss l, hiss r).
    fn noise(&mut self) -> (f32, f32, f32, f32) {
        if self.pop_p > 0.0 && self.rng.f32() < self.pop_p {
            self.pop_env = 1.0;
            self.pop_amp = self.pop_gain * (0.3 + 0.7 * self.rng.f32() * self.rng.f32());
            self.pop_side = self.rng.bipolar() * 0.6;
        }
        let mut pop = 0.0;
        if self.pop_env > 1e-4 {
            pop = self.pop_bp.process(self.rng.bipolar()) * self.pop_env * self.pop_amp * 3.0;
            self.pop_env *= self.pop_decay;
        }
        let (hl, hr) = if self.hiss_gain > 0.0 {
            (
                self.hiss_l.process(self.rng.bipolar()) * self.hiss_gain,
                self.hiss_r.process(self.rng.bipolar()) * self.hiss_gain,
            )
        } else {
            (0.0, 0.0)
        };
        (
            pop * (1.0 - self.pop_side),
            pop * (1.0 + self.pop_side),
            hl,
            hr,
        )
    }

    pub fn step(&mut self, xl_in: f32, xr_in: f32) -> (f32, f32) {
        let i = self.i;
        let t = i as f32 / SR;
        let d = self.base
            + self.dw * (2.0 * PI * self.wow_rate * t + self.ph0 * 2.0 * PI).sin()
            + self.df * (2.0 * PI * self.flut_rate * t).sin();
        self.bl[self.w] = xl_in;
        self.br[self.w] = xr_in;
        // fractional read d samples back
        let size = self.size;
        let pos = self.w as f32 - d + size as f32 * 2.0;
        let i0 = pos.floor() as usize % size;
        let i1 = (i0 + 1) % size;
        let fr = pos - pos.floor();
        let (mut xl, mut xr) = if i >= self.base as usize + 2 {
            (
                self.bl[i0] + (self.bl[i1] - self.bl[i0]) * fr,
                self.br[i0] + (self.br[i1] - self.br[i0]) * fr,
            )
        } else {
            (xl_in, xr_in)
        };
        self.w = (self.w + 1) % size;
        if self.age > 0.0 {
            let a = self.f[0].process(xl);
            xl = self.f[1].process(a);
            let b = self.f[2].process(xr);
            xr = self.f[3].process(b);
        }
        if self.mono > 0.0 {
            let m = 0.5 * (xl + xr);
            xl += (m - xl) * self.mono;
            xr += (m - xr) * self.mono;
        }
        // noise floor of the record
        let (pl, pr, hl, hr) = self.noise();
        let wl = xl + pl + hl;
        let wr = xr + pr + hr;
        self.i += 1;
        (
            xl_in * (1.0 - self.mix) + wl * self.mix,
            xr_in * (1.0 - self.mix) + wr * self.mix,
        )
    }
}

/// `vinyl` for a buffer that starts `offset` samples into the song: wow and
/// flutter read the song position, the delay line's slot counts from the
/// song's start, and the record's noise (crackle and hiss, which do not
/// depend on the audio) is run through the first `offset` samples, so a
/// window matches the same samples of a whole-song pass.
pub fn vinyl_at(p: &VinylFx, l: &mut [f32], r: &mut [f32], offset: usize) {
    let n = l.len().min(r.len());
    if n == 0 {
        return;
    }
    let mut core = VinylCore::new(p, offset);
    for i in 0..n {
        let (yl, yr) = core.step(l[i], r[i]);
        l[i] = yl;
        r[i] = yr;
    }
}

pub const WAVESHAPER_PRESETS: &[&str] = &[
    "custom",
    "soft",
    "hard",
    "tube",
    "fold",
    "sine",
    "steps",
    "crush_curve",
];

fn preset_points(name: &str) -> Option<Vec<(f32, f32)>> {
    Some(match name {
        "soft" => vec![
            (0.0, 0.0),
            (0.25, 0.32),
            (0.5, 0.58),
            (0.75, 0.8),
            (1.0, 0.9),
        ],
        "hard" => vec![(0.0, 0.0), (0.6, 0.85), (0.7, 0.9), (1.0, 0.9)],
        "tube" => vec![
            (0.0, 0.0),
            (0.2, 0.3),
            (0.45, 0.6),
            (0.7, 0.78),
            (1.0, 0.85),
        ],
        "fold" => vec![
            (0.0, 0.0),
            (0.35, 0.8),
            (0.55, 0.3),
            (0.75, 0.7),
            (1.0, 0.2),
        ],
        "sine" => (0..=16)
            .map(|k| {
                let x = k as f32 / 16.0;
                (x, (x * PI * 0.75).sin() * 0.9)
            })
            .collect(),
        "steps" => (0..=16)
            .map(|k| {
                let x = k as f32 / 16.0;
                (x, (x * 6.0).round() / 6.0 * 0.9)
            })
            .collect(),
        "crush_curve" => vec![
            (0.0, 0.0),
            (0.1, 0.3),
            (0.2, 0.3),
            (0.4, 0.6),
            (0.6, 0.6),
            (0.8, 0.85),
            (1.0, 0.85),
        ],
        _ => return None,
    })
}

/// Parse 'x:y, x:y' points (0..1), sorted, anchored at (0,0) when missing.
pub fn parse_points(s: &str) -> Result<Vec<(f32, f32)>, String> {
    let mut v = Vec::new();
    for part in s.split(',').map(str::trim).filter(|x| !x.is_empty()) {
        let (a, b) = part
            .split_once(':')
            .ok_or_else(|| format!("point '{part}' is not x:y"))?;
        let x: f32 = a.trim().parse().map_err(|_| format!("x in '{part}'"))?;
        let y: f32 = b.trim().parse().map_err(|_| format!("y in '{part}'"))?;
        if !(0.0..=1.0).contains(&x) || !(-1.0..=1.0).contains(&y) {
            return Err(format!("point '{part}': x 0..1, y -1..1"));
        }
        v.push((x, y));
    }
    if v.len() < 2 {
        return Err("curve needs at least 2 points 'x:y, x:y'".into());
    }
    v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    if v[0].0 > 0.0 {
        v.insert(0, (0.0, 0.0));
    }
    Ok(v)
}

fn curve_of(p: &WaveshaperFx) -> Vec<(f32, f32)> {
    if !p.curve.trim().is_empty() {
        if let Ok(v) = parse_points(&p.curve) {
            return v;
        }
    }
    preset_points(&p.preset).unwrap_or_else(|| preset_points("soft").unwrap())
}

pub fn check_waveshaper(p: &WaveshaperFx) -> Result<(), String> {
    if !p.curve.trim().is_empty() {
        parse_points(&p.curve)?;
    } else if preset_points(&p.preset).is_none() {
        return Err(format!(
            "preset '{}' ({}), or draw curve 'x:y, x:y'",
            p.preset,
            WAVESHAPER_PRESETS.join(", ")
        ));
    }
    if !(-24.0..=36.0).contains(&p.drive_db) {
        return Err(format!("drive_db {} outside -24..36", p.drive_db));
    }
    Ok(())
}

fn shape(c: &[(f32, f32)], x: f32, symmetric: bool) -> f32 {
    let (s, a) = if x < 0.0 && symmetric {
        (-1.0, -x)
    } else {
        (1.0, x)
    };
    let a = a.clamp(if symmetric { 0.0 } else { -1.0 }, 1.0);
    if !symmetric && a < 0.0 {
        // asymmetric: the negative half passes through softly
        return a / (1.0 - 0.5 * a);
    }
    let k = c.partition_point(|p| p.0 <= a).clamp(1, c.len() - 1);
    let (x0, y0) = c[k - 1];
    let (x1, y1) = c[k];
    let u = if x1 > x0 { (a - x0) / (x1 - x0) } else { 0.0 };
    s * (y0 + (y1 - y0) * u.clamp(0.0, 1.0))
}

pub fn waveshaper(p: &WaveshaperFx, l: &mut [f32], r: &mut [f32]) {
    let mut core = WaveshaperCore::new(p);
    for (ch, buf) in [l, r].into_iter().enumerate() {
        for x in buf.iter_mut() {
            *x = core.step(ch, *x);
        }
    }
}

/// The waveshaper's per-sample state machine (the DC-removal low cut per
/// channel), shared by the offline pass and the RT StreamFx arm so a
/// chunked render is the full render bit for bit.
pub struct WaveshaperCore {
    c: Vec<(f32, f32)>,
    g: f32,
    out: f32,
    mix: f32,
    symmetric: bool,
    dc: [Biquad; 2],
}

impl WaveshaperCore {
    pub fn new(p: &WaveshaperFx) -> Self {
        WaveshaperCore {
            c: curve_of(p),
            g: crate::dsp::db_to_gain(p.drive_db),
            out: crate::dsp::db_to_gain(p.output_db),
            mix: p.mix.clamp(0.0, 1.0),
            symmetric: p.symmetric,
            // the curve's DC (asymmetric shapes) is removed with a gentle low cut
            dc: [
                Biquad::new(BiquadKind::LowCut, 12.0, 0.707, 0.0),
                Biquad::new(BiquadKind::LowCut, 12.0, 0.707, 0.0),
            ],
        }
    }

    #[inline]
    pub fn step(&mut self, ch: usize, x: f32) -> f32 {
        let y = self.dc[ch].process(shape(&self.c, x * self.g, self.symmetric)) * self.out;
        x * (1.0 - self.mix) + y * self.mix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI * f * i as f32 / SR).sin() * 0.5)
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn vinyl_wow_bends_pitch_and_crackle_pops() {
        let n = SR as usize * 2;
        // silence in: crackle + hiss only
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        vinyl(
            &VinylFx {
                crackle: 20.0,
                hiss_db: -60.0,
                wow_cents: 0.0,
                flutter_cents: 0.0,
                ..Default::default()
            },
            &mut l,
            &mut r,
        );
        let peaks = l.iter().filter(|x| x.abs() > 0.02).count();
        assert!(peaks > 0, "no pops");
        assert!(rms(&l) < 0.05 && l.iter().all(|x| x.is_finite()));
        // a tone through wow: zero-crossing rate drifts over time
        let mut a = tone(440.0, n);
        let mut b = a.clone();
        vinyl(
            &VinylFx {
                crackle: 0.0,
                hiss_db: -120.0,
                wow_cents: 30.0,
                wow_hz: 1.0,
                flutter_cents: 0.0,
                age: 0.0,
                ..Default::default()
            },
            &mut a,
            &mut b,
        );
        let zc = |s: &[f32]| s.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let q = SR as usize / 4;
        let counts: Vec<usize> = (0..8).map(|k| zc(&a[k * q..(k + 1) * q])).collect();
        let (mn, mx) = (*counts.iter().min().unwrap(), *counts.iter().max().unwrap());
        assert!(mx > mn, "wow moved nothing: {counts:?}");
        assert!(check_vinyl(&VinylFx {
            age: 2.0,
            ..Default::default()
        })
        .is_err());
    }

    #[test]
    fn waveshaper_follows_the_drawn_curve() {
        let mut l = vec![0.25f32, 0.5, -0.5, 1.0];
        let mut r = l.clone();
        let p = WaveshaperFx {
            curve: "0:0, 0.5:0.8, 1:1".into(),
            ..Default::default()
        };
        let c = curve_of(&p);
        assert!((shape(&c, 0.25, true) - 0.4).abs() < 1e-4);
        assert!((shape(&c, -0.5, true) + 0.8).abs() < 1e-4);
        waveshaper(&p, &mut l, &mut r);
        assert!(l.iter().all(|x| x.is_finite()));
        assert!(check_waveshaper(&WaveshaperFx {
            curve: "0.5".into(),
            ..Default::default()
        })
        .is_err());
        assert!(check_waveshaper(&WaveshaperFx {
            preset: "nope".into(),
            ..Default::default()
        })
        .is_err());
        for pr in WAVESHAPER_PRESETS.iter().filter(|x| **x != "custom") {
            assert!(
                check_waveshaper(&WaveshaperFx {
                    preset: pr.to_string(),
                    ..Default::default()
                })
                .is_ok(),
                "{pr}"
            );
        }
    }
}
