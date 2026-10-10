//! Sound palettes: named, curated sets of voices (and optionally CC0
//! sample one-shots) a producer or an AI picks by character
//! ("dark_trap", "boom_bap_dusty", ...).
//!
//! A palette maps roles (kick, snare, clap, hat, open_hat, perc, bass,
//! keys, pad, lead, bell, pluck) to instrument presets. `apply` swaps the
//! matching tracks of the current project to those voices and level-matches
//! each swap so the mix balance survives. With `use_samples`, the drum roles
//! use curated CC0 one-shots instead (fetched on first use from pinned
//! upstream commits, SHA-256 checked; see SAMPLES_LICENSES.md).
//!
//! API for the producer: [`get`], [`for_genre`], [`voice`], [`apply`].

use crate::engine::Engine;
use crate::instruments::{preset, DrumKind, Instrument, SamplerParams};
use crate::samples::SampleInfo;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Roles a palette can fill.
pub const ROLES: &[&str] = &[
    "kick", "snare", "clap", "hat", "open_hat", "perc", "bass", "keys", "pad", "lead", "bell",
    "pluck",
];

pub struct Palette {
    pub name: &'static str,
    pub description: &'static str,
    /// Genres this palette is the default for (producer genre ids).
    pub genres: &'static [&'static str],
    /// Character tags (dark, dusty, airy, gritty, ...).
    pub character: &'static [&'static str],
    /// role -> preset name.
    pub voices: &'static [(&'static str, &'static str)],
    /// role -> curated sample id (palette_samples.json), used with
    /// `use_samples`.
    pub samples: &'static [(&'static str, &'static str)],
    /// One-line mix hint for the producer / AI.
    pub mix_hint: &'static str,
}

