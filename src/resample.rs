//! Kaiser-windowed-sinc resampling (beta 9, 512-phase table with linear
//! interpolation between phases; cutoff lowered to 0.94 x ratio when
//! decimating so pitched-up samples and rate conversion do not alias).
//!
//! Ported from SoundCraft (https://github.com/storytold/soundcraft,
//! `crates/dsp/src/offline.rs`: `SincTable`, `resample_channel`,
//! `resample_ratio`, `bessel_i0`), Copyright (c) 2026 ArtCraft Team and the
//! SoundCraft contributors, MIT OR Apache-2.0 (used here under MIT).
//! Adapted to edition 2021, mono slices and a selectable kernel width.
//! See THIRD_PARTY_NOTICES.md.

use std::f64::consts::PI;

const SINC_RES: usize = 512;
/// Zero crossings per side for offline-quality conversion.
pub const ZC_HQ: f64 = 32.0;
/// Narrower kernel used for per-note sample pitching (speed).
pub const ZC_NOTE: f64 = 12.0;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..64 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-12 {
            break;
        }
    }
    sum
}

/// Precomputed one-sided windowed-sinc kernel for a given cutoff (fraction of input Nyquist).
pub struct SincTable {
    table: Vec<f32>,
    half_width: f64,
}

impl SincTable {
    pub fn new(cutoff: f64, zero_crossings: f64) -> Self {
        let cutoff = cutoff.clamp(0.01, 1.0);
        let half_width = zero_crossings / cutoff;
        let n = (half_width * SINC_RES as f64).ceil() as usize + 2;
        let beta = 9.0;
        let i0b = bessel_i0(beta);
        let table = (0..n)
            .map(|i| {
                let t = i as f64 / SINC_RES as f64;
                let x = t / half_width;
                if x >= 1.0 {
                    return 0.0;
                }
                let arg = PI * cutoff * t;
                let sinc = if arg.abs() < 1e-12 {
                    1.0
                } else {
                    arg.sin() / arg
                };
                let w = bessel_i0(beta * (1.0 - x * x).max(0.0).sqrt()) / i0b;
                (cutoff * sinc * w) as f32
            })
            .collect();
        SincTable { table, half_width }
    }

    #[inline]
    fn at(&self, t: f64) -> f32 {
        let pos = t.abs() * SINC_RES as f64;
        let i = pos as usize;
        let f = (pos - i as f64) as f32;
        match (self.table.get(i), self.table.get(i + 1)) {
            (Some(a), Some(b)) => a + (b - a) * f,
            (Some(a), None) => *a,
            _ => 0.0,
        }
    }
}

/// Table suited to a conversion `ratio` (output rate / input rate).
pub fn table_for(ratio: f64, zero_crossings: f64) -> SincTable {
    let cutoff = if ratio < 1.0 { ratio * 0.94 } else { 0.97 };
    SincTable::new(cutoff, zero_crossings)
}

/// `out_len` samples where output sample `n` reads input time `n / ratio`.
pub fn resample_with(x: &[f32], ratio: f64, out_len: usize, table: &SincTable) -> Vec<f32> {
    let len = x.len() as i64;
    let hw = table.half_width;
    (0..out_len)
        .map(|n| {
            let t = n as f64 / ratio;
            let lo = (t - hw).ceil().max(0.0) as i64;
            let hi = ((t + hw).floor() as i64).min(len - 1);
            let mut acc = 0.0f32;
            let mut j = lo;
            while j <= hi {
                acc += x[j as usize] * table.at(t - j as f64);
                j += 1;
            }
            acc
        })
        .collect()
}

/// High-quality sample-rate conversion from `from` Hz to `to` Hz.
pub fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == 0 || to == 0 || from == to {
        return x.to_vec();
    }
    let ratio = f64::from(to) / f64::from(from);
    let out_len = (x.len() as f64 * ratio).round() as usize;
    resample_with(x, ratio, out_len, &table_for(ratio, ZC_HQ))
}

/// Read `n` samples of `x` at playback `rate` (2.0 = an octave up), band-limited.
pub fn pitch_read(x: &[f32], rate: f64, n: usize) -> Vec<f32> {
    if (rate - 1.0).abs() < 1e-9 {
        let mut v: Vec<f32> = x.iter().take(n).copied().collect();
        v.resize(n, 0.0);
        return v;
    }
    let ratio = 1.0 / rate.max(1e-3);
    resample_with(x, ratio, n, &table_for(ratio, ZC_NOTE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI as PI32;

    fn sine(f: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI32 * f * i as f32 / sr).sin())
            .collect()
    }

    fn tone_power(x: &[f32], f: f32, sr: f32) -> f32 {
        // Goertzel-style single-bin power with a Hann window
        let n = x.len();
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (2.0 * PI32 * i as f32 / n as f32).cos();
            let ph = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / sr as f64;
            re += (v * w) as f64 * ph.cos();
            im += (v * w) as f64 * ph.sin();
        }
        ((re * re + im * im) / (n as f64 * n as f64)) as f32
    }

    #[test]
    fn rate_conversion_keeps_a_sine() {
        let x = sine(1000.0, 48000.0, 48000);
        let y = resample(&x, 48000, 44100);
        assert_eq!(y.len(), 44100);
        let mid = &y[1000..43000];
        let pk = mid.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((pk - 1.0).abs() < 0.01, "peak {pk}");
    }

    #[test]
    fn pitching_up_does_not_alias() {
        // 15 kHz pitched up an octave would fold to 44.1k-30k = 14.1 kHz with
        // linear interpolation; the sinc kernel removes it
        let sr = 44100.0;
        let x = sine(15000.0, sr, 44100);
        let y = pitch_read(&x, 2.0, 20000);
        let lin: Vec<f32> = (0..20000)
            .map(|i| crate::dsp::lerp_read(&x, i as f32 * 2.0))
            .collect();
        let alias_sinc = tone_power(&y[500..19500], 14100.0, sr);
        let alias_lin = tone_power(&lin[500..19500], 14100.0, sr);
        assert!(alias_lin > 1e-3, "linear should alias: {alias_lin}");
        assert!(
            alias_sinc < alias_lin * 1e-3,
            "sinc {alias_sinc} vs linear {alias_lin}"
        );
        // an in-band tone survives pitching
        let z = pitch_read(&sine(1000.0, sr, 44100), 1.5, 20000);
        assert!(tone_power(&z[500..19500], 1500.0, sr) > 0.05);
    }
}
