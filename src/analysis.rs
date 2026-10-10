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
    /// RMS while the track actually sounds (sparse parts read honestly).
    pub active_rms_db: f32,
    /// Share of the song where the track sounds (%).
    pub active_percent: f32,
}

/// Broadcast-style loudness and safety metrics of a stereo signal.
#[derive(Serialize, Clone, Debug, Default)]
pub struct Loudness {
    /// Integrated loudness, ITU-R BS.1770 K-weighted with gating (LUFS).
    pub integrated_lufs: f32,
    /// Loudest 3 s window (short-term max, LUFS).
    pub short_term_max_lufs: f32,
    /// Loudness range estimate (LU): spread of short-term loudness (10th..95th pct).
    pub loudness_range_lu: f32,
    /// Inter-sample peak via 4x oversampling (dBTP).
    pub true_peak_dbtp: f32,
    /// Mean sample value per channel (DC offset, linear).
    pub dc_offset: [f32; 2],
    /// Level change when folded to mono (dB, negative = loss / phase cancellation).
    pub mono_fold_db: f32,
    /// Percent of 400 ms blocks below -60 dBFS.
    pub silent_percent: f32,
    /// Seconds of silence before the first sound.
    pub leading_silence_s: f32,
}

struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The two-stage K-weighting filter of BS.1770 for sample rate `fs`.
fn k_weighting(fs: f64) -> [Biquad; 2] {
    let (f0, g, q) = (
        1681.974450955533f64,
        3.999843853973347f64,
        0.7071752369554196f64,
    );
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    let (f0, q) = (38.13547087602444f64, 0.5003270373238773f64);
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    [shelf, hp]
}

fn lufs_of(ms: f64) -> f64 {
    -0.691 + 10.0 * ms.max(1e-20).log10()
}

/// Inter-sample (true) peak of one channel, 4x oversampled with a
/// Hann-windowed sinc interpolator (32 taps per phase).
pub fn true_peak(x: &[f32]) -> f32 {
    const UP: usize = 4;
    const HALF: isize = 16;
    let mut phases: Vec<Vec<f32>> = Vec::new();
    for ph in 1..UP {
        let frac = ph as f32 / UP as f32;
        let mut h = Vec::new();
        for k in -HALF + 1..=HALF {
            let t = k as f32 - frac;
            let sinc = if t.abs() < 1e-6 {
                1.0
            } else {
                (PI * t).sin() / (PI * t)
            };
            let w = 0.5 + 0.5 * (PI * t / HALF as f32).cos();
            h.push(sinc * w);
        }
        let sum: f32 = h.iter().sum();
        phases.push(h.into_iter().map(|v| v / sum).collect());
    }
    let n = x.len() as isize;
    let mut peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    for i in 0..n {
        // only interpolate around loud-ish samples (cheap and exact enough)
        if x[i as usize].abs() < peak * 0.5 {
            continue;
        }
        for h in &phases {
            let mut acc = 0.0f32;
            for (j, c) in h.iter().enumerate() {
                let idx = i + (j as isize - HALF + 1);
                if idx >= 0 && idx < n {
                    acc += c * x[idx as usize];
                }
            }
            peak = peak.max(acc.abs());
        }
    }
    peak
}

pub fn loudness(l: &[f32], r: &[f32]) -> Loudness {
    loudness_at(l, r, SR)
}

