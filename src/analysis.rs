//! "Ears" for the AI: loudness, peaks, spectral balance, stereo image and
//! plain-English mix suggestions, for the full mix and each track.

use crate::dsp::{gain_to_db, SR};
use crate::render::Mix;
use serde::Serialize;
use std::f32::consts::PI;

/// In-place iterative radix-2 FFT. `re.len()` must be a power of two.
pub fn fft(re: &mut [f32], im: &mut [f32]) {
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
        let ang = -2.0 * PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let a = start + k;
                let b = a + len / 2;
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
        }
        len <<= 1;
    }
}

const FRAME: usize = 4096;

/// Average power spectrum (FRAME/2 bins) of a mono signal.
pub fn power_spectrum(x: &[f32]) -> Vec<f32> {
    let mut acc = vec![0.0f32; FRAME / 2];
    if x.len() < FRAME {
        let mut padded = x.to_vec();
        padded.resize(FRAME, 0.0);
        return power_spectrum_frames(&padded, &mut acc, 1);
    }
    let hop = FRAME;
    let frames = ((x.len() - FRAME) / hop + 1).min(400);
    let stride = ((x.len() - FRAME) / hop + 1) / frames.max(1);
    let mut used = 0;
    for f in 0..frames {
        let off = f * stride.max(1) * hop;
        if off + FRAME > x.len() {
            break;
        }
        frame_power(&x[off..off + FRAME], &mut acc);
        used += 1;
    }
    for a in acc.iter_mut() {
        *a /= used.max(1) as f32;
    }
    acc
}

fn power_spectrum_frames(x: &[f32], acc: &mut [f32], _n: usize) -> Vec<f32> {
    frame_power(&x[..FRAME], acc);
    acc.to_vec()
}

fn frame_power(x: &[f32], acc: &mut [f32]) {
    let mut re: Vec<f32> = x
        .iter()
        .enumerate()
        .map(|(i, s)| s * (0.5 - 0.5 * (2.0 * PI * i as f32 / (FRAME - 1) as f32).cos()))
        .collect();
    let mut im = vec![0.0f32; FRAME];
    fft(&mut re, &mut im);
    for k in 0..FRAME / 2 {
        acc[k] += re[k] * re[k] + im[k] * im[k];
    }
}

pub const BANDS: [(&str, f32, f32); 6] = [
    ("sub", 20.0, 60.0),
    ("bass", 60.0, 250.0),
    ("low_mid", 250.0, 1000.0),
    ("mid", 1000.0, 4000.0),
    ("presence", 4000.0, 8000.0),
    ("air", 8000.0, 20000.0),
];

#[derive(Serialize, Clone, Debug)]
pub struct BandShare {
    pub band: &'static str,
    pub range_hz: String,
    pub percent: f32,
}

#[derive(Serialize, Clone, Debug)]
pub struct Stats {
    pub peak_dbfs: f32,
    pub rms_dbfs: f32,
    pub crest_db: f32,
    pub clipped_samples: usize,
    pub spectral_centroid_hz: f32,
    pub bands: Vec<BandShare>,
}

pub fn stats(l: &[f32], r: &[f32]) -> Stats {
    let n = l.len().min(r.len()).max(1);
    let mut peak = 0.0f32;
    let mut sum = 0.0f64;
    let mut clipped = 0;
    for i in 0..l.len().min(r.len()) {
        let a = l[i].abs().max(r[i].abs());
        peak = peak.max(a);
        if a >= 0.999 {
            clipped += 1;
        }
        sum += (l[i] as f64).powi(2) + (r[i] as f64).powi(2);
    }
    let rms = (sum / (2 * n) as f64).sqrt() as f32;
    let mono: Vec<f32> = l.iter().zip(r.iter()).map(|(a, b)| 0.5 * (a + b)).collect();
    let spec = power_spectrum(&mono);
    let bin_hz = SR / FRAME as f32;
    let total: f32 = spec.iter().skip(1).sum::<f32>().max(1e-12);
    let mut centroid = 0.0;
    for (k, p) in spec.iter().enumerate() {
        centroid += k as f32 * bin_hz * p;
    }
    let bands = BANDS
        .iter()
        .map(|(name, lo, hi)| {
            let e: f32 = spec
                .iter()
                .enumerate()
                .filter(|(k, _)| {
                    let f = *k as f32 * bin_hz;
                    f >= *lo && f < *hi
                })
                .map(|(_, p)| p)
                .sum();
            BandShare {
                band: name,
                range_hz: format!("{lo:.0}-{hi:.0}"),
                percent: (e / total * 1000.0).round() / 10.0,
            }
        })
        .collect();
    let peak_db = gain_to_db(peak);
    let rms_db = gain_to_db(rms);
    Stats {
        peak_dbfs: round1(peak_db),
        rms_dbfs: round1(rms_db),
        crest_db: round1(peak_db - rms_db),
        clipped_samples: clipped,
        spectral_centroid_hz: (centroid / total).round(),
        bands,
    }
}

