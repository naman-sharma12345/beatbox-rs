//! "AI ears": windowed and per-section measurements, artifact detection
//! (clicks, DC steps, end-level jumps, truncated tails, hiss/noise beds) and
//! spectrogram images. Everything here works on plain stereo buffers so it
//! applies to the mix, a stem, a sample or any file.

use crate::analysis::{self, fft};
use crate::dsp::{gain_to_db, SR};
use crate::project::Project;
use crate::render::{TrackInfo, INFO_BLOCK};
use serde::Serialize;
use serde_json::{json, Value};

// ---------------------------------------------------------------- sections

/// One arrangement section in song time.
#[derive(Clone, Debug, Serialize)]
pub struct Span {
    pub index: usize,
    pub pattern: String,
    pub start_beat: f32,
    pub end_beat: f32,
    pub start_s: f32,
    pub end_s: f32,
}

/// Seconds per quarter-note beat.
pub fn beat_secs(p: &Project) -> f32 {
    p.step_secs() * 4.0
}

pub fn spans(p: &Project) -> Vec<Span> {
    let bs = beat_secs(p);
    let mut start = 0.0f32;
    p.song_sections()
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let len = p
                .pattern_index(&s.pattern)
                .map(|pi| (p.patterns[pi].steps() * s.repeats.max(1)) as f32 / 4.0)
                .unwrap_or(0.0);
            let sp = Span {
                index: i,
                pattern: s.pattern.clone(),
                start_beat: start,
                end_beat: start + len,
                start_s: start * bs,
                end_s: (start + len) * bs,
            };
            start += len;
            sp
        })
        .collect()
}

/// Musical role guessed from a pattern name.
pub fn role(name: &str) -> &'static str {
    let n = name.to_lowercase();
    let has = |k: &[&str]| k.iter().any(|w| n.contains(w));
    if has(&["hook", "chorus", "drop", "refrain"]) {
        "hook"
    } else if has(&["verse", "main", "groove"]) {
        "verse"
    } else if has(&["intro"]) {
        "intro"
    } else if has(&["outro", "end"]) {
        "outro"
    } else if has(&["break", "bridge", "breakdown", "interlude"]) {
        "break"
    } else if has(&["build", "riser", "pre", "lift"]) {
        "build"
    } else {
        "other"
    }
}

// ---------------------------------------------------------------- windows

/// Loudness/spectral measurements of one window of audio.
#[derive(Clone, Debug, Serialize, Default)]
pub struct Metrics {
    pub start_s: f32,
    pub end_s: f32,
    pub integrated_lufs: f32,
    pub short_term_max_lufs: f32,
    pub true_peak_dbtp: f32,
    pub peak_dbfs: f32,
    pub rms_dbfs: f32,
    pub crest_db: f32,
    pub spectral_centroid_hz: f32,
    pub stereo_correlation: f32,
    /// Percent energy per band (sub, bass, low_mid, mid, presence, air).
    pub bands: Vec<(String, f32)>,
}

pub fn clamp_range(n: usize, s0: usize, s1: usize) -> (usize, usize) {
    let a = s0.min(n);
    (a, s1.clamp(a, n))
}

pub fn measure(l: &[f32], r: &[f32], s0: usize, s1: usize) -> Metrics {
    let n = l.len().min(r.len());
    let (a, b) = clamp_range(n, s0, s1);
    let (wl, wr) = (&l[a..b], &r[a..b]);
    let loud = analysis::loudness(wl, wr);
    let st = analysis::stats(wl, wr);
    Metrics {
        start_s: round2(a as f32 / SR),
        end_s: round2(b as f32 / SR),
        integrated_lufs: loud.integrated_lufs,
        short_term_max_lufs: loud.short_term_max_lufs,
        true_peak_dbtp: loud.true_peak_dbtp,
        peak_dbfs: st.peak_dbfs,
        rms_dbfs: st.rms_dbfs,
        crest_db: st.crest_db,
        spectral_centroid_hz: st.spectral_centroid_hz,
        stereo_correlation: round2(analysis::correlation(wl, wr)),
        bands: st
            .bands
            .iter()
            .map(|b| (b.band.to_string(), b.percent))
            .collect(),
    }
}

fn round2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}
fn round1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

