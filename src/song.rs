//! Songs as data. One [`SongSpec`] JSON in, one finished song out:
//! composed, vocal placed, mixed to the targets in [`crate::mix_rules`],
//! delivered (wav/mp3/flac) and checked, in ONE engine call. The spec is kept
//! in the project (`Project.song`), so any part can be changed later by its
//! JSON path (`song_patch`) and only what that path touches is redone:
//!
//! * `/mix/...`, `/master/...`, `/export/...`, `/vocal/gain_db` -> re-mix and
//!   re-deliver (the beat and the vocal clips stay; unchanged tracks come
//!   from the render cache);
//! * `/vocal/...` -> the vocal track is rebuilt, then re-mixed;
//! * anything else (`/bpm`, `/structure/...`, `/instruments/...`, `/seed`...)
//!   -> the beat is recomposed with the SAME seed (unchanged choices stay
//!   the same), then the vocal and the mix.
//!
//! Every problem in a spec is reported at once, each with its JSON path and
//! the fix. A song call is one engine tool call: one undo step, and any
//! failure rolls the whole project back.

use crate::engine::Engine;
use crate::mix_rules::{self, Targets, VocalChain};
use crate::tools::{obj, Tool};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SECTION_KINDS: &[&str] = &["intro", "verse", "pre", "hook", "chorus", "drop", "bridge", "breakdown", "outro"];
const VOCAL_SAMPLE: &str = "song_vocal";

fn d_track() -> String {
    "vocal".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct SongSpec {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Free words about the song (style, references, feel); optional.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f32>,
    /// "D", "D minor", "F#m".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mood: Option<String>,
    /// sparse | balanced | dense (rap usually wants sparse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub structure: Vec<SectionSpec>,
    /// role -> instrument (roles: lead, harmony, counter, texture, bass,
    /// kick, snare, hat, open_hat, perc, tabla, bayan): a preset name, a
    /// recorded instrument "catalog:<id>" (search_instruments), or one-shot
    /// samples {"samples": [files], "gain_db": 0} (e.g. real dhol hits).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub instruments: BTreeMap<String, InstrumentSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vocal: Option<VocalSpec>,
    #[serde(default)]
    pub mix: MixSpec,
    #[serde(default)]
    pub master: MasterSpec,
    #[serde(default)]
    pub export: ExportSpec,
}

/// What plays a role.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum InstrumentSpec {
    /// A preset name, or "catalog:<id>" for a recorded (SFZ) instrument.
    Name(String),
    /// One-shot recordings played round-robin at the role's own pitch.
    Samples {
        samples: Vec<String>,
        #[serde(default)]
        gain_db: f32,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SectionSpec {
    pub kind: String,
    pub bars: u32,
    /// Default: kind + its count ("verse1", "hook2").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VocalSpec {
    /// A wav/mp3/flac on disk: the raw take.
    pub file: String,
    #[serde(default = "d_track")]
    pub track: String,
    /// Where each piece of the take goes. Empty = cut at the pauses and
    /// placed in take order from `start_section` (default: the first verse).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phrases: Vec<PhraseSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_section: Option<String>,
    /// The words (kept with the song; placement uses `phrases`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<String>,
    /// Extra fader dB on top of the automatic balance.
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default)]
    pub chain: VocalChain,
}

/// One piece of the take: from `src_s` for `len_s` seconds (or to `end_s`),
/// placed at `section` + `bar` (1-based) + `beat` (1-based, fractions ok:
/// 4.5 = the "and" of 4), or at an absolute `start_beat` / `at_s`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct PhraseSpec {
    #[serde(alias = "src")]
    pub src_s: f32,
    #[serde(default, alias = "len", skip_serializing_if = "Option::is_none")]
    pub len_s: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_s: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bar: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beat: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_beat: Option<f32>,
    #[serde(default, alias = "start_s", skip_serializing_if = "Option::is_none")]
    pub at_s: Option<f32>,
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_in_ms: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_out_ms: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MixSpec {
    pub vocal_over_beat_db: f32,
    pub vocal_over_beat_max_db: f32,
    /// Least hooks/drops sit over verses on the delivered file (dB).
    pub drop_lift_db: f32,
    /// Render-measure-correct passes for the vocal balance.
    pub balance_passes: u32,
    /// Role -> fader offset (dB, -12..12) on top of the genre's balance,
    /// e.g. {"bass": 2, "hat": -1}. Applied when the beat is composed, so a
    /// change recomposes (same seed) and re-mixes. song_improve writes here.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub levels: BTreeMap<String, f32>,
}