/// BS.1770 loudness of audio at sample rate `sr` (K-weighting, 100 ms hops
/// and 400 ms blocks are all derived from `sr`, so a 48 kHz file is
/// measured at its own rate rather than as if it were 44.1 kHz).
pub fn loudness_at(l: &[f32], r: &[f32], sr: f32) -> Loudness {
    let n = l.len().min(r.len());
    if n == 0 {
        return Loudness {
            integrated_lufs: -70.0,
            short_term_max_lufs: -70.0,
            true_peak_dbtp: -120.0,
            ..Default::default()
        };
    }
    // K-weighted squares, accumulated per 100 ms
    let hop = (sr * 0.1) as usize;
    let mut kw = [k_weighting(sr as f64), k_weighting(sr as f64)];
    let mut hops: Vec<f64> = Vec::new();
    let mut acc = 0.0f64;
    for i in 0..n {
        let a0 = kw[0][0].run(l[i] as f64);
        let a = kw[0][1].run(a0);
        let b0 = kw[1][0].run(r[i] as f64);
        let b = kw[1][1].run(b0);
        acc += a * a + b * b;
        if (i + 1) % hop == 0 {
            hops.push(acc / hop as f64);
            acc = 0.0;
        }
    }
    if hops.is_empty() {
        hops.push(acc / n as f64);
    }
    let window = |w: usize| -> Vec<f64> {
        if hops.len() < w {
            return vec![hops.iter().sum::<f64>() / hops.len() as f64];
        }
        (0..=hops.len() - w)
            .map(|k| hops[k..k + w].iter().sum::<f64>() / w as f64)
            .collect()
    };
    // integrated: 400 ms blocks, absolute gate -70, relative gate -10 LU
    let blocks = window(4);
    let abs: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|m| lufs_of(*m) > -70.0)
        .collect();
    let integrated = if abs.is_empty() {
        -70.0
    } else {
        let rel = lufs_of(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
        let g: Vec<f64> = abs.iter().copied().filter(|m| lufs_of(*m) > rel).collect();
        lufs_of(g.iter().sum::<f64>() / g.len().max(1) as f64)
    };
    let short = window(30);
    let mut st: Vec<f64> = short
        .iter()
        .map(|m| lufs_of(*m))
        .filter(|v| *v > -70.0)
        .collect();
    st.sort_by(|a, b| a.total_cmp(b));
    let st_max = st.last().copied().unwrap_or(-70.0);
    let lra = if st.len() >= 2 {
        let at = |q: f64| st[((st.len() - 1) as f64 * q).round() as usize];
        at(0.95) - at(0.10)
    } else {
        0.0
    };
    // silence (plain dBFS, unweighted) on 400 ms blocks
    let blk = (sr * 0.4) as usize;
    let mut silent = 0usize;
    let mut nblk = 0usize;
    let mut lead = None;
    for (bi, c) in (0..n).step_by(blk).enumerate() {
        let e = (c + blk).min(n);
        let pk = (c..e).fold(0.0f32, |m, i| m.max(l[i].abs()).max(r[i].abs()));
        nblk += 1;
        if pk < 0.001 {
            silent += 1;
        } else if lead.is_none() {
            lead = Some(bi);
        }
    }
    let lead_s = match lead {
        Some(bi) => {
            let s0 = bi * blk;
            let first = (s0..n)
                .find(|&i| l[i].abs().max(r[i].abs()) >= 0.001)
                .unwrap_or(s0);
            first as f32 / sr
        }
        None => n as f32 / sr,
    };
    let (mut sl, mut sr_, mut sm, mut el, mut er) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        sl += l[i] as f64;
        sr_ += r[i] as f64;
        let m = 0.5 * (l[i] as f64 + r[i] as f64);
        sm += m * m;
        el += (l[i] as f64).powi(2);
        er += (r[i] as f64).powi(2);
    }
    let stereo_ms = 0.5 * (el + er) / n as f64;
    let mono_fold = if stereo_ms < 1e-14 {
        0.0
    } else {
        10.0 * ((sm / n as f64).max(1e-20) / stereo_ms).log10()
    };
    let tp = true_peak(&l[..n]).max(true_peak(&r[..n]));
    let r2 = |x: f64| ((x * 100.0).round() / 100.0) as f32;
    Loudness {
        integrated_lufs: r2(integrated.max(-70.0)),
        short_term_max_lufs: r2(st_max),
        loudness_range_lu: r2(lra),
        true_peak_dbtp: r2(gain_to_db(tp) as f64),
        dc_offset: [
            ((sl / n as f64) * 1e5).round() as f32 / 1e5,
            ((sr_ / n as f64) * 1e5).round() as f32 / 1e5,
        ],
        mono_fold_db: r2(mono_fold),
        silent_percent: r2(100.0 * silent as f64 / nblk.max(1) as f64),
        leading_silence_s: (lead_s * 100.0).round() / 100.0,
    }
}