pub const PALETTES: &[Palette] = &[
    Palette {
        name: "dark_trap",
        description: "Hard, dark trap: punchy clicky kick, cracking snare + wide clap, crisp hats with a choked open hat, a distorted 808 that reads on phone speakers, dark keys and a glassy bell",
        genres: &["trap", "dark_trap", "rage"],
        character: &["dark", "hard", "distorted", "crisp"],
        voices: &[
            ("kick", "kick_punchy"),
            ("snare", "snare_crack"),
            ("clap", "clap_wide"),
            ("hat", "hat_crisp"),
            ("open_hat", "open_hat_airy"),
            ("perc", "rim"),
            ("bass", "808_grit"),
            ("keys", "dark_keys"),
            ("pad", "dark_pad"),
            ("lead", "lead_dark"),
            ("bell", "bell_glass"),
            ("pluck", "pluck_soft"),
        ],
        samples: &[
            ("kick", "tr808_bd1025"),
            ("snare", "sp_sn_dub"),
            ("clap", "tr808_cp"),
            ("hat", "sp_hat_raw"),
            ("open_hat", "tr808_oh50"),
            ("perc", "tr808_rs"),
        ],
        mix_hint: "808 carries the low end: keep the kick short and sidechain the 808 lightly; saturate the 808 bus, not the kick",
    },
    Palette {
        name: "dhh_grit",
        description: "Desi hip-hop grit: driven punchy kick, tight SP-style snare, crisp hats, gritty 808, with harmonium keys, sitar, bansuri, santoor and tanpura for the melodic roles (pairs with the desi_hiphop generator: tabla + dholak in Kafi / Asavari / Bhairav)",
        genres: &["desi_hiphop", "dhh", "gully"],
        character: &["gritty", "raw", "desi", "punchy"],
        voices: &[
            ("kick", "kick_grit"),
            ("snare", "snare_tight"),
            ("clap", "clap_wide"),
            ("hat", "hat_crisp"),
            ("open_hat", "open_hat_airy"),
            ("perc", "tabla"),
            ("bass", "808_grit"),
            ("keys", "harmonium"),
            ("pad", "tanpura"),
            ("lead", "bansuri"),
            ("bell", "santoor"),
            ("pluck", "sitar"),
        ],
        samples: &[
            ("kick", "sp_drum_bass_hard"),
            ("snare", "sp_elec_snare"),
            ("clap", "tr808_cp"),
            ("hat", "sp_hat_cab"),
            ("open_hat", "tr808_oh25"),
        ],
        mix_hint: "let the sitar/bansuri own 1-4 kHz: dip the hats there; parallel-saturate the drum bus for grit",
    },
    Palette {
        name: "boom_bap_dusty",
        description: "90s boom bap, dusty: round saturated kick, fat dark snare, soft dark hats, warm round bass, dusty keys and a lo-fi pad with tape-like drift",
        genres: &["boom_bap", "lofi", "jazz_rap"],
        character: &["dusty", "warm", "swung", "lofi"],
        voices: &[
            ("kick", "kick_dusty"),
            ("snare", "snare_dusty"),
            ("clap", "snare_dusty"),
            ("hat", "hat_dusty"),
            ("open_hat", "open_hat_dusty"),
            ("perc", "rim"),
            ("bass", "bass_round"),
            ("keys", "keys_dusty"),
            ("pad", "pad_dusty"),
            ("lead", "felt_piano"),
            ("bell", "marimba"),
            ("pluck", "guitar_pluck"),
        ],
        samples: &[
            ("kick", "sp_bd_jazz"),
            ("snare", "sp_sn_dolf"),
            ("clap", "sp_elec_lo_snare"),
            ("hat", "sp_drum_cymbal_closed"),
            ("open_hat", "sp_drum_cymbal_open"),
            ("perc", "sp_drum_cymbal_pedal"),
        ],
        mix_hint: "low-pass the drum bus around 9-11 kHz, a touch of tape saturation, swing 54-58 %",
    },
    Palette {
        name: "drill_slide",
        description: "UK/NY drill: short tight kick, high tight snare, crisp hats, a long sliding 808 with tuned glides, dark lead and an eerie bell",
        genres: &["drill", "uk_drill", "ny_drill"],
        character: &["dark", "sliding", "tight", "cold"],
        voices: &[
            ("kick", "kick_tight"),
            ("snare", "snare_drill"),
            ("clap", "clap_wide"),
            ("hat", "hat_drill"),
            ("open_hat", "open_hat_airy"),
            ("perc", "rim"),
            ("bass", "808_slide"),
            ("keys", "dark_keys"),
            ("pad", "choir"),
            ("lead", "lead_dark"),
            ("bell", "bell_dark"),
            ("pluck", "pluck_soft"),
        ],
        samples: &[
            ("kick", "sp_bd_klub"),
            ("snare", "tr808_sd2575"),
            ("clap", "tr808_cp"),
            ("hat", "tr808_ch"),
            ("open_hat", "tr808_oh25"),
            ("perc", "tr808_rs"),
        ],
        mix_hint: "the 808 slides are the hook: give them space (no long pads under 200 Hz) and keep the kick short",
    },
    Palette {
        name: "melodic_airy",
        description: "Melodic rap / R&B, airy: deep soft kick, clean snare + snap, bright airy hats, a clean 808, chorused e-piano, a wide drifting pad and glassy bells",
        genres: &["melodic_rap", "rnb", "pop_rap", "melodic_trap"],
        character: &["airy", "wide", "soft", "clean"],
        voices: &[
            ("kick", "kick_deep"),
            ("snare", "snare_soft"),
            ("clap", "clap_wide"),
            ("hat", "hat_crisp"),
            ("open_hat", "open_hat_airy"),
            ("perc", "shaker"),
            ("bass", "808_clean"),
            ("keys", "keys_airy"),
            ("pad", "pad_airy"),
            ("lead", "bell_glass"),
            ("bell", "bell_glass"),
            ("pluck", "pluck_soft"),
        ],
        samples: &[
            ("kick", "sp_bd_808"),
            ("snare", "sp_sn_generic"),
            ("clap", "sp_perc_snap"),
            ("hat", "sp_hat_snap"),
            ("open_hat", "tr808_oh50"),
            ("perc", "sp_ride_tri"),
        ],
        mix_hint: "long reverb on keys/pad via a send, keep the 808 clean and the vocal pocket (1-4 kHz) open",
    },
];