fn round1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

/// Stereo correlation: +1 mono, 0 wide/uncorrelated, negative = phase problems.
pub fn correlation(l: &[f32], r: &[f32]) -> f32 {
    let (mut lr, mut ll, mut rr) = (0.0f64, 0.0f64, 0.0f64);
    for (a, b) in l.iter().zip(r.iter()) {
        lr += (*a as f64) * (*b as f64);
        ll += (*a as f64).powi(2);
        rr += (*b as f64).powi(2);
    }
    if ll < 1e-12 || rr < 1e-12 {
        return 1.0;
    }
    round1((lr / (ll * rr).sqrt()) as f32 * 10.0) / 10.0
}

#[derive(Serialize, Debug)]
pub struct TrackReport {
    pub track: String,
    pub rms_dbfs: f32,
    pub peak_dbfs: f32,
    pub energy_share_percent: f32,
    pub spectral_centroid_hz: f32,
    pub dominant_band: &'static str,
}

#[derive(Serialize, Debug)]
pub struct Report {
    pub seconds: f32,
    pub master: Stats,
    pub stereo_correlation: f32,
    pub tracks: Vec<TrackReport>,
    pub suggestions: Vec<String>,
    pub score: u32,
}

fn band(s: &Stats, name: &str) -> f32 {
    s.bands
        .iter()
        .find(|b| b.band == name)
        .map(|b| b.percent)
        .unwrap_or(0.0)
}

pub fn analyze(mix: &Mix) -> Report {
    let master = stats(&mix.left, &mix.right);
    let corr = correlation(&mix.left, &mix.right);
    let energies: Vec<f64> = mix
        .stems
        .iter()
        .map(|s| {
            s.left
                .iter()
                .chain(s.right.iter())
                .map(|x| (*x as f64).powi(2))
                .sum()
        })
        .collect();
    let etotal: f64 = energies.iter().sum::<f64>().max(1e-12);
    let mut tracks = Vec::new();
    for (stem, e) in mix.stems.iter().zip(energies.iter()) {
        let st = stats(&stem.left, &stem.right);
        let dom = st
            .bands
            .iter()
            .max_by(|a, b| a.percent.partial_cmp(&b.percent).unwrap())
            .map(|b| b.band)
            .unwrap_or("mid");
        tracks.push(TrackReport {
            track: stem.name.clone(),
            rms_dbfs: st.rms_dbfs,
            peak_dbfs: st.peak_dbfs,
            energy_share_percent: ((e / etotal) as f32 * 1000.0).round() / 10.0,
            spectral_centroid_hz: st.spectral_centroid_hz,
            dominant_band: dom,
        });
    }

    let mut sug = Vec::new();
    let mut score: i32 = 100;
    if master.rms_dbfs < -60.0 {
        sug.push("The mix is silent. Add notes to the pattern(s) used by the arrangement.".into());
        return Report {
            seconds: mix.seconds,
            master,
            stereo_correlation: corr,
            tracks,
            suggestions: sug,
            score: 0,
        };
    }
    if master.clipped_samples > 0 || master.peak_dbfs > -0.1 {
        sug.push(format!("Clipping risk ({} samples at full scale). Lower track volumes or keep a limiter on the master.", master.clipped_samples));
        score -= 15;
    }
    if master.rms_dbfs < -20.0 {
        sug.push(format!("Quiet mix (RMS {:.1} dBFS). Raise master_volume_db or add master compression; streaming-ready beats sit around -12 to -9 dBFS RMS.", master.rms_dbfs));
        score -= 10;
    } else if master.rms_dbfs > -7.0 {
        sug.push(format!(
            "Very loud (RMS {:.1} dBFS); transients will be squashed. Back off master gain 2-3 dB.",
            master.rms_dbfs
        ));
        score -= 8;
    }
    if master.crest_db < 6.0 {
        sug.push("Over-compressed: crest factor under 6 dB. Ease compressor ratios or master gain for more punch.".into());
        score -= 8;
    }
    let low = band(&master, "sub") + band(&master, "bass");
    let lowmid = band(&master, "low_mid");
    let high = band(&master, "presence") + band(&master, "air");
    if low > 75.0 {
        sug.push(format!("Bass-heavy ({low:.0}% of energy below 250 Hz). Lower the 808/bass 2-4 dB or high-pass non-bass tracks at ~150 Hz."));
        score -= 10;
    } else if low < 25.0 {
        sug.push(format!(
            "Thin low end ({low:.0}% below 250 Hz). Add a sub/808 or boost kick and bass."
        ));
        score -= 10;
    }
    if lowmid > 30.0 {
        sug.push(format!("Muddy low-mids ({lowmid:.0}% in 250-1000 Hz). Cut 300-500 Hz on pads/keys with an eq (mid_db -3) or a highpass filter."));
        score -= 8;
    }
    if high < 1.5 {
        sug.push(
            "Dull top end. Add hats/shakers, open a filter, or boost high_db on the master EQ."
                .into(),
        );
        score -= 8;
    } else if high > 25.0 {
        sug.push(format!(
            "Harsh/bright ({high:.0}% above 4 kHz). Lower hats or low-pass bright synths."
        ));
        score -= 6;
    }
    if corr < 0.0 {
        sug.push("Phase problems: stereo correlation is negative, the mix will collapse in mono. Reduce width amount.".into());
        score -= 12;
    } else if corr > 0.97 {
        sug.push("Almost mono. Pan hats/percs, add reverb or a width effect on pads/leads.".into());
        score -= 5;
    }
    // masking: two loud tracks with the same dominant low band
    let lows: Vec<&TrackReport> = tracks
        .iter()
        .filter(|t| {
            (t.dominant_band == "sub" || t.dominant_band == "bass") && t.energy_share_percent > 15.0
        })
        .collect();
    if lows.len() >= 2 {
        let names: Vec<&str> = lows.iter().map(|t| t.track.as_str()).collect();
        let kick = names.iter().find(|n| n.contains("kick"));
        let other = names.iter().find(|n| !n.contains("kick"));
        if let (Some(k), Some(o)) = (kick, other) {
            sug.push(format!("'{k}' and '{o}' fight in the low end. Add a sidechain effect on '{o}' with source '{k}'."));
        } else {
            sug.push(format!(
                "Low-end masking between {}. Give each its own octave or sidechain one.",
                names.join(" and ")
            ));
        }
        score -= 6;
    }
    for t in &tracks {
        if t.energy_share_percent > 65.0 && tracks.len() > 2 {
            sug.push(format!(
                "'{}' dominates the mix ({:.0}% of energy). Turn it down a few dB.",
                t.track, t.energy_share_percent
            ));
            score -= 5;
        }
    }
    if sug.is_empty() {
        sug.push("Balanced mix. Try arrangement contrast next: a breakdown pattern without drums, then a drop.".into());
    }
    Report {
        seconds: mix.seconds,
        master,
        stereo_correlation: corr,
        tracks,
        suggestions: sug,
        score: score.clamp(0, 100) as u32,
    }
}