/// Per-track loudness inside a window, from the 400 ms render blocks.
pub fn top_tracks(infos: &[TrackInfo], s0: usize, s1: usize, n: usize) -> Vec<Value> {
    let (b0, b1) = (s0 / INFO_BLOCK, s1.div_ceil(INFO_BLOCK));
    let mut rows: Vec<(String, f64)> = infos
        .iter()
        .map(|t| {
            let hi = b1.min(t.blocks.len());
            let lo = b0.min(hi);
            let e: f64 = t.blocks[lo..hi].iter().map(|v| *v as f64).sum();
            (t.name.clone(), e / (hi - lo).max(1) as f64)
        })
        .collect();
    let total: f64 = rows.iter().map(|r| r.1).sum::<f64>().max(1e-20);
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    rows.into_iter()
        .filter(|r| r.1 > 1e-9)
        .take(n)
        .map(|(name, e)| {
            json!({"track": name, "rms_dbfs": round1(10.0 * (e.max(1e-20)).log10() as f32), "share_percent": round1((100.0 * e / total) as f32)})
        })
        .collect()
}

fn band(m: &Metrics, name: &str) -> f32 {
    m.bands
        .iter()
        .find(|b| b.0 == name)
        .map(|b| b.1)
        .unwrap_or(0.0)
}

/// Per-section report with deltas and musical flags.
pub fn analyze_sections(
    p: &Project,
    l: &[f32],
    r: &[f32],
    infos: &[TrackInfo],
    top_n: usize,
) -> Value {
    let sp = spans(p);
    let mut rows = Vec::new();
    let mut metrics: Vec<Metrics> = Vec::new();
    for s in &sp {
        let (a, b) = ((s.start_s * SR) as usize, (s.end_s * SR) as usize);
        let m = measure(l, r, a, b);
        let mut row = json!({
            "index": s.index, "pattern": s.pattern, "role": role(&s.pattern),
            "start_beat": s.start_beat, "end_beat": s.end_beat,
            "metrics": m, "top_tracks": top_tracks(infos, a, b, top_n),
        });
        if let Some(prev) = metrics.last() {
            let bands: Vec<Value> = m
                .bands
                .iter()
                .map(|(k, v)| json!({"band": k, "delta_percent": round1(v - band(prev, k))}))
                .collect();
            row["delta_vs_previous"] = json!({
                "lufs": round1(m.integrated_lufs - prev.integrated_lufs),
                "short_term_max": round1(m.short_term_max_lufs - prev.short_term_max_lufs),
                "true_peak": round1(m.true_peak_dbtp - prev.true_peak_dbtp),
                "centroid_hz": (m.spectral_centroid_hz - prev.spectral_centroid_hz).round(),
                "bands": bands,
            });
        }
        metrics.push(m);
        rows.push(row);
    }
    let flags = section_flags(&sp, &metrics);
    json!({"sections": rows, "flags": flags, "count": sp.len()})
}

fn mean(v: &[f32]) -> Option<f32> {
    if v.is_empty() {
        None
    } else {
        Some(v.iter().sum::<f32>() / v.len() as f32)
    }
}