fn norm(s: &str) -> String {
    s.trim().to_lowercase().replace([' ', '-'], "_")
}

/// Palette by name (case/space-insensitive).
pub fn get(name: &str) -> Result<&'static Palette> {
    let n = norm(name);
    PALETTES
        .iter()
        .find(|p| p.name == n)
        .or_else(|| for_genre(&n))
        .ok_or_else(|| {
            anyhow!(
                "unknown palette '{name}'. Palettes: {}",
                PALETTES
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// Default palette for a producer genre id, if one fits.
pub fn for_genre(genre: &str) -> Option<&'static Palette> {
    let g = norm(genre);
    PALETTES.iter().find(|p| p.genres.contains(&g.as_str()))
}

/// The palette's instrument for a role.
pub fn voice(p: &Palette, role: &str) -> Option<Instrument> {
    p.voices
        .iter()
        .find(|(r, _)| *r == role)
        .and_then(|(_, n)| preset(n))
}

/// Palette role of a track, or None for tracks a palette leaves alone
/// (Indian instruments, vocals, fx, crashes, featured modelled
/// instruments).
pub fn track_role(name: &str, inst: &Instrument) -> Option<&'static str> {
    let n = name.to_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| n.contains(w));
    if has(&[
        "tabla",
        "bayan",
        "sitar",
        "santoor",
        "bansuri",
        "tanpura",
        "flute",
        "vocal",
        "vox",
        "dholak",
        "harmonium",
        "shehnai",
        "chop",
        "fx",
        "riser",
        "impact",
        "sweep",
        "crash",
        "drone",
    ]) {
        return None;
    }
    let inst = match inst {
        Instrument::Layer(l) => l.layers.first().map(|x| &x.instrument).unwrap_or(inst),
        other => other,
    };
    if let Instrument::Drum(d) = inst {
        return match d.kind {
            DrumKind::Kick => Some("kick"),
            DrumKind::Snare => Some("snare"),
            DrumKind::Clap => Some("clap"),
            DrumKind::ClosedHat | DrumKind::Shaker => Some("hat"),
            DrumKind::OpenHat => Some("open_hat"),
            DrumKind::Rim | DrumKind::Cowbell | DrumKind::Tom => Some("perc"),
            DrumKind::Crash | DrumKind::Tabla | DrumKind::Bayan => None,
        };
    }
    if matches!(
        inst,
        Instrument::Piano(_)
            | Instrument::Ensemble(_)
            | Instrument::Multisample(_)
            | Instrument::Granular(_)
    ) {
        return None;
    }
    let sampler = matches!(inst, Instrument::Sampler(_));
    Some(if has(&["open"]) && has(&["hat", "hh"]) {
        "open_hat"
    } else if has(&["kick", "bd"]) {
        "kick"
    } else if has(&["clap", "snap"]) {
        "clap"
    } else if has(&["snare", "rim"]) {
        "snare"
    } else if has(&["hat", "hh", "shaker"]) {
        "hat"
    } else if has(&["perc", "cowbell", "conga", "bongo", "tom"]) {
        "perc"
    } else if sampler {
        // a melodic or unknown sample: leave it
        return None;
    } else if matches!(inst, Instrument::Bass808(_)) || has(&["808", "bass", "sub"]) {
        "bass"
    } else if has(&["bell", "mallet", "marimba", "glock"]) {
        "bell"
    } else if has(&["pluck", "arp", "guitar"]) || matches!(inst, Instrument::Pluck(_)) {
        "pluck"
    } else if has(&["pad", "string", "choir", "atmos"]) {
        "pad"
    } else if has(&["chord", "keys", "piano", "rhodes", "organ", "epiano"]) {
        "keys"
    } else if has(&["lead", "melody", "hook", "top"]) {
        "lead"
    } else {
        return None;
    })
}

