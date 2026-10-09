//! Offline audio editing and listening: transient / onset detection and
//! slicing, WSOLA time-stretch, pitch shift, reverse / normalize / fade /
//! trim, tempo and key estimation, and loudness/spectrum/dynamics
//! "profiles" used to compare a mix against a reference track.

use crate::analysis;
use crate::dsp::{gain_to_db, Fft, SR};
use crate::midi_ops;
use crate::theory;
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

const FRAME: usize = 1024;
const HOP: usize = 256;

fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos())
        .collect()
}

/// Spectral-flux onset strength, one value per HOP samples.
pub fn onset_envelope(x: &[f32]) -> Vec<f32> {
    if x.len() < FRAME {
        return Vec::new();
    }
    let fft = Fft::new(FRAME);
    let w = hann(FRAME);
    let mut prev = vec![0.0f32; FRAME / 2];
    let mut out = Vec::with_capacity(x.len() / HOP);
    let mut re = vec![0.0f32; FRAME];
    let mut im = vec![0.0f32; FRAME];
    let mut off = 0;
    while off + FRAME <= x.len() {
        for i in 0..FRAME {
            re[i] = x[off + i] * w[i];
            im[i] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        let mut flux = 0.0f32;
        for k in 1..FRAME / 2 {
            let m = (re[k] * re[k] + im[k] * im[k]).sqrt();
            let lm = (1.0 + 100.0 * m).ln();
            flux += (lm - prev[k]).max(0.0);
            prev[k] = lm;
        }
        out.push(flux);
        off += HOP;
    }
    out
}

/// Onset sample positions (peak-picked flux with an adaptive threshold).
/// `sensitivity` 0..1 (higher = more onsets), `min_gap_ms` between hits.
pub fn onsets(x: &[f32], sensitivity: f32, min_gap_ms: f32) -> Vec<usize> {
    let env = onset_envelope(x);
    if env.is_empty() {
        return if x.is_empty() { vec![] } else { vec![0] };
    }
    let mean = env.iter().sum::<f32>() / env.len() as f32;
    let k = 1.6 - sensitivity.clamp(0.0, 1.0) * 1.3;
    let gap = ((min_gap_ms.max(5.0) * 0.001 * SR) as usize / HOP).max(1);
    let mut out: Vec<usize> = Vec::new();
    let mut last: Option<usize> = None;
    for i in 0..env.len().saturating_sub(1) {
        let lo = i.saturating_sub(8);
        let hi = (i + 8).min(env.len());
        let local = env[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
        let thr = local.max(mean) * k + mean * 0.1;
        let before = if i == 0 { 0.0 } else { env[i - 1] };
        if env[i] > thr && env[i] >= before && env[i] >= env[i + 1] {
            if let Some(l) = last {
                if i - l < gap {
                    continue;
                }
            }
            last = Some(i);
            out.push(i * HOP);
        }
    }
    // refine to the energy rise inside the frame
    out.iter()
        .map(|&o| {
            let end = (o + FRAME).min(x.len());
            let peak = x[o..end].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            (o..end)
                .find(|&j| x[j].abs() >= peak * 0.3)
                .unwrap_or(o)
                .saturating_sub(64)
        })
        .collect()
}

/// Tempo estimate from onset autocorrelation: (bpm, confidence 0..1).
pub fn estimate_bpm(x: &[f32]) -> (f32, f32) {
    let env = onset_envelope(x);
    if env.len() < 64 {
        return (0.0, 0.0);
    }
    let mean = env.iter().sum::<f32>() / env.len() as f32;
    let e: Vec<f32> = env.iter().map(|v| (v - mean).max(0.0)).collect();
    let fps = SR / HOP as f32;
    let lag_for = |bpm: f32| 60.0 * fps / bpm;
    let ac = |lag: f32| -> f32 {
        let l0 = lag.floor() as usize;
        let f = lag - l0 as f32;
        let mut s = 0.0;
        for i in 0..e.len().saturating_sub(l0 + 1) {
            s += e[i] * (e[i + l0] * (1.0 - f) + e[i + l0 + 1] * f);
        }
        s
    };
    let zero: f32 = e.iter().map(|v| v * v).sum::<f32>().max(1e-9);
    let mut best = (0.0f32, f32::MIN);
    let mut bpm = 60.0f32;
    let mut scores = Vec::new();
    while bpm <= 200.0 {
        // pulse-train comb: beat, 2 beats, half beat
        let s = ac(lag_for(bpm)) + 0.5 * ac(lag_for(bpm / 2.0)) + 0.25 * ac(lag_for(bpm * 2.0));
        // prefer common tempi (log-gaussian around 115)
        let w = (-0.5 * ((bpm / 115.0).log2() / 0.9).powi(2)).exp();
        let sc = s * w;
        scores.push(sc);
        if sc > best.1 {
            best = (bpm, sc);
        }
        bpm += 0.5;
    }
    let mean_sc = scores.iter().sum::<f32>() / scores.len() as f32;
    let conf = ((best.1 - mean_sc) / zero).clamp(0.0, 1.0);
    ((best.0 * 2.0).round() / 2.0, (conf * 3.0).min(1.0))
}

/// Pitch-class energy (C..B) from the spectrum between ~55 Hz and 5 kHz.
pub fn chroma(x: &[f32]) -> [f32; 12] {
    const N: usize = 8192;
    let mut c = [0.0f32; 12];
    if x.len() < N {
        return c;
    }
    let fft = Fft::new(N);
    let w = hann(N);
    let frames = (x.len() - N) / (N / 2) + 1;
    let stride = (frames / 200).max(1);
    let mut re = vec![0.0f32; N];
    let mut im = vec![0.0f32; N];
    for f in (0..frames).step_by(stride) {
        let off = f * N / 2;
        for i in 0..N {
            re[i] = x[off + i] * w[i];
            im[i] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        for k in 10..N / 2 {
            let hz = k as f32 * SR / N as f32;
            if hz > 5000.0 {
                break;
            }
            let m = (re[k] * re[k] + im[k] * im[k]).sqrt();
            let midi = 69.0 + 12.0 * (hz / 440.0).log2();
            let pc = (midi.round() as i32).rem_euclid(12) as usize;
            // weight by closeness to the semitone centre
            let d = (midi - midi.round()).abs();
            c[pc] += m * (1.0 - d);
        }
    }
    c
}

/// Best key guesses for audio: [(name like "A minor", confidence)].
pub fn estimate_key(x: &[f32]) -> Vec<(String, f32)> {
    let ch = chroma(x);
    if ch.iter().all(|v| *v == 0.0) {
        return vec![];
    }
    midi_ops::key_from_chroma(&ch)
        .into_iter()
        .take(3)
        .map(|(pc, mode, r)| {
            (
                format!("{} {mode}", theory::NOTE_NAMES[pc as usize]),
                (r * 100.0).round() / 100.0,
            )
        })
        .collect()
}

// ---------------- editing ----------------

/// WSOLA time stretch: `factor` > 1 = longer/slower, pitch unchanged.
pub fn time_stretch(x: &[f32], factor: f32) -> Vec<f32> {
    let factor = factor.clamp(0.25, 4.0);
    if (factor - 1.0).abs() < 1e-3 || x.len() < 4096 {
        if x.len() < 4096 {
            return crate::samples::resample(x, 1.0, factor);
        }
        return x.to_vec();
    }
    let n = 1764; // 40 ms window
    let hop_out = n / 2;
    let hop_in = hop_out as f32 / factor;
    let tol = 441; // +-10 ms search
    let w = hann(n);
    let out_len = (x.len() as f32 * factor) as usize;
    let mut out = vec![0.0f32; out_len + n];
    let mut norm = vec![0.0f32; out_len + n];
    let mut prev_end: isize = 0; // where the natural continuation would read
    let mut k = 0usize;
    loop {
        let o = k * hop_out;
        if o >= out_len {
            break;
        }
        let nominal = (k as f32 * hop_in) as isize;
        let pos = if k == 0 {
            0
        } else {
            // find the offset whose start best matches the continuation
            let mut best = nominal;
            let mut bv = f32::MIN;
            let probe = 512;
            let lo = (nominal - tol as isize).max(0);
            let hi = (nominal + tol as isize).min(x.len() as isize - n as isize - 1);
            let mut c = lo;
            while c <= hi {
                let mut s = 0.0f32;
                for j in (0..probe).step_by(2) {
                    let a = x
                        .get((prev_end + j as isize) as usize)
                        .copied()
                        .unwrap_or(0.0);
                    s += a * x[(c + j as isize) as usize];
                }
                if s > bv {
                    bv = s;
                    best = c;
                }
                c += 4;
            }
            best
        };
        if pos < 0 || pos as usize + n > x.len() {
            break;
        }
        for j in 0..n {
            out[o + j] += x[pos as usize + j] * w[j];
            norm[o + j] += w[j];
        }
        prev_end = pos + hop_out as isize;
        k += 1;
    }
    for (v, g) in out.iter_mut().zip(norm.iter()) {
        if *g > 1e-3 {
            *v /= g;
        }
    }
    out.truncate(out_len);
    out
}

/// Pitch shift by semitones keeping duration (stretch + resample).
pub fn pitch_shift(x: &[f32], semitones: f32) -> Vec<f32> {
    let r = 2f32.powf(semitones.clamp(-24.0, 24.0) / 12.0);
    let stretched = time_stretch(x, r);
    crate::samples::resample(&stretched, r, 1.0)
}

pub fn reverse(x: &mut [f32]) {
    x.reverse();
}

/// Scale so the peak sits at `peak_db` dBFS. Returns applied gain (dB).
pub fn normalize(x: &mut [f32], peak_db: f32) -> f32 {
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if peak <= 1e-9 {
        return 0.0;
    }
    let g = 10f32.powf(peak_db.min(0.0) / 20.0) / peak;
    x.iter_mut().for_each(|v| *v *= g);
    gain_to_db(g)
}

pub fn fade(x: &mut [f32], in_ms: f32, out_ms: f32) {
    let n = x.len();
    let fi = ((in_ms.max(0.0) * 0.001 * SR) as usize).min(n);
    let fo = ((out_ms.max(0.0) * 0.001 * SR) as usize).min(n);
    for i in 0..fi {
        x[i] *= (i as f32 / fi as f32).powi(2);
    }
    for i in 0..fo {
        x[n - 1 - i] *= (i as f32 / fo as f32).powi(2);
    }
}

/// Remove leading / trailing audio below `threshold_db`. Returns (cut_start, cut_end) samples.
pub fn trim(x: &mut Vec<f32>, threshold_db: f32) -> (usize, usize) {
    let t = 10f32.powf(threshold_db / 20.0);
    let first = x.iter().position(|v| v.abs() >= t).unwrap_or(0);
    let last = x
        .iter()
        .rposition(|v| v.abs() >= t)
        .map(|i| i + 1)
        .unwrap_or(x.len());
    let start = first.saturating_sub((0.002 * SR) as usize);
    let end = (last + (0.01 * SR) as usize).min(x.len());
    let cut = (start, x.len() - end);
    *x = x[start..end].to_vec();
    cut
}

// ---------------- profiles / reference matching ----------------

/// Everything the AI needs to know about how a piece of audio "sits".
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct AudioProfile {
    pub name: String,
    #[serde(default)]
    pub path: String,
    pub seconds: f32,
    pub integrated_lufs: f32,
    pub short_term_max_lufs: f32,
    pub loudness_range_lu: f32,
    pub true_peak_dbtp: f32,
    pub crest_db: f32,
    pub spectral_centroid_hz: f32,
    /// Percent energy per band (sub, bass, low_mid, mid, presence, air).
    pub bands: Vec<(String, f32)>,
    pub stereo_correlation: f32,
    /// Side / mid energy ratio in dB (more negative = narrower).
    pub side_to_mid_db: f32,
    pub bpm: f32,
    pub bpm_confidence: f32,
    pub key: String,
}

pub fn profile(name: &str, l: &[f32], r: &[f32]) -> AudioProfile {
    let st = analysis::stats(l, r);
    let loud = analysis::loudness(l, r);
    let corr = analysis::correlation(l, r);
    let (mut em, mut es) = (0.0f64, 0.0f64);
    for i in 0..l.len().min(r.len()) {
        let m = (l[i] + r[i]) as f64 * 0.5;
        let s = (l[i] - r[i]) as f64 * 0.5;
        em += m * m;
        es += s * s;
    }
    let mono: Vec<f32> = l.iter().zip(r.iter()).map(|(a, b)| 0.5 * (a + b)).collect();
    // tempo / key on at most ~60 s from the middle (speed)
    let mid = mono.len() / 2;
    let half = (30.0 * SR) as usize;
    let seg = &mono[mid.saturating_sub(half)..(mid + half).min(mono.len())];
    let (bpm, conf) = estimate_bpm(seg);
    let key = estimate_key(seg)
        .first()
        .map(|k| k.0.clone())
        .unwrap_or_default();
    AudioProfile {
        name: name.to_string(),
        path: String::new(),
        seconds: (l.len() as f32 / SR * 10.0).round() / 10.0,
        integrated_lufs: loud.integrated_lufs,
        short_term_max_lufs: loud.short_term_max_lufs,
        loudness_range_lu: loud.loudness_range_lu,
        true_peak_dbtp: loud.true_peak_dbtp,
        crest_db: st.crest_db,
        spectral_centroid_hz: st.spectral_centroid_hz,
        bands: st
            .bands
            .iter()
            .map(|b| (b.band.to_string(), b.percent))
            .collect(),
        stereo_correlation: (corr * 100.0).round() / 100.0,
        side_to_mid_db: ((10.0 * (es.max(1e-12) / em.max(1e-12)).log10()) as f32 * 10.0).round()
            / 10.0,
        bpm,
        bpm_confidence: (conf * 100.0).round() / 100.0,
        key,
    }
}

/// A difference between the mix and the reference, with a concrete fix.
#[derive(Clone, Debug, Serialize)]
pub struct RefDelta {
    pub metric: String,
    pub mix: f32,
    pub reference: f32,
    pub delta: f32,
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<serde_json::Value>,
}

/// Compare a mix profile with a reference profile: loudness, tonal balance
/// per band, dynamics and width. Tempo/key are reported for context only;
/// the goal is matching sound, never copying composition.
pub fn compare(mix: &AudioProfile, rf: &AudioProfile) -> (Vec<RefDelta>, u32) {
    use serde_json::json;
    let mut out = Vec::new();
    let mut penalty = 0.0f32;
    let d_lufs = mix.integrated_lufs - rf.integrated_lufs;
    out.push(RefDelta {
        metric: "integrated_lufs".into(),
        mix: mix.integrated_lufs,
        reference: rf.integrated_lufs,
        delta: d_lufs,
        verdict: if d_lufs.abs() <= 1.0 {
            "matched".into()
        } else if d_lufs < 0.0 {
            format!("{:.1} LU quieter than the reference", -d_lufs)
        } else {
            format!("{d_lufs:.1} LU louder than the reference")
        },
        fix: (d_lufs.abs() > 1.0).then(|| {
            json!({"tool": "master_assistant", "args": {"target_lufs": (rf.integrated_lufs * 2.0).round() / 2.0}})
        }),
    });
    penalty += (d_lufs.abs() - 1.0).max(0.0) * 3.0;
    // tonal balance in dB per band (energy share ratio)
    let fixes: [(&str, serde_json::Value, serde_json::Value); 6] = [
        (
            "sub",
            json!({"kind": "low_shelf", "freq": 60.0}),
            json!("bass/808 volume or a low_shelf on the master"),
        ),
        (
            "bass",
            json!({"kind": "low_shelf", "freq": 150.0}),
            json!("bass/kick level or a low_shelf at 150 Hz"),
        ),
        (
            "low_mid",
            json!({"kind": "bell", "freq": 450.0, "q": 0.8}),
            json!("cut mud on pads/keys around 300-500 Hz"),
        ),
        (
            "mid",
            json!({"kind": "bell", "freq": 2000.0, "q": 0.7}),
            json!("lead/chord presence"),
        ),
        (
            "presence",
            json!({"kind": "bell", "freq": 5500.0, "q": 0.8}),
            json!("hats/snare/lead bite"),
        ),
        (
            "air",
            json!({"kind": "high_shelf", "freq": 10000.0}),
            json!("air: a high_shelf on the master or brighter hats"),
        ),
    ];
    for (band, eq, hint) in fixes.iter() {
        let m = mix
            .bands
            .iter()
            .find(|b| b.0 == *band)
            .map(|b| b.1)
            .unwrap_or(0.0);
        let r = rf
            .bands
            .iter()
            .find(|b| b.0 == *band)
            .map(|b| b.1)
            .unwrap_or(0.0);
        if r <= 0.05 && m <= 0.05 {
            continue;
        }
        let db = 10.0 * ((m.max(0.05)) / (r.max(0.05))).log10();
        let db = (db * 10.0).round() / 10.0;
        let ok = db.abs() <= 1.5;
        if !ok {
            penalty += (db.abs() - 1.5).min(10.0) * 2.0;
        }
        let gain = (-db * 0.6).clamp(-4.0, 4.0);
        let mut band_json = eq.clone();
        band_json["gain_db"] = json!((gain * 10.0).round() / 10.0);
        out.push(RefDelta {
            metric: format!("band_{band}"),
            mix: m,
            reference: r,
            delta: db,
            verdict: if ok {
                "matched".into()
            } else if db > 0.0 {
                format!("{band} {db:.1} dB heavier than the reference ({})", hint.as_str().unwrap_or(""))
            } else {
                format!("{band} {:.1} dB lighter than the reference ({})", -db, hint.as_str().unwrap_or(""))
            },
            fix: (!ok).then(|| {
                json!({"tool": "add_effect", "args": {"track": "master", "type": "parametric_eq", "params": {"bands": [band_json]}}, "note": "insert before the limiter (reorder_effects), or apply on the offending track instead"})
            }),
        });
    }
    let d_crest = mix.crest_db - rf.crest_db;
    let crest_ok = d_crest.abs() <= 2.5;
    if !crest_ok {
        penalty += (d_crest.abs() - 2.5) * 2.0;
    }
    out.push(RefDelta {
        metric: "crest_db".into(),
        mix: mix.crest_db,
        reference: rf.crest_db,
        delta: d_crest,
        verdict: if crest_ok {
            "dynamics matched".into()
        } else if d_crest > 0.0 {
            "more dynamic/peaky than the reference: more glue compression or soft clipping on drums".into()
        } else {
            "more squashed than the reference: ease the limiter/clipper, lower target loudness".into()
        },
        fix: (!crest_ok).then(|| {
            if d_crest > 0.0 {
                json!({"tool": "add_effect", "args": {"track": "master", "type": "soft_clipper", "params": {"threshold_db": -6.0, "ceiling_db": -0.5}}})
            } else {
                json!({"tool": "master_assistant", "args": {"target_lufs": rf.integrated_lufs - 1.0, "style": "clean"}})
            }
        }),
    });
    let d_w = mix.side_to_mid_db - rf.side_to_mid_db;
    let w_ok = d_w.abs() <= 3.0;
    if !w_ok {
        penalty += (d_w.abs() - 3.0) * 1.5;
    }
    out.push(RefDelta {
        metric: "side_to_mid_db".into(),
        mix: mix.side_to_mid_db,
        reference: rf.side_to_mid_db,
        delta: d_w,
        verdict: if w_ok {
            "stereo width matched".into()
        } else if d_w < 0.0 {
            "narrower than the reference: widen pads/chords (width 1.4-1.7, chorus) and pan percussion".into()
        } else {
            "wider than the reference: narrow the stereo FX, keep low end mono".into()
        },
        fix: (!w_ok).then(|| {
            json!({"tool": "add_effect", "args": {"track": "chords", "type": "width", "params": {"amount": if d_w < 0.0 { 1.6 } else { 0.8 }}}})
        }),
    });
    let score = (100.0 - penalty).clamp(0.0, 100.0).round() as u32;
    (out, score)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clicks(bpm: f32, secs: f32) -> Vec<f32> {
        let n = (secs * SR) as usize;
        let period = (60.0 / bpm * SR) as usize;
        let mut x = vec![0.0f32; n];
        let mut rng = crate::dsp::Rng::new(5);
        for start in (0..n).step_by(period) {
            for j in 0..2000.min(n - start) {
                x[start + j] = rng.bipolar() * (-(j as f32) / 300.0).exp() * 0.8;
            }
        }
        x
    }

    #[test]
    fn onsets_find_clicks() {
        let x = clicks(120.0, 4.0);
        let o = onsets(&x, 0.5, 60.0);
        assert!((7..=9).contains(&o.len()), "{}", o.len());
        let period = (0.5 * SR) as i64;
        for (i, p) in o.iter().enumerate() {
            assert!((*p as i64 - i as i64 * period).abs() < 600, "{i}: {p}");
        }
    }

    #[test]
    fn bpm_estimate() {
        let (bpm, _) = estimate_bpm(&clicks(128.0, 12.0));
        assert!(
            (bpm - 128.0).abs() < 2.0 || (bpm - 64.0).abs() < 1.0,
            "{bpm}"
        );
        let (bpm, _) = estimate_bpm(&clicks(90.0, 12.0));
        assert!(
            (bpm - 90.0).abs() < 2.0 || (bpm - 180.0).abs() < 2.0,
            "{bpm}"
        );
    }

    #[test]
    fn stretch_keeps_pitch_and_shift_keeps_length() {
        let x: Vec<f32> = (0..44100)
            .map(|i| (2.0 * PI * 440.0 * i as f32 / SR).sin() * 0.5)
            .collect();
        let y = time_stretch(&x, 1.5);
        assert!((y.len() as f32 / x.len() as f32 - 1.5).abs() < 0.01);
        let zc = |v: &[f32]| v.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f32;
        let f = zc(&y[2000..y.len() - 2000]) / ((y.len() - 4000) as f32 / SR);
        assert!((f - 440.0).abs() < 15.0, "{f}");
        let s = pitch_shift(&x, 12.0);
        assert!((s.len() as f32 / x.len() as f32 - 1.0).abs() < 0.02);
        let f2 = zc(&s[2000..s.len() - 2000]) / ((s.len() - 4000) as f32 / SR);
        assert!((f2 - 880.0).abs() < 30.0, "{f2}");
    }

    #[test]
    fn key_of_a_minor_triad_audio() {
        let mut x = vec![0.0f32; (3.0 * SR) as usize];
        for (m, a) in [
            (57.0f32, 1.0f32),
            (60.0, 0.8),
            (64.0, 0.8),
            (45.0, 0.6),
            (69.0, 0.5),
        ] {
            let f = crate::dsp::midi_to_hz(m);
            for (i, v) in x.iter_mut().enumerate() {
                *v += (2.0 * PI * f * i as f32 / SR).sin() * 0.1 * a;
            }
        }
        let k = estimate_key(&x);
        assert!(k[0].0 == "A minor" || k[0].0 == "C major", "{:?}", k);
    }

    #[test]
    fn edits() {
        let mut x: Vec<f32> = vec![0.0; 1000];
        x.extend((0..1000).map(|i| (i as f32 * 0.1).sin() * 0.25));
        x.extend(vec![0.0; 3000]);
        let (a, b) = trim(&mut x, -60.0);
        assert!(a > 800 && b > 2000);
        let g = normalize(&mut x, -1.0);
        assert!(g > 11.0);
        fade(&mut x, 5.0, 5.0);
        assert_eq!(x[0], 0.0);
        let p = profile("t", &x, &x);
        assert!(p.stereo_correlation > 0.99);
        let (d, score) = compare(&p, &p);
        assert!(score >= 99, "{score}");
        assert!(d.iter().all(|r| r.fix.is_none()));
    }
}
