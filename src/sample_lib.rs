//! Sample library: curated public-domain/CC0 drum kits, a local sample
//! index (role / pitch / key / BPM / length tags from Beatbox's own ears),
//! search, and the flip workflows (chop + re-pitch + rearrange, vocal chops).

use crate::dsp::{Rng, SR};
use crate::engine::Engine;
use crate::project::Note;
use crate::samples::{self, SampleInfo};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct Kit {
    pub name: &'static str,
    pub description: &'static str,
    pub base: &'static str,
    pub license: &'static str,
    pub source: &'static str,
    /// preset-name -> file under base
    pub files: &'static [(&'static str, &'static str)],
}

const SMPLD: &str = "https://raw.githubusercontent.com/smpldsnds/drum-machines/main/";

pub const KITS: &[Kit] = &[
    Kit {
        name: "tr808",
        description: "Roland TR-808 multisampled by Michael Fischer: the trap/drill/R&B kit (boomy kick, snare, clap, hats, rim, cowbell, maraca, congas)",
        base: "TR-808/",
        license: "Public domain / CC0 1.0 (TR-808 samples by Michael Fischer, 1994, via smpldsnds/drum-machines)",
        source: "https://github.com/smpldsnds/drum-machines/tree/main/TR-808",
        files: &[
            ("kick", "kick/bd2510.ogg"),
            ("layered_kick", "kick/bd5025.ogg"),
            ("snare", "snare/sd5050.ogg"),
            ("layered_snare", "snare/sd7575.ogg"),
            ("clap", "clap/cp.ogg"),
            ("hat", "hihat-close/ch.ogg"),
            ("open_hat", "hihat-open/oh25.ogg"),
            ("rim", "rimshot/rs.ogg"),
            ("cowbell", "cowbell/cb.ogg"),
            ("shaker", "maraca/ma.ogg"),
            ("tom", "mid-tom/mt50.ogg"),
            ("crash", "cymbal/cy5050.ogg"),
        ],
    },
    Kit {
        name: "lm2",
        description: "LinnDrum LM-2: the classic 80s/90s hip-hop kit (punchy kick, snappy snare, crisp hats, claps, sticks, tambourine)",
        base: "LM-2/",
        license: "Public domain (smpldsnds/drum-machines)",
        source: "https://github.com/smpldsnds/drum-machines/tree/main/LM-2",
        files: &[
            ("kick", "kick.ogg"),
            ("layered_kick", "kick-alt.ogg"),
            ("snare", "snare-m.ogg"),
            ("layered_snare", "snare-h.ogg"),
            ("clap", "clap.ogg"),
            ("hat", "hhclosed.ogg"),
            ("open_hat", "hhopen.ogg"),
            ("rim", "stick-m.ogg"),
            ("cowbell", "cowbell.ogg"),
            ("shaker", "tambourine.ogg"),
            ("tom", "tom-m.ogg"),
            ("crash", "crash.ogg"),
        ],
    },
    Kit {
        name: "rz1",
        description: "Casio RZ-1: gritty 12-bit lo-fi drum machine kit",
        base: "Casio-RZ1/",
        license: "Public domain (smpldsnds/drum-machines)",
        source: "https://github.com/smpldsnds/drum-machines/tree/main/Casio-RZ1",
        files: &[
            ("kick", "kick.ogg"),
            ("snare", "snare.ogg"),
            ("clap", "clap.ogg"),
            ("hat", "hihat-closed.ogg"),
            ("open_hat", "hihat-open.ogg"),
            ("rim", "clave.ogg"),
            ("cowbell", "cowbell.ogg"),
            ("tom", "tom-2.ogg"),
            ("crash", "crash.ogg"),
        ],
    },
];

pub fn kit(name: &str) -> Result<&'static Kit> {
    let n = name.trim().to_lowercase().replace(['-', ' ', '_'], "");
    KITS.iter()
        .find(|k| {
            k.name == n || (n == "808" && k.name == "tr808") || (n == "linndrum" && k.name == "lm2")
        })
        .ok_or_else(|| {
            anyhow!(
                "unknown kit '{name}'. Kits: {}",
                KITS.iter().map(|k| k.name).collect::<Vec<_>>().join(", ")
            )
        })
}

pub fn kit_dir(e: &Engine, k: &Kit) -> PathBuf {
    e.samples_dir().join("kits").join(k.name)
}