// ------------------------------------------------------------ samples

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SampleEntry {
    pub id: String,
    pub role: String,
    pub tags: Vec<String>,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub license: String,
    pub author: String,
    pub source: String,
    pub note: String,
}

/// The curated CC0 sample manifest (palette_samples.json).
pub fn manifest() -> &'static [SampleEntry] {
    static M: std::sync::OnceLock<Vec<SampleEntry>> = std::sync::OnceLock::new();
    M.get_or_init(|| {
        serde_json::from_str(include_str!("palette_samples.json")).expect("palette_samples.json")
    })
}

pub fn sample_entry(id: &str) -> Result<&'static SampleEntry> {
    manifest()
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| anyhow!("unknown palette sample '{id}'"))
}

pub fn sample_dir(e: &Engine) -> PathBuf {
    e.samples_dir().join("palette")
}

pub fn sample_path(e: &Engine, s: &SampleEntry) -> PathBuf {
    let ext = s.url.rsplit('.').next().unwrap_or("wav").to_lowercase();
    sample_dir(e).join(format!("{}.{ext}", s.id))
}

/// Local copy of a curated sample: downloaded on first use and verified
/// against its pinned SHA-256 (a mismatching file is fetched again).
pub fn fetch_sample(e: &Engine, s: &SampleEntry) -> Result<PathBuf> {
    let path = sample_path(e, s);
    if let Ok(b) = std::fs::read(&path) {
        if sha256_hex(&b) == s.sha256 {
            return Ok(path);
        }
    }
    std::fs::create_dir_all(sample_dir(e))?;
    let resp = ureq::get(&s.url)
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .with_context(|| format!("download {}", s.url))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut resp.into_reader(), &mut bytes)?;
    let got = sha256_hex(&bytes);
    if got != s.sha256 {
        bail!(
            "{}: checksum mismatch (expected {}, got {got}); not using it",
            s.id,
            s.sha256
        );
    }
    std::fs::write(&path, &bytes)?;
    std::fs::write(
        sample_dir(e).join("LICENSE.txt"),
        "CC0 1.0 Universal (public domain). Sources, authors and checksums: SAMPLES_LICENSES.md / src/palette_samples.json in beatbox-rs.\n",
    )?;
    Ok(path)
}

/// Fetch curated samples (all, or those matching ids/role/tag) and index
/// them into the sample library with their role and character tags.
pub fn install_samples(
    e: &Engine,
    ids: &[String],
    role: Option<&str>,
    tag: Option<&str>,
) -> Result<Value> {
    let pick: Vec<&SampleEntry> = manifest()
        .iter()
        .filter(|s| ids.is_empty() || ids.contains(&s.id))
        .filter(|s| role.map(|r| s.role == r).unwrap_or(true))
        .filter(|s| tag.map(|t| s.tags.iter().any(|x| x == t)).unwrap_or(true))
        .collect();
    if pick.is_empty() {
        bail!("no curated samples match");
    }
    let mut idx = crate::sample_lib::load_index(e);
    let (mut ok, mut failed) = (Vec::new(), Vec::new());
    for s in pick {
        match fetch_sample(e, s) {
            Ok(path) => {
                let ps = path.to_string_lossy().to_string();
                idx.retain(|x| x.path != ps);
                match crate::sample_lib::analyze_file(&path) {
                    Ok(mut ent) => {
                        ent.role = s.role.clone();
                        ent.license = format!("{} ({}, {})", s.license, s.author, s.source);
                        ent.tags = s.tags.clone();
                        idx.push(ent);
                        ok.push(s.id.clone());
                    }
                    Err(err) => failed.push(format!("{}: {err}", s.id)),
                }
            }
            Err(err) => failed.push(format!("{}: {err}", s.id)),
        }
    }
    std::fs::create_dir_all(e.samples_dir())?;
    std::fs::write(
        crate::sample_lib::index_path(e),
        serde_json::to_string_pretty(&idx)?,
    )?;
    Ok(
        json!({"installed": ok.len(), "ids": ok, "failed": failed, "dir": sample_dir(e).to_string_lossy(), "license": "CC0 1.0 (every file); see SAMPLES_LICENSES.md"}),
    )
}