/// Musical sanity flags across sections (the producer's ear in rules).
pub fn section_flags(sp: &[Span], m: &[Metrics]) -> Vec<Value> {
    let mut out = Vec::new();
    let of_role = |r: &str| -> Vec<usize> {
        sp.iter()
            .enumerate()
            .filter(|(_, s)| role(&s.pattern) == r)
            .map(|(i, _)| i)
            .collect()
    };
    let lufs = |idx: &[usize]| {
        mean(
            &idx.iter()
                .map(|i| m[*i].integrated_lufs)
                .collect::<Vec<_>>(),
        )
    };
    let st = |idx: &[usize]| {
        mean(
            &idx.iter()
                .map(|i| m[*i].short_term_max_lufs)
                .collect::<Vec<_>>(),
        )
    };
    let hooks = of_role("hook");
    let verses = of_role("verse");
    if let (Some(h), Some(v)) = (lufs(&hooks), lufs(&verses)) {
        if h < v + 0.5 {
            out.push(json!({"flag": "hook not louder than verse", "severity": "warn",
                "detail": format!("hook averages {h:.1} LUFS vs verse {v:.1} LUFS (want +1..+3 LU)"),
                "fix": "raise hook energy: add a layer/octave double, open a filter, +1-2 dB section mix (set_section_mix), or thin the verse"}));
        }
        if let (Some(hs), Some(vs)) = (st(&hooks), st(&verses)) {
            if hs < vs {
                out.push(
                    json!({"flag": "hook short-term peak below verse", "severity": "warn",
                    "detail": format!("hook short-term max {hs:.1} vs verse {vs:.1} LUFS")}),
                );
            }
        }
    }
    for i in of_role("intro") {
        if let Some(h) = lufs(&hooks) {
            if m[i].integrated_lufs > h {
                out.push(
                    json!({"flag": "intro louder than hook", "severity": "warn", "section": i,
                    "detail": format!("intro {:.1} vs hook {h:.1} LUFS", m[i].integrated_lufs)}),
                );
            }
        }
    }
    for (i, s) in sp.iter().enumerate() {
        let mi = &m[i];
        if mi.integrated_lufs <= -60.0 {
            out.push(json!({"flag": "silent section", "severity": "fail", "section": i, "pattern": s.pattern}));
            continue;
        }
        if mi.true_peak_dbtp > -1.0 {
            out.push(
                json!({"flag": "section true peak over -1 dBTP", "severity": "warn", "section": i,
                "detail": format!("{:.2} dBTP", mi.true_peak_dbtp)}),
            );
        }
        if i > 0 {
            let prev = &m[i - 1];
            let d = mi.integrated_lufs - prev.integrated_lufs;
            if d.abs() > 6.0 && role(&s.pattern) != "break" && role(&sp[i - 1].pattern) != "break" {
                out.push(json!({"flag": "large loudness jump between sections", "severity": "info", "section": i,
                    "detail": format!("{d:+.1} LU from '{}' to '{}'", sp[i - 1].pattern, s.pattern)}));
            }
            let (ls, ps) = (
                band(mi, "sub") + band(mi, "bass"),
                band(prev, "sub") + band(prev, "bass"),
            );
            if ps > 20.0 && ls < ps * 0.5 && role(&s.pattern) == "hook" {
                out.push(
                    json!({"flag": "low end drops out in hook", "severity": "warn", "section": i,
                    "detail": format!("sub+bass share {ls:.0}% vs {ps:.0}% before")}),
                );
            }
        }
        if role(&s.pattern) == "hook" && mi.crest_db < 6.0 {
            out.push(
                json!({"flag": "hook over-compressed", "severity": "info", "section": i,
                "detail": format!("crest {:.1} dB", mi.crest_db)}),
            );
        }
    }
    out
}

// ---------------------------------------------------------------- artifacts

#[derive(Clone, Debug, Serialize)]
pub struct Artifact {
    pub kind: String,
    pub severity: String,
    pub time_s: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_s: Option<f32>,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
}

fn art(kind: &str, sev: &str, t: f32, end: Option<f32>, detail: String) -> Artifact {
    Artifact {
        kind: kind.into(),
        severity: sev.into(),
        time_s: round2(t),
        end_s: end.map(round2),
        detail,
        track: None,
    }
}

