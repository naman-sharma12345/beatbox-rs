//! Musical novelty: a fingerprint per beat and a distance that compares what
//! a listener would hear as "the same beat again".
//!
//! * rhythm: per-voice onset grids folded onto two bars, with the genre's
//!   conventions masked out (a trap snare on 3 is not similarity, it is trap),
//!   plus the hat subdivision profile (32nds, triplets, 16ths, 8ths);
//! * melody: interval bigrams and the up/down contour of the hook lead,
//!   which is transposition-invariant;
//! * harmony: bar-by-bar bass roots relative to the key (functional, so the
//!   same progression in another key is the same harmony);
//! * palette: instrument signatures per track;
//! * arrangement: the section map at 4-bar resolution (order and lengths);
//! * tempo/key (small weight) and a coarse key-relative chroma + spectrum
//!   summary of the render; `embedding` is reserved for an audio model.
//!
//! Different seeds are not variation by themselves: the distance only sees
//! the music. Novelty is not quality either; it only stops repeats.

use crate::project::{Note, Project};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_THRESHOLD: f32 = 0.25;
pub const DEFAULT_WINDOW: usize = 30;

const VOICES: &[(&str, f32)] = &[
    ("kick", 1.5),
    ("snare", 1.0),
    ("hat", 1.0),
    ("open_hat", 0.4),
    ("perc", 0.7),
    ("tabla", 0.7),
    ("bayan", 0.5),
];

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct Fingerprint {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub genre: String,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub created_unix: u64,
    pub bpm: f32,
    pub key_pc: u8,
    pub mode: String,
    /// voice -> 32 bins (two bars of 16ths): share of 2-bar cycles with an onset there.
    pub rhythm: BTreeMap<String, Vec<f32>>,
    /// Hat inter-onset profile: 32nd, 16th-triplet, 16th, 8th-triplet, 8th+.
    pub hat_rates: Vec<f32>,
    /// voice -> onsets per bar.
    pub density: BTreeMap<String, f32>,
    /// Hook lead interval bigrams ("+2,-1") -> share.
    pub intervals: BTreeMap<String, f32>,
    /// Hook lead contour: -1/0/1 per step.
    pub contour: Vec<i8>,
    /// Bass-root bigrams relative to the key ("0>8") -> share.
    pub harmony: BTreeMap<String, f32>,
    pub roots: Vec<i32>,
    /// (kind, bars) in song order.
    pub sections: Vec<(String, u32)>,
    pub palette: Vec<String>,
    #[serde(default)]
    pub chroma: Vec<f32>,
    #[serde(default)]
    pub spectrum: Vec<f32>,
    /// Reserved for an audio embedding (CLAP/EfficientAT); compared by cosine when both have one.
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
}

fn kind_of(pattern: &str) -> String {
    pattern
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .to_string()
}

fn top_line(notes: &[Note]) -> Vec<Note> {
    let mut by: BTreeMap<i64, Note> = BTreeMap::new();
    for n in notes {
        let k = (n.start * 100.0).round() as i64;
        if by.get(&k).map(|x| n.pitch > x.pitch).unwrap_or(true) {
            by.insert(k, n.clone());
        }
    }
    by.into_values().collect()
}

fn instrument_sig(t: &crate::project::Track) -> String {
    let v = serde_json::to_value(&t.instrument).unwrap_or(Value::Null);
    let mut s = v["type"].as_str().unwrap_or("?").to_string();
    for k in ["kind", "osc1", "osc2", "sample", "name", "preset"] {
        if let Some(x) = v[k].as_str() {
            s.push(':');
            s.push_str(x);
        }
    }
    format!("{}={}", t.name, s)
}