/// Register a curated sample in the project (fetching it if needed) and
/// return a one-shot sampler instrument for it.
fn sample_instrument(e: &mut Engine, id: &str) -> Result<Instrument> {
    let s = sample_entry(id)?;
    let path = fetch_sample(e, s)?;
    let name = format!("pal_{id}");
    if !e.project.samples.iter().any(|x| x.name == name) {
        crate::tools::register_sample(
            e,
            SampleInfo {
                name: name.clone(),
                path: path.to_string_lossy().into(),
                source: s.source.clone(),
                license: s.license.clone(),
                author: s.author.clone(),
                duration: 0.0,
            },
        )?;
    } else if !e.bank.contains(&name) {
        let data = crate::samples::decode_file(&path)?;
        e.bank.insert(&name, data);
    }
    Ok(Instrument::Sampler(SamplerParams {
        sample: name,
        root: 60,
        one_shot: true,
        gain: 0.8,
        ..Default::default()
    }))
}

pub struct ApplyOpts {
    /// Use the palette's curated samples for the drum roles it lists.
    pub use_samples: bool,
    /// Only these roles (empty = all).
    pub roles: Vec<String>,
    /// Also replace featured modelled instruments and Indian voices.
    pub force: bool,
    /// Keep each track's loudness (fader compensation); default true.
    pub level_match: bool,
    /// Also apply the palette's mix moves (EQ carving, kick/808 separation,
    /// brightness); default true.
    pub mix: bool,
}

impl Default for ApplyOpts {
    fn default() -> Self {
        ApplyOpts {
            use_samples: false,
            roles: Vec::new(),
            force: false,
            level_match: true,
            mix: true,
        }
    }
}

/// Apply a palette to the current project. Returns what changed.
pub fn apply(e: &mut Engine, name: &str, o: &ApplyOpts) -> Result<Value> {
    let pal = get(name)?;
    let mut changed = Vec::new();
    let mut kept = Vec::new();
    let mut sample_errors = Vec::new();
    for ti in 0..e.project.tracks.len() {
        let (tname, old) = {
            let t = &e.project.tracks[ti];
            (t.name.clone(), t.instrument.clone())
        };
        let role = track_role(&tname, &old).or_else(|| {
            if o.force {
                let r = crate::tools_mix::role_of(&tname, &old);
                Some(match r {
                    "hats" => "hat",
                    "cymbal" => "open_hat",
                    other => ROLES.iter().find(|x| **x == other).copied()?,
                })
            } else {
                None
            }
        });
        let Some(role) = role else {
            kept.push(tname);
            continue;
        };
        if !o.roles.is_empty() && !o.roles.iter().any(|r| r == role) {
            continue;
        }
        let mut new = None;
        let mut via = "preset";
        if o.use_samples {
            if let Some((_, id)) = pal.samples.iter().find(|(r, _)| *r == role) {
                match sample_instrument(e, id) {
                    Ok(i) => {
                        new = Some(i);
                        via = *id;
                    }
                    Err(err) => sample_errors.push(format!("{role}: {err}")),
                }
            }
        }
        if new.is_none() {
            new = voice(pal, role);
        }
        let Some(new) = new else {
            kept.push(tname);
            continue;
        };
        let voice_name = if via == "preset" {
            pal.voices
                .iter()
                .find(|(r, _)| *r == role)
                .map(|(_, n)| *n)
                .unwrap_or("")
                .to_string()
        } else {
            via.to_string()
        };
        let mut gain_change = 0.0f32;
        if o.level_match {
            let mrole = crate::tools_mix::role_of(&tname, &old);
            let a = crate::tools_mix::note_level_db(&old, mrole, &e.bank);
            let b = crate::tools_mix::note_level_db(&new, mrole, &e.bank);
            if a > -90.0 && b > -90.0 {
                gain_change = ((a - b) * 2.0).round() / 2.0;
                gain_change = gain_change.clamp(-18.0, 18.0);
            }
        }
        let t = &mut e.project.tracks[ti];
        t.instrument = new;
        t.volume_db += gain_change;
        changed.push(json!({"track": tname, "role": role, "voice": voice_name, "fader_change_db": gain_change}));
    }
    let mix = if o.mix {
        mix_moves(e, pal.name)
    } else {
        Value::Null
    };
    Ok(json!({
        "palette": pal.name,
        "changed": changed,
        "mix_moves": mix,
        "kept": kept,
        "sample_errors": sample_errors,
        "mix_hint": pal.mix_hint,
        "next": "render or ears_report to hear it; ab_compare against the previous snapshot",
    }))
}