/// Isolated discontinuities: a second-difference spike far above the local
/// second-difference level (real transients are broadband for many samples,
/// a click is one or two samples).
pub fn clicks(x: &[f32], max: usize) -> Vec<(usize, f32)> {
    let n = x.len();
    if n < 300 {
        return Vec::new();
    }
    let d: Vec<f32> = (2..n).map(|i| x[i] - 2.0 * x[i - 1] + x[i - 2]).collect();
    // running mean of |d| over 129 samples via prefix sums
    let mut pre = vec![0.0f64; d.len() + 1];
    for i in 0..d.len() {
        pre[i + 1] = pre[i] + d[i].abs() as f64;
    }
    let mut out: Vec<(usize, f32)> = Vec::new();
    const W: usize = 64;
    for i in W..d.len().saturating_sub(W) {
        let a = d[i].abs();
        if a < 0.03 {
            continue;
        }
        // neighbourhood excluding the spike itself (+/- 3 samples)
        let around = (pre[i + W] - pre[i - W]) - (pre[i + 4] - pre[i - 3]);
        let local = (around / (2 * W - 7) as f64) as f32;
        if a > 12.0 * local.max(1e-5) {
            let at = i + 1;
            match out.last_mut() {
                Some(last) if at - last.0 < 441 => {
                    if a > last.1 {
                        *last = (at, a);
                    }
                }
                _ => out.push((at, a)),
            }
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// Sudden steps in the DC level between 100 ms blocks.
pub fn dc_steps(x: &[f32]) -> Vec<(usize, f32)> {
    let blk = (SR * 0.1) as usize;
    let means: Vec<f32> = x
        .chunks(blk)
        .filter(|c| c.len() == blk)
        .map(|c| c.iter().sum::<f32>() / blk as f32)
        .collect();
    let mut out = Vec::new();
    for k in 1..means.len() {
        let d = means[k] - means[k - 1];
        if d.abs() > 0.03 {
            out.push((k * blk, d));
        }
    }
    out
}

/// RMS envelope in dBFS, `hop` samples per point (max of both channels).
pub fn envelope_db(l: &[f32], r: &[f32], hop: usize) -> Vec<f32> {
    let n = l.len().min(r.len());
    (0..n.div_ceil(hop))
        .map(|k| {
            let (a, b) = (k * hop, ((k + 1) * hop).min(n));
            let mut s = 0.0f64;
            for i in a..b {
                s += 0.5 * ((l[i] as f64).powi(2) + (r[i] as f64).powi(2));
            }
            10.0 * ((s / (b - a).max(1) as f64).max(1e-14)).log10() as f32
        })
        .collect()
}

/// After the song fades down in its final seconds, does the level jump back
/// up (a stray hit, a loop wrap, an un-faded tail)? Returns (time_s, jump_db).
pub fn end_level_jump(l: &[f32], r: &[f32], tail_s: f32) -> Option<(f32, f32)> {
    let hop = (SR * 0.05) as usize;
    let env = envelope_db(l, r, hop);
    let k = ((tail_s.max(1.0) * SR) as usize / hop).min(env.len());
    if k < 12 {
        return None;
    }
    let start = env.len() - k;
    // 400 ms power average so gaps between drum hits are not "fades"
    let pw: Vec<f64> = env[start..]
        .iter()
        .map(|d| 10f64.powf(*d as f64 / 10.0))
        .collect();
    let seg: Vec<f32> = (0..pw.len())
        .map(|i| {
            let a = i.saturating_sub(7);
            (10.0
                * (pw[a..=i].iter().sum::<f64>() / (i - a + 1) as f64)
                    .max(1e-14)
                    .log10()) as f32
        })
        .collect();
    let peak = seg.iter().cloned().fold(-200.0f32, f32::max);
    let mut low_run = 0usize;
    let mut low = f32::MAX;
    let mut faded = false;
    let mut best: Option<(usize, f32)> = None;
    for (i, v) in seg.iter().enumerate() {
        if *v < peak - 20.0 {
            low_run += 1;
            low = low.min(*v);
            if low_run >= 10 {
                faded = true;
            }
        } else {
            low_run = 0;
        }
        if faded {
            let jump = v - low;
            if jump > 12.0 && *v > -60.0 && best.map(|b| jump > b.1).unwrap_or(true) {
                best = Some((i, jump));
            }
        }
    }
    best.map(|(i, j)| (((start + i) * hop) as f32 / SR, round1(j)))
}

/// Does audio end while still sounding? Returns the last-50 ms level.
pub fn truncated_tail(l: &[f32], r: &[f32]) -> Option<f32> {
    let n = l.len().min(r.len());
    let w = ((SR * 0.05) as usize).min(n);
    if w == 0 {
        return None;
    }
    let env = envelope_db(&l[n - w..n], &r[n - w..n], w);
    let last = env.first().copied().unwrap_or(-140.0);
    let edge = l[n - 1].abs().max(r[n - 1].abs());
    if last > -45.0 || edge > 0.02 {
        Some(round1(last))
    } else {
        None
    }
}

/// A sustained broadband/HF noise bed (hiss): the high band never drops
/// below a floor, the spectrum there is flat (noise-like), and it covers
/// most of the audio, including the quiet parts.
#[derive(Clone, Debug, Serialize)]
pub struct NoiseBed {
    pub detected: bool,
    /// Level of the 6-16 kHz band floor (10th percentile), dBFS.
    pub hf_floor_db: f32,
    /// Spectral flatness of 6-16 kHz in floor frames (1 = white noise).
    pub flatness: f32,
    /// Percent of non-silent frames sitting on the noise floor band.
    pub coverage_percent: f32,
    /// Longest continuous stretch of the bed.
    pub start_s: f32,
    pub end_s: f32,
    /// Strength of a periodic repeat in the noise envelope (looped bed), 0-1.
    pub loop_score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_period_s: Option<f32>,
}

const NFRAME: usize = 2048;

fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos())
        .collect()
}

/// Power spectrum of one frame (NFRAME/2 bins), normalised so a full-scale
/// sine reads about 0 dB in its bin.
fn frame_spec(x: &[f32], at: usize, win: &[f32]) -> Vec<f32> {
    let mut re = vec![0.0f32; NFRAME];
    let mut im = vec![0.0f32; NFRAME];
    for i in 0..NFRAME {
        re[i] = x.get(at + i).copied().unwrap_or(0.0) * win[i];
    }
    fft(&mut re, &mut im);
    let norm = 2.0 / (NFRAME as f32 * 0.5);
    (0..NFRAME / 2)
        .map(|k| (re[k] * re[k] + im[k] * im[k]) * norm * norm)
        .collect()
}

pub fn noise_bed(l: &[f32], r: &[f32]) -> NoiseBed {
    let n = l.len().min(r.len());
    let mono: Vec<f32> = (0..n).map(|i| 0.5 * (l[i] + r[i])).collect();
    let hop = (SR * 0.05) as usize; // 50 ms frames
    let win = hann(NFRAME);
    let bin_hz = SR / NFRAME as f32;
    let (k0, k1) = ((6000.0 / bin_hz) as usize, (16000.0 / bin_hz) as usize);
    let mut frames: Vec<(f32, f32, f32)> = Vec::new(); // (full dB, hf dB, flatness)
    let mut at = 0;
    while at + NFRAME <= n {
        let sp = frame_spec(&mono, at, &win);
        let full: f32 = sp.iter().skip(1).sum();
        let hf = &sp[k0..k1];
        let hsum: f32 = hf.iter().sum::<f32>().max(1e-20);
        let am = hsum / hf.len() as f32;
        let gm = (hf.iter().map(|p| (p.max(1e-20) as f64).ln()).sum::<f64>() / hf.len() as f64)
            .exp() as f32;
        frames.push((
            10.0 * full.max(1e-20).log10(),
            10.0 * hsum.log10(),
            (gm / am.max(1e-20)).clamp(0.0, 1.0),
        ));
        at += hop;
    }
    let empty = NoiseBed {
        detected: false,
        hf_floor_db: -140.0,
        flatness: 0.0,
        coverage_percent: 0.0,
        start_s: 0.0,
        end_s: 0.0,
        loop_score: 0.0,
        loop_period_s: None,
    };
    let live: Vec<usize> = (0..frames.len()).filter(|&i| frames[i].0 > -70.0).collect();
    if live.len() < 20 {
        return empty;
    }
    let mut hfs: Vec<f32> = live.iter().map(|&i| frames[i].1).collect();
    hfs.sort_by(|a, b| a.total_cmp(b));
    let floor = hfs[hfs.len() / 10];
    // frames sitting on (or above) the floor with a noise-like spectrum
    let on: Vec<bool> = frames
        .iter()
        .map(|f| f.0 > -70.0 && f.1 > floor - 3.0 && f.2 > 0.3)
        .collect();
    let flat_floor: Vec<f32> = frames
        .iter()
        .filter(|f| f.0 > -70.0 && (f.1 - floor).abs() < 3.0)
        .map(|f| f.2)
        .collect();
    let mut ff = flat_floor.clone();
    ff.sort_by(|a, b| a.total_cmp(b));
    let flatness = ff.get(ff.len() / 2).copied().unwrap_or(0.0);
    let coverage = 100.0 * on.iter().filter(|b| **b).count() as f32 / live.len() as f32;
    // longest run
    let (mut best, mut cur, mut best_end) = (0usize, 0usize, 0usize);
    for (i, b) in on.iter().enumerate() {
        if *b {
            cur += 1;
            if cur > best {
                best = cur;
                best_end = i;
            }
        } else {
            cur = 0;
        }
    }
    // a looped bed repeats its HF envelope: autocorrelation of the hf track
    let hf_env: Vec<f32> = frames.iter().map(|f| f.1).collect();
    let (loop_score, period) = periodicity(&hf_env, 10, 400);
    let detected = floor > -75.0 && flatness > 0.35 && coverage > 50.0;
    NoiseBed {
        detected,
        hf_floor_db: round1(floor),
        flatness: round2(flatness),
        coverage_percent: round1(coverage),
        start_s: round2(((best_end + 1).saturating_sub(best) * hop) as f32 / SR),
        end_s: round2(((best_end + 1) * hop + NFRAME) as f32 / SR),
        loop_score: round2(loop_score),
        loop_period_s: (loop_score > 0.5).then(|| round2((period * hop) as f32 / SR)),
    }
}

/// Best normalised autocorrelation of `x` over lags `lo..hi` (score, lag).
fn periodicity(x: &[f32], lo: usize, hi: usize) -> (f32, usize) {
    let n = x.len();
    if n < lo * 3 {
        return (0.0, 0);
    }
    let m = x.iter().sum::<f32>() / n as f32;
    let v: Vec<f32> = x.iter().map(|a| a - m).collect();
    let e0: f32 = v.iter().map(|a| a * a).sum::<f32>().max(1e-9);
    let mut best = (0.0f32, 0usize);
    for lag in lo..hi.min(n / 2) {
        let mut s = 0.0f32;
        for i in 0..n - lag {
            s += v[i] * v[i + lag];
        }
        let c = s / e0 * (n as f32 / (n - lag) as f32);
        if c > best.0 {
            best = (c, lag);
        }
    }
    best
}

#[derive(Clone, Copy, Debug)]
pub struct ArtifactOptions {
    pub clicks: bool,
    pub max_events: usize,
    /// Seconds at the end that count as the outro/tail for end-level jumps.
    pub tail_s: f32,
}

impl Default for ArtifactOptions {
    fn default() -> Self {
        ArtifactOptions {
            clicks: true,
            max_events: 20,
            tail_s: 6.0,
        }
    }
}

/// Sample positions (in a whole-song render) where a percussive track
/// (kick, snare, hats, perc) starts a hit. A sharp discontinuity right at a
/// drum attack is the drum's own transient (a beater click), not a defect.
pub fn drum_onsets(p: &Project) -> Vec<usize> {
    let (events, _) = crate::render::schedule(p, &Default::default());
    let mut out: Vec<usize> = Vec::new();
    for (t, ev) in p.tracks.iter().zip(events.iter()) {
        let role = crate::tools_mix::role_of(&t.name, &t.instrument);
        if matches!(role, "kick" | "snare" | "hats" | "cymbal" | "perc") && !t.mute {
            out.extend(ev.iter().map(|e| e.start));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Drop click events that sit on a drum attack (-6 ms .. +18 ms of an
/// onset); `offset` is the window start in samples. Returns how many were
/// dropped.
pub fn mask_drum_clicks(arts: &mut Vec<Artifact>, onsets: &[usize], offset: usize) -> usize {
    let before = arts.len();
    // time_s is rounded to 10 ms, hence the margins
    let (pre, post) = ((0.006 * SR) as i64, (0.018 * SR) as i64);
    arts.retain(|a| {
        if a.kind != "click" {
            return true;
        }
        let at = (a.time_s * SR) as i64 + offset as i64;
        let i = onsets.partition_point(|&o| (o as i64) < at - post);
        !onsets[i..]
            .iter()
            .take_while(|&&o| (o as i64) <= at + pre)
            .any(|_| true)
    });
    before - arts.len()
}

/// Run every detector on a stereo buffer.
pub fn detect_artifacts(l: &[f32], r: &[f32], o: &ArtifactOptions) -> (Vec<Artifact>, NoiseBed) {
    let mut out = Vec::new();
    if o.clicks {
        let mut cl = clicks(l, o.max_events);
        for c in clicks(r, o.max_events) {
            if !cl.iter().any(|x| (x.0 as i64 - c.0 as i64).abs() < 441) {
                cl.push(c);
            }
        }
        cl.sort_by_key(|c| c.0);
        for (at, a) in cl.into_iter().take(o.max_events) {
            out.push(art(
                "click",
                if a > 0.2 { "fail" } else { "warn" },
                at as f32 / SR,
                None,
                format!("isolated discontinuity, step {:.1} dBFS", gain_to_db(a)),
            ));
        }
    }
    for ch in [l, r] {
        for (at, d) in dc_steps(ch).into_iter().take(o.max_events) {
            if !out
                .iter()
                .any(|a: &Artifact| a.kind == "dc_step" && (a.time_s - at as f32 / SR).abs() < 0.2)
            {
                out.push(art(
                    "dc_step",
                    "warn",
                    at as f32 / SR,
                    None,
                    format!("DC level jumps by {d:+.3}"),
                ));
            }
        }
    }
    if let Some((t, j)) = end_level_jump(l, r, o.tail_s) {
        out.push(art(
            "end_level_jump",
            "fail",
            t,
            None,
            format!("after fading out, the level jumps back up {j:.1} dB near the end"),
        ));
    }
    if let Some(lv) = truncated_tail(l, r) {
        let n = l.len().min(r.len());
        out.push(art(
            "truncated_tail",
            "warn",
            n as f32 / SR,
            None,
            format!(
                "audio ends while still sounding (last 50 ms at {lv:.1} dBFS); add tail or a fade"
            ),
        ));
    }
    let bed = noise_bed(l, r);
    if bed.detected {
        let sev = if bed.hf_floor_db > -55.0 {
            "fail"
        } else {
            "warn"
        };
        let looped = bed
            .loop_period_s
            .map(|p| format!(", looping every ~{p:.2} s"))
            .unwrap_or_default();
        out.push(art(
            "noise_bed",
            sev,
            bed.start_s,
            Some(bed.end_s),
            format!(
                "sustained hiss/noise bed: 6-16 kHz floor {:.1} dB, flatness {:.2}, covers {:.0}% of the audio{looped}",
                bed.hf_floor_db, bed.flatness, bed.coverage_percent
            ),
        ));
    }
    out.sort_by(|a, b| a.time_s.total_cmp(&b.time_s));
    (out, bed)
}

// ---------------------------------------------------------------- images

/// Magma-like colour map, t in 0..1.
fn colour(t: f32) -> [u8; 3] {
    const STOPS: [(f32, [f32; 3]); 6] = [
        (0.0, [0.0, 0.0, 4.0]),
        (0.2, [40.0, 11.0, 84.0]),
        (0.45, [140.0, 41.0, 129.0]),
        (0.7, [229.0, 80.0, 57.0]),
        (0.88, [252.0, 165.0, 50.0]),
        (1.0, [252.0, 253.0, 191.0]),
    ];
    let t = t.clamp(0.0, 1.0);
    for w in STOPS.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t <= b.0 {
            let f = (t - a.0) / (b.0 - a.0);
            return [
                (a.1[0] + (b.1[0] - a.1[0]) * f) as u8,
                (a.1[1] + (b.1[1] - a.1[1]) * f) as u8,
                (a.1[2] + (b.1[2] - a.1[2]) * f) as u8,
            ];
        }
    }
    [252, 253, 191]
}

/// Log-frequency spectrogram (20 Hz at the bottom, 20 kHz at the top) as
/// RGB pixels. `marks` are vertical marker times (seconds from the window
/// start), drawn as thin lines (section boundaries).
pub fn spectrogram_rgb(
    l: &[f32],
    r: &[f32],
    width: usize,
    height: usize,
    floor_db: f32,
    marks: &[f32],
) -> Vec<u8> {
    let n = l.len().min(r.len());
    let mono: Vec<f32> = (0..n).map(|i| 0.5 * (l[i] + r[i])).collect();
    let win = hann(NFRAME);
    let bin_hz = SR / NFRAME as f32;
    let mut img = vec![0u8; width * height * 3];
    // row -> (bin lo, bin hi)
    let rows: Vec<(usize, usize)> = (0..height)
        .map(|y| {
            let t0 = (height - 1 - y) as f32 / height as f32;
            let t1 = (height - y) as f32 / height as f32;
            let f0 = 20.0 * 1000f32.powf(t0);
            let f1 = 20.0 * 1000f32.powf(t1);
            let k0 = ((f0 / bin_hz) as usize).clamp(1, NFRAME / 2 - 1);
            let k1 = ((f1 / bin_hz).ceil() as usize).clamp(k0 + 1, NFRAME / 2);
            (k0, k1)
        })
        .collect();
    for x in 0..width {
        let center = if width > 1 {
            (x as f32 / (width - 1) as f32 * n as f32) as usize
        } else {
            0
        };
        let at = center.saturating_sub(NFRAME / 2);
        let sp = frame_spec(&mono, at, &win);
        for (y, (k0, k1)) in rows.iter().enumerate() {
            let p = sp[*k0..*k1].iter().cloned().fold(0.0f32, f32::max);
            let db = 10.0 * p.max(1e-20).log10();
            let c = colour((db - floor_db) / -floor_db);
            let o = (y * width + x) * 3;
            img[o..o + 3].copy_from_slice(&c);
        }
    }
    // faint frequency grid at 100 Hz, 1 kHz, 10 kHz
    for f in [100.0f32, 1000.0, 10000.0] {
        let t = (f / 20.0).log10() / 3.0;
        let y = ((1.0 - t) * height as f32) as usize;
        if y < height {
            for x in (0..width).step_by(3) {
                let o = (y * width + x) * 3;
                img[o..o + 3].copy_from_slice(&[90, 90, 110]);
            }
        }
    }
    let secs = n as f32 / SR;
    for m in marks {
        if secs <= 0.0 {
            break;
        }
        let x = ((m / secs) * width as f32) as usize;
        if x < width {
            for y in 0..height {
                let o = (y * width + x) * 3;
                img[o..o + 3].copy_from_slice(&[230, 230, 230]);
            }
        }
    }
    img
}

/// Min/max waveform peaks in `cols` columns (mono fold).
pub fn peaks(l: &[f32], r: &[f32], cols: usize) -> Vec<[f32; 2]> {
    let n = l.len().min(r.len());
    let mono: Vec<f32> = (0..n).map(|i| 0.5 * (l[i] + r[i])).collect();
    analysis::waveform_peaks(&mono, cols)
        .into_iter()
        .map(|(a, b)| [(a * 1000.0).round() / 1000.0, (b * 1000.0).round() / 1000.0])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn music(secs: f32) -> Vec<f32> {
        let n = (secs * SR) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / SR;
                let beat = (t * 2.0).fract();
                let kick = (2.0 * PI * 55.0 * t).sin() * (-beat * 12.0).exp() * 0.5;
                let pad = 0.1 * (2.0 * PI * 220.0 * t).sin() + 0.05 * (2.0 * PI * 330.0 * t).sin();
                kick + pad
            })
            .collect()
    }

    fn noise(n: usize, amp: f32, seed: u64) -> Vec<f32> {
        let mut rng = crate::dsp::Rng::new(seed);
        (0..n).map(|_| rng.bipolar() * amp).collect()
    }

    #[test]
    fn finds_click_but_not_clean_music() {
        let mut x = music(3.0);
        assert!(clicks(&x, 10).is_empty(), "{:?}", clicks(&x, 10));
        x[50_000] += 0.4;
        let c = clicks(&x, 10);
        assert_eq!(c.len(), 1);
        assert!((c[0].0 as i64 - 50_000).abs() < 3);
    }

    #[test]
    fn hiss_bed_detected_and_clean_is_clean() {
        let x = music(8.0);
        let (_, clean) = detect_artifacts(&x, &x, &ArtifactOptions::default());
        assert!(!clean.detected, "{clean:?}");
        let nz = noise(x.len(), 0.01, 7);
        let y: Vec<f32> = x.iter().zip(&nz).map(|(a, b)| a + b).collect();
        let (arts, bed) = detect_artifacts(&y, &y, &ArtifactOptions::default());
        assert!(bed.detected, "{bed:?}");
        assert!(arts.iter().any(|a| a.kind == "noise_bed"));
        // hi-hats (short noise bursts) are not a bed
        let hats: Vec<f32> = x
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let ph = (i as f32 / SR * 4.0).fract();
                a + if ph < 0.03 { nz[i] * 20.0 } else { 0.0 }
            })
            .collect();
        assert!(!noise_bed(&hats, &hats).detected);
    }

    #[test]
    fn end_jump_and_truncation() {
        let mut x = music(6.0);
        let n = x.len();
        // fade out over the last 3 s ...
        for i in (n - 3 * 44100)..n {
            x[i] *= ((n - i) as f32 / (3.0 * SR)).powi(3);
        }
        assert!(end_level_jump(&x, &x, 4.0).is_none());
        assert!(truncated_tail(&x, &x).is_none());
        // ... then a stray hit in the last half second
        for i in (n - 15000)..(n - 10000) {
            x[i] += 0.3 * (2.0 * PI * 200.0 * i as f32 / SR).sin();
        }
        let j = end_level_jump(&x, &x, 4.0).expect("jump");
        assert!(j.1 > 10.0);
        let cut = music(2.0);
        assert!(truncated_tail(&cut, &cut).is_some());
    }

    #[test]
    fn spectrogram_has_size() {
        let x = music(1.0);
        let img = spectrogram_rgb(&x, &x, 64, 32, -100.0, &[0.5]);
        assert_eq!(img.len(), 64 * 32 * 3);
        assert!(img.iter().any(|v| *v > 100));
    }
}