/// The symbolic fingerprint of a project (audio fields empty).
pub fn fingerprint(p: &Project) -> Fingerprint {
    let key_pc = crate::theory::pitch_class(&p.key_root).unwrap_or(0);
    let mut f = Fingerprint {
        id: crate::listen::project_hash(p),
        bpm: p.bpm,
        key_pc,
        mode: p.scale.clone(),
        ..Default::default()
    };
    let order: Vec<(usize, u32)> = if p.arrangement.is_empty() {
        if p.patterns.is_empty() {
            Vec::new()
        } else {
            vec![(0, 1)]
        }
    } else {
        p.arrangement
            .iter()
            .filter_map(|s| {
                p.pattern_index(&s.pattern)
                    .ok()
                    .map(|i| (i, s.repeats.max(1)))
            })
            .collect()
    };
    // ---- rhythm
    let mut total_bars = 0u32;
    let mut grids: BTreeMap<String, Vec<f32>> = BTreeMap::new();
    let mut counts: BTreeMap<String, f32> = BTreeMap::new();
    let mut iois: Vec<f32> = Vec::new();
    for &(pi, rep) in &order {
        let pat = &p.patterns[pi];
        total_bars += pat.bars * rep;
        for (v, _) in VOICES {
            let notes = pat.notes(v);
            if notes.is_empty() {
                continue;
            }
            let g = grids.entry(v.to_string()).or_insert_with(|| vec![0.0; 32]);
            let mut seen = std::collections::BTreeSet::new();
            for n in notes {
                let cyc = (n.start / 32.0).floor() as i64;
                let bin = (n.start.rem_euclid(32.0)).floor() as usize % 32;
                if seen.insert((cyc, bin)) {
                    g[bin] += rep as f32;
                }
            }
            *counts.entry(v.to_string()).or_insert(0.0) += notes.len() as f32 * rep as f32;
            if *v == "hat" {
                let mut st: Vec<f32> = notes.iter().map(|n| n.start).collect();
                st.sort_by(|a, b| a.partial_cmp(b).unwrap());
                for w in st.windows(2) {
                    let d = w[1] - w[0];
                    if d > 0.01 {
                        iois.push(d);
                    }
                }
            }
        }
    }
    let cycles = (total_bars as f32 / 2.0).max(1.0);
    for (v, g) in grids.iter_mut() {
        for x in g.iter_mut() {
            *x = (*x / cycles).min(1.0);
        }
        f.density.insert(
            v.clone(),
            counts.get(v).copied().unwrap_or(0.0) / total_bars.max(1) as f32,
        );
    }
    f.rhythm = grids;
    let mut hr = vec![0.0f32; 5];
    for d in &iois {
        let i = if *d <= 0.55 {
            0
        } else if *d <= 0.75 {
            1
        } else if *d <= 1.05 {
            2
        } else if *d <= 1.4 {
            3
        } else {
            4
        };
        hr[i] += 1.0;
    }
    let s: f32 = hr.iter().sum();
    if s > 0.0 {
        for x in hr.iter_mut() {
            *x /= s;
        }
    }
    f.hat_rates = hr;
    // ---- melody (hook lead, else any lead)
    let mut hook_pats: Vec<usize> = Vec::new();
    for &(pi, _) in &order {
        if kind_of(&p.patterns[pi].name) == "hook" && !hook_pats.contains(&pi) {
            hook_pats.push(pi);
        }
    }
    if hook_pats.is_empty() {
        for &(pi, _) in &order {
            if !hook_pats.contains(&pi) {
                hook_pats.push(pi);
            }
        }
    }
    let mut ints: Vec<i32> = Vec::new();
    for pi in &hook_pats {
        let line = top_line(p.patterns[*pi].notes("lead"));
        for w in line.windows(2) {
            ints.push((w[1].pitch as i32 - w[0].pitch as i32).clamp(-12, 12));
        }
    }
    let mut ib: BTreeMap<String, f32> = BTreeMap::new();
    for w in ints.windows(2) {
        *ib.entry(format!("{:+},{:+}", w[0], w[1])).or_insert(0.0) += 1.0;
    }
    normalize(&mut ib);
    f.intervals = ib;
    f.contour = ints.iter().take(48).map(|x| x.signum() as i8).collect();
    // ---- harmony: bass roots per bar relative to the key
    let bass_track = if p.tracks.iter().any(|t| t.name == "bass") {
        "bass"
    } else {
        "chords"
    };
    let mut roots = Vec::new();
    for &(pi, rep) in &order {
        let pat = &p.patterns[pi];
        let notes = pat.notes(bass_track);
        for _ in 0..rep {
            for bar in 0..pat.bars {
                let t = bar as f32 * 16.0;
                let r = notes
                    .iter()
                    .filter(|n| n.start <= t + 1.0 && n.start + n.len > t)
                    .map(|n| n.pitch)
                    .min();
                if let Some(r) = r {
                    roots.push((r as i32 - key_pc as i32).rem_euclid(12));
                }
            }
        }
    }
    let mut hb: BTreeMap<String, f32> = BTreeMap::new();
    for w in roots.windows(2) {
        *hb.entry(format!("{}>{}", w[0], w[1])).or_insert(0.0) += 1.0;
    }
    normalize(&mut hb);
    f.harmony = hb;
    f.roots = roots;
    // ---- sections and palette
    f.sections = order
        .iter()
        .map(|(pi, rep)| (kind_of(&p.patterns[*pi].name), p.patterns[*pi].bars * rep))
        .collect();
    f.palette = p.tracks.iter().map(instrument_sig).collect();
    f.palette.sort();
    f
}