/// Download a kit (cached on disk). Returns the local files.
pub fn install_kit(e: &Engine, name: &str) -> Result<Vec<(String, PathBuf)>> {
    let k = kit(name)?;
    let dir = kit_dir(e, k);
    std::fs::create_dir_all(&dir)?;
    let mut out = Vec::new();
    for (role, file) in k.files {
        let ext = file.rsplit('.').next().unwrap_or("ogg");
        let dest = dir.join(format!("{role}.{ext}"));
        if !dest.exists()
            || std::fs::metadata(&dest)
                .map(|m| m.len() < 64)
                .unwrap_or(true)
        {
            let url = format!("{SMPLD}{}{file}", k.base);
            let resp = ureq::get(&url)
                .timeout(std::time::Duration::from_secs(20))
                .call()
                .with_context(|| format!("download {url}"))?;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut resp.into_reader(), &mut bytes)?;
            if bytes.len() < 64 {
                bail!("{url}: empty download");
            }
            std::fs::write(&dest, bytes)?;
        }
        out.push((role.to_string(), dest));
    }
    std::fs::write(
        dir.join("LICENSE.txt"),
        format!("{}\nSource: {}\n", k.license, k.source),
    )?;
    Ok(out)
}

/// Register the kit's files that match the palette in the current project;
/// returns role -> sample name.
pub fn kit_map(
    e: &mut Engine,
    kit_name: &str,
    palette: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let k = kit(kit_name)?;
    let dir = kit_dir(e, k);
    let mut out = BTreeMap::new();
    for (role, preset) in palette {
        if !["kick", "snare", "hat", "open_hat", "perc"].contains(&role.as_str()) {
            continue;
        }
        let Some((_, file)) = k.files.iter().find(|(p, _)| p == preset) else {
            continue;
        };
        let ext = file.rsplit('.').next().unwrap_or("ogg");
        let path = dir.join(format!("{preset}.{ext}"));
        if !path.exists() {
            continue;
        }
        let name = format!("{}_{}", k.name, preset);
        if !e.project.samples.iter().any(|s| s.name == name) {
            crate::tools::register_sample(
                e,
                SampleInfo {
                    name: name.clone(),
                    path: path.to_string_lossy().into(),
                    source: k.source.into(),
                    license: k.license.into(),
                    author: String::new(),
                    duration: 0.0,
                },
            )?;
        }
        out.insert(role.clone(), name);
    }
    Ok(out)
}

// ---------------------------------------------------------------- index

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct IndexEntry {
    pub path: String,
    pub name: String,
    pub role: String,
    pub duration: f32,
    /// YIN fundamental (Hz) over the sustain, if pitched.
    pub pitch_hz: Option<f32>,
    pub note: Option<String>,
    pub bpm: Option<f32>,
    pub onsets: usize,
    pub centroid_hz: f32,
    pub rms_db: f32,
    #[serde(default)]
    pub license: String,
}

pub fn index_path(e: &Engine) -> PathBuf {
    e.samples_dir().join("index.json")
}

pub fn load_index(e: &Engine) -> Vec<IndexEntry> {
    std::fs::read_to_string(index_path(e))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn role_from_name(n: &str) -> Option<&'static str> {
    let n = n.to_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| n.contains(w));
    Some(if has(&["808", "sub"]) {
        "bass"
    } else if has(&["kick", "bd", "bassdrum", "bass drum"]) {
        "kick"
    } else if has(&["snare", "sd", "rim", "stick"]) {
        "snare"
    } else if has(&["clap", "cp", "snap"]) {
        "clap"
    } else if has(&["open", "oh", "crash", "ride", "cymbal", "cy"]) {
        "cymbal"
    } else if has(&["hat", "hh", "ch", "shaker", "maraca", "tamb", "cabasa"]) {
        "hat"
    } else if has(&[
        "tabla", "conga", "bongo", "tom", "perc", "darbuka", "cajon", "cowbell", "clave",
    ]) {
        "perc"
    } else if has(&["vox", "vocal", "voice", "choir", "acapella", "chant"]) {
        "vocal"
    } else if has(&["break", "loop", "beat"]) {
        "loop"
    } else if has(&["bass"]) {
        "bass"
    } else if has(&["pad", "drone", "atmos", "texture"]) {
        "pad"
    } else if has(&["fx", "riser", "impact", "sweep", "noise", "vinyl"]) {
        "fx"
    } else {
        return None;
    })
}