/// Min/max peaks for drawing a waveform with `n` columns.
pub fn waveform_peaks(x: &[f32], n: usize) -> Vec<(f32, f32)> {
    if x.is_empty() || n == 0 {
        return Vec::new();
    }
    let chunk = (x.len() as f32 / n as f32).max(1.0);
    (0..n)
        .map(|i| {
            let a = (i as f32 * chunk) as usize;
            let b = (((i + 1) as f32 * chunk) as usize)
                .min(x.len())
                .max(a + 1)
                .min(x.len());
            if a >= x.len() {
                return (0.0, 0.0);
            }
            x[a..b]
                .iter()
                .fold((0.0f32, 0.0f32), |(lo, hi), s| (lo.min(*s), hi.max(*s)))
        })
        .collect()
}

/// Log-spaced spectrum in dB with `bins` points from 20 Hz to 20 kHz.
pub fn log_spectrum_db(x: &[f32], bins: usize) -> Vec<f32> {
    let spec = power_spectrum(x);
    let bin_hz = SR / FRAME as f32;
    (0..bins)
        .map(|i| {
            let f0 = 20.0 * 1000f32.powf(i as f32 / bins as f32);
            let f1 = 20.0 * 1000f32.powf((i + 1) as f32 / bins as f32);
            let k0 = ((f0 / bin_hz) as usize).min(spec.len() - 1);
            let k1 = ((f1 / bin_hz) as usize).clamp(k0 + 1, spec.len());
            let p = spec[k0..k1].iter().sum::<f32>() / (k1 - k0) as f32;
            10.0 * (p.max(1e-12)).log10()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_finds_sine() {
        let n = 1024;
        let mut re: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 64.0 * i as f32 / n as f32).sin())
            .collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im);
        let mags: Vec<f32> = (0..n / 2)
            .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt())
            .collect();
        let max_k = mags
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(max_k, 64);
    }

    #[test]
    fn low_sine_lands_in_bass_band() {
        let x: Vec<f32> = (0..44100)
            .map(|i| (2.0 * PI * 110.0 * i as f32 / SR).sin() * 0.5)
            .collect();
        let s = stats(&x, &x);
        assert!(band(&s, "bass") > 90.0, "{:?}", s.bands);
        assert!((s.peak_dbfs - -6.0).abs() < 0.2);
        assert_eq!(correlation(&x, &x), 1.0);
    }
}