impl Default for MixSpec {
    fn default() -> Self {
        let t = Targets::default();
        MixSpec { vocal_over_beat_db: t.vocal_over_beat_db, vocal_over_beat_max_db: t.vocal_over_beat_max_db, drop_lift_db: t.drop_lift_db[0], balance_passes: 3, levels: BTreeMap::new() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MasterSpec {
    pub lufs: f32,
    pub true_peak: f32,
    pub preserve_dynamics: f32,
}

impl Default for MasterSpec {
    fn default() -> Self {
        let t = Targets::default();
        MasterSpec { lufs: t.lufs, true_peak: t.true_peak_max_db, preserve_dynamics: mix_rules::SONG_PRESERVE_DYNAMICS }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ExportSpec {
    /// Output file; default renders/<title>.<format>.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// mp3 | wav | flac.
    pub format: String,
    pub bitrate_kbps: u32,
}

impl Default for ExportSpec {
    fn default() -> Self {
        ExportSpec { path: None, format: "mp3".into(), bitrate_kbps: 320 }
    }
}

impl SongSpec {
    pub fn targets(&self) -> Targets {
        let d = Targets::default();
        Targets {
            vocal_over_beat_db: self.mix.vocal_over_beat_db,
            vocal_over_beat_max_db: self.mix.vocal_over_beat_max_db,
            drop_lift_db: [self.mix.drop_lift_db, self.mix.drop_lift_db.max(d.drop_lift_db[1])],
            lufs: self.master.lufs,
            lufs_tolerance: d.lufs_tolerance,
            true_peak_max_db: self.master.true_peak,
            ..d
        }
    }

    /// Section names in order (given names, else kind + count).
    pub fn section_names(&self) -> Vec<String> {
        let mut count: BTreeMap<String, u32> = BTreeMap::new();
        self.structure
            .iter()
            .map(|s| {
                let k = canon_kind(&s.kind);
                let n = count.entry(k.clone()).or_insert(0);
                *n += 1;
                s.name.clone().unwrap_or_else(|| format!("{k}{n}"))
            })
            .collect()
    }
}

/// The song as the project keeps it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SongState {
    /// The spec as last given/patched (what song_get returns).
    pub spec: Value,
    /// The seed the beat was composed with (patches reuse it).
    pub seed: u64,
    pub sections: Vec<mix_rules::Span>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    #[serde(default)]
    pub checks: Value,
}

fn canon_kind(k: &str) -> String {
    match k.trim().to_lowercase().as_str() {
        "chorus" | "drop" => "hook".into(),
        "prechorus" | "pre_chorus" | "pre-chorus" => "pre".into(),
        x => x.into(),
    }
}

// ----------------------------------------------------------------- validation

#[derive(Clone, Debug, Serialize)]
pub struct Problem {
    pub path: String,
    pub problem: String,
    pub fix: String,
}

fn p(path: impl Into<String>, problem: impl Into<String>, fix: impl Into<String>) -> Problem {
    Problem { path: path.into(), problem: problem.into(), fix: fix.into() }
}

fn lev(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + (ca != *cb) as usize).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The closest known word, if any is close.
fn closest<'a>(w: &str, known: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let w = w.to_lowercase();
    known
        .into_iter()
        .map(|k| (lev(&w, k), k))
        .filter(|(d, k)| *d <= (k.len() / 3).max(2) || k.contains(w.as_str()) || w.contains(*k))
        .min_by_key(|(d, _)| *d)
        .map(|(_, k)| k)
}

fn did_you_mean(w: &str, known: &[&str], alias: &[(&str, &str)]) -> String {
    if let Some((_, to)) = alias.iter().find(|(a, _)| a.eq_ignore_ascii_case(w)) {
        return format!("use '{to}'");
    }
    match closest(w, known.iter().copied()) {
        Some(k) => format!("did you mean '{k}'? (known: {})", known.join(", ")),
        None => format!("remove it (known: {})", known.join(", ")),
    }
}

const TOP_KEYS: &[&str] = &["title", "prompt", "genre", "bpm", "key", "scale", "mood", "density", "seed", "structure", "instruments", "vocal", "mix", "master", "export"];
const TOP_ALIAS: &[(&str, &str)] = &[("tempo", "bpm"), ("sections", "structure"), ("form", "structure"), ("arrangement", "structure"), ("vocals", "vocal"), ("voice", "vocal"), ("instrumentation", "instruments"), ("palette", "instruments"), ("style", "genre"), ("lufs", "master.lufs"), ("name", "title"), ("brief", "prompt")];
const VOCAL_KEYS: &[&str] = &["file", "track", "phrases", "start_section", "lyrics", "gain_db", "chain"];
const PHRASE_KEYS: &[&str] = &["src_s", "src", "len_s", "len", "end_s", "section", "bar", "beat", "start_beat", "at_s", "start_s", "gain_db", "name", "fade_in_ms", "fade_out_ms"];
const SECTION_KEYS: &[&str] = &["kind", "bars", "name"];
const MIX_KEYS: &[&str] = &["vocal_over_beat_db", "vocal_over_beat_max_db", "drop_lift_db", "balance_passes", "levels"];
const MASTER_KEYS: &[&str] = &["lufs", "true_peak", "preserve_dynamics"];
const EXPORT_KEYS: &[&str] = &["path", "format", "bitrate_kbps"];
const CHAIN_KEYS: &[&str] = &["hpf_hz", "deess_freq_hz", "deess_range_db", "comp_ratio", "comp_threshold_db", "comp_makeup_db", "reverb_send_db", "delay_send_db"];

fn unknown_keys(v: &Value, path: &str, known: &[&str], alias: &[(&str, &str)], out: &mut Vec<Problem>) {
    if let Some(m) = v.as_object() {
        for k in m.keys() {
            if !known.contains(&k.as_str()) {
                out.push(p(format!("{path}/{k}"), format!("'{k}' is not a field here"), did_you_mean(k, known, alias)));
            }
        }
    }
}

/// Split "D minor" / "Dm" / "F#" into (root, scale).
pub fn split_key(k: &str) -> (String, Option<String>) {
    let k = k.trim();
    if let Some((r, s)) = k.split_once(char::is_whitespace) {
        return (r.trim().to_string(), Some(s.trim().to_lowercase().replace(' ', "_")));
    }
    if k.len() >= 2 && k.ends_with('m') && !k.ends_with("dim") {
        return (k[..k.len() - 1].to_string(), Some("minor".into()));
    }
    (k.to_string(), None)
}

/// Every problem in a spec, with paths and fixes; the parsed spec when the
/// shape is right (semantic problems are still listed).
pub fn validate(v: &Value) -> (Option<SongSpec>, Vec<Problem>) {
    let mut out = Vec::new();
    if !v.is_object() {
        out.push(p("", "the song spec must be a JSON object", "send {\"genre\": \"desi_hiphop\", \"bpm\": 100, \"structure\": [...], ...}; song_schema shows every field"));
        return (None, out);
    }
    unknown_keys(v, "", TOP_KEYS, TOP_ALIAS, &mut out);
    for (i, s) in v["structure"].as_array().into_iter().flatten().enumerate() {
        unknown_keys(s, &format!("/structure/{i}"), SECTION_KEYS, &[("type", "kind"), ("length", "bars"), ("label", "name")], &mut out);
    }
    unknown_keys(&v["vocal"], "/vocal", VOCAL_KEYS, &[("path", "file"), ("take", "file"), ("phrase_map", "phrases"), ("gain", "gain_db")], &mut out);
    for (i, s) in v["vocal"]["phrases"].as_array().into_iter().flatten().enumerate() {
        if let Some(m) = s.as_object() {
            // phrase maps from other tools carry their own notes (err, words...): kept, ignored
            for k in m.keys() {
                if !PHRASE_KEYS.contains(&k.as_str()) && closest(k, PHRASE_KEYS.iter().copied()).map(|c| lev(k, c) <= 2 && k.len() > 3).unwrap_or(false) {
                    out.push(p(format!("/vocal/phrases/{i}/{k}"), format!("'{k}' looks like a typo"), did_you_mean(k, PHRASE_KEYS, &[])));
                }
            }
        }
    }
    unknown_keys(&v["vocal"]["chain"], "/vocal/chain", CHAIN_KEYS, &[], &mut out);
    unknown_keys(&v["mix"], "/mix", MIX_KEYS, &[("vocal_db", "vocal_over_beat_db")], &mut out);
    unknown_keys(&v["master"], "/master", MASTER_KEYS, &[("tp", "true_peak"), ("ceiling", "true_peak"), ("loudness", "lufs")], &mut out);
    unknown_keys(&v["export"], "/export", EXPORT_KEYS, &[("file", "path"), ("out", "path"), ("bitrate", "bitrate_kbps")], &mut out);
    if !out.is_empty() {
        return (None, out);
    }
    let spec: SongSpec = match serde_json::from_value(v.clone()) {
        Ok(s) => s,
        Err(e) => {
            out.push(p("", format!("wrong type: {e}"), "numbers as numbers (\"bpm\": 100), lists as lists; song_schema shows each field's type"));
            return (None, out);
        }
    };
    // genre
    if let Some(g) = &spec.genre {
        if crate::producer::playbook(g).is_err() {
            let names: Vec<&str> = crate::producer::playbooks().iter().map(|b| b.name.as_str()).collect();
            out.push(p("/genre", format!("'{g}' is not a genre this engine knows"), did_you_mean(g, &names, &[("desi", "desi_hiphop"), ("punjabi", "punjabi_party"), ("bhangra", "punjabi_party"), ("honey", "punjabi_party"), ("party", "punjabi_party"), ("hiphop", "boom_bap"), ("hip_hop", "boom_bap")])));
        }
    }
    if spec.genre.is_none() && spec.prompt.trim().is_empty() {
        out.push(p("/genre", "no genre and no prompt", "give \"genre\" (e.g. punjabi_party, desi_hiphop, trap, boom_bap, lofi, drill, rnb, afrobeats) or a \"prompt\""));
    }
    if let Some(b) = spec.bpm {
        if !(50.0..=200.0).contains(&b) {
            out.push(p("/bpm", format!("{b} is outside 50..200"), "use the song's tempo, e.g. 100 for desi hip-hop or 140 for trap"));
        }
    }
    let mut scale = spec.scale.clone();
    if let Some(k) = &spec.key {
        let (root, sc) = split_key(k);
        if crate::theory::pitch_class(&root).is_err() {
            out.push(p("/key", format!("'{k}' has no readable root note"), "use a note name: \"D\", \"F#\", \"Bb\", or \"D minor\""));
        }
        if scale.is_none() {
            scale = sc;
        }
    }
    if let Some(s) = &scale {
        if crate::theory::scale_intervals(s).is_err() {
            let names: Vec<&str> = crate::theory::SCALES.iter().map(|x| x.0).collect();
            out.push(p(if spec.scale.is_some() { "/scale" } else { "/key" }, format!("'{s}' is not a scale"), did_you_mean(s, &names, &[("min", "minor"), ("maj", "major")])));
        }
    }
    if let Some(d) = &spec.density {
        if !["sparse", "balanced", "dense"].contains(&d.as_str()) {
            out.push(p("/density", format!("'{d}' is not a density"), "use sparse, balanced or dense"));
        }
    }
    // structure
    let names = spec.section_names();
    for (i, s) in spec.structure.iter().enumerate() {
        if !SECTION_KINDS.contains(&s.kind.trim().to_lowercase().as_str()) {
            out.push(p(format!("/structure/{i}/kind"), format!("'{}' is not a section kind", s.kind), did_you_mean(&s.kind, SECTION_KINDS, &[("refrain", "hook"), ("break", "breakdown"), ("end", "outro"), ("start", "intro")])));
        }
        if s.bars == 0 || s.bars > 64 {
            out.push(p(format!("/structure/{i}/bars"), format!("{} bars", s.bars), "use 1..64 (4, 8 or 16 are usual)"));
        }
        if names.iter().filter(|n| **n == names[i]).count() > 1 {
            out.push(p(format!("/structure/{i}/name"), format!("the name '{}' is used twice", names[i]), "give each section its own name, or leave names out"));
        }
    }
    if !spec.structure.is_empty() && spec.structure.len() < 2 {
        out.push(p("/structure", "a song needs at least two sections", "e.g. [{\"kind\":\"intro\",\"bars\":4},{\"kind\":\"verse\",\"bars\":16},{\"kind\":\"hook\",\"bars\":8}]"));
    }
    // instruments
    for (role, ins) in &spec.instruments {
        if !crate::creative::ROLES.contains(&role.as_str()) {
            out.push(p(format!("/instruments/{role}"), format!("'{role}' is not a role"), did_you_mean(role, crate::creative::ROLES, &[("melody", "lead"), ("chords", "harmony"), ("keys", "harmony"), ("pad", "texture"), ("808", "bass"), ("hats", "hat"), ("drums", "kick"), ("dhol", "bayan"), ("dagga", "bayan"), ("tilli", "tabla")])));
            continue;
        }
        match ins {
            InstrumentSpec::Name(n) => {
                if let Some(id) = n.strip_prefix("catalog:") {
                    if crate::sound_catalog::entry(id).is_none() {
                        out.push(p(format!("/instruments/{role}"), format!("no recorded instrument '{id}' in the catalog"), "search_instruments (instrument category) lists the ids, e.g. catalog:euterpea_harmonium"));
                    }
                } else if crate::instruments::preset(n).is_none() {
                    let names: Vec<&str> = crate::instruments::PRESETS.iter().map(|x| x.0).collect();
                    out.push(p(format!("/instruments/{role}"), format!("'{n}' is not an instrument preset"), did_you_mean(n, &names, &[])));
                }
            }
            InstrumentSpec::Samples { samples, gain_db } => {
                if samples.is_empty() {
                    out.push(p(format!("/instruments/{role}/samples"), "no files", "give one or more wav/mp3 one-shots (several = round robin)"));
                }
                for (i, f) in samples.iter().enumerate() {
                    if !Path::new(f).is_file() {
                        out.push(p(format!("/instruments/{role}/samples/{i}"), format!("'{f}' is not a file on this machine"), "give the full path of a wav/mp3 one-shot"));
                    }
                }
                if !(-24.0..=24.0).contains(gain_db) {
                    out.push(p(format!("/instruments/{role}/gain_db"), format!("{gain_db} dB"), "use -24..24"));
                }
            }
        }
    }
    // vocal
    if let Some(vs) = &spec.vocal {
        if !Path::new(&vs.file).is_file() {
            out.push(p("/vocal/file", format!("'{}' is not a file on this machine", vs.file), "give the full path of a wav/mp3/flac take"));
        }
        if vs.track.trim().is_empty() {
            out.push(p("/vocal/track", "empty track name", "leave it out (default 'vocal')"));
        }
        for (path, prob, fix) in vs.chain.problems("/vocal/chain") {
            out.push(p(path, prob, fix));
        }
        if !(-24.0..=24.0).contains(&vs.gain_db) {
            out.push(p("/vocal/gain_db", format!("{} dB", vs.gain_db), "use -24..24; the balance loop already sets the level"));
        }
        if let Some(ss) = &vs.start_section {
            if !spec.structure.is_empty() && !names.contains(ss) {
                out.push(p("/vocal/start_section", format!("no section '{ss}'"), format!("use one of: {}", names.join(", "))));
            }
        }
        for (i, ph) in vs.phrases.iter().enumerate() {
            let base = format!("/vocal/phrases/{i}");
            if ph.src_s < 0.0 {
                out.push(p(format!("{base}/src_s"), "negative start in the take", "seconds from the start of the take, >= 0"));
            }
            let len = ph.len_s.or(ph.end_s.map(|e| e - ph.src_s));
            match len {
                None => out.push(p(base.clone(), "no length", "give len_s (seconds) or end_s (seconds in the take)")),
                Some(l) if l <= 0.05 => out.push(p(format!("{base}/len_s"), format!("{l:.2} s is too short"), "len_s > 0.05, or end_s after src_s")),
                _ => {}
            }
            let placed = ph.section.is_some() || ph.start_beat.is_some() || ph.at_s.is_some();
            if !placed {
                out.push(p(base.clone(), "no position in the song", "give section (+ bar, beat), or start_beat, or at_s"));
            }
            if let Some(sec) = &ph.section {
                if spec.structure.is_empty() {
                    out.push(p(format!("{base}/section"), "phrases name sections but the spec has no structure", "add \"structure\", or place phrases with start_beat / at_s"));
                } else if let Err(e) = resolve_section(&spec, &names, sec) {
                    out.push(p(format!("{base}/section"), e.to_string(), format!("use one of: {}", names.join(", "))));
                } else if let Ok(j) = resolve_section(&spec, &names, sec) {
                    let bars = spec.structure[j].bars as f32;
                    let bar = ph.bar.unwrap_or(1.0);
                    if bar < 0.0 || bar > bars {
                        out.push(p(format!("{base}/bar"), format!("bar {bar} but '{}' has {bars} bars", names[j]), format!("use 1..{bars} (0 = the bar before, for a pickup)")));
                    }
                    let beat = ph.beat.unwrap_or(1.0);
                    if !(0.0..5.0).contains(&beat) {
                        out.push(p(format!("{base}/beat"), format!("beat {beat}"), "use 1..4.99 in 4/4 (1 = the downbeat, 4.5 = the and of 4)"));
                    }
                }
            }
        }
    }
    for (role, db) in &spec.mix.levels {
        if !crate::creative::ROLES.contains(&role.as_str()) {
            out.push(p(format!("/mix/levels/{role}"), format!("'{role}' is not a role"), did_you_mean(role, crate::creative::ROLES, &[("melody", "lead"), ("chords", "harmony"), ("808", "bass"), ("hats", "hat"), ("dhol", "bayan")])));
        } else if !(-12.0..=12.0).contains(db) || !db.is_finite() {
            out.push(p(format!("/mix/levels/{role}"), format!("{db} dB"), "a fader offset in -12..12 dB"));
        }
    }
    if !(1..=6).contains(&spec.mix.balance_passes) {
        out.push(p("/mix/balance_passes", format!("{}", spec.mix.balance_passes), "use 1..6"));
    }
    if !(0.0..=20.0).contains(&spec.mix.vocal_over_beat_db) || spec.mix.vocal_over_beat_max_db <= spec.mix.vocal_over_beat_db + 1.0 {
        out.push(p("/mix/vocal_over_beat_db", format!("{}..{} dB", spec.mix.vocal_over_beat_db, spec.mix.vocal_over_beat_max_db), "a floor in 0..20 and a max at least 2 dB above it (default 6..12)"));
    }
    if !(-20.0..=-6.0).contains(&spec.master.lufs) {
        out.push(p("/master/lufs", format!("{} LUFS", spec.master.lufs), "use -20..-6 (-14 streaming-safe, -10 loud rap, -8 very loud)"));
    }
    if !(-6.0..=-0.1).contains(&spec.master.true_peak) {
        out.push(p("/master/true_peak", format!("{} dBTP", spec.master.true_peak), "use -6..-0.1 (-1 is the usual delivery ceiling)"));
    }
    if !(0.0..=1.0).contains(&spec.master.preserve_dynamics) {
        out.push(p("/master/preserve_dynamics", format!("{}", spec.master.preserve_dynamics), "use 0..1 (0.8 default)"));
    }
    if !["mp3", "wav", "flac"].contains(&spec.export.format.as_str()) {
        out.push(p("/export/format", format!("'{}'", spec.export.format), "use mp3, wav or flac"));
    }
    if let Some(path) = &spec.export.path {
        let parent = Path::new(path).parent().filter(|x| !x.as_os_str().is_empty());
        if let Some(d) = parent {
            if !d.is_dir() {
                out.push(p("/export/path", format!("the folder {} does not exist", d.display()), "use an existing folder, or leave path out"));
            }
        }
    }
    (Some(spec), out)
}

/// A section by name ("hook2"), or by kind when it occurs once ("hook").
fn resolve_section(spec: &SongSpec, names: &[String], key: &str) -> Result<usize> {
    if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(key)) {
        return Ok(i);
    }
    let k = canon_kind(key);
    let idx: Vec<usize> = (0..spec.structure.len()).filter(|&i| canon_kind(&spec.structure[i].kind) == k).collect();
    match idx.len() {
        1 => Ok(idx[0]),
        0 => bail!("no section '{key}'"),
        _ => bail!("'{key}' occurs {} times; name which one", idx.len()),
    }
}

fn problems_err(probs: &[Problem]) -> anyhow::Error {
    let lines: Vec<String> = probs.iter().map(|x| format!("{}: {} -> {}", if x.path.is_empty() { "/" } else { &x.path }, x.problem, x.fix)).collect();
    anyhow!("the song spec has {} problem(s), nothing was changed:\n- {}", probs.len(), lines.join("\n- "))
}

// ------------------------------------------------------------------- building

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub enum Scope {
    /// re-mix and re-deliver only
    Mix,
    /// rebuild the vocal track, then mix
    Vocal,
    /// recompose the beat (same seed), then vocal and mix
    Full,
}

/// What a JSON path in the spec touches.
pub fn scope_of(path: &str) -> Scope {
    let p = path.trim_start_matches('/');
    let head = p.split('/').next().unwrap_or("");
    match head {
        // role levels are set when the beat is composed
        "mix" if p == "mix/levels" || p.starts_with("mix/levels/") => Scope::Full,
        "mix" | "master" | "export" | "title" => Scope::Mix,
        "vocal" if p == "vocal/gain_db" => Scope::Mix,
        "vocal" => Scope::Vocal,
        _ => Scope::Full,
    }
}

fn seed_for(spec: &SongSpec) -> u64 {
    spec.seed.unwrap_or_else(crate::creative::fresh_seed)
}

fn compose_beat(e: &mut Engine, spec: &SongSpec, seed: u64, log: &mut Vec<String>) -> Result<()> {
    let names = spec.section_names();
    let mut intent = Map::new();
    if !spec.structure.is_empty() {
        intent.insert("form".into(), json!(spec.structure.iter().map(|s| json!({"kind": canon_kind(&s.kind), "bars": s.bars})).collect::<Vec<_>>()));
    }
    let presets: BTreeMap<&String, &String> = spec
        .instruments
        .iter()
        .filter_map(|(r, i)| match i {
            InstrumentSpec::Name(n) if !n.starts_with("catalog:") => Some((r, n)),
            _ => None,
        })
        .collect();
    if !presets.is_empty() {
        intent.insert("palette".into(), json!(presets));
    }
    if let Some(d) = &spec.density {
        intent.insert("density".into(), json!(d));
    }
    if spec.vocal.is_some() && spec.density.is_none() {
        intent.insert("density".into(), json!("sparse"));
        intent.insert("hero".into(), json!("groove"));
    }
    if let Some(m) = &spec.mood {
        intent.insert("mood".into(), json!(m));
    }
    intent.insert("target_lufs".into(), json!(spec.master.lufs));
    let (root, kscale) = spec.key.as_deref().map(split_key).unzip();
    let brief = if spec.prompt.trim().is_empty() {
        format!("{} song", spec.genre.clone().unwrap_or_default().replace('_', " "))
    } else {
        spec.prompt.clone()
    };
    let args = crate::producer::PlanArgs {
        brief,
        genre: spec.genre.clone(),
        bpm: spec.bpm,
        key: root,
        scale: spec.scale.clone().or(kscale.flatten()),
        mood: spec.mood.clone(),
        duration_s: None,
        seed,
        use_samples: true,
        flip_sample: None,
        seed_source: "explicit".into(),
        method: None,
        reference: None,
        authored: Default::default(),
        intent: Value::Object(intent),
    };
    let mut plan = crate::producer::plan_track(&args)?;
    // the structure is a contract: the creative wildcards may not move it
    if !spec.structure.is_empty() {
        if plan.sections.len() != spec.structure.len() {
            // a wildcard inserted a section (a breakdown, a switch): the
            // structure asked for wins; keep the planned sections that match
            // it in order, drop the rest
            let mut kept = Vec::new();
            let mut j = 0;
            for s in plan.sections.drain(..) {
                if j < spec.structure.len() && s.kind == canon_kind(&spec.structure[j].kind) {
                    kept.push(s);
                    j += 1;
                } else {
                    log.push(format!("left out a planned extra '{}' section: the structure is the one asked for", s.kind));
                }
            }
            if kept.len() != spec.structure.len() {
                bail!("the producer could not keep the asked structure ({} of {} sections matched); try another seed, and please report it", kept.len(), spec.structure.len());
            }
            plan.sections = kept;
        }
        for (i, s) in plan.sections.iter_mut().enumerate() {
            if s.bars != spec.structure[i].bars {
                log.push(format!("kept {} at {} bars (a wildcard wanted {})", names[i], spec.structure[i].bars, s.bars));
                s.bars = spec.structure[i].bars;
            }
            s.name = names[i].clone();
        }
    }
    if !spec.title.is_empty() {
        plan.title = spec.title.clone();
    }
    crate::producer::prepare_kit(e, &mut plan);
    crate::producer::compose(e, &plan)?;
    swap_instruments(e, spec, log)?;
    crate::producer::balance_beat(e, &plan)?;
    apply_role_levels(e, spec, log)?;
    // loudness and peaks belong to delivery: a limiter in the project would
    // flatten the drops before the mix rules see them
    e.project.master_effects.retain(|f| {
        let t = serde_json::to_value(f).ok().and_then(|v| v["type"].as_str().map(String::from)).unwrap_or_default();
        !["limiter", "gain", "maximizer", "soft_clipper", "clipper"].contains(&t.as_str())
    });
    log.push(format!(
        "beat: {} {} bpm {} {}, {} sections, seed {seed}",
        plan.genre, plan.bpm, plan.key, plan.scale, plan.sections.len()
    ));
    Ok(())
}

/// The spec's role level offsets (/mix/levels), after the genre balance.
fn apply_role_levels(e: &mut Engine, spec: &SongSpec, log: &mut Vec<String>) -> Result<()> {
    for (role, db) in &spec.mix.levels {
        let track = crate::producer::track_name(role);
        let ti = e.project.track_index(&track).map_err(|_| no_part(e, role))?;
        e.project.tracks[ti].volume_db += db;
        log.push(format!("level: {role} {db:+.1} dB (/mix/levels)"));
    }
    Ok(())
}

/// Recorded instruments for the roles that asked for them (after compose,
/// before the level balance, so the balance hears the real sound).
fn swap_instruments(e: &mut Engine, spec: &SongSpec, log: &mut Vec<String>) -> Result<()> {
    for (role, ins) in &spec.instruments {
        let track = crate::producer::track_name(role);
        let InstrumentSpec::Name(n) = ins else {
            if let InstrumentSpec::Samples { samples, gain_db } = ins {
                let ti = e.project.track_index(&track).map_err(|_| no_part(e, role))?;
                // the pitch the part plays: the hits sound at their own pitch there
                let mut count: BTreeMap<u8, usize> = BTreeMap::new();
                for pt in &e.project.patterns {
                    for nt in pt.clips.get(&track.to_lowercase()).into_iter().flatten() {
                        *count.entry(nt.pitch).or_default() += 1;
                    }
                }
                let root = count.iter().max_by_key(|(_, c)| **c).map(|(p, _)| *p).unwrap_or(60);
                let mut zones = Vec::new();
                for (i, f) in samples.iter().enumerate() {
                    let name = format!("song_{role}_{i}");
                    e.project.samples.retain(|s| s.name != name);
                    e.call_nested("import_sample", &json!({"path": f, "name": name, "source": format!("song instrument for {role}")}), "song")?;
                    zones.push(json!({"sample": name, "root": root, "lo_note": 0, "hi_note": 127, "vel_lo": 0.0, "vel_hi": 1.0, "tune_cents": 0.0, "gain_db": gain_db, "rr": i}));
                }
                let inst = json!({"type": "multisample", "zones": zones, "attack": 0.002, "release": 0.35, "one_shot": true, "round_robin": samples.len() > 1, "vel_crossfade": true, "vel_to_gain": 0.6, "gain": 0.9, "source": ""});
                e.project.tracks[ti].instrument = serde_json::from_value(inst).map_err(|err| anyhow!("instrument for {role}: {err}"))?;
                e.bank.sync(&e.project.samples);
                log.push(format!("{role}: {} recorded one-shot(s) at the part's pitch", samples.len()));
            }
            continue;
        };
        let Some(id) = n.strip_prefix("catalog:") else { continue };
        let ti = e.project.track_index(&track).map_err(|_| no_part(e, role))?;
        let tmp = format!("__song_{role}");
        e.call_nested("load_instrument", &json!({"track": tmp, "catalog_id": id}), "song")?;
        let tj = e.project.track_index(&tmp)?;
        let inst = e.project.tracks[tj].instrument.clone();
        e.project.tracks.remove(tj);
        let ti = if tj < ti { ti - 1 } else { ti };
        e.project.tracks[ti].instrument = inst;
        e.bank.sync(&e.project.samples);
        log.push(format!("{role}: recorded instrument {id}"));
    }
    Ok(())
}

fn no_part(e: &Engine, role: &str) -> anyhow::Error {
    let have: Vec<&str> = crate::creative::ROLES.iter().copied().filter(|r| e.project.track_index(&crate::producer::track_name(r)).is_ok()).collect();
    anyhow!("/instruments/{role}: this arrangement has no {role} part; roles that play here: {}", have.join(", "))
}

fn remove_vocal(e: &mut Engine, track: &str) {
    e.project.audio_clips.retain(|c| !c.track.eq_ignore_ascii_case(track));
    e.project.automation.retain(|l| l.target != track);
    if let Ok(i) = e.project.track_index(track) {
        e.project.tracks.remove(i);
    }
    for t in e.project.tracks.iter_mut() {
        t.effects.retain(|f| {
            let v = serde_json::to_value(f).unwrap_or(Value::Null);
            !(v["type"] == "sidechain" && v["source"].as_str().map(|s| s.eq_ignore_ascii_case(track)).unwrap_or(false))
        });
    }
}

fn place_vocal(e: &mut Engine, spec: &SongSpec, vs: &VocalSpec, log: &mut Vec<String>) -> Result<Value> {
    remove_vocal(e, &vs.track);
    let file = std::fs::canonicalize(&vs.file).map_err(|err| anyhow!("/vocal/file: {} ({err})", vs.file))?;
    let fresh = e.project.samples.iter().find(|s| s.name == VOCAL_SAMPLE).map(|s| Path::new(&s.path) != file).unwrap_or(true);
    if fresh {
        e.project.samples.retain(|s| s.name != VOCAL_SAMPLE);
        e.call_nested("import_sample", &json!({"path": file.to_string_lossy(), "name": VOCAL_SAMPLE, "source": "the song's vocal take"}), "song")?;
    }
    e.bank.sync(&e.project.samples);
    let take_s = e.bank.get(VOCAL_SAMPLE).map(|d| d.len() as f32 / crate::dsp::SR).unwrap_or(0.0);
    e.call_nested("add_track", &json!({"name": vs.track, "instrument": {"type": "sampler", "sample": VOCAL_SAMPLE, "one_shot": true}, "volume_db": vs.gain_db}), "song")?;
    let spans = mix_rules::section_spans(&e.project);
    let names = spec.section_names();
    let bs = 60.0 / e.project.bpm;
    let mut placed = 0;
    if vs.phrases.is_empty() {
        let start = match &vs.start_section {
            Some(s) => spans.iter().find(|x| &x.name == s).map(|x| x.start_beat).ok_or_else(|| anyhow!("/vocal/start_section: no section '{s}'"))?,
            None => spans.iter().find(|x| x.kind == "verse").or(spans.first()).map(|x| x.start_beat).unwrap_or(0.0),
        };
        let r = e.call_nested("place_vocal_phrases", &json!({"sample": VOCAL_SAMPLE, "track": vs.track, "start_beat": start, "grid": "beat", "keep_gaps": true}), "song")?;
        placed = r["phrases"].as_u64().unwrap_or(0) as usize;
        log.push(format!("vocal: {placed} phrases cut at the pauses, in take order from beat {start}"));
    } else {
        let mut problems = Vec::new();
        for (i, ph) in vs.phrases.iter().enumerate() {
            let len = ph.len_s.or(ph.end_s.map(|x| x - ph.src_s)).unwrap_or(0.0);
            if take_s > 0.0 && ph.src_s + len > take_s + 0.05 {
                problems.push(p(format!("/vocal/phrases/{i}"), format!("{:.2}..{:.2} s runs past the end of the take ({take_s:.2} s)", ph.src_s, ph.src_s + len), "shorten len_s or move src_s"));
                continue;
            }
            let start_beat = if let Some(sec) = &ph.section {
                let j = resolve_section(spec, &names, sec)?;
                let sp = spans.get(j).ok_or_else(|| anyhow!("section {sec} is not in the arrangement"))?;
                sp.start_beat + (ph.bar.unwrap_or(1.0) - 1.0) * 4.0 + (ph.beat.unwrap_or(1.0) - 1.0)
            } else if let Some(b) = ph.start_beat {
                b
            } else {
                ph.at_s.unwrap_or(0.0) / bs
            };
            if start_beat < -0.01 {
                problems.push(p(format!("/vocal/phrases/{i}"), format!("lands before the song starts (beat {start_beat:.2})"), "move it later"));
                continue;
            }
            e.call_nested("add_audio_clip", &json!({
                "track": vs.track, "sample": VOCAL_SAMPLE, "start_beat": start_beat, "offset_s": ph.src_s, "length_s": len,
                "gain_db": ph.gain_db, "fade_in_ms": ph.fade_in_ms.unwrap_or(8.0), "fade_out_ms": ph.fade_out_ms.unwrap_or(40.0)
            }), "song")?;
            placed += 1;
        }
        if !problems.is_empty() {
            return Err(problems_err(&problems));
        }
        log.push(format!("vocal: {placed} phrases placed whole (no time-stretch)"));
    }
    let chain = mix_rules::apply_vocal_chain(e, &vs.track, &vs.chain, "song")?;
    Ok(json!({"track": vs.track, "phrases": placed, "take_s": (take_s * 10.0).round() / 10.0, "chain": chain}))
}

fn out_path(e: &Engine, spec: &SongSpec) -> PathBuf {
    match &spec.export.path {
        Some(p) => e.resolve(p),
        None => {
            let t = if spec.title.is_empty() { "song".to_string() } else { crate::samples::sample_name(&spec.title) };
            e.renders_dir().join(format!("{t}.{}", spec.export.format))
        }
    }
}

/// Mix to the targets, deliver, check. The vocal (if any) is balanced
/// section by section; hooks are lifted over verses.
fn mix_deliver(e: &mut Engine, spec: &SongSpec, log: &mut Vec<String>) -> Result<(Value, PathBuf, Value)> {
    // every mix rule and delivery round after this moves faders, buses,
    // fader lanes or the master only: synthesise each track once and re-sum
    // (dropped when we return)
    let prev = e.track_cache.replace(Default::default());
    let t0 = std::time::Instant::now();
    let r = mix_deliver_inner(e, spec, log);
    e.track_cache = prev;
    log.push(format!("mix + delivery: {:.0} s", t0.elapsed().as_secs_f32()));
    r
}

fn lap(log: &mut Vec<String>, t: &mut std::time::Instant, what: &str) {
    log.push(format!("{what}: {:.1} s", t.elapsed().as_secs_f32()));
    *t = std::time::Instant::now();
}

/// Most deliveries one song build makes (deliver -> check -> fix): lift,
/// loudness and both tone bands each own a knob, and the top end can need a
/// round from each side.
const DELIVERY_ROUNDS: usize = 5;

fn mix_deliver_inner(e: &mut Engine, spec: &SongSpec, log: &mut Vec<String>) -> Result<(Value, PathBuf, Value)> {
    let t = spec.targets();
    let mut tl = std::time::Instant::now();
    let voice = spec.vocal.as_ref().map(|v| v.track.clone()).filter(|v| e.project.track_index(v).is_ok());
    let mut mix = Map::new();
    // the beat's bright parts are voiced softer before anything is measured
    mix.insert("source_tone".into(), mix_rules::soften_beat_source(e, voice.as_deref())?);
    mix.insert("peaks".into(), mix_rules::tame_peaks(e)?);
    if let Some(v) = &voice {
        let i = e.project.track_index(v)?;
        e.project.tracks[i].volume_db = spec.vocal.as_ref().map(|x| x.gain_db).unwrap_or(0.0);
        // level first (the top-end rule compares the beat to the vocal as
        // it will sit), then the top end, then the level again
        mix_rules::balance_vocal(e, v, &t, 2)?;
    }
    mix.insert("top_end".into(), mix_rules::tame_top(e, voice.as_deref(), &t, 0.0)?);
    if let Some(v) = &voice {
        let b = mix_rules::balance_vocal(e, v, &t, spec.mix.balance_passes as usize)?;
        log.push(format!("vocal balance: {}", b["fader_offsets_db"]));
        mix.insert("vocal_balance".into(), b);
    }
    mix.insert("glue".into(), mix_rules::set_glue(e)?);
    // the top end: the beat darkens on its own bus first (the vocal keeps
    // its top), the vocal is re-balanced over the darker beat, and the
    // master tone only takes what the beat could not (small, capped)
    lap(log, &mut tl, "peaks, vocal balance, top end, glue");
    let (mut top_p, mut top_a) = (0.0f32, 0.0f32);
    let m0 = mix_rules::TOP_DELIVERY_MARGIN_DB;
    mix.insert("top_tone".into(), mix_rules::shape_top_with(e, voice.as_deref(), &t, m0, m0)?);
    lap(log, &mut tl, "beat tone");
    let lift = mix_rules::ensure_drop_lift(e, &t)?;
    mix.insert("drop_lift".into(), lift);
    mix.insert("quiet_sections".into(), mix_rules::lift_quiet_sections(e, &t)?);
    let pre = mix_rules::measure_sections(e, voice.as_deref())?;
    let path = out_path(e, spec);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let o = crate::tools_delivery::DeliverOpts {
        format: spec.export.format.clone(),
        target_lufs: Some(spec.master.lufs),
        ceiling_dbtp: spec.master.true_peak - mix_rules::DELIVERY_CEILING_MARGIN_DB,
        soft_clip_db: mix_rules::DELIVERY_SOFT_CLIP_DB,
        preserve_dynamics: spec.master.preserve_dynamics,
        bitrate_kbps: spec.export.bitrate_kbps,
        ..Default::default()
    };
    let spans = mix_rules::section_spans(&e.project);
    // deliver -> check the delivered file -> fix each failed target with
    // the knob that owns it -> deliver again (at most DELIVERY_ROUNDS deliveries):
    // lift -> verses step down on the master automation (undone if the
    // limiter took it back); loudness -> less of the contrast is kept
    // (only while the lift has room); harshness -> the top-end rule again,
    // aimed lower by what delivery added
    let mut o = o;
    let mut d = Value::Null;
    let mut checks = Value::Null;
    let mut lift_log = Vec::new();
    let mut last_try: Option<(f32, f32)> = None;
    let mut tone_rounds = 0usize;
    // what the level rule took from each bright part (the air floor may
    // give some of it back)
    let mut air_owed: Vec<(String, f32)> = mix["source_tone"]["levels"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| Some((x["part"].as_str()?.to_string(), x["down_db"].as_f64()? as f32))).collect())
        .unwrap_or_default();
    for round in 0..DELIVERY_ROUNDS {
        let full = e.mix()?;
        d = crate::tools_delivery::deliver(&full.left, &full.right, &path, &o)?;
        checks = mix_rules::check(&path, &pre, &spans, e.project.bpm, &t)?;
        lap(log, &mut tl, &format!("delivery round {}", round + 1));
        let get = |name: &str| checks["checks"].as_array().and_then(|c| c.iter().find(|x| x["check"] == name)).cloned().unwrap_or(Value::Null);
        let lift = get("drop_lift")["measured"].as_f64().map(|x| x as f32);
        let lufs = get("loudness")["measured"].as_f64().map(|x| x as f32);
        let harsh = get("not_harsh");
        if round + 1 == DELIVERY_ROUNDS {
            break;
        }
        let mut changed = false;
        if let (Some(l), Some((prev_l, prev_need))) = (lift, last_try) {
            if l < prev_l + 0.3 {
                mix_rules::deepen_verses(e, -prev_need)?;
                lift_log.push(json!({"undone": true, "why": "the limiter took the extra contrast back"}));
                last_try = None;
                changed = true;
            }
        } else if let Some(l) = lift.filter(|l| *l < t.drop_lift_db[0]) {
            let need = (t.drop_lift_db[0] + 0.5 - l).clamp(0.3, 3.0);
            mix_rules::deepen_verses(e, need)?;
            last_try = Some((l, need));
            lift_log.push(json!({"delivered_lift_db": (l * 10.0).round() / 10.0, "verses_down_db": (need * 10.0).round() / 10.0}));
            changed = true;
        }
        if let Some(lu) = lufs {
            let room = lift.map(|l| l >= t.drop_lift_db[0] + 0.3).unwrap_or(true);
            if lu < t.lufs - t.lufs_tolerance && room && o.preserve_dynamics > 0.3 {
                o.preserve_dynamics = (o.preserve_dynamics - 0.15).max(0.3);
                lift_log.push(json!({"delivered_lufs": (lu * 10.0).round() / 10.0, "preserve_dynamics": (o.preserve_dynamics * 100.0).round() / 100.0}));
                changed = true;
            }
        }
        if harsh["pass"] == false && tone_rounds < 2 {
            // each band aims at its own window on the delivered file: too
            // bright -> the top rule again aimed lower; too dark -> eased
            let pr = harsh["measured"]["presence_rel_lufs_db"].as_f64().map(|x| x as f32);
            let ar = harsh["measured"]["air_rel_lufs_db"].as_f64().map(|x| x as f32);
            let [lo, hi] = t.presence_rel_lufs_db;
            let mut moved = Vec::new();
            if let Some(pr) = pr.filter(|p| *p > hi + 0.15 || *p < lo - 0.15) {
                let d = pr - t.presence_rel_aim_db();
                top_p = (top_p + d).clamp(-4.0, 8.0);
                moved.push(json!({"band": "2-5k", "delivered_rel_lufs_db": (pr * 10.0).round() / 10.0, "aim_db": t.presence_rel_aim_db()}));
            }
            let [a_lo, a_hi] = t.air_rel_lufs_db;
            if let Some(ar) = ar.filter(|a| *a > a_hi + 0.15) {
                top_a = (top_a + ar - (a_hi - 0.2)).clamp(-4.0, 8.0);
                moved.push(json!({"band": "5-10k", "delivered_rel_lufs_db": (ar * 10.0).round() / 10.0, "max_db": a_hi}));
            }
            // too dull up top: first the beat bus's 7.5k shelf eases (it is
            // the cut that made it dull), then the bright parts get back some
            // of what the level rule took (a fader move: the cache re-sums it)
            let air_aim = 0.5 * (a_lo + a_hi);
            let shelf = mix_rules::beat_shelf_db(&e.project);
            if let Some(ar) = ar.filter(|a| *a < a_lo - 0.15 && shelf < -0.5) {
                top_a = (top_a + ar - air_aim).clamp(-6.0, 8.0);
                moved.push(json!({"band": "5-10k", "delivered_rel_lufs_db": (ar * 10.0).round() / 10.0, "floor_db": a_lo, "beat_shelf_7k5_db": (shelf * 10.0).round() / 10.0}));
            } else if let Some(ar) = ar.filter(|a| *a < a_lo - 0.15) {
                let up = mix_rules::give_back_air(e, &mut air_owed, ar, &t)?;
                if !up.is_empty() {
                    lift_log.push(json!({"delivered_air_rel_lufs_db": (ar * 10.0).round() / 10.0, "floor_db": a_lo, "air_given_back": up}));
                    changed = true;
                }
            }
            if !moved.is_empty() {
                tone_rounds += 1;
                let r = mix_rules::shape_top_with(e, voice.as_deref(), &t, m0 + top_p, m0 + top_a)?;
                lift_log.push(json!({"delivered_tone": moved, "top_tone": r}));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let pre = mix_rules::measure_sections(e, voice.as_deref())?;
    checks = mix_rules::check(&path, &pre, &spans, e.project.bpm, &t)?;
    if !lift_log.is_empty() {
        log.push(format!("delivery loop: {} fix(es) after checking the delivered file", lift_log.len()));
        mix.insert("delivery_fixes".into(), json!(lift_log));
    }
    mix.insert("delivery".into(), json!({"format": spec.export.format, "lufs_before": d.get("before").cloned().unwrap_or(Value::Null), "notes": d.get("notes").cloned().unwrap_or(Value::Null)}));
    Ok((Value::Object(mix), path, checks))
}

/// Build (or rebuild from `scope`) the whole song from a valid spec.
pub fn build(e: &mut Engine, spec_v: &Value, scope: Scope) -> Result<Value> {
    let t0 = std::time::Instant::now();
    let (spec, probs) = validate(spec_v);
    if !probs.is_empty() {
        return Err(problems_err(&probs));
    }
    let spec = spec.expect("valid");
    let prev = e.project.song.clone();
    let scope = if prev.is_none() { Scope::Full } else { scope };
    let seed = match (&prev, spec.seed) {
        (_, Some(s)) => s,
        (Some(p), None) if scope != Scope::Full || spec_v.get("seed").is_none() => p.seed,
        _ => seed_for(&spec),
    };
    let mut log = Vec::new();
    if scope == Scope::Full {
        compose_beat(e, &spec, seed, &mut log)?;
    }
    let mut vocal = Value::Null;
    if scope >= Scope::Vocal {
        if let Some(vs) = &spec.vocal {
            vocal = place_vocal(e, &spec, vs, &mut log)?;
        } else if let Some(p) = &prev {
            // a vocal removed from the spec leaves the song
            if let Some(t) = p.spec["vocal"]["track"].as_str().or(p.spec.get("vocal").map(|_| "vocal")) {
                remove_vocal(e, t);
            }
        }
    }
    let (mix, path, checks) = mix_deliver(e, &spec, &mut log)?;
    let spans = mix_rules::section_spans(&e.project);
    let bs = 60.0 / e.project.bpm;
    let mut stored = spec_v.clone();
    if let Value::Object(m) = &mut stored {
        m.remove("seed");
    }
    e.project.song = Some(SongState { spec: stored, seed, sections: spans.clone(), audio: Some(path.to_string_lossy().to_string()), checks: checks.clone() });
    if !spec.title.is_empty() {
        e.project.name = spec.title.clone();
    }
    Ok(json!({
        "audio": path.to_string_lossy(),
        "scope": format!("{scope:?}").to_lowercase(),
        "seed": seed,
        "bpm": e.project.bpm,
        "key": format!("{} {}", e.project.key_root, e.project.scale),
        "duration_s": spans.last().map(|s| ((s.end_beat * bs) * 10.0).round() / 10.0),
        "sections": spans.iter().map(|s| json!({"name": s.name, "kind": s.kind, "bars": ((s.end_beat - s.start_beat) / 4.0).round(), "start_s": ((s.start_beat * bs) * 100.0).round() / 100.0})).collect::<Vec<_>>(),
        "vocal": vocal,
        "checks": checks,
        "mix": mix,
        "log": log,
        "wall_seconds": (t0.elapsed().as_secs_f32() * 10.0).round() / 10.0,
        "next": ["song_patch {set: {\"/mix/vocal_over_beat_db\": 8}} re-mixes only", "song_patch {set: {\"/vocal/phrases/0/beat\": 2}} rebuilds only the vocal", "song_get shows the spec; undo restores the song before this call"],
    }))
}

/// Set (or remove with null) one value by JSON pointer, creating objects on
/// the way; "-" appends to an array.
pub fn set_pointer(root: &mut Value, ptr: &str, val: Value) -> Result<()> {
    if ptr.is_empty() || ptr == "/" {
        bail!("give a path like /mix/vocal_over_beat_db");
    }
    if !ptr.starts_with('/') {
        bail!("'{ptr}' is not a JSON path: start it with '/', e.g. /vocal/gain_db");
    }
    let parts: Vec<String> = ptr[1..].split('/').map(|s| s.replace("~1", "/").replace("~0", "~")).collect();
    let mut cur = root;
    for (i, k) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        if cur.is_null() {
            *cur = json!({});
        }
        match cur {
            Value::Object(m) => {
                if last {
                    if val.is_null() {
                        m.remove(k);
                    } else {
                        m.insert(k.clone(), val);
                    }
                    return Ok(());
                }
                cur = m.entry(k.clone()).or_insert(Value::Null);
            }
            Value::Array(a) => {
                let idx = if k == "-" { a.len() } else { k.parse::<usize>().map_err(|_| anyhow!("{ptr}: '{k}' is not a list index"))? };
                if last {
                    if val.is_null() {
                        if idx < a.len() {
                            a.remove(idx);
                        }
                    } else if idx == a.len() {
                        a.push(val);
                    } else if idx < a.len() {
                        a[idx] = val;
                    } else {
                        bail!("{ptr}: index {idx} but the list has {} items", a.len());
                    }
                    return Ok(());
                }
                if idx >= a.len() {
                    bail!("{ptr}: index {idx} but the list has {} items", a.len());
                }
                cur = &mut a[idx];
            }
            _ => bail!("{ptr}: '{}' is not an object or list", parts[..i].join("/")),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------- tools

fn spec_arg(a: &Value) -> Value {
    match a.get("spec") {
        Some(s) if s.is_object() => s.clone(),
        _ => {
            let mut m = a.as_object().cloned().unwrap_or_default();
            m.remove("spec");
            Value::Object(m)
        }
    }
}

/// The genres this engine knows, from the playbooks (never a hand list that
/// drifts): "trap, ..., punjabi_party (honey_singh, badshah, bhangra), ...".
fn genre_help() -> String {
    let names: Vec<String> = crate::producer::playbooks()
        .iter()
        .map(|b| match b.name.as_str() {
            "punjabi_party" => "punjabi_party (Honey Singh / Badshah party rap: dhol, tumbi, 808; also 'bhangra', 'badshah', 'honey_singh')".to_string(),
            "desi_hiphop" => "desi_hiphop (tabla, sitar, tanpura, 808)".to_string(),
            n => n.to_string(),
        })
        .collect();
    names.join(", ")
}

fn spec_props() -> Value {
    json!({
        "spec": {"type": "object", "description": "The whole SongSpec (or give its fields flat next to action)."},
        "title": {"type": "string"},
        "prompt": {"type": "string", "description": "Free words about the sound (optional)."},
        "genre": {"type": "string", "description": genre_help()},
        "bpm": {"type": "number"},
        "key": {"type": "string", "description": "\"D\", \"D minor\", \"F#m\""},
        "scale": {"type": "string"},
        "mood": {"type": "string"},
        "density": {"type": "string", "enum": ["sparse", "balanced", "dense"]},
        "seed": {"type": "integer", "description": "Same spec + same seed = same song. Omitted: a fresh one, returned and kept for patches."},
        "structure": {"type": "array", "items": {"type": "object", "properties": {"kind": {"type": "string", "enum": SECTION_KINDS}, "bars": {"type": "integer"}, "name": {"type": "string"}}, "required": ["kind", "bars"]}},
        "instruments": {"type": "object", "description": "role -> preset name | \"catalog:<id>\" (a recorded instrument, e.g. catalog:euterpea_harmonium) | {\"samples\": [one-shot files], \"gain_db\": 0} (e.g. real dhol hits on tabla/bayan)"},
        "vocal": {"type": "object", "description": "{file, track?, phrases?: [{src_s, len_s|end_s, section, bar, beat, gain_db?}], start_section?, lyrics?, gain_db?, chain?}"},
        "mix": {"type": "object", "description": "{vocal_over_beat_db: 6, vocal_over_beat_max_db: 12, drop_lift_db: 2, balance_passes: 3, levels: {role: dB offset, e.g. \"bass\": 2}}"},
        "master": {"type": "object", "description": "{lufs: -10, true_peak: -1, preserve_dynamics: 0.8}"},
        "export": {"type": "object", "description": "{path?, format: mp3|wav|flac, bitrate_kbps: 320}"},
    })
}

pub fn example() -> Value {
    json!({
        "title": "nikal ja", "genre": "desi_hiphop", "bpm": 100, "key": "D minor",
        "structure": [{"kind": "intro", "bars": 4}, {"kind": "verse", "bars": 16}, {"kind": "hook", "bars": 8}, {"kind": "verse", "bars": 16}, {"kind": "hook", "bars": 8}, {"kind": "outro", "bars": 4}],
        "instruments": {"lead": "indian_flute", "harmony": "harmonium"},
        "vocal": {"file": "/path/take.wav", "phrases": [{"src_s": 3.34, "len_s": 4.3, "section": "verse1", "bar": 1, "beat": 1}, {"src_s": 50.2, "len_s": 3.9, "section": "hook1", "bar": 1, "beat": 1, "gain_db": 1}]},
        "export": {"path": "/tmp/song.mp3", "format": "mp3"}
    })
}

fn create_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let mut spec = spec_arg(a);
    // {"improve": N} beside the spec: N self-critique rounds after the build
    let improve = spec.as_object_mut().and_then(|m| m.remove("improve")).or_else(|| a.get("improve").cloned());
    let mut r = build(e, &spec, Scope::Full)?;
    if let Some(n) = improve.and_then(|v| v.as_u64()).filter(|n| *n > 0) {
        let imp = crate::song_critic::improve(e, (n as usize).min(4), 3, None)?;
        r["checks"] = e.project.song.as_ref().map(|s| s.checks.clone()).unwrap_or(Value::Null);
        r["improve"] = imp;
    }
    Ok(r)
}

fn patch_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let set = a.get("set").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let remove: Vec<String> = a.get("remove").and_then(|v| v.as_array()).map(|x| x.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default();
    patch_song(e, &set, &remove)
}

/// Set / remove spec values by JSON pointer and rebuild only what they touch
/// (the same seed unless /seed is set).
pub fn patch_song(e: &mut Engine, set: &Map<String, Value>, remove: &[String]) -> Result<Value> {
    let st = e.project.song.clone().ok_or_else(|| anyhow!("this project has no song yet: song_create builds one from a spec first"))?;
    if set.is_empty() && remove.is_empty() {
        bail!("nothing to change: give set {{\"/path\": value}} and/or remove [\"/path\"]; song_get shows the spec and its paths");
    }
    let mut spec = st.spec.clone();
    let mut scope = Scope::Mix;
    for (k, v) in set {
        set_pointer(&mut spec, k, v.clone())?;
        scope = scope.max_by(scope_of(k));
    }
    for k in remove {
        set_pointer(&mut spec, k, Value::Null)?;
        scope = scope.max_by(scope_of(k));
    }
    if set.contains_key("/seed") {
        scope = Scope::Full;
    }
    let mut spec_with_seed = spec.clone();
    if !set.contains_key("/seed") {
        spec_with_seed["seed"] = json!(st.seed);
    }
    let mut r = build(e, &spec_with_seed, scope)?;
    if let Some(s) = e.project.song.as_mut() {
        s.spec = spec;
    }
    r["changed_paths"] = json!(set.keys().chain(remove.iter()).collect::<Vec<_>>());
    Ok(r)
}

trait MaxBy {
    fn max_by(self, o: Self) -> Self;
}
impl MaxBy for Scope {
    fn max_by(self, o: Self) -> Self {
        if o > self { o } else { self }
    }
}

fn get_tool(e: &mut Engine, _a: &Value) -> Result<Value> {
    match &e.project.song {
        Some(s) => Ok(json!({"spec": s.spec, "seed": s.seed, "sections": s.sections, "audio": s.audio, "checks": s.checks})),
        None => Ok(json!({"song": null, "note": "no song in this project; song_create builds one", "example": example()})),
    }
}

fn validate_tool(_e: &mut Engine, a: &Value) -> Result<Value> {
    let spec = spec_arg(a);
    let (s, probs) = validate(&spec);
    let mut out = json!({
        "valid": probs.is_empty(),
        "problems": probs,
        "sections": s.as_ref().map(|s| s.section_names()),
        "targets": s.as_ref().map(|s| s.targets()),
    });
    // say what a genre word resolved to ("honey_singh" -> punjabi_party), and
    // without a structure, the sections the genre's own form names, so
    // phrases can be placed before anything is built
    if let Some(pb) = s.as_ref().and_then(|s| s.genre.as_deref()).and_then(|g| crate::producer::playbook(g).ok()) {
        out["genre"] = json!({"asked": s.as_ref().and_then(|s| s.genre.clone()), "resolved": pb.name, "bpm_range": pb.bpm, "about": pb.description});
        if s.as_ref().is_some_and(|s| s.structure.is_empty()) {
            let mut count: BTreeMap<String, usize> = BTreeMap::new();
            let names: Vec<String> = pb
                .arrangement
                .iter()
                .map(|t| {
                    let k = canon_kind(&t.kind);
                    let c = count.entry(k.clone()).or_insert(0);
                    *c += 1;
                    format!("{k}{c}")
                })
                .collect();
            out["sections"] = json!(names);
            out["sections_from"] = json!(format!(
                "the {} template form ({} bars); a fresh seed may vary it, so give \"structure\" when phrases must land in named sections",
                pb.name,
                pb.arrangement.iter().map(|t| t.bars).sum::<u32>()
            ));
        }
    }
    Ok(out)
}

fn schema_tool(_e: &mut Engine, _a: &Value) -> Result<Value> {
    Ok(json!({
        "fields": spec_props(),
        "section_kinds": SECTION_KINDS,
        "roles": crate::creative::ROLES,
        "genres": crate::producer::playbooks().iter().map(|b| b.name.clone()).collect::<Vec<_>>(),
        "defaults": {"mix": MixSpec::default(), "master": MasterSpec::default(), "export": ExportSpec::default(), "vocal_chain": VocalChain::default()},
        "example": example(),
        "patch_scopes": {"mix": "/mix (except /mix/levels), /master, /export, /title, /vocal/gain_db: re-mix and deliver", "vocal": "/vocal/...: rebuild the vocal track", "full": "anything else: recompose the beat with the same seed"},
    }))
}

fn render_tool(e: &mut Engine, _a: &Value) -> Result<Value> {
    let st = e.project.song.clone().ok_or_else(|| anyhow!("this project has no song yet: song_create builds one from a spec first"))?;
    let mut spec = st.spec.clone();
    spec["seed"] = json!(st.seed);
    let mut r = build(e, &spec, Scope::Mix)?;
    if let Some(s) = e.project.song.as_mut() {
        s.spec = st.spec;
    }
    r["note"] = json!("re-mixed and delivered the project as it is now (edits made with other actions are kept)");
    Ok(r)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "song_create",
            description: "START HERE for a whole song. One SongSpec in, one finished song out: the beat composed to your structure and instruments, your vocal take cut and placed (phrases at section/bar/beat, or cut at its pauses), the vocal chain (HPF, light de-ess, 3:1, small sends), the vocal balanced over the beat section by section, hooks lifted over verses, delivered as mp3/wav/flac at -10 LUFS / -1 dBTP, and every target CHECKED on the delivered file (checks[] with pass/fail and the fix). One undo step. song_schema shows every field; song_validate checks a spec without building.",
            mutates: true,
            schema: || {
                let mut p = spec_props();
                p["improve"] = json!({"type": "integer", "description": "self-critique rounds after the build (song_improve: judge against the genre profile, fix, rebuild, keep the best take; 0 = none, max 4)"});
                obj(p, &[])
            },
            run: create_tool,
        },
        Tool {
            name: "song_patch",
            description: "Change the song by JSON path and redo only what the path touches: set {\"/mix/vocal_over_beat_db\": 8} re-mixes, {\"/vocal/phrases/3/beat\": 2} rebuilds the vocal, {\"/bpm\": 96} or {\"/instruments/lead\": \"sitar\"} recomposes the beat with the same seed. remove [\"/vocal\"] drops a part. Returns the new checks. One undo step.",
            mutates: true,
            schema: || obj(json!({"set": {"type": "object", "description": "JSON path -> new value"}, "remove": {"type": "array", "items": {"type": "string"}}}), &[]),
            run: patch_tool,
        },
        Tool {
            name: "song_get",
            description: "The song's spec (every path song_patch can change), seed, sections with start times, the delivered file and its last checks.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: get_tool,
        },
        Tool {
            name: "song_validate",
            description: "Check a SongSpec without building anything: every problem at once, each with its JSON path and the fix; the section names phrases can refer to; the mix targets that will be checked.",
            mutates: false,
            schema: || obj(spec_props(), &[]),
            run: validate_tool,
        },
        Tool {
            name: "song_schema",
            description: "Every SongSpec field with its type and default, section kinds, instrument roles, genres, a full example and what each patch path redoes.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: schema_tool,
        },
        Tool {
            name: "song_render",
            description: "Re-mix, deliver and check the song as the project is now (after edits with other actions): vocal balance, hook lift, loudness and peak targets, checks[] in the result.",
            mutates: true,
            schema: || obj(json!({}), &[]),
            run: render_tool,
        },
        Tool {
            name: "song_critique",
            description: "Judge the song as delivered against its genre's target profile (data: low end, hook lift, loudness range, width, groove density, the hook's build, top end, vocal over beat) and name each weakness in plain words, with the spec change that fixes it. Changes nothing. Give {file, genre} to judge any audio file.",
            mutates: false,
            schema: || obj(json!({"genre": {"type": "string", "description": "judge against this genre's profile (default: the song's genre)"}, "file": {"type": "string", "description": "judge this audio file instead of the song (needs genre for its profile)"}}), &[]),
            run: crate::song_critic::critique_tool,
        },
        Tool {
            name: "song_improve",
            description: "Make the song better on its own: critique it against its genre profile, apply the fixes for the biggest weaknesses (spec knobs: /mix/levels, /mix/drop_lift_db, /density, /master/preserve_dynamics, /mix/vocal_over_beat_db), rebuild, critique again and keep the best take, for up to `rounds` rebuilds (default 2, max 4; each is a full render). The song's file ends as the best take. song_create {..., \"improve\": 2} does the same in one call.",
            mutates: true,
            schema: || obj(json!({"rounds": {"type": "integer", "description": "rebuilds at most (1..4, default 2)"}, "max_fixes": {"type": "integer", "description": "fixes per round (default 3)"}, "genre": {"type": "string", "description": "judge against this genre's profile (default: the song's genre)"}}), &[]),
            run: crate::song_critic::improve_tool,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_problem_comes_with_a_path_and_a_fix() {
        let v = json!({"genre": "desi", "tempo": 100, "structure": [{"kind": "hok", "bars": 8}, {"kind": "verse", "bars": 99}], "master": {"lufs": -3}});
        let (_, probs) = validate(&v);
        assert!(probs.iter().any(|x| x.path == "/tempo" && x.fix.contains("bpm")), "{probs:?}");
        let v = json!({"genre": "dezi_hiphopp", "bpm": 300, "structure": [{"kind": "hok", "bars": 8}, {"kind": "verse", "bars": 99}], "master": {"lufs": -3}});
        let (_, probs) = validate(&v);
        let paths: Vec<&str> = probs.iter().map(|x| x.path.as_str()).collect();
        for want in ["/genre", "/bpm", "/structure/0/kind", "/structure/1/bars", "/master/lufs"] {
            assert!(paths.contains(&want), "{want} missing in {paths:?}");
        }
        assert!(probs.iter().find(|x| x.path == "/structure/0/kind").unwrap().fix.contains("hook"));
        assert!(probs.iter().find(|x| x.path == "/genre").unwrap().fix.contains("desi_hiphop"));
    }

    #[test]
    fn a_good_spec_validates_and_names_sections() {
        let mut v = example();
        v.as_object_mut().unwrap().remove("vocal");
        v["export"] = json!({"format": "wav"});
        let (s, probs) = validate(&v);
        assert!(probs.is_empty(), "{probs:?}");
        assert_eq!(s.unwrap().section_names(), vec!["intro1", "verse1", "hook1", "verse2", "hook2", "outro1"]);
    }

    #[test]
    fn phrases_are_checked_against_the_structure() {
        let mut v = example();
        v["vocal"] = json!({"file": "/etc/hostname", "phrases": [{"src_s": 0, "len_s": 2, "section": "hook", "bar": 1}, {"src_s": 0, "len_s": 2, "section": "hook1", "bar": 12}, {"src_s": 1}]});
        let (_, probs) = validate(&v);
        let paths: Vec<&str> = probs.iter().map(|x| x.path.as_str()).collect();
        assert!(paths.contains(&"/vocal/phrases/0/section"), "{paths:?}"); // hook occurs twice
        assert!(paths.contains(&"/vocal/phrases/1/bar"), "{paths:?}");
        assert!(paths.contains(&"/vocal/phrases/2"), "{paths:?}");
    }

    #[test]
    fn patch_paths_pick_the_smallest_redo() {
        assert_eq!(scope_of("/mix/vocal_over_beat_db"), Scope::Mix);
        assert_eq!(scope_of("/vocal/gain_db"), Scope::Mix);
        assert_eq!(scope_of("/mix/levels/bass"), Scope::Full);
        assert_eq!(scope_of("/mix/levels"), Scope::Full);
        assert_eq!(scope_of("/vocal/phrases/3/beat"), Scope::Vocal);
        assert_eq!(scope_of("/bpm"), Scope::Full);
        assert_eq!(scope_of("/structure/1/bars"), Scope::Full);
        let mut v = json!({"a": [1, 2]});
        set_pointer(&mut v, "/b/c", json!(3)).unwrap();
        set_pointer(&mut v, "/a/-", json!(9)).unwrap();
        set_pointer(&mut v, "/a/0", Value::Null).unwrap();
        assert_eq!(v, json!({"a": [2, 9], "b": {"c": 3}}));
        assert!(set_pointer(&mut v, "a", json!(1)).is_err());
    }

    #[test]
    fn keys_split() {
        assert_eq!(split_key("D minor"), ("D".into(), Some("minor".into())));
        assert_eq!(split_key("F#m"), ("F#".into(), Some("minor".into())));
        assert_eq!(split_key("Bb"), ("Bb".into(), None));
    }

    #[test]
    fn song_create_takes_improve_beside_the_spec() {
        let tool = tools().into_iter().find(|t| t.name == "song_create").unwrap();
        assert!((tool.schema)()["properties"]["improve"].is_object(), "improve must pass the argument check");
    }

    #[test]
    fn validate_names_the_genre_a_word_resolved_to_and_its_sections() {
        let mut e = Engine::new(std::env::temp_dir().join("bb_song_validate_test"));
        let v = validate_tool(&mut e, &json!({"genre": "honey_singh"})).unwrap();
        assert_eq!(v["valid"], true);
        assert_eq!(v["genre"]["resolved"], "punjabi_party");
        let secs: Vec<String> = serde_json::from_value(v["sections"].clone()).unwrap();
        assert_eq!(secs.first().map(String::as_str), Some("intro1"));
        assert!(secs.iter().any(|s| s == "hook2"));
        assert!(v["sections_from"].as_str().unwrap().contains("structure"));
        // a given structure is the contract: its names, no template note
        let w = validate_tool(&mut e, &json!({"genre": "badshah", "structure": [{"kind": "verse", "bars": 8}]})).unwrap();
        assert_eq!(w["sections"], json!(["verse1"]));
        assert!(w.get("sections_from").is_none());
    }

    #[test]
    fn instrumental_song_builds_delivers_and_checks() {
        let dir = std::env::temp_dir().join("bb_song_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        let spec = json!({"genre": "boom_bap", "bpm": 90, "seed": 3,
            "structure": [{"kind": "intro", "bars": 2}, {"kind": "verse", "bars": 4}, {"kind": "hook", "bars": 4}],
            "export": {"path": dir.join("t.wav").to_string_lossy(), "format": "wav"}});
        let r = e.call("song_create", &spec).unwrap();
        assert!(std::path::Path::new(r["audio"].as_str().unwrap()).is_file());
        let names: Vec<&str> = r["sections"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["intro1", "verse1", "hook1"]);
        assert_eq!(r["sections"][1]["bars"], 4.0);
        let checks = r["checks"]["checks"].as_array().unwrap();
        assert!(checks.iter().any(|c| c["check"] == "loudness"));
        assert!(checks.iter().any(|c| c["check"] == "true_peak" && c["pass"] == true), "{checks:?}");
        // a mix-only patch keeps the beat
        let before = e.project.patterns.clone();
        let r2 = e.call("song_patch", &json!({"set": {"/master/lufs": -12}})).unwrap();
        assert_eq!(r2["scope"], "mix");
        assert_eq!(e.project.patterns, before);
        assert_eq!(e.call("song_get", &json!({})).unwrap()["spec"]["master"]["lufs"], -12);
        // one undo step back to the first song
        e.call("undo", &json!({})).unwrap();
        assert_ne!(e.project.song.as_ref().unwrap().spec["master"]["lufs"], -12);
    }
}