fn effect(v: Value) -> Option<crate::fx::Effect> {
    serde_json::from_value(v).ok()
}

/// Put `fx` on a track's chain under a fixed id (replacing an earlier one with
/// that id, so re-applying a palette never stacks).
fn put_fx(t: &mut crate::project::Track, id: &str, fx: Option<crate::fx::Effect>, front: bool) {
    let Some(mut fx) = fx else { return };
    fx.set_id(id);
    if let Some(i) = t.effects.iter().position(|x| x.id() == id) {
        t.effects[i] = fx;
    } else if front {
        t.effects.insert(0, fx);
    } else {
        t.effects.push(fx);
    }
}

/// Shorten held bass/808 notes that do not slide or connect: each one stops
/// half a step before the next kick and lasts at most `max_steps`, so the
/// kick and the 808 alternate instead of the 808 filling every gap.
pub fn shorten_bass_holds(e: &mut Engine, max_steps: f32) -> usize {
    let bass: Vec<String> = e
        .project
        .tracks
        .iter()
        .filter(|t| crate::tools_mix::role_of(&t.name, &t.instrument) == "bass")
        .map(|t| t.name.to_lowercase())
        .collect();
    let kicks: Vec<String> = e
        .project
        .tracks
        .iter()
        .filter(|t| crate::tools_mix::role_of(&t.name, &t.instrument) == "kick")
        .map(|t| t.name.to_lowercase())
        .collect();
    let mut n = 0;
    for p in e.project.patterns.iter_mut() {
        let total = p.steps() as f32;
        let mut ks: Vec<f32> = kicks
            .iter()
            .flat_map(|k| p.notes(k).iter().map(|x| x.start).collect::<Vec<_>>())
            .collect();
        ks.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for b in &bass {
            let Some(v) = p.clips.get_mut(b) else {
                continue;
            };
            v.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
            let starts: Vec<f32> = v.iter().map(|x| x.start).collect();
            for (i, note) in v.iter_mut().enumerate() {
                let next = starts.get(i + 1).copied().unwrap_or(total);
                if note.slide_to.is_some() || note.start + note.len > next + 0.01 {
                    continue; // a glide or a legato hand-off: leave it
                }
                let next_kick = ks
                    .iter()
                    .copied()
                    .find(|k| *k > note.start + 0.75)
                    .unwrap_or(f32::MAX);
                let cap = max_steps.min(next_kick - note.start - 0.5).max(1.5);
                if note.len > cap + 0.01 {
                    note.len = cap;
                    n += 1;
                }
            }
        }
    }
    n
}