fn normalize(m: &mut BTreeMap<String, f32>) {
    let s: f32 = m.values().sum();
    if s > 0.0 {
        for v in m.values_mut() {
            *v /= s;
        }
    }
}

/// Key-relative chroma (12) and an 8-band spectrum summary of a render.
pub fn audio_summary(l: &[f32], r: &[f32], key_pc: u8) -> (Vec<f32>, Vec<f32>) {
    const N: usize = 4096;
    let mono: Vec<f32> = l.iter().zip(r).map(|(a, b)| 0.5 * (a + b)).collect();
    if mono.len() < N {
        return (Vec::new(), Vec::new());
    }
    let frames = 240usize;
    let mut acc = vec![0.0f32; N / 2];
    for i in 0..frames {
        let s = i * (mono.len() - N) / frames;
        let ps = crate::analysis::power_spectrum(&mono[s..s + N]);
        for (a, p) in acc.iter_mut().zip(ps.iter()) {
            *a += *p;
        }
    }
    let sr = crate::dsp::SR;
    let hz = |i: usize| i as f32 * sr / N as f32;
    let mut chroma = vec![0.0f32; 12];
    let edges = [
        30.0f32, 60.0, 120.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 16000.0,
    ];
    let mut bands = [0.0f32; 8];
    for (i, p) in acc.iter().enumerate().skip(1) {
        let f = hz(i);
        if (55.0..2000.0).contains(&f) {
            let midi = 69.0 + 12.0 * (f / 440.0).log2();
            let pc = (midi.round() as i32).rem_euclid(12);
            chroma[((pc - key_pc as i32).rem_euclid(12)) as usize] += p;
        }
        if let Some(b) = edges.windows(2).position(|w| f >= w[0] && f < w[1]) {
            bands[b] += p;
        }
    }
    let cs: f32 = chroma.iter().sum::<f32>().max(1e-12);
    for x in chroma.iter_mut() {
        *x /= cs;
    }
    let bands = bands
        .iter()
        .map(|x| ((10.0 * (x.max(1e-12)).log10() + 120.0) / 120.0).clamp(0.0, 1.5))
        .collect();
    (chroma, bands)
}

/// Positions per voice that are conventions of the genre (in >= 60% of its
/// templates): shared conventions are not counted as similarity.
pub fn conventions(genre: &str) -> BTreeMap<String, Vec<bool>> {
    let mut out = BTreeMap::new();
    let Ok(pb) = crate::producer::playbook(genre) else {
        return out;
    };
    for (v, list) in [
        ("kick", &pb.drums.kick),
        ("snare", &pb.drums.snare),
        ("hat", &pb.drums.hat),
        ("perc", &pb.drums.perc),
        ("tabla", &pb.drums.tabla),
        ("bayan", &pb.drums.bayan),
    ] {
        let fq = crate::creative::template_freq(list);
        out.insert(v.to_string(), fq.iter().map(|x| *x >= 0.6).collect());
    }
    out
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Distance {
    pub total: f32,
    pub rhythm: Option<f32>,
    pub melody: Option<f32>,
    pub harmony: Option<f32>,
    pub arrangement: Option<f32>,
    pub palette: Option<f32>,
    pub tempo_key: Option<f32>,
    pub audio: Option<f32>,
}

fn cos_map(a: &BTreeMap<String, f32>, b: &BTreeMap<String, f32>) -> Option<f32> {
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let dot: f32 = a
        .iter()
        .map(|(k, v)| v * b.get(k).copied().unwrap_or(0.0))
        .sum();
    let na: f32 = a.values().map(|v| v * v).sum::<f32>().sqrt();
    let nb: f32 = b.values().map(|v| v * v).sum::<f32>().sqrt();
    Some((1.0 - dot / (na * nb).max(1e-9)).clamp(0.0, 1.0))
}

fn cos_vec(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|v| v * v).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|v| v * v).sum::<f32>().sqrt();
    Some((1.0 - dot / (na * nb).max(1e-9)).clamp(0.0, 1.0))
}