fn centroid(x: &[f32]) -> f32 {
    let n = 4096.min(x.len().next_power_of_two() / 2).max(256);
    let start = x.iter().position(|v| v.abs() > 0.01).unwrap_or(0);
    let frame: Vec<f32> = x
        .iter()
        .skip(start)
        .take(n)
        .copied()
        .chain(std::iter::repeat(0.0))
        .take(n)
        .collect();
    let ps = crate::analysis::power_spectrum(&frame);
    let (mut num, mut den) = (0.0f32, 0.0f32);
    for (i, p) in ps.iter().enumerate() {
        let f = i as f32 * SR / (2.0 * ps.len() as f32);
        num += f * p;
        den += p;
    }
    if den > 0.0 {
        num / den
    } else {
        0.0
    }
}

fn pitch_of(x: &[f32]) -> Option<f32> {
    // median YIN over frames after the attack
    let start =
        (x.iter().position(|v| v.abs() > 0.02).unwrap_or(0) + (0.03 * SR) as usize).min(x.len());
    let mut fs = Vec::new();
    let hop = 2048;
    let mut i = start;
    while i + 2048 <= x.len() && fs.len() < 24 {
        if let Some(f) = crate::sc_dsp::yin(&x[i..i + 2048], SR, 30.0, 1500.0, 0.15) {
            fs.push(f);
        }
        i += hop;
    }
    if fs.len() < 2 {
        return None;
    }
    fs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(fs[fs.len() / 2])
}

fn hz_to_note(f: f32) -> (i32, String) {
    let m = (69.0 + 12.0 * (f / 440.0).log2()).round() as i32;
    (m, crate::theory::note_name(m.clamp(0, 127) as u8))
}

pub fn analyze_file(path: &Path) -> Result<IndexEntry> {
    let x = samples::decode_file(path)?;
    let x: Vec<f32> = x.into_iter().take((SR * 30.0) as usize).collect();
    let dur = x.len() as f32 / SR;
    let rms = (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
    let on = crate::audio_edit::onsets(&x, 0.5, 70.0).len();
    let c = centroid(&x);
    let pitch = pitch_of(&x);
    let bpm = if dur > 1.8 && on >= 4 {
        let (b, conf) = crate::audio_edit::estimate_bpm(&x);
        (conf > 0.3 && b > 50.0).then_some((b * 10.0).round() / 10.0)
    } else {
        None
    };
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("sample")
        .to_string();
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let role = role_from_name(&name)
        .or_else(|| role_from_name(parent))
        .unwrap_or(if dur < 0.6 {
            if c < 900.0 {
                "kick"
            } else if c > 5000.0 {
                "hat"
            } else {
                "perc"
            }
        } else if bpm.is_some() {
            "loop"
        } else if pitch.is_some() {
            "melodic"
        } else {
            "fx"
        })
        .to_string();
    Ok(IndexEntry {
        path: path.to_string_lossy().into(),
        name,
        role,
        duration: (dur * 100.0).round() / 100.0,
        pitch_hz: pitch.map(|f| (f * 10.0).round() / 10.0),
        note: pitch.map(|f| hz_to_note(f).1),
        bpm,
        onsets: on,
        centroid_hz: c.round(),
        rms_db: (20.0 * rms.max(1e-9).log10() * 10.0).round() / 10.0,
        license: String::new(),
    })
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                walk(&p, out, depth + 1);
            } else if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                if ["wav", "flac", "mp3", "ogg", "aif", "aiff"]
                    .contains(&ext.to_lowercase().as_str())
                {
                    out.push(p);
                }
            }
        }
    }
}