/// The palette's mix moves: the arrangement-level EQ carve for every
/// palette, plus what the palette's character needs (drill: shorter 808
/// holds, kick/808 ducking, a brighter top end and sharper hits).
pub fn mix_moves(e: &mut Engine, name: &str) -> Value {
    let carve = crate::carve::carve(e, &[]);
    let mut moves: Vec<String> = vec!["eq carve (see carve)".into()];
    if name == "drill_slide" {
        let n = shorten_bass_holds(e, 6.0);
        moves.push(format!(
            "808 holds shortened on {n} notes (max 6 steps, off before the next kick)"
        ));
        let has_kick = e
            .project
            .tracks
            .iter()
            .any(|t| crate::tools_mix::role_of(&t.name, &t.instrument) == "kick");
        for t in e.project.tracks.iter_mut() {
            let role = crate::tools_mix::role_of(&t.name, &t.instrument);
            match role {
                "bass" if has_kick && !t.effects.iter().any(|x| x.type_name() == "sidechain") => {
                    put_fx(
                        t,
                        "pal_duck",
                        effect(
                            json!({"type": "sidechain", "source": "kick", "amount": 0.65, "release_ms": 110.0}),
                        ),
                        false,
                    );
                    moves.push(format!("{}: ducked under the kick (0.65, 110 ms)", t.name));
                }
                "kick" => {
                    put_fx(
                        t,
                        "pal_punch",
                        effect(json!({"type": "transient", "attack": 0.5, "sustain": -0.2})),
                        false,
                    );
                    moves.push(format!("{}: transient attack +0.5", t.name));
                }
                "snare" | "hats" | "cymbal" | "perc" => {
                    let (f, g) = if role == "snare" {
                        (4500.0, 3.5)
                    } else {
                        (7000.0, 4.0)
                    };
                    put_fx(
                        t,
                        "pal_air",
                        effect(
                            json!({"type": "parametric_eq", "bands": [{"kind": "high_shelf", "freq": f, "gain_db": g, "q": 0.7}]}),
                        ),
                        false,
                    );
                    if role == "snare" {
                        put_fx(
                            t,
                            "pal_snap",
                            effect(json!({"type": "transient", "attack": 0.4, "sustain": 0.0})),
                            false,
                        );
                    }
                    t.volume_db += if role == "snare" { 1.0 } else { 1.5 };
                    moves.push(format!("{}: high shelf +{g} dB at {f} Hz", t.name));
                }
                _ => {}
            }
        }
        e.project.ensure_fx_ids();
    }
    json!({"moves": moves, "carve": carve})
}

/// Palette summary for list_palettes.
pub fn describe(p: &Palette) -> Value {
    let voices: BTreeMap<&str, &str> = p.voices.iter().copied().collect();
    let samples: Vec<Value> = p
        .samples
        .iter()
        .map(|(r, id)| {
            let s = sample_entry(id).ok();
            json!({"role": r, "id": id, "tags": s.map(|x| x.tags.clone()), "author": s.map(|x| x.author.clone()), "license": s.map(|x| x.license.clone())})
        })
        .collect();
    json!({
        "name": p.name,
        "description": p.description,
        "genres": p.genres,
        "character": p.character,
        "voices": voices,
        "samples": samples,
        "mix_hint": p.mix_hint,
    })
}

// ------------------------------------------------------------ sha-256