fn lev<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(x != y))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

pub fn distance(a: &Fingerprint, b: &Fingerprint) -> Distance {
    let mut d = Distance::default();
    // rhythm, conventions masked
    let mask = if a.genre == b.genre && !a.genre.is_empty() {
        conventions(&a.genre)
    } else {
        BTreeMap::new()
    };
    let (mut acc, mut wsum) = (0.0f32, 0.0f32);
    for (v, w) in VOICES {
        let ga = a.rhythm.get(*v);
        let gb = b.rhythm.get(*v);
        let dv = match (ga, gb) {
            (None, None) => continue,
            (Some(x), None) | (None, Some(x)) => {
                if x.iter().sum::<f32>() < 0.05 {
                    continue;
                }
                1.0
            }
            (Some(x), Some(y)) => {
                let mk = mask.get(*v);
                let (mut num, mut den) = (0.0f32, 0.0f32);
                for (i, (p, q)) in x.iter().zip(y.iter()).enumerate() {
                    if mk.and_then(|m| m.get(i)).copied().unwrap_or(false) {
                        continue;
                    }
                    num += (p - q).abs();
                    den += p.max(*q);
                }
                if den < 0.05 {
                    continue;
                }
                num / den
            }
        };
        acc += dv * w;
        wsum += w;
    }
    if a.hat_rates.len() == 5
        && b.hat_rates.len() == 5
        && a.hat_rates.iter().sum::<f32>() > 0.0
        && b.hat_rates.iter().sum::<f32>() > 0.0
    {
        acc += 0.5
            * a.hat_rates
                .iter()
                .zip(&b.hat_rates)
                .map(|(x, y)| (x - y).abs())
                .sum::<f32>();
        wsum += 1.0;
    }
    d.rhythm = (wsum > 0.0).then_some(acc / wsum);
    // melody: interval bigrams + contour
    let mi = cos_map(&a.intervals, &b.intervals);
    let mc = if a.contour.is_empty() || b.contour.is_empty() {
        None
    } else {
        Some(lev(&a.contour, &b.contour) as f32 / a.contour.len().max(b.contour.len()) as f32)
    };
    d.melody = match (mi, mc) {
        (Some(x), Some(y)) => Some(0.6 * x + 0.4 * y),
        (x, y) => x.or(y),
    };
    d.harmony = cos_map(&a.harmony, &b.harmony);
    // arrangement at 4-bar resolution
    let toks = |f: &Fingerprint| -> Vec<String> {
        f.sections
            .iter()
            .flat_map(|(k, bars)| {
                std::iter::repeat_n(k.clone(), (*bars as usize).div_ceil(4).max(1))
            })
            .collect()
    };
    let (ta, tb) = (toks(a), toks(b));
    if !ta.is_empty() && !tb.is_empty() {
        d.arrangement = Some(lev(&ta, &tb) as f32 / ta.len().max(tb.len()) as f32);
    }
    if !a.palette.is_empty() && !b.palette.is_empty() {
        let inter = a.palette.iter().filter(|x| b.palette.contains(x)).count() as f32;
        let uni = (a.palette.len() + b.palette.len()) as f32 - inter;
        d.palette = Some(1.0 - inter / uni.max(1.0));
    }
    d.tempo_key = Some(
        0.5 * ((a.bpm - b.bpm).abs() / 20.0).min(1.0)
            + 0.25 * f32::from(a.key_pc != b.key_pc)
            + 0.25 * f32::from(a.mode != b.mode),
    );
    let au = match (
        cos_vec(&a.chroma, &b.chroma),
        cos_vec(&a.spectrum, &b.spectrum),
    ) {
        (Some(x), Some(y)) => Some(((x + y) * 2.0).min(1.0)),
        _ => None,
    };
    d.audio = match (&a.embedding, &b.embedding) {
        (Some(x), Some(y)) => cos_vec(x, y).or(au),
        _ => au,
    };
    let parts = [
        (d.rhythm, 0.30),
        (d.melody, 0.20),
        (d.harmony, 0.15),
        (d.arrangement, 0.15),
        (d.palette, 0.10),
        (d.tempo_key, 0.05),
        (d.audio, 0.05),
    ];
    let (mut s, mut w) = (0.0f32, 0.0f32);
    for (x, wt) in parts {
        if let Some(x) = x {
            s += x * wt;
            w += wt;
        }
    }
    d.total = if w > 0.0 { s / w } else { 0.0 };
    d
}