/// Scan folders and merge the results into the on-disk index.
pub fn index_dirs(e: &Engine, dirs: &[PathBuf], max_files: usize) -> Result<Value> {
    let mut idx = load_index(e);
    let mut files = Vec::new();
    for d in dirs {
        walk(d, &mut files, 0);
    }
    files.sort();
    files.truncate(max_files);
    let (mut added, mut failed) = (0, Vec::new());
    for f in &files {
        let ps = f.to_string_lossy().to_string();
        if idx.iter().any(|x| x.path == ps) {
            continue;
        }
        match analyze_file(f) {
            Ok(mut ent) => {
                // credit: a LICENSE.txt next to the file
                if let Some(l) = f
                    .parent()
                    .map(|p| p.join("LICENSE.txt"))
                    .and_then(|p| std::fs::read_to_string(p).ok())
                {
                    ent.license = l.lines().next().unwrap_or("").to_string();
                }
                idx.push(ent);
                added += 1;
            }
            Err(err) => failed.push(format!("{}: {err}", f.display())),
        }
    }
    std::fs::create_dir_all(e.samples_dir())?;
    std::fs::write(index_path(e), serde_json::to_string_pretty(&idx)?)?;
    let mut by_role: BTreeMap<String, usize> = BTreeMap::new();
    for x in &idx {
        *by_role.entry(x.role.clone()).or_insert(0) += 1;
    }
    Ok(
        json!({"indexed": added, "total": idx.len(), "by_role": by_role, "failed": failed.iter().take(5).collect::<Vec<_>>(), "index": index_path(e).to_string_lossy()}),
    )
}

pub struct Query {
    pub role: Option<String>,
    pub text: Option<String>,
    pub key: Option<String>,
    pub bpm: Option<f32>,
    pub max_duration: Option<f32>,
    pub limit: usize,
}

pub fn find(e: &Engine, q: &Query) -> Vec<(f32, IndexEntry)> {
    let key_pcs: Option<Vec<u8>> = q.key.as_ref().and_then(|k| {
        let mut it = k.split_whitespace();
        let r = crate::theory::pitch_class(it.next()?).ok()?;
        let iv = crate::theory::scale_intervals(it.next().unwrap_or("minor")).ok()?;
        Some(iv.iter().map(|i| (r + i) % 12).collect())
    });
    let mut out: Vec<(f32, IndexEntry)> = load_index(e)
        .into_iter()
        .filter(|x| {
            q.role
                .as_ref()
                .map(|r| &x.role == r || (r == "snare" && x.role == "clap"))
                .unwrap_or(true)
        })
        .filter(|x| q.max_duration.map(|d| x.duration <= d).unwrap_or(true))
        .map(|x| {
            let mut s = 1.0f32;
            if let Some(t) = &q.text {
                let hay = format!("{} {}", x.name, x.path).to_lowercase();
                let words: Vec<String> = t
                    .to_lowercase()
                    .split_whitespace()
                    .map(String::from)
                    .collect();
                let hits = words.iter().filter(|w| hay.contains(w.as_str())).count();
                s += hits as f32 * 2.0
                    - if hits == 0 && !words.is_empty() {
                        1.0
                    } else {
                        0.0
                    };
            }
            if let (Some(pcs), Some(f)) = (&key_pcs, x.pitch_hz) {
                let (m, _) = hz_to_note(f);
                s += if pcs.contains(&((m.rem_euclid(12)) as u8)) {
                    1.0
                } else {
                    -0.5
                };
            }
            if let (Some(b), Some(xb)) = (q.bpm, x.bpm) {
                let r = (xb / b).max(b / xb);
                let near = [(r - 1.0).abs(), (r - 2.0).abs()]
                    .iter()
                    .cloned()
                    .fold(9.0, f32::min);
                s += (1.0 - near * 10.0).max(-1.0);
            }
            (s, x)
        })
        .collect();
    out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(q.limit.max(1));
    out
}

// ---------------------------------------------------------------- flips

/// Semitone shift that moves `hz` onto the nearest note of the key whose
/// pitch class is a stable degree (root/third/fifth), within +-6.
pub fn fit_shift(hz: f32, key_pc: u8, scale: &[u8]) -> i32 {
    let (m, _) = hz_to_note(hz);
    let stable: Vec<u8> = [0usize, 2, 4]
        .iter()
        .filter_map(|d| scale.get(*d))
        .map(|i| (key_pc + i) % 12)
        .collect();
    (-6..=6)
        .filter(|s| stable.contains(&((m + s).rem_euclid(12) as u8)))
        .min_by_key(|s| s.abs())
        .unwrap_or(0)
}

/// Re-pitch by resampling (speed changes with pitch, the classic sampler
/// flip). shift in semitones.
pub fn repitch(x: &[f32], semis: f32) -> Vec<f32> {
    if semis.abs() < 1e-3 {
        return x.to_vec();
    }
    let ratio = 2f32.powf(semis / 12.0);
    samples::resample(x, SR * ratio, SR)
}