/// SHA-256 (FIPS 180-4) of `data`, lowercase hex. Small and dependency-free;
/// only used to verify downloaded samples.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut a = h;
        for i in 0..64 {
            let s1 = a[4].rotate_right(6) ^ a[4].rotate_right(11) ^ a[4].rotate_right(25);
            let ch = (a[4] & a[5]) ^ (!a[4] & a[6]);
            let t1 = a[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a[0].rotate_right(2) ^ a[0].rotate_right(13) ^ a[0].rotate_right(22);
            let maj = (a[0] & a[1]) ^ (a[0] & a[2]) ^ (a[1] & a[2]);
            let t2 = s0.wrapping_add(maj);
            a[7] = a[6];
            a[6] = a[5];
            a[5] = a[4];
            a[4] = a[3].wrapping_add(t1);
            a[3] = a[2];
            a[2] = a[1];
            a[1] = a[0];
            a[0] = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip(a.iter()) {
            *x = x.wrapping_add(*y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let long = vec![b'a'; 1000];
        assert_eq!(
            sha256_hex(&long),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn every_palette_voice_and_sample_resolves() {
        for p in PALETTES {
            for (role, name) in p.voices {
                assert!(ROLES.contains(role), "{}: bad role {role}", p.name);
                assert!(preset(name).is_some(), "{}: preset {name} missing", p.name);
            }
            for (role, id) in p.samples {
                let s = sample_entry(id).unwrap_or_else(|_| panic!("{}: sample {id}", p.name));
                assert!(ROLES.contains(role), "{role}");
                assert!(s.license.starts_with("CC0"), "{id} license {}", s.license);
            }
            assert!(get(p.name).is_ok());
        }
        assert_eq!(get("trap").unwrap().name, "dark_trap");
        assert_eq!(for_genre("boom_bap").unwrap().name, "boom_bap_dusty");
        assert!(get("nope").is_err());
    }

    #[test]
    fn manifest_is_cc0_pinned_and_checksummed() {
        let m = manifest();
        assert!(m.len() >= 40);
        let mut total = 0;
        let mut ids = std::collections::HashSet::new();
        for s in m {
            assert!(ids.insert(s.id.clone()), "duplicate {}", s.id);
            assert_eq!(s.license, "CC0-1.0", "{}", s.id);
            assert_eq!(s.sha256.len(), 64, "{}", s.id);
            assert!(s.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            // pinned to a commit, never a moving branch
            assert!(
                s.url
                    .split('/')
                    .any(|seg| seg.len() == 40 && seg.chars().all(|c| c.is_ascii_hexdigit())),
                "{} not pinned: {}",
                s.id,
                s.url
            );
            assert!(!s.author.is_empty() && s.source.starts_with("https://"));
            total += s.bytes;
        }
        assert!(total < 10_000_000, "palette samples {total} bytes");
    }

    #[test]
    fn apply_swaps_voices_and_keeps_levels() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_palette_tests"));
        e.call(
            "generate_beat",
            &json!({"style": "trap", "bars": 1, "seed": 2}),
        )
        .unwrap();
        let before: Vec<Instrument> = e
            .project
            .tracks
            .iter()
            .map(|t| t.instrument.clone())
            .collect();
        let r = apply(&mut e, "dark_trap", &ApplyOpts::default()).unwrap();
        let changed = r["changed"].as_array().unwrap();
        assert!(changed.len() >= 3, "{r}");
        let after: Vec<Instrument> = e
            .project
            .tracks
            .iter()
            .map(|t| t.instrument.clone())
            .collect();
        assert_ne!(before, after);
        for c in changed {
            assert!(c["fader_change_db"].as_f64().unwrap().abs() <= 18.0);
        }
        // renders cleanly
        let m = e.mix().unwrap();
        let pk = m
            .left
            .iter()
            .chain(m.right.iter())
            .fold(0.0f32, |a, v| a.max(v.abs()));
        assert!(pk.is_finite() && pk > 0.01);
    }

    #[test]
    fn track_roles() {
        let k = preset("kick").unwrap();
        assert_eq!(track_role("kick", &k), Some("kick"));
        assert_eq!(track_role("tabla", &preset("tabla").unwrap()), None);
        assert_eq!(track_role("808", &preset("808").unwrap()), Some("bass"));
        assert_eq!(
            track_role("open hat", &preset("open_hat").unwrap()),
            Some("open_hat")
        );
        assert_eq!(track_role("piano", &preset("grand_piano").unwrap()), None);
    }
}