pub fn round_d(d: &Distance) -> Value {
    let r = |x: Option<f32>| x.map(|v| (v * 1000.0).round() / 1000.0);
    json!({
        "total": (d.total * 1000.0).round() / 1000.0,
        "rhythm": r(d.rhythm), "melody": r(d.melody), "harmony": r(d.harmony),
        "arrangement": r(d.arrangement), "palette": r(d.palette), "tempo_key": r(d.tempo_key), "audio": r(d.audio),
    })
}

// ---------------------------------------------------------------- history

#[derive(Clone, Debug)]
pub struct NoveltyOpts {
    pub enabled: bool,
    pub threshold: f32,
    pub history: Option<PathBuf>,
    pub window: usize,
    pub max_attempts: usize,
    /// Append the delivered beat to the history.
    pub record: bool,
}

impl Default for NoveltyOpts {
    fn default() -> Self {
        NoveltyOpts {
            enabled: true,
            threshold: std::env::var("BEATBOX_NOVELTY_THRESHOLD")
                .ok()
                .and_then(|x| x.parse().ok())
                .unwrap_or(DEFAULT_THRESHOLD),
            history: std::env::var_os("BEATBOX_NOVELTY_HISTORY").map(PathBuf::from),
            window: DEFAULT_WINDOW,
            max_attempts: 4,
            record: true,
        }
    }
}

pub fn history_path(e: &crate::engine::Engine, o: &NoveltyOpts) -> PathBuf {
    o.history
        .clone()
        .unwrap_or_else(|| e.resolve("beatbox_history/fingerprints.jsonl"))
}

pub fn load_history(path: &Path, window: usize) -> Vec<Fingerprint> {
    let Ok(s) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let all: Vec<Fingerprint> = s
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let n = all.len();
    all.into_iter().skip(n.saturating_sub(window)).collect()
}

pub fn append_history(path: &Path, f: &Fingerprint) -> Result<()> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut h = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(h, "{}", serde_json::to_string(f)?)?;
    Ok(())
}

/// The nearest entry in `hist` (skipping identical ids, i.e. the beat itself).
pub fn nearest<'a>(
    f: &Fingerprint,
    hist: &'a [Fingerprint],
) -> Option<(&'a Fingerprint, Distance)> {
    hist.iter()
        .filter(|h| h.id != f.id || h.label != f.label)
        .map(|h| (h, distance(f, h)))
        .min_by(|a, b| {
            a.1.total
                .partial_cmp(&b.1.total)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// Load a project file (or the first project json in a directory) and the
/// audio next to it (same stem .mp3/.wav, or any audio in the directory).
pub fn load_project(path: &Path) -> Result<(Project, Option<PathBuf>, String)> {
    let file = if path.is_dir() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(path)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                let n = p.file_name().and_then(|x| x.to_str()).unwrap_or("");
                n.ends_with(".json") && !n.ends_with(".plan.json") && !n.ends_with(".result.json")
            })
            .collect();
        v.sort();
        v.into_iter()
            .next()
            .ok_or_else(|| anyhow!("no project json in {}", path.display()))?
    } else {
        path.to_path_buf()
    };
    let p: Project = serde_json::from_str(&std::fs::read_to_string(&file)?)?;
    let stem = file.with_extension("");
    let audio = ["mp3", "wav", "flac"]
        .iter()
        .map(|x| stem.with_extension(x))
        .find(|p| p.exists());
    let label = path
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .trim_end_matches(".json")
        .to_string();
    Ok((p, audio, label))
}

pub fn fingerprint_file(path: &Path, with_audio: bool) -> Result<Fingerprint> {
    let (p, audio, label) = load_project(path)?;
    let mut f = fingerprint(&p);
    f.label = label;
    if let Some(plan) = plan_beside(path) {
        f.genre = plan.genre.clone();
        f.seed = Some(plan.seed);
    }
    if with_audio {
        if let Some(a) = audio {
            let (l, r) = crate::samples::decode_stereo(&a)?;
            let (c, s) = audio_summary(&l, &r, f.key_pc);
            f.chroma = c;
            f.spectrum = s;
        }
    }
    Ok(f)
}