fn write_sample(
    e: &mut Engine,
    data: &[f32],
    name: &str,
    source: &str,
    license: &str,
) -> Result<String> {
    let dir = e.samples_dir().join("flips");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.wav", samples::sample_name(name)));
    crate::render::write_wav(&path, data, data)?;
    let v = crate::tools::register_sample(
        e,
        SampleInfo {
            name: samples::sample_name(name),
            path: path.to_string_lossy().into(),
            source: source.into(),
            license: license.into(),
            author: String::new(),
            duration: 0.0,
        },
    )?;
    Ok(v["sample"].as_str().unwrap_or(name).to_string())
}

pub struct FlipOpts {
    pub sample: String,
    pub track: String,
    pub pattern: Option<String>,
    pub fit_key: bool,
    pub max_slices: usize,
    pub seed: u64,
    pub density: f32,
}

/// Chop a sample on its transients, re-pitch it into the project key,
/// and program a new 2-bar phrase from the chops (downbeat anchored,
/// call/response, a variation on the repeat).
pub fn flip(e: &mut Engine, o: &FlipOpts) -> Result<Value> {
    let info = e
        .project
        .samples
        .iter()
        .find(|s| s.name == o.sample)
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "no sample '{}' (import_sample / download_sample first)",
                o.sample
            )
        })?;
    let data = samples::decode_file(Path::new(&info.path))?;
    let key_pc = crate::theory::pitch_class(&e.project.key_root)?;
    let scale = crate::theory::scale_intervals(&e.project.scale)?.to_vec();
    let mut shift = 0;
    let hz = pitch_of(&data);
    if o.fit_key {
        if let Some(f) = hz {
            shift = fit_shift(f, key_pc, &scale);
        }
    }
    let flipped = repitch(&data, shift as f32);
    let name = write_sample(
        e,
        &flipped,
        &format!("{}_flip", info.name),
        &info.source,
        &info.license,
    )?;
    // slice kit on transients
    let pat = o.pattern.clone().unwrap_or_else(|| {
        e.project
            .patterns
            .first()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "main".into())
    });
    e.call_from("slice_sample", &json!({"sample": name, "track": o.track, "method": "transients", "max_slices": o.max_slices, "write_pattern": false, "root": 36}), "producer")?;
    let ti = e.project.track_index(&o.track)?;
    let nslices = match &e.project.tracks[ti].instrument {
        crate::instruments::Instrument::Sampler(s) => s.slices.len(),
        _ => 0,
    }
    .max(1);
    let pi = e.project.pattern_index(&pat)?;
    let steps = e.project.patterns[pi].steps();
    let mut rng = Rng::new(o.seed);
    // a 2-bar phrase: 8th-note grid with 16th pickups, slice 0 on the 1
    let mut phrase: Vec<Note> = Vec::new();
    let mut t = 0.0;
    while t < 32.0 {
        let on_beat = (t as u32) % 4 == 0;
        if t == 0.0 || t == 16.0 || rng.chance(o.density * if on_beat { 1.0 } else { 0.6 }) {
            let slice = if t == 0.0 {
                0
            } else if t == 16.0 {
                rng.below(nslices.min(3))
            } else {
                rng.below(nslices)
            };
            let len = if rng.chance(0.3) { 1.0 } else { 2.0 };
            phrase.push(Note::new(
                t,
                len,
                (36 + slice).min(127) as u8,
                if on_beat { 0.95 } else { 0.75 },
            ));
        }
        t += if rng.chance(0.2) { 1.0 } else { 2.0 };
    }
    // repeat the phrase across the pattern; every other repeat varies the tail
    let mut notes = Vec::new();
    let mut base = 0.0;
    let mut rep = 0;
    while base < steps as f32 {
        for n in &phrase {
            let mut m = n.clone();
            m.start += base;
            if rep % 2 == 1 && n.start >= 24.0 && rng.chance(0.5) {
                m.pitch = (36 + rng.below(nslices)).min(127) as u8;
            }
            if m.start < steps as f32 {
                notes.push(m);
            }
        }
        base += 32.0;
        rep += 1;
    }
    let count = notes.len();
    *e.project.patterns[pi].notes_mut(&o.track) = notes;
    Ok(json!({
        "track": o.track, "sample": name, "slices": nslices, "pattern": pat, "notes": count,
        "detected_pitch_hz": hz, "repitch_semitones": shift,
        "credit": {"source": info.source, "license": info.license},
    }))
}