#[derive(Serialize, Debug)]
pub struct Report {
    pub seconds: f32,
    pub master: Stats,
    pub loudness: Loudness,
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
    let loud = loudness(&mix.left, &mix.right);
    let corr = correlation(&mix.left, &mix.right);
    let infos: Vec<crate::render::TrackInfo> = if mix.track_info.is_empty() {
        mix.stems
            .iter()
            .map(|s| crate::render::track_info(&s.name, &s.left, &s.right))
            .collect()
    } else {
        mix.track_info.clone()
    };
    let etotal: f64 = infos.iter().map(|t| t.energy).sum::<f64>().max(1e-12);
    let mut tracks = Vec::new();
    for t in &infos {
        let st = &t.stats;
        let dom = st
            .bands
            .iter()
            .max_by(|a, b| {
                a.percent
                    .partial_cmp(&b.percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|b| b.band)
            .unwrap_or("mid");
        tracks.push(TrackReport {
            track: t.name.clone(),
            rms_dbfs: st.rms_dbfs,
            peak_dbfs: st.peak_dbfs,
            energy_share_percent: ((t.energy / etotal) as f32 * 1000.0).round() / 10.0,
            spectral_centroid_hz: st.spectral_centroid_hz,
            dominant_band: dom,
            active_rms_db: t.active_rms_db,
            active_percent: t.active_percent,
        });
    }

    let mut sug = Vec::new();
    let mut score: i32 = 100;
    if master.rms_dbfs < -60.0 {
        sug.push("The mix is silent. Add notes to the pattern(s) used by the arrangement.".into());
        return Report {
            seconds: mix.seconds,
            master,
            loudness: loud,
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
    if loud.true_peak_dbtp > -0.5 {
        sug.push(format!("True peak {:.1} dBTP: inter-sample overs will clip on streaming encoders. Lower the master limiter ceiling to -1 dB (tweak_effect on master).", loud.true_peak_dbtp));
        score -= 5;
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
    let mut low_ok = false;
    if lows.len() >= 2 {
        let names: Vec<&str> = lows.iter().map(|t| t.track.as_str()).collect();
        let kick = names.iter().find(|n| n.contains("kick"));
        let other = names.iter().find(|n| !n.contains("kick"));
        if let (Some(k), Some(o)) = (kick, other) {
            // measure the real (post-sidechain) overlap instead of guessing
            let env = |n: &str| {
                infos
                    .iter()
                    .find(|t| t.name == n)
                    .map(|t| t.low_env.clone())
                    .unwrap_or_default()
            };
            let ov = crate::render::low_overlap(&env(k), &env(o));
            if ov > 0.12 {
                sug.push(format!("'{k}' and '{o}' fight in the low end: {:.0}% of '{o}' low-end energy sounds under the '{k}'. Add (or deepen) a sidechain effect on '{o}' with source '{k}', or shorten '{o}' notes under kicks.", ov * 100.0));
            } else {
                low_ok = true;
            }
        } else {
            sug.push(format!(
                "Low-end masking between {}. Give each its own octave or sidechain one.",
                names.join(" and ")
            ));
        }
        if !low_ok {
            score -= 6;
        }
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
        loudness: loud,
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

#[cfg(test)]
mod loudness_tests {
    use super::*;

    #[test]
    fn lufs_and_true_peak_of_a_sine() {
        // -20 dBFS 1 kHz sine in both channels reads about -20 LUFS
        let a = 0.1f32;
        let x: Vec<f32> = (0..(SR as usize * 4))
            .map(|i| a * (2.0 * PI * 1000.0 * i as f32 / SR).sin())
            .collect();
        let l = loudness(&x, &x);
        assert!(
            (l.integrated_lufs + 20.0).abs() < 0.6,
            "{}",
            l.integrated_lufs
        );
        assert!(
            (l.true_peak_dbtp + 20.0).abs() < 0.3,
            "{}",
            l.true_peak_dbtp
        );
        assert!(l.mono_fold_db.abs() < 0.01);
        // a sine at fs/4 sampled off-peak: sample peak under-reads, true peak doesn't
        let y: Vec<f32> = (0..8192)
            .map(|i| (2.0 * PI * (SR / 4.0) * i as f32 / SR + PI / 4.0).sin())
            .collect();
        let sp = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(sp < 0.72);
        assert!(true_peak(&y) > 0.97, "true peak {}", true_peak(&y));
        // inverted channels fold to nothing
        let inv: Vec<f32> = x.iter().map(|v| -v).collect();
        assert!(loudness(&x, &inv).mono_fold_db < -20.0);
    }
}