fn plan_beside(path: &Path) -> Option<crate::producer::Plan> {
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()?.to_path_buf()
    };
    let pf = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.to_string_lossy().ends_with(".plan.json"))?;
    serde_json::from_str(&std::fs::read_to_string(pf).ok()?).ok()
}

/// Pairwise matrix of total distances (and per-component tables).
pub fn matrix(fps: &[Fingerprint]) -> Value {
    let labels: Vec<String> = fps
        .iter()
        .map(|f| {
            if f.label.is_empty() {
                f.id.clone()
            } else {
                f.label.clone()
            }
        })
        .collect();
    let mut total = Vec::new();
    let mut comps = Vec::new();
    for a in fps {
        let mut row = Vec::new();
        let mut crow = Vec::new();
        for b in fps {
            let d = distance(a, b);
            row.push((d.total * 1000.0).round() / 1000.0);
            crow.push(round_d(&d));
        }
        total.push(row);
        comps.push(crow);
    }
    json!({"labels": labels, "total": total, "components": comps})
}

pub fn summary(f: &Fingerprint) -> Value {
    json!({
        "id": f.id, "label": f.label, "genre": f.genre, "seed": f.seed, "bpm": f.bpm,
        "key": format!("{} {}", crate::theory::NOTE_NAMES[f.key_pc as usize % 12], f.mode),
        "kick_grid": f.rhythm.get("kick").map(|g| g.iter().map(|x| if *x >= 0.5 { 'X' } else if *x >= 0.15 { 'x' } else { '.' }).collect::<String>()),
        "snare_grid": f.rhythm.get("snare").map(|g| g.iter().map(|x| if *x >= 0.5 { 'X' } else if *x >= 0.15 { 'x' } else { '.' }).collect::<String>()),
        "hat_rates": f.hat_rates, "density": f.density,
        "roots": f.roots.iter().take(32).collect::<Vec<_>>(),
        "sections": f.sections, "has_audio": !f.chroma.is_empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_is_zero_and_levenshtein_works() {
        let mut p = Project::new("t", 140.0);
        p.patterns.clear();
        let mut pat = crate::project::Pattern::new("hook1", 2);
        pat.clips.insert(
            "kick".into(),
            vec![Note::new(0.0, 1.0, 60, 1.0), Note::new(7.0, 1.0, 60, 0.8)],
        );
        pat.clips.insert(
            "lead".into(),
            vec![
                Note::new(0.0, 1.0, 60, 1.0),
                Note::new(4.0, 1.0, 62, 1.0),
                Note::new(8.0, 1.0, 59, 1.0),
            ],
        );
        p.patterns.push(pat);
        let f = fingerprint(&p);
        assert!(distance(&f, &f).total < 1e-6);
        assert_eq!(lev(&[1, 2, 3], &[1, 3]), 1);
        // transposing the melody does not change the melodic fingerprint
        let mut q = p.clone();
        for n in q.patterns[0].clips.get_mut("lead").unwrap() {
            n.pitch += 5;
        }
        let g = fingerprint(&q);
        assert_eq!(f.intervals, g.intervals);
        assert_eq!(f.contour, g.contour);
    }

    #[test]
    fn history_roundtrip() {
        let d = std::env::temp_dir().join(format!("bb_nov_{}", crate::creative::fresh_seed()));
        let path = d.join("h.jsonl");
        let f = Fingerprint {
            id: "a".into(),
            bpm: 140.0,
            mode: "minor".into(),
            ..Default::default()
        };
        append_history(&path, &f).unwrap();
        append_history(
            &path,
            &Fingerprint {
                id: "b".into(),
                bpm: 90.0,
                ..f.clone()
            },
        )
        .unwrap();
        let h = load_history(&path, 10);
        assert_eq!(h.len(), 2);
        let (n, _) = nearest(
            &Fingerprint {
                id: "c".into(),
                ..f.clone()
            },
            &h,
        )
        .unwrap();
        assert_eq!(n.id, "a");
        let _ = std::fs::remove_dir_all(d);
    }
}