pub struct ChopOpts {
    pub sample: String,
    pub track: String,
    pub pattern: Option<String>,
    pub chops: usize,
    pub seed: u64,
}

/// Vocal chops: pick the cleanest short syllables, render each at five
/// scale degrees into one chop sheet, map them as a slice kit, and write a
/// call-and-response chop melody in the key.
pub fn vocal_chop(e: &mut Engine, o: &ChopOpts) -> Result<Value> {
    let info = e
        .project
        .samples
        .iter()
        .find(|s| s.name == o.sample)
        .cloned()
        .ok_or_else(|| anyhow!("no sample '{}'", o.sample))?;
    let data = samples::decode_file(Path::new(&info.path))?;
    let mut on = crate::sc_dsp::detect_transients(&data, 0.5);
    if on.first().copied().unwrap_or(1) > 0 {
        on.insert(0, 0);
    }
    // syllables: segments 80-400 ms with the most energy
    let mut segs: Vec<(usize, usize, f32)> = Vec::new();
    for (i, &s) in on.iter().enumerate() {
        let end = on
            .get(i + 1)
            .copied()
            .unwrap_or(data.len())
            .min(s + (0.4 * SR) as usize);
        if end <= s + (0.08 * SR) as usize {
            continue;
        }
        let rms = (data[s..end].iter().map(|v| v * v).sum::<f32>() / (end - s) as f32).sqrt();
        segs.push((s, end, rms));
    }
    if segs.is_empty() {
        bail!("no usable syllables found in '{}'", o.sample);
    }
    segs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
    segs.truncate(o.chops.clamp(1, 6));
    let key_pc = crate::theory::pitch_class(&e.project.key_root)?;
    let scale = crate::theory::scale_intervals(&e.project.scale)?.to_vec();
    let degrees = [0usize, 2, 4, 5, 7];
    let mut sheet: Vec<f32> = Vec::new();
    let mut slices = Vec::new();
    for (s, end, _) in &segs {
        let seg = &data[*s..*end];
        let base_shift = pitch_of(seg)
            .map(|f| fit_shift(f, key_pc, &scale))
            .unwrap_or(0) as f32;
        for d in degrees {
            let semis = base_shift
                + scale.get(d % scale.len()).copied().unwrap_or(0) as f32
                + if d >= scale.len() { 12.0 } else { 0.0 }
                - scale[0] as f32;
            let mut x = repitch(seg, semis);
            // 5 ms fades: chops never click
            let f = (0.005 * SR) as usize;
            let n = x.len();
            for i in 0..f.min(n) {
                let g = i as f32 / f as f32;
                x[i] *= g;
                x[n - 1 - i] *= g;
            }
            slices.push(sheet.len() as f32 / SR);
            sheet.extend_from_slice(&x);
            sheet.extend(std::iter::repeat(0.0).take((0.02 * SR) as usize));
        }
    }
    let name = write_sample(
        e,
        &sheet,
        &format!("{}_chops", info.name),
        &info.source,
        &info.license,
    )?;
    let inst = crate::instruments::Instrument::Sampler(crate::instruments::SamplerParams {
        sample: name.clone(),
        root: 48,
        one_shot: true,
        slices: slices.clone(),
        ..Default::default()
    });
    match e.project.track_index(&o.track) {
        Ok(i) => e.project.tracks[i].instrument = inst,
        Err(_) => e
            .project
            .tracks
            .push(crate::project::Track::new(&o.track, inst)),
    }
    let pat = o.pattern.clone().unwrap_or_else(|| {
        e.project
            .patterns
            .first()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "main".into())
    });
    let pi = e.project.pattern_index(&pat)?;
    let steps = e.project.patterns[pi].steps();
    let mut rng = Rng::new(o.seed);
    let nd = degrees.len();
    let mut notes = Vec::new();
    let mut bar = 0;
    while bar * 16 < steps {
        // call (bars 0/2): 3-4 chops rising; response (1/3): 2 chops falling to the root
        let call = bar % 2 == 0;
        let chop = rng.below(segs.len());
        let picks: Vec<(f32, usize)> = if call {
            vec![(0.0, 0), (3.0, 1), (6.0, 2), (10.0, 3 + rng.below(2))]
        } else {
            vec![(2.0, 2), (8.0, 1), (12.0, 0)]
        };
        for (t, d) in picks {
            let start = (bar * 16) as f32 + t;
            if start < steps as f32 {
                notes.push(Note::new(
                    start,
                    2.0,
                    (48 + chop * nd + d.min(nd - 1)).min(127) as u8,
                    if call { 0.85 } else { 0.75 },
                ));
            }
        }
        bar += 1;
    }
    let count = notes.len();
    *e.project.patterns[pi].notes_mut(&o.track) = notes;
    Ok(
        json!({"track": o.track, "sample": name, "syllables": segs.len(), "pitched_chops": slices.len(), "notes": count, "pattern": pat, "map": "note 48 + syllable*5 + degree (root, 3rd, 5th, 6th, octave)", "credit": {"source": info.source, "license": info.license}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Engine {
        let d = std::env::temp_dir().join("beatbox_samplelib_tests");
        std::fs::create_dir_all(&d).unwrap();
        Engine::new(d)
    }

    fn tone(hz: f32, secs: f32, pulses: usize) -> Vec<f32> {
        let n = (secs * SR) as usize;
        let seg = n / pulses.max(1);
        (0..n)
            .map(|i| {
                let t = (i % seg) as f32 / SR;
                (2.0 * std::f32::consts::PI * hz * i as f32 / SR).sin() * 0.6 * (-t * 6.0).exp()
            })
            .collect()
    }

    #[test]
    fn fit_shift_lands_on_stable_degrees() {
        let minor = crate::theory::scale_intervals("minor").unwrap();
        // A4 (440) into C minor: nearest of C/Eb/G is G (-2) or C (+3)
        let s = fit_shift(440.0, 0, minor);
        assert_eq!(s, -2);
        assert_eq!(fit_shift(261.63, 0, minor), 0);
    }

    #[test]
    fn analyze_and_find() {
        let mut e = engine();
        let dir = e.samples_dir().join("testlib");
        std::fs::create_dir_all(&dir).unwrap();
        let x = tone(220.0, 1.2, 1);
        crate::render::write_wav(&dir.join("mellow_pluck.wav"), &x, &x).unwrap();
        let k = tone(50.0, 0.3, 1);
        crate::render::write_wav(&dir.join("big_kick.wav"), &k, &k).unwrap();
        let ent = analyze_file(&dir.join("mellow_pluck.wav")).unwrap();
        assert!(
            ent.pitch_hz
                .map(|f| (f - 220.0).abs() < 5.0)
                .unwrap_or(false),
            "{:?}",
            ent.pitch_hz
        );
        std::fs::remove_file(index_path(&e)).ok();
        index_dirs(&e, &[dir.clone()], 100).unwrap();
        let r = find(
            &e,
            &Query {
                role: Some("kick".into()),
                text: None,
                key: None,
                bpm: None,
                max_duration: None,
                limit: 5,
            },
        );
        assert_eq!(r.len(), 1);
        assert!(r[0].1.path.ends_with("big_kick.wav"));
        // flip + chop on a synthetic phrase
        e.project.patterns = vec![crate::project::Pattern::new("main", 4)];
        let phr = tone(330.0, 2.0, 8);
        crate::render::write_wav(&dir.join("phrase.wav"), &phr, &phr).unwrap();
        crate::tools::register_sample(
            &mut e,
            SampleInfo {
                name: "phrase".into(),
                path: dir.join("phrase.wav").to_string_lossy().into(),
                ..Default::default()
            },
        )
        .unwrap();
        let f = flip(
            &mut e,
            &FlipOpts {
                sample: "phrase".into(),
                track: "flip".into(),
                pattern: None,
                fit_key: true,
                max_slices: 8,
                seed: 1,
                density: 0.6,
            },
        )
        .unwrap();
        assert!(f["notes"].as_u64().unwrap() > 4);
        let c = vocal_chop(
            &mut e,
            &ChopOpts {
                sample: "phrase".into(),
                track: "chops".into(),
                pattern: None,
                chops: 3,
                seed: 2,
            },
        )
        .unwrap();
        assert!(c["pitched_chops"].as_u64().unwrap() >= 5);
        assert!(e.project.patterns[0].notes("chops").len() > 4);
    }
}
