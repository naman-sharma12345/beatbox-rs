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
    chroma_split(x).0
}

/// (full-range chroma 60 Hz-5 kHz, bass chroma 30-260 Hz). Each frame is
/// log-compressed and normalised to its own maximum, so a few loud frames
/// (kicks, a crash) cannot outvote the harmony of the whole piece.
pub fn chroma_split(x: &[f32]) -> ([f32; 12], [f32; 12]) {
    const N: usize = 8192;
    let mut c = [0.0f32; 12];
    let mut bass = [0.0f32; 12];
    if x.len() < N {
        return (c, bass);
    }
    let fft = Fft::new(N);
    let w = hann(N);
    let frames = (x.len() - N) / (N / 2) + 1;
    let stride = (frames / 300).max(1);
    let mut re = vec![0.0f32; N];
    let mut im = vec![0.0f32; N];
    for f in (0..frames).step_by(stride) {
        let off = f * N / 2;
        let mut energy = 0.0f32;
        for i in 0..N {
            re[i] = x[off + i] * w[i];
            im[i] = 0.0;
            energy += re[i] * re[i];
        }
        if energy < 1e-6 {
            continue;
        }
        fft.forward(&mut re, &mut im);
        let mut fc = [0.0f32; 12];
        let mut fb = [0.0f32; 12];
        for k in 5..N / 2 {
            let hz = k as f32 * SR / N as f32;
            if hz > 5000.0 {
                break;
            }
            let m = (re[k] * re[k] + im[k] * im[k]).sqrt();
            let midi = 69.0 + 12.0 * (hz / 440.0).log2();
            let pc = (midi.round() as i32).rem_euclid(12) as usize;
            let d = (midi - midi.round()).abs();
            let v = (1.0 + 10.0 * m).ln() * (1.0 - 2.0 * d).max(0.0);
            if (30.0..260.0).contains(&hz) {
                fb[pc] += v;
            }
            if hz >= 60.0 {
                fc[pc] += v;
            }
        }
        for (acc, fr) in [(&mut c, fc), (&mut bass, fb)] {
            let mx = fr.iter().cloned().fold(0.0f32, f32::max);
            if mx > 1e-6 {
                for i in 0..12 {
                    acc[i] += fr[i] / mx;
                }
            }
        }
    }
    (c, bass)
}

/// One ranked key hypothesis.
#[derive(Clone, Debug, Serialize)]
pub struct KeyCandidate {
    pub key: String,
    /// Combined score (profile correlation + bass-tonic evidence).
    pub score: f32,
    /// Plain Krumhansl profile correlation.
    pub profile_r: f32,
    /// How strongly the bass sits on this key's tonic (0..1).
    pub bass_tonic: f32,
}

/// Rank keys from (full, bass) chroma: Krumhansl-Schmuckler correlation on
/// the full chroma plus bass-tonic evidence (the bass sits on the tonic and
/// dominant far more than on other degrees), which separates relative
/// major/minor and neighbouring keys that share most of their notes.
pub fn rank_keys(full: &[f32; 12], bass: &[f32; 12]) -> Vec<KeyCandidate> {
    let bsum: f32 = bass.iter().sum::<f32>().max(1e-9);
    let bn: Vec<f32> = bass.iter().map(|v| v / bsum).collect();
    let bmax = bn.iter().cloned().fold(0.0f32, f32::max).max(1e-9);
    let mut out: Vec<KeyCandidate> = midi_ops::key_from_chroma(full)
        .into_iter()
        .map(|(pc, mode, r)| {
            let t = pc as usize;
            let third = if mode == "minor" { 3 } else { 4 };
            // tonic dominates, fifth supports, the modal third breaks ties
            let ev = (bn[t] + 0.35 * bn[(t + 7) % 12] + 0.2 * bn[(t + third) % 12]) / (1.55 * bmax);
            let has_bass = bsum > 1e-6 && bass.iter().any(|v| *v > 0.0);
            let score = if has_bass { 0.6 * r + 0.4 * ev } else { r };
            KeyCandidate {
                key: format!("{} {mode}", theory::NOTE_NAMES[t]),
                score: (score * 1000.0).round() / 1000.0,
                profile_r: (r * 1000.0).round() / 1000.0,
                bass_tonic: (ev * 1000.0).round() / 1000.0,
            }
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// Top-3 keys plus the margin between the first two and a confidence label.
pub fn key_report(x: &[f32]) -> serde_json::Value {
    let (c, b) = chroma_split(x);
    if c.iter().all(|v| *v == 0.0) {
        return serde_json::json!({"candidates": [], "confidence": "none"});
    }
    let r = rank_keys(&c, &b);
    let margin = r[0].score - r[1].score;
    let conf = if margin > 0.08 {
        "high"
    } else if margin > 0.03 {
        "medium"
    } else {
        "low"
    };
    serde_json::json!({
        "key": r[0].key, "margin": (margin * 1000.0).round() / 1000.0, "confidence": conf,
        "candidates": r.iter().take(3).collect::<Vec<_>>(),
        "note": if conf == "low" { "low margin: check the bass line / a stated key before trusting this" } else { "" },
    })
}

/// Best key guesses for audio: [(name like "A minor", score)], top 3.
pub fn estimate_key(x: &[f32]) -> Vec<(String, f32)> {
    let (c, b) = chroma_split(x);
    if c.iter().all(|v| *v == 0.0) {
        return vec![];
    }
    rank_keys(&c, &b)
        .into_iter()
        .take(3)
        .map(|k| (k.key, (k.score * 100.0).round() / 100.0))
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

/// Transient-first stretch ("beats" mode): cut the take at its onsets and
/// lay the slices on the stretched grid - drums keep their crack instead of
/// smearing like they do under WSOLA (Ableton's Beats, FL's Slice mode).
/// Slower leaves short silences between hits; faster truncates each slice
/// where the next lands. Sustained or tonal material wants `time_stretch`.
pub fn time_stretch_beats(x: &[f32], factor: f32) -> Vec<f32> {
    let factor = factor.clamp(0.25, 4.0);
    if (factor - 1.0).abs() < 1e-3 || x.len() < 4096 {
        return time_stretch(x, factor);
    }
    let out_len = ((x.len() as f32 * factor) as usize).max(1);
    let mut out = vec![0.0f32; out_len];
    let mut marks: Vec<usize> = onsets(x, 0.5, 60.0);
    marks.retain(|&m| m > 512 && m + 512 < x.len());
    marks.dedup();
    marks.insert(0, 0);
    marks.push(x.len());
    let fade = (0.005 * SR) as usize;
    for w in marks.windows(2) {
        let (s, e) = (w[0], w[1]);
        let d0 = ((s as f32 * factor) as usize).min(out_len);
        let d1 = ((e as f32 * factor) as usize).min(out_len);
        if d1 <= d0 {
            continue;
        }
        let take = (e - s).min(d1 - d0);
        out[d0..d0 + take].copy_from_slice(&x[s..s + take]);
        // click-free edge at the slice's end (gap or truncation point)
        let f = fade.min(take / 2);
        for i in 0..f {
            let g = 0.5 * (1.0 + (std::f32::consts::PI * i as f32 / f as f32).cos());
            out[d0 + take - f + i] *= g;
        }
    }
    out
}

/// Snap a take's hits to a grid (beat detective / flex-time quantize):
/// every onset moves toward the nearest grid point by `amount` (1 = hard
/// quantize), the audio between hits stretches to fit its new slot, and the
/// overall length is kept. The take should start on a downbeat. Returns the
/// warped audio, the number of hits found, and how many of them moved.
pub fn quantize_onsets(x: &[f32], grid_samples: usize, amount: f32) -> (Vec<f32>, usize, usize) {
    let amount = amount.clamp(0.0, 1.0);
    let g = grid_samples.max(HOP * 4) as f64;
    if x.len() < 4096 || amount < 1e-3 {
        return (x.to_vec(), 0, 0);
    }
    let mut marks: Vec<usize> = onsets(x, 0.5, ((g / SR as f64 * 500.0).max(30.0)) as f32);
    marks.retain(|&m| m > 256 && m + 256 < x.len());
    marks.dedup();
    let hits = marks.len();
    if hits < 2 {
        return (x.to_vec(), hits, 0);
    }
    marks.insert(0, 0);
    marks.push(x.len());
    // moved position of each mark: the ends stay put, the rest snap
    let mut moved = 0usize;
    let mut pos: Vec<f64> = Vec::with_capacity(marks.len());
    for (i, &m) in marks.iter().enumerate() {
        if i == 0 || i == marks.len() - 1 {
            pos.push(m as f64);
            continue;
        }
        let t = ((m as f64 / g).round() * g).clamp(0.0, x.len() as f64);
        if (t - m as f64).abs() > SR as f64 * 0.002 {
            moved += 1;
        }
        pos.push(m as f64 + amount as f64 * (t - m as f64));
    }
    // a snapped hit must not cross its neighbours
    for i in 1..pos.len() {
        if pos[i] < pos[i - 1] {
            pos[i] = pos[i - 1];
        }
    }
    let fade = (0.005 * SR) as usize;
    let mut out = vec![0.0f32; x.len()];
    for i in 0..marks.len() - 1 {
        let seg = &x[marks[i]..marks[i + 1]];
        let slot = (pos[i + 1] - pos[i]).max(1.0);
        let factor = (slot / seg.len() as f64) as f32;
        let mut piece = if (factor - 1.0).abs() < 0.02 {
            seg.to_vec()
        } else {
            time_stretch(seg, factor.clamp(0.5, 2.0))
        };
        piece.resize(slot as usize, 0.0);
        piece.truncate(slot as usize);
        let start = pos[i] as usize;
        if start >= out.len() {
            break;
        }
        let n = piece.len().min(out.len() - start);
        // click-free joins: the piece's tail fades into the next hit, whose
        // attack starts unfaded (beat-slice style, like Recycle)
        let f = fade.min(n / 2);
        for j in 0..f {
            let gc = 0.5 * (1.0 + (std::f32::consts::PI * j as f32 / f as f32).cos());
            piece[n - f + j] *= gc;
        }
        out[start..start + n].copy_from_slice(&piece[..n]);
    }
    (out, hits, moved)
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

/// Region of a spectral edit: a time span and a frequency band, with soft
/// edges so the cut leaves no clicks or ringing walls.
#[derive(Clone, Copy, Debug)]
pub struct SpectralRegion {
    pub t0: f32,
    pub t1: f32,
    pub lo_hz: f32,
    pub hi_hz: f32,
    /// gain applied (dB): -60 removes, +6 lifts
    pub gain_db: f32,
    /// false: the gain hits the band inside the span; true: it hits everything
    /// in the span except the band (keep only the band)
    pub outside: bool,
    pub soft_ms: f32,
    pub soft_oct: f32,
}

fn ramp_w(d: f32, soft: f32) -> f32 {
    // 1 inside (d <= 0), raised-cosine fall over `soft`, 0 beyond
    if d <= 0.0 {
        1.0
    } else if soft <= 0.0 || d >= soft {
        0.0
    } else {
        0.5 + 0.5 * (std::f32::consts::PI * d / soft).cos()
    }
}

/// Edison-style spectral edit: STFT (2048/512, Hann), a gain mask over the
/// region, overlap-add back. Returns the new audio and the energy change of
/// the whole clip in dB.
pub fn spectral_edit(x: &[f32], sr: f32, r: &SpectralRegion) -> (Vec<f32>, f32) {
    const N: usize = 2048;
    const H: usize = 512;
    if x.is_empty() {
        return (Vec::new(), 0.0);
    }
    let mut pad = vec![0.0f32; N];
    pad.extend_from_slice(x);
    pad.extend(std::iter::repeat_n(0.0, 2 * N));
    let win: Vec<f32> = (0..N)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N as f32).cos())
        .collect();
    let frames = (pad.len() - N) / H + 1;
    let g = crate::dsp::db_to_gain(r.gain_db.clamp(-120.0, 24.0));
    let soft_t = r.soft_ms.max(0.0) / 1000.0;
    let soft_o = r.soft_oct.max(0.0);
    let (lo, hi) = (
        r.lo_hz.max(1.0).min(r.hi_hz.max(1.0)),
        r.hi_hz.max(r.lo_hz).max(1.0),
    );
    let fw: Vec<f32> = (0..=N / 2)
        .map(|k| {
            let f = (k as f32 * sr / N as f32).max(1.0);
            let d = if f < lo {
                (lo / f).log2()
            } else if f > hi {
                (f / hi).log2()
            } else {
                0.0
            };
            ramp_w(d, soft_o)
        })
        .collect();
    let mut out = vec![0.0f32; pad.len()];
    let mut wsum = vec![0.0f32; pad.len()];
    for fr in 0..frames {
        let c = (fr * H + N / 2) as f32 / sr - N as f32 / sr; // frame centre in the clip's time
        let dt = if c < r.t0 {
            r.t0 - c
        } else if c > r.t1 {
            c - r.t1
        } else {
            0.0
        };
        let wt = ramp_w(dt, soft_t);
        let mut re: Vec<f32> = (0..N).map(|i| pad[fr * H + i] * win[i]).collect();
        if wt > 0.0 {
            let mut im = vec![0.0f32; N];
            crate::analysis::fft(&mut re, &mut im);
            for k in 0..=N / 2 {
                let w = if r.outside {
                    wt * (1.0 - fw[k])
                } else {
                    wt * fw[k]
                };
                let v = 1.0 + w * (g - 1.0);
                re[k] *= v;
                im[k] *= v;
                if k > 0 && k < N / 2 {
                    re[N - k] *= v;
                    im[N - k] *= v;
                }
            }
            let mut i2: Vec<f32> = im.iter().map(|v| -v).collect();
            crate::analysis::fft(&mut re, &mut i2);
            for v in re.iter_mut() {
                *v /= N as f32;
            }
        }
        for i in 0..N {
            out[fr * H + i] += re[i] * win[i];
            wsum[fr * H + i] += win[i] * win[i];
        }
    }
    for (o, w) in out.iter_mut().zip(wsum.iter()) {
        if *w > 1e-6 {
            *o /= *w;
        }
    }
    let y: Vec<f32> = out[N..N + x.len()].to_vec();
    let e0: f64 = x.iter().map(|v| (*v as f64) * (*v as f64)).sum();
    let e1: f64 = y.iter().map(|v| (*v as f64) * (*v as f64)).sum();
    let ch = if e0 > 1e-12 {
        (10.0 * ((e1 + 1e-12) / e0).log10()) as f32
    } else {
        0.0
    };
    (y, ch)
}

/// Spectrogram grid for display: `frames` evenly spaced 2048-point Hann
/// spectra summed into `bands` log-spaced bands between lo and hi Hz.
/// Returns (band edges, dB per frame per band, peak dB).
pub fn spectrogram_grid(
    x: &[f32],
    sr: f32,
    frames: usize,
    bands: usize,
    lo: f32,
    hi: f32,
) -> (Vec<f32>, Vec<Vec<f32>>, f32) {
    const N: usize = 2048;
    let frames = frames.clamp(8, 1024);
    let bands = bands.clamp(4, 256);
    let (lo, hi) = (lo.max(10.0), hi.min(sr / 2.0).max(lo.max(10.0) * 2.0));
    let edges: Vec<f32> = (0..=bands)
        .map(|i| lo * (hi / lo).powf(i as f32 / bands as f32))
        .collect();
    let win: Vec<f32> = (0..N)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N as f32).cos())
        .collect();
    let mut grid = Vec::with_capacity(frames);
    let mut peak = -200.0f32;
    for f in 0..frames {
        let c = if x.is_empty() {
            0
        } else {
            (f as f64 + 0.5) as usize * x.len() / frames
        };
        let s0 = c as isize - (N / 2) as isize;
        let mut re: Vec<f32> = (0..N)
            .map(|i| {
                let j = s0 + i as isize;
                if j >= 0 && (j as usize) < x.len() {
                    x[j as usize] * win[i]
                } else {
                    0.0
                }
            })
            .collect();
        let mut im = vec![0.0f32; N];
        crate::analysis::fft(&mut re, &mut im);
        let mut row = vec![0.0f32; bands];
        for (b, v) in row.iter_mut().enumerate() {
            let k0 = ((edges[b] * N as f32 / sr) as usize).max(1);
            let k1 = ((edges[b + 1] * N as f32 / sr).ceil() as usize)
                .max(k0 + 1)
                .min(N / 2);
            let p: f32 = (k0..k1).map(|k| re[k] * re[k] + im[k] * im[k]).sum::<f32>()
                / (k1 - k0).max(1) as f32;
            *v = 10.0 * (p + 1e-12).log10();
            peak = peak.max(*v);
        }
        grid.push(row);
    }
    (edges, grid, peak)
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

    /// i-VI-III-VII in C# minor (C#m A E B, one bar each, bass on the roots,
    /// a melody leaning on C#/G#): plain Krumhansl tends to pick A major or
    /// E major here; bass-tonic evidence must put C# minor first.
    #[test]
    fn c_sharp_minor_progression_beats_its_neighbours() {
        let bar = (SR * 2.0) as usize;
        let chords: [[f32; 4]; 4] = [
            [49.0, 61.0, 64.0, 68.0], // C#m
            [45.0, 57.0, 61.0, 64.0], // A
            [40.0, 59.0, 64.0, 68.0], // E
            [47.0, 59.0, 63.0, 66.0], // B
        ];
        let mel = [73.0f32, 68.0, 73.0, 71.0];
        let mut x = Vec::new();
        for rep in 0..3 {
            for (ci, ch) in chords.iter().enumerate() {
                for i in 0..bar {
                    let t = i as f32 / SR;
                    let env = (-t * 1.5).exp();
                    let mut v = 0.0;
                    for (k, m) in ch.iter().enumerate() {
                        let a = if k == 0 { 0.35 } else { 0.1 };
                        v += a * env * (2.0 * PI * crate::dsp::midi_to_hz(*m) * t).sin();
                    }
                    if i < bar / 2 {
                        v += 0.08 * (2.0 * PI * crate::dsp::midi_to_hz(mel[ci]) * t).sin();
                    }
                    // tonic pedal in the last bar of each pass
                    if rep < 3 && ci == 3 && i > bar / 2 {
                        v += 0.2 * (2.0 * PI * crate::dsp::midi_to_hz(49.0) * t).sin();
                    }
                    x.push(v);
                }
            }
        }
        let rep = key_report(&x);
        assert_eq!(rep["key"], "C# minor", "{rep}");
        assert_eq!(rep["candidates"].as_array().unwrap().len(), 3);
        assert!(rep["margin"].as_f64().unwrap() > 0.0);
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
    #[test]
    fn beats_stretch_keeps_the_crack() {
        // four hits at 120 bpm (0, 0.5, 1.0, 1.5 s)
        let x = clicks(120.0, 2.0);
        let y = time_stretch_beats(&x, 2.0);
        assert_eq!(y.len(), x.len() * 2, "2x length");
        let ons = onsets(&y, 0.5, 200.0);
        assert_eq!(ons.len(), 4, "stretched onsets {ons:?}");
        for (&o, want) in ons.iter().zip([0.0f32, 1.0, 2.0, 3.0]) {
            assert!(
                (o as f32 / SR - want).abs() < 0.03,
                "onset {o} not near {want} s"
            );
        }
        // each hit keeps its full-height attack (WSOLA smears these lower)
        for &o in &ons {
            let hi = (o + (0.02 * SR) as usize).min(y.len());
            let p = y[o..hi].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(p > 0.5, "onset at {o} smeared to peak {p}");
        }
        assert_eq!(y, time_stretch_beats(&x, 2.0), "deterministic");
        // faster keeps every hit too
        let z = time_stretch_beats(&x, 0.5);
        assert_eq!(z.len(), x.len() / 2, "0.5x length");
        let oz = onsets(&z, 0.5, 100.0);
        assert_eq!(oz.len(), 4, "fast onsets {oz:?}");
    }
    #[test]
    fn quantize_snaps_drunk_hits_to_the_grid() {
        // four hits at 120 bpm played drunk: +20 ms, -30 ms, +15 ms late
        let n = (2.0 * SR) as usize;
        let mut x = vec![0.0f32; n];
        let mut rng = crate::dsp::Rng::new(7);
        let grid = (0.5 * SR) as usize; // quarter notes at 120 bpm
        for (k, off) in [441isize, -1323, 662, 0].iter().enumerate() {
            let start = (k * grid) as isize + off;
            for j in 0..2000usize {
                let idx = start + j as isize;
                if idx >= 0 && (idx as usize) < n {
                    x[idx as usize] = rng.bipolar() * (-(j as f32) / 300.0).exp() * 0.8;
                }
            }
        }
        // hard quantize: every hit lands on the grid
        let (y, hits, moved) = quantize_onsets(&x, grid, 1.0);
        assert_eq!(y.len(), x.len(), "length kept");
        assert_eq!(hits, 4, "four hits found");
        assert!(moved >= 3, "the drunk hits moved: {moved}");
        let ons = onsets(&y, 0.5, 200.0);
        assert_eq!(ons.len(), 4, "quantized onsets {ons:?}");
        for (k, &o) in ons.iter().enumerate() {
            let want = k * grid;
            assert!(
                (o as i64 - want as i64).abs() < (0.015 * SR) as i64,
                "hit {k} at {o}, want {want}"
            );
        }
        // half quantize halves the error instead
        let (h, _, _) = quantize_onsets(&x, grid, 0.5);
        let oh = onsets(&h, 0.5, 200.0);
        assert_eq!(oh.len(), 4);
        let err_half = (oh[1] as i64 - grid as i64).abs();
        assert!(
            err_half > (0.008 * SR) as i64 && err_half < (0.022 * SR) as i64,
            "half-quantize error {err_half} (want ~half of 30 ms)"
        );
        // a pad with no clear hits comes back untouched
        let pad: Vec<f32> = (0..n)
            .map(|i| 0.3 * (i as f32 * 220.0 / SR * std::f32::consts::TAU).sin())
            .collect();
        let (p, ph, _) = quantize_onsets(&pad, grid, 1.0);
        assert!(ph < 2);
        assert_eq!(p, pad);
    }
}
