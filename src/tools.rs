//! The tool registry. Each tool = name + description + JSON schema + handler.
//! This list IS the product surface: MCP `tools/list`, the CLI `call`
//! command and the studio's buttons all read from it.

use crate::analysis;
use crate::dsp::Rng;
use crate::engine::Engine;
use crate::fx::{self, Effect};
use crate::instruments::{self, Instrument};
use crate::project::{Note, Pattern, Project, Section, Track};
use crate::render::{self, RenderOptions};
use crate::samples::{self, SampleInfo};
use crate::theory::{self, MelodySpec};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

fn rename_pattern(e: &mut Engine, a: &Value) -> Result<Value> {
    let i = e.project.pattern_index(&s_req(a, "pattern")?)?;
    let new = a
        .get("new_name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("new_name must be a string"))?
        .trim()
        .to_string();
    if new.is_empty() || new.to_lowercase().starts_with(crate::tools_fl::PL) {
        bail!("Pattern name cannot be blank or start with pl:");
    }
    let old = e.project.patterns[i].name.clone();
    let id = e.project.patterns[i].id.clone();
    if old.starts_with(crate::tools_fl::PL) {
        bail!("Generated playlist patterns cannot be renamed");
    }
    if e.project
        .patterns
        .iter()
        .enumerate()
        .any(|(j, p)| j != i && p.name.eq_ignore_ascii_case(&new))
    {
        bail!("Pattern name already exists");
    }
    // A display name must not steal another pattern's stable-ID alias.
    if e.project
        .patterns
        .iter()
        .enumerate()
        .any(|(j, p)| j != i && !p.id.is_empty() && p.id.eq_ignore_ascii_case(&new))
    {
        bail!("Pattern name conflicts with a stable ID");
    }
    let refs = |v: &str| v.trim().eq_ignore_ascii_case(&old) || (!id.is_empty() && v.trim() == id);
    let mut arrangement = 0;
    let mut playlist = 0;
    for section in &mut e.project.arrangement {
        if refs(&section.pattern) {
            section.pattern = new.clone();
            arrangement += 1;
        }
    }
    for clip in &mut e.project.playlist {
        if refs(&clip.pattern) {
            clip.pattern = new.clone();
            playlist += 1;
        }
    }
    e.project.patterns[i].name = new.clone();
    Ok(
        json!({"renamed":old,"name":new,"id":id,"arrangement_references":arrangement,"playlist_references":playlist}),
    )
}

fn duplicate_pattern(e: &mut Engine, a: &Value) -> Result<Value> {
    let i = e.project.pattern_index(&s_req(a, "pattern")?)?;
    let source = &e.project.patterns[i];
    if source.name.to_lowercase().starts_with(crate::tools_fl::PL) {
        bail!("Generated playlist patterns cannot be duplicated; choose a user pattern");
    }
    let unavailable = |name: &str| {
        e.project.patterns.iter().any(|p| {
            p.name.eq_ignore_ascii_case(name)
                || (!p.id.is_empty() && p.id.eq_ignore_ascii_case(name))
        })
    };
    let name = if let Some(value) = a.get("new_name") {
        let name = value
            .as_str()
            .ok_or_else(|| anyhow!("new_name must be a string"))?
            .trim()
            .to_string();
        if name.is_empty() || name.to_lowercase().starts_with(crate::tools_fl::PL) {
            bail!("Pattern name cannot be blank or start with pl:");
        }
        if unavailable(&name) {
            bail!("Pattern name conflicts with an existing pattern name or stable ID");
        }
        name
    } else {
        let base = format!("{} copy", source.name);
        let mut name = base.clone();
        let mut suffix = 2u64;
        while unavailable(&name) {
            name = format!("{base} {suffix}");
            suffix += 1;
        }
        name
    };
    let mut copy = source.clone();
    let source_name = source.name.clone();
    copy.name = name.clone();
    // A duplicate is independent material, never an alias to the source.
    copy.id.clear();
    for notes in copy.clips.values_mut() {
        for note in notes {
            note.id.clear();
        }
    }
    let bars = copy.bars;
    e.project.patterns.push(copy);
    Ok(
        json!({"pattern":name,"copied_from":source_name,"bars":bars,"index":e.project.patterns.len()-1,"placed_in_song":false}),
    )
}

fn set_track_insert_bypass(e: &mut Engine, a: &Value) -> Result<Value> {
    let i = e.project.track_index(&s_req(a, "track")?)?;
    let bypass = a
        .get("bypass")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow::anyhow!("bypass must be true or false"))?;
    let track = &mut e.project.tracks[i];
    let mut changed = 0;
    for effect in &mut track.effects {
        if effect.bypassed() != bypass {
            changed += 1;
            effect.set_bypass(bypass);
        }
    }
    Ok(
        json!({"track":track.name,"id":track.id,"bypassed":bypass,"inserts":track.effects.len(),"changed":changed,
        "restore_previous_mixed_state":"Undo"}),
    )
}

fn offset_track_faders(e: &mut Engine, a: &Value) -> Result<Value> {
    let delta = a
        .get("delta_db")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && (-12.0..=12.0).contains(n))
        .ok_or_else(|| anyhow::anyhow!("delta_db must be a finite number from -12 to 12"))?
        as f32;
    if delta == 0.0 || e.project.tracks.is_empty() {
        return Ok(json!({"delta_db":delta,"changed_tracks":0}));
    }
    for track in &e.project.tracks {
        if e.project
            .automation
            .iter()
            .any(|l| l.enabled && !l.points.is_empty() && l.is_target(&track.name, "volume"))
        {
            bail!("Track '{}' has active volume automation. Edit or disable that lane first; no faders changed",track.name);
        }
        let next = track.volume_db + delta;
        if !next.is_finite() || !(-60.0..=12.0).contains(&next) {
            bail!(
                "Track '{}' would exceed the -60 to +12 dB fader range; no faders changed",
                track.name
            );
        }
    }
    for track in &mut e.project.tracks {
        track.volume_db += delta;
    }
    Ok(
        json!({"delta_db":delta,"changed_tracks":e.project.tracks.len(),"relative_fader_balance_preserved":true,
        "bus_master_unchanged":true}),
    )
}

fn clear_track_solos(e: &mut Engine, _: &Value) -> Result<Value> {
    let mut cleared = Vec::new();
    for track in &mut e.project.tracks {
        if track.solo {
            track.solo = false;
            cleared.push(track.name.clone());
        }
    }
    Ok(json!({"cleared":cleared,"remaining_solos":0,"mutes_preserved":true}))
}

fn solo_track_exclusive(e: &mut Engine, a: &Value) -> Result<Value> {
    let selected = e.project.track_index(&s_req(a, "track")?)?;
    let mut cleared = Vec::new();
    for (i, track) in e.project.tracks.iter_mut().enumerate() {
        if i != selected && track.solo {
            cleared.push(track.name.clone());
        }
        track.solo = i == selected;
    }
    let track = &e.project.tracks[selected];
    Ok(
        json!({"track":track.name,"id":track.id,"solo":true,"track_muted":track.mute,
        "cleared":cleared,"mutes_preserved":true}),
    )
}

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub mutates: bool,
    pub schema: fn() -> Value,
    pub run: fn(&mut Engine, &Value) -> Result<Value>,
}

pub const GUIDE: &str = "Beatbox is a beat-making engine you drive with tools. Time is measured in \
16th-note steps (16 per bar). Pitches accept MIDI numbers or names like C4 (=60), F#2. \
Workflow that produces great beats: (1) set_key + set_tempo (or generate_beat for an instant full \
song in a style); (2) generate_drums or set_steps for drums; (3) generate_bassline (808 for trap/drill); \
(4) generate_chords with a progression like 'i VI III VII'; (5) generate_melody; (6) add_effect: \
reverb/delay on chords & leads, sidechain on bass/pads with source 'kick', eq/filter to carve space; \
(7) add_pattern with copy_from to make intro/break/drop variants, then set_arrangement; \
(8) render, then analyze_mix and apply its suggestions; iterate. Use search_samples + download_sample \
to pull real sounds from Freesound (CC0 by default) or any audio URL, then add_sample_track. \
Mix like a pro: add_bus a 'reverb'/'delay' return and set_send leads/pads to it (-12 dB) instead of \
per-track reverbs; route_track drums into a 'drum' bus for glue. Automation brings sections to life: \
generate_automation a riser on a filter cutoff (fx.<i>.cutoff) before the drop, fade_in/fade_out on \
volume, lfo/pump synced to the tempo; times are song beats (list_automation shows sections and every \
automatable parameter). Before a bold change call snapshot, then diff_project / compare_variants to \
pick the better version (restore_snapshot to go back). Finish with validate_project and check_master \
(true peak <= -1 dBTP, about -14 LUFS for streaming). undo/redo are always available, so experiment freely.";

pub(crate) fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": type_untyped(props), "required": required, "additionalProperties": false })
}

/// Arguments that accept more than one JSON type (a pitch as 60 or "C4", a
/// rate as 4 or "1/16", one track name or a list) get their union type
/// spelled out, so every argument a model sees carries a type.
fn type_untyped(mut props: Value) -> Value {
    if let Value::Object(m) = &mut props {
        for (k, p) in m.iter_mut() {
            let Value::Object(o) = p else { continue };
            if ["type", "enum", "oneOf", "anyOf", "$ref", "const"]
                .iter()
                .any(|f| o.contains_key(*f))
            {
                continue;
            }
            let (ty, desc): (Value, &str) = match k.as_str() {
                "pitch" | "pitch_min" | "pitch_max" | "pitch_from" | "pitch_to" | "key_min"
                | "key_max" | "root" | "register" => (
                    json!(["integer", "string"]),
                    "MIDI number or note name (C4 = 60)",
                ),
                "rate" | "grid" | "every" => (
                    json!(["number", "string"]),
                    "16th steps, or a note value like '1/16', '1/8t', '1/4'",
                ),
                "patterns" | "to_patterns" | "track" => (
                    json!(["string", "array"]),
                    "a name, 'a,b', or a list of names",
                ),
                "sections" => (json!(["array", "string"]), "list of sections"),
                _ => continue,
            };
            o.insert("type".into(), ty);
            if k == "patterns" || k == "to_patterns" || k == "track" || k == "sections" {
                o.entry("items")
                    .or_insert(json!({"type": ["string", "object"]}));
            }
            o.entry("description").or_insert(Value::String(desc.into()));
        }
    }
    props
}

// ---------- argument helpers ----------

pub(crate) fn s_req(a: &Value, k: &str) -> Result<String> {
    a.get(k)
        .and_then(|v| {
            v.as_str()
                .map(String::from)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        })
        .ok_or_else(|| anyhow!("missing required argument '{k}'"))
}
pub(crate) fn s_opt(a: &Value, k: &str) -> Option<String> {
    a.get(k).and_then(|v| {
        v.as_str()
            .map(String::from)
            .or_else(|| v.as_i64().map(|n| n.to_string()))
    })
}
pub(crate) fn f_opt(a: &Value, k: &str) -> Option<f32> {
    a.get(k)
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .map(|x| x as f32)
}
pub(crate) fn f_or(a: &Value, k: &str, d: f32) -> f32 {
    f_opt(a, k).unwrap_or(d)
}
pub(crate) fn u_or(a: &Value, k: &str, d: u64) -> u64 {
    a.get(k)
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
        .unwrap_or(d)
}
pub(crate) fn b_or(a: &Value, k: &str, d: bool) -> bool {
    a.get(k).and_then(|v| v.as_bool()).unwrap_or(d)
}
pub(crate) fn pitch_of(v: &Value) -> Result<u8> {
    if let Some(n) = v.as_f64() {
        return Ok(n.clamp(0.0, 127.0) as u8);
    }
    if let Some(s) = v.as_str() {
        return theory::parse_note(s);
    }
    bail!("pitch must be a MIDI number or a note name like C4")
}
/// The call's seed. Top-level calls that omit it get a fresh one injected by
/// `Engine::call_from` (returned in the result as `seed`); the fallback only
/// applies to nested calls a tool makes without one.
pub(crate) fn seed_of(a: &Value) -> u64 {
    u_or(a, "seed", 0x5EED)
}

thread_local! {
    static CALL_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static SEED_FRESH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Nesting depth of tool calls on this thread (top level = 1).
pub struct CallDepth(u32);

impl CallDepth {
    pub fn enter() -> Self {
        let d = CALL_DEPTH.with(|c| {
            c.set(c.get() + 1);
            c.get()
        });
        CallDepth(d)
    }
    /// Current nesting depth on this thread (0 outside any tool call).
    pub fn current() -> u32 {
        CALL_DEPTH.with(|c| c.get())
    }
    pub fn top(&self) -> bool {
        self.0 == 1
    }
}

impl Drop for CallDepth {
    fn drop(&mut self) {
        CALL_DEPTH.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

pub(crate) fn set_seed_fresh(v: bool) {
    SEED_FRESH.with(|c| c.set(v));
}

/// Whether the current top-level call's seed was injected (not given).
pub(crate) fn seed_was_fresh() -> bool {
    SEED_FRESH.with(|c| c.get())
}

pub(crate) fn pattern_idx(p: &Project, a: &Value) -> Result<usize> {
    match s_opt(a, "pattern") {
        Some(name) => p.pattern_index(&name),
        None => {
            if p.patterns.is_empty() {
                bail!("project has no patterns; call add_pattern")
            }
            Ok(0)
        }
    }
}

pub(crate) fn instrument_from(a: &Value) -> Result<Option<Instrument>> {
    if let Some(inst) = a.get("instrument").filter(|v| v.is_object()) {
        let i: Instrument =
            serde_json::from_value(inst.clone()).context("invalid instrument object")?;
        reject_unknown(inst, &serde_json::to_value(&i)?, "instrument")?;
        return Ok(Some(i));
    }
    if let Some(p) = s_opt(a, "preset") {
        return instruments::preset(&p)
            .map(Some)
            .ok_or_else(|| anyhow!("unknown preset '{p}'. Call list_presets."));
    }
    Ok(None)
}

pub(crate) fn ensure_track(
    p: &mut Project,
    name: &str,
    default_preset: &str,
    override_inst: Option<Instrument>,
) -> Result<String> {
    match p.track_index(name) {
        Ok(i) => {
            if let Some(inst) = override_inst {
                p.tracks[i].instrument = inst;
            }
            Ok(p.tracks[i].name.clone())
        }
        Err(_) => {
            let inst = match override_inst {
                Some(i) => i,
                None => instruments::preset(default_preset)
                    .ok_or_else(|| anyhow!("bad preset {default_preset}"))?,
            };
            let (vol, _, _) = crate::tools_mix::calibrated_volume(
                name,
                &inst,
                &crate::samples::SampleBank::default(),
            );
            p.tracks.push(Track {
                volume_db: vol,
                ..Track::new(name, inst)
            });
            Ok(name.to_string())
        }
    }
}

/// Keys in `patch` that did not survive deserialization into `effective`
/// (serde silently drops unknown fields). Missing keys whose value is a
/// default-looking null/false/""/[] are accepted (skipped when serialized).
pub(crate) fn unknown_keys(patch: &Value, effective: &Value, path: &str, out: &mut Vec<String>) {
    match (patch, effective) {
        (Value::Object(p), Value::Object(e)) => {
            for (k, v) in p {
                if k == "type" {
                    continue;
                }
                let here = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                match e.get(k) {
                    Some(ev) => unknown_keys(v, ev, &here, out),
                    None => {
                        let defaultish = v.is_null()
                            || v == &Value::Bool(false)
                            || v.as_str() == Some("")
                            || v.as_array().is_some_and(|a| a.is_empty());
                        if !defaultish {
                            let mut valid: Vec<&String> = e.keys().collect();
                            valid.sort();
                            out.push(format!(
                                "unknown parameter '{here}' (valid here: {})",
                                valid
                                    .iter()
                                    .map(|s| s.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                    }
                }
            }
        }
        (Value::Array(p), Value::Array(e)) => {
            for (i, (a, b)) in p.iter().zip(e.iter()).enumerate() {
                unknown_keys(a, b, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

/// Error out when a parameter patch contains keys the target type ignores.
pub(crate) fn reject_unknown(patch: &Value, effective: &Value, what: &str) -> Result<()> {
    let mut bad = Vec::new();
    unknown_keys(patch, effective, "", &mut bad);
    if bad.is_empty() {
        Ok(())
    } else {
        bail!("{what}: {}. Nothing was changed. Call describe_effect / describe_instrument for the parameter names.", bad.join("; "))
    }
}

pub(crate) fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                if k == "type" {
                    continue;
                }
                merge(b.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (b, p) => *b = p.clone(),
    }
}

pub(crate) fn effects_of<'a>(p: &'a mut Project, track: &str) -> Result<&'a mut Vec<Effect>> {
    if track.eq_ignore_ascii_case("master") {
        Ok(&mut p.master_effects)
    } else if let Ok(i) = p.track_index(track) {
        Ok(&mut p.tracks[i].effects)
    } else if let Ok(i) = p.bus_index(track) {
        Ok(&mut p.buses[i].effects)
    } else {
        Err(anyhow!(
            "no track or bus '{track}'. Tracks: [{}], buses: [{}], or 'master'",
            p.tracks
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            p.buses
                .iter()
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

pub(crate) fn key_pc(p: &Project) -> u8 {
    theory::pitch_class(&p.key_root).unwrap_or(0)
}

pub(crate) fn notes_json(notes: &[Note]) -> Value {
    Value::Array(
        notes
            .iter()
            .map(|n| json!({"id": n.id, "start": n.start, "len": n.len, "pitch": n.pitch, "note": theory::note_name(n.pitch), "vel": (n.vel * 100.0).round() / 100.0}))
            .collect(),
    )
}

pub(crate) fn grid(notes: &[Note], steps: u32) -> String {
    let mut g: Vec<char> = vec!['.'; steps as usize];
    for n in notes {
        let i = n.start.round() as usize;
        if i < g.len() {
            g[i] = if n.vel >= 0.95 {
                'X'
            } else if n.vel >= 0.6 {
                'x'
            } else {
                'o'
            };
        }
    }
    g.chunks(16)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("|")
}

pub fn summary(p: &Project) -> Value {
    json!({
        "name": p.name,
        "bpm": p.bpm,
        "swing": p.swing,
        "key": format!("{} {}", p.key_root, p.scale),
        "song_seconds": (p.song_seconds() * 10.0).round() / 10.0,
        "tracks": p.tracks.iter().map(|t| json!({
            "name": t.name,
            "instrument": t.instrument.kind_name(),
            "volume_db": t.volume_db,
            "pan": t.pan,
            "mute": t.mute,
            "solo": t.solo,
            "effects": t.effects.iter().map(|e| e.type_name()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "patterns": p.patterns.iter().map(|pt| json!({
            "name": pt.name,
            "bars": pt.bars,
            "notes_per_track": pt.clips.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.clone(), json!(v.len()))).collect::<Map<String, Value>>(),
        })).collect::<Vec<_>>(),
        "arrangement": p.song_sections().iter().map(|s| format!("{} x{}", s.pattern, s.repeats)).collect::<Vec<_>>(),
        "master_effects": p.master_effects.iter().map(|e| e.type_name()).collect::<Vec<_>>(),
        "master_volume_db": p.master_volume_db,
        "samples": p.samples.iter().map(|s| &s.name).collect::<Vec<_>>(),
        "buses": p.buses.iter().map(|b| json!({"name": b.name, "volume_db": b.volume_db, "effects": b.effects.iter().map(|e| e.type_name()).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "routing": p.tracks.iter().filter(|t| t.output.is_some() || !t.sends.is_empty()).map(|t| json!({"track": t.name, "output": t.output.clone().unwrap_or_else(|| "master".into()), "sends": t.sends.iter().map(|s| format!("{} {:+.1} dB", s.bus, s.db)).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "automation": p.automation.iter().map(|l| format!("{}:{} ({} pts)", l.target, l.param, l.points.len())).collect::<Vec<_>>(),
    })
}

fn catalog() -> Value {
    json!({
        "instrument_presets": instruments::PRESETS.iter().map(|(n, d)| json!({"name": n, "description": d})).collect::<Vec<_>>(),
        "instrument_types": {
            "drum": "kind (kick|snare|clap|closed_hat|open_hat|rim|tom|cowbell|shaker|crash), tune, decay, drive",
            "synth": "osc1/osc2 (sine|saw|square|triangle|noise), osc2_semitones, osc2_cents, osc_mix, unison 1-9, unison_spread_cents, sub_level, noise_level, filter_mode, cutoff, resonance, filter_env_amount, filter_env{attack,decay,sustain,release}, amp_env{...}, lfo_rate, lfo_to_cutoff, lfo_to_pitch, pitch_env_semitones, pitch_env_time, drive, gain",
            "fm": "ratio, index, mod_env, amp_env, ratio2, index2, feedback, gain",
            "pluck": "damping, brightness, gain",
            "bass808": "decay, punch, drive, sustain, gain",
            "sampler": "sample, root, one_shot, reverse, start, max_length, attack, release, gain",
        },
        "effects": fx::EFFECT_TYPES.iter().map(|(n, d)| json!({"type": n, "params": d})).collect::<Vec<_>>(),
        "drum_styles": theory::GROOVES.iter().map(|g| json!({"style": g.name, "bpm": g.bpm})).collect::<Vec<_>>(),
        "scales": theory::SCALES.iter().map(|s| s.0).collect::<Vec<_>>(),
        "chord_qualities": theory::CHORD_QUALITIES.iter().map(|s| s.0).collect::<Vec<_>>(),
        "chord_styles": ["block", "stabs", "offbeat", "pulse", "arp_up", "arp_down", "arp_updown"],
        "bass_styles": ["root", "eighths", "octave", "offbeat", "syncopated", "808", "walking"],
        "step_string": "X accent, x hit, o ghost, . rest; | and spaces ignored; repeats to fill the pattern",
    })
}

// ---------- genre profiles for generate_beat ----------

pub(crate) struct Profile {
    pub(crate) progression: &'static str,
    pub(crate) bass_preset: &'static str,
    pub(crate) bass_style: &'static str,
    pub(crate) bass_octave: i32,
    pub(crate) chord_preset: &'static str,
    pub(crate) chord_style: &'static str,
    pub(crate) lead_preset: &'static str,
    pub(crate) lead_density: f32,
    pub(crate) sidechain: bool,
}

pub(crate) fn profile(style: &str) -> Profile {
    let p = |progression,
             bass_preset,
             bass_style,
             bass_octave,
             chord_preset,
             chord_style,
             lead_preset,
             lead_density,
             sidechain| Profile {
        progression,
        bass_preset,
        bass_style,
        bass_octave,
        chord_preset,
        chord_style,
        lead_preset,
        lead_density,
        sidechain,
    };
    match style {
        "house" => p(
            "i VII VI VII",
            "pluck_bass",
            "offbeat",
            2,
            "epiano",
            "stabs",
            "pluck_lead",
            0.35,
            true,
        ),
        "techno" => p(
            "i i VI VII",
            "acid_bass",
            "syncopated",
            2,
            "dark_pad",
            "block",
            "fm_bell",
            0.25,
            true,
        ),
        "trap" => p(
            "i VI III VII",
            "808",
            "808",
            1,
            "warm_pad",
            "block",
            "fm_bell",
            0.4,
            false,
        ),
        "drill" => p(
            "i VI iv V",
            "808",
            "808",
            1,
            "strings",
            "block",
            "koto",
            0.45,
            false,
        ),
        "boom_bap" => p(
            "i7 iv7 VII7 IIImaj7",
            "sub_bass",
            "syncopated",
            2,
            "epiano",
            "block",
            "dark_keys",
            0.3,
            false,
        ),
        "lofi" => p(
            "ii7 V7 Imaj7 vi7",
            "sub_bass",
            "root",
            2,
            "epiano",
            "block",
            "guitar_pluck",
            0.3,
            false,
        ),
        "dnb" => p(
            "i VI III VII",
            "reese_bass",
            "syncopated",
            1,
            "warm_pad",
            "block",
            "supersaw",
            0.3,
            true,
        ),
        "reggaeton" => p(
            "i VI III VII",
            "808",
            "syncopated",
            1,
            "pluck_lead",
            "offbeat",
            "marimba",
            0.4,
            false,
        ),
        "afrobeats" => p(
            "vi IV I V",
            "sub_bass",
            "syncopated",
            2,
            "epiano",
            "stabs",
            "marimba",
            0.45,
            false,
        ),
        "phonk" => p(
            "i i VI VII",
            "808",
            "808",
            1,
            "dark_keys",
            "block",
            "chip_lead",
            0.3,
            false,
        ),
        "desi_hiphop" => p(
            "i VII i VI",
            "808_grit",
            "808",
            1,
            "harmonium",
            "block",
            "sitar",
            0.45,
            false,
        ),
        "garage" => p(
            "i7 VI7 iv7 v7",
            "reese_bass",
            "syncopated",
            2,
            "brass_stab",
            "stabs",
            "pluck_lead",
            0.35,
            true,
        ),
        _ => p(
            "i VI III VII",
            "sub_bass",
            "root",
            2,
            "warm_pad",
            "block",
            "pluck_lead",
            0.35,
            false,
        ),
    }
}

/// Desi character for generate_beat: tabla and dholak tuned to the key,
/// a tanpura drone (root + fifth, above the bass register), meend glides on
/// a bansuri lead and the Indian percussion sat back in the mix.
fn desi_touches(e: &mut Engine, seed: u64, meend: bool) -> Result<()> {
    let kp = key_pc(&e.project) as i32;
    let tabla_pitch = (60 + kp - if kp > 6 { 12 } else { 0 }) as u8;
    let dholak_pitch = (tabla_pitch as i32 - 5).clamp(40, 80) as u8;
    let bass_pitch = (55 + kp - if kp > 8 { 12 } else { 0 }).clamp(40, 80) as u8;
    let mut rng = Rng::new(seed ^ 0xD401);
    let iv = theory::scale_intervals(&e.project.scale)?.to_vec();
    let drone_track = ensure_track(&mut e.project, "tanpura", "tanpura", None)?;
    for pat in e.project.patterns.iter_mut() {
        let steps = pat.steps() as f32;
        for (t, p) in [
            ("tabla", tabla_pitch),
            ("dholak", dholak_pitch),
            ("dholak_bass", bass_pitch),
        ] {
            for n in pat.notes_mut(t).iter_mut() {
                n.pitch = p;
                // open bols on the beat, muted ones between
                if (n.start as u32) % 2 == 1 {
                    n.vel = n.vel.min(0.45);
                }
            }
        }
        // tanpura: Sa + Pa (+ upper Sa), re-struck every 2 bars, kept above ~250 Hz
        let base = 60 + kp - if kp > 4 { 12 } else { 0 };
        let mut d = Vec::new();
        let mut t = 0.0;
        while t < steps {
            for (k, iv) in [0, 7, 12].iter().enumerate() {
                d.push(Note {
                    start: t + k as f32 * 0.5,
                    len: (32.0f32).min(steps - t) - k as f32 * 0.5,
                    pitch: (base + iv) as u8,
                    vel: 0.5,
                    ..Default::default()
                });
            }
            t += 32.0;
        }
        *pat.notes_mut(&drone_track) = d;
        if meend {
            // meend: glide into the next scale note on some held notes
            let lead = pat.notes_mut("lead");
            lead.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
            for i in 0..lead.len().saturating_sub(1) {
                let (a, b) = (lead[i].clone(), lead[i + 1].clone());
                let step = (b.pitch as i32 - a.pitch as i32).abs();
                let pc = ((b.pitch as i32 - kp).rem_euclid(12)) as u8;
                if a.len >= 1.5 && (1..=4).contains(&step) && iv.contains(&pc) && rng.chance(0.45) {
                    lead[i].slide_to = Some(b.pitch);
                }
            }
        }
    }
    for t in e.project.tracks.iter_mut() {
        match t.name.as_str() {
            // the dayan rings at Sa (~280-350 Hz): keep it under the kit
            "tabla" => {
                t.volume_db = -9.0;
                t.pan = -0.2;
            }
            "dholak" => {
                t.volume_db = -10.0;
                t.pan = 0.25;
            }
            "dholak_bass" => t.volume_db = -8.0,
            "tanpura" => t.volume_db = -14.0,
            _ => {}
        }
    }
    Ok(())
}

// ---------- the registry ----------

pub fn registry() -> &'static [Tool] {
    static R: OnceLock<Vec<Tool>> = OnceLock::new();
    R.get_or_init(build)
}

pub fn find(name: &str) -> Option<&'static Tool> {
    registry().iter().find(|t| t.name == name)
}

pub fn tools_json() -> Value {
    Value::Array(
        registry()
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": (t.schema)()}))
            .collect(),
    )
}

pub(crate) fn pattern_prop() -> Value {
    json!({"type": "string", "description": "Pattern name (default: first pattern)"})
}

fn build() -> Vec<Tool> {
    let mut v = core_tools();
    v.extend(crate::tools_studio::tools());
    v.extend(crate::tools_midi::tools());
    v.extend(crate::tools_sound::tools());
    v.extend(crate::tools_compose::tools());
    v.extend(crate::tools_ears::tools());
    v.extend(crate::tools_tp_limit::tools());
    v.extend(crate::tools_delivery::tools());
    v.extend(crate::judge::tools());
    v.extend(crate::tools_mix::tools());
    v.extend(crate::tools_multiband::tools());
    v.extend(crate::tools_parity::tools());
    v.extend(crate::midi_in::tools());
    v.extend(crate::tools_producer::tools());
    v.extend(crate::tools_creative::tools());
    v.extend(crate::tools_listen::tools());
    v.extend(crate::tools_mix_analysis::tools());
    v.extend(crate::tools_mix_primitives::tools());
    v.extend(crate::tools_ears_pro::tools());
    v.extend(crate::tools_fl::tools());
    v.extend(crate::tools_fx2::tools());
    v.extend(crate::tools_palette::tools());
    v.extend(crate::tools_vocal::tools());
    v.extend(crate::tools_groove::tools());
    v.extend(crate::tools_prompt::tools());
    v.extend(crate::tools_meta::tools());
    v.extend(crate::tools_drone::tools());
    v.extend(crate::tools_critic::tools());
    v.extend(crate::tools_chain::tools());
    v.extend(crate::tools_ids::tools());
    v.extend(crate::tools_timeline::tools());
    v.extend(crate::tools_ab_listen::tools());
    v.extend(crate::tools_cover::tools());
    v.extend(crate::tools_discovery::tools());
    v.extend(crate::tools_bounce::tools());
    v.extend(crate::tools_click::tools());
    v.extend(crate::tools_markers::tools());
    v.extend(crate::tools_tempo::tools());
    v.extend(crate::stems::tools());
    v.extend(crate::tools_transcribe::tools());
    v.extend(crate::record_map::tools());
    v.extend(crate::timbre_match::tools());
    v.extend(crate::arrange_tx::tools());
    v.extend(crate::resynth::tools());
    v.extend(crate::rack::cmd::tools());
    v.extend(crate::vocal_flex::tools());
    v.extend(crate::vocal_repair::tools());
    v.extend(crate::vocal_clarity::tools());
    v.extend(crate::vocal_edits::tools());
    v.extend(crate::vocal_sections::tools());
    v.extend(crate::sc_strip::tools());
    v.extend(crate::vocal_style::tools());
    v.extend(crate::drill_engine::tools());
    v.extend(crate::voice_ai::tools());
    v.extend(crate::tools_library::tools());
    v.extend(crate::song::tools());
    v.extend(crate::recorder::tools());
    v.extend(crate::sound_catalog::tools());
    v.extend(crate::web_import::tools());
    v
}

pub(crate) fn core_tools() -> Vec<Tool> {
    vec![
        // ----- discovery -----
        Tool {
            name: "get_guide",
            description: "START HERE. How to make great beats with this engine, plus every preset, effect, scale, chord style and drum style available.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |_, _| Ok(json!({"guide": GUIDE, "catalog": catalog()})),
        },
        Tool {
            name: "list_presets",
            description: "List instrument presets, instrument parameter schemas, effect types, drum styles, scales and chord/bass styles.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |_, _| Ok(catalog()),
        },
        Tool {
            name: "list_tools",
            description: "Every tool, grouped by category (start, discover, project, notes, sound, compose, listen, mix, fx, playlist, produce, vocal, export...), one line each. Filter with category or search. Then describe_tool {name} for arguments and an example.",
            mutates: false,
            schema: || obj(json!({
                "category": {"type": "string", "description": "Only this category, e.g. vocal, fx, mix, notes"},
                "search": {"type": "string", "description": "Only tools whose name or description contains this word"}
            }), &[]),
            run: |_, a| crate::tools_meta::list(a),
        },
        // ----- project -----
        Tool {
            name: "new_project",
            description: "Start a fresh project (master compressor + limiter). patterns=[{name, bars}] creates your sections up front (default one empty 4-bar pattern 'A').",
            mutates: true,
            schema: || obj(json!({
                "patterns": {"type": "array", "items": {"type": "object", "properties": {"name": {"type": "string"}, "bars": {"type": "integer"}}}},
                "name": {"type": "string"},
                "bpm": {"type": "number", "minimum": 40, "maximum": 300},
                "key": {"type": "string", "description": "Root note, e.g. C, F#, Bb"},
                "scale": {"type": "string", "description": "e.g. minor, major, dorian, phrygian, harmonic_minor"}
            }), &[]),
            run: |e, a| {
                let mut p = Project::new(&s_opt(a, "name").unwrap_or_else(|| "untitled".into()), f_or(a, "bpm", 120.0));
                if let Some(k) = s_opt(a, "key") {
                    theory::pitch_class(&k)?;
                    p.key_root = k;
                }
                if let Some(s) = s_opt(a, "scale") {
                    theory::scale_intervals(&s)?;
                    p.scale = s;
                }
                if let Some(Value::Array(ps)) = a.get("patterns") {
                    let mut v = Vec::new();
                    for x in ps {
                        let n = x.get("name").and_then(|n| n.as_str()).ok_or_else(|| anyhow!("pattern needs a name"))?;
                        if v.iter().any(|q: &Pattern| q.name.eq_ignore_ascii_case(n)) { bail!("duplicate pattern '{n}'"); }
                        v.push(Pattern::new(n, x.get("bars").and_then(|b| b.as_u64()).unwrap_or(4) as u32));
                    }
                    if !v.is_empty() { p.patterns = v; }
                }
                e.project = p;
                Ok(summary(&e.project))
            },
        },
        Tool {
            name: "get_project",
            description: "Get the current project. detail='summary' (default) or 'full' for the complete JSON including every note and parameter.",
            mutates: false,
            schema: || obj(json!({"detail": {"type": "string", "enum": ["summary", "full"]}}), &[]),
            run: |e, a| {
                if s_opt(a, "detail").as_deref() == Some("full") {
                    Ok(serde_json::to_value(&e.project)?)
                } else {
                    Ok(summary(&e.project))
                }
            },
        },
        Tool {
            name: "save_project",
            description: "Save the project as JSON (with the MIDI controller setup: links, keys track, record grid). Snapshots (A/B versions) are saved alongside in <path>.snapshots.json and come back with load_project, so compare_variants works across sessions.",
            mutates: false,
            schema: || obj(json!({"path": {"type": "string", "description": "File path, e.g. mybeat.beatbox.json"}}), &["path"]),
            run: |e, a| {
                let path = e.resolve(&s_req(a, "path")?);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                e.project.midi = crate::midi_in::snapshot(e);
                std::fs::write(&path, serde_json::to_string_pretty(&e.project)?)?;
                let side = snapshots_path(&path);
                if e.snapshots.is_empty() {
                    let _ = std::fs::remove_file(&side);
                } else {
                    let snaps: Vec<Value> = e.snapshots.iter().map(|(n, s)| json!({"name": n, "note": s.note, "revision": s.revision, "project": s.project})).collect();
                    std::fs::write(&side, serde_json::to_string(&snaps)?)?;
                }
                Ok(json!({"saved": path, "snapshots": e.snapshots.len(), "midi_links": e.project.midi.as_ref().map_or(0, |m| m.links.len())}))
            },
        },
        Tool {
            name: "load_project",
            description: "Load a project JSON file (replaces the current project; undo restores it). Its saved MIDI links and keys track come back; a connected controller stays connected. Replaces session snapshots with this file's valid companion snapshots (none if missing/unreadable/malformed), never retains snapshots from the previous song. A failed main-project read/validation keeps the current project and snapshots intact.",
            mutates: false,
            schema: || obj(json!({"path": {"type": "string"}}), &["path"]),
            run: |e, a| {
                let path = e.resolve(&s_req(a, "path")?);
                let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
                let mut p: Project = serde_json::from_str(&text).context("not a beatbox project")?;
                p.validate_effects().map_err(|m| anyhow!("load_project: {m}. Nothing was changed."))?;
                p.sanitize_timing();
                e.replace_project(p);
                let saved_midi = e.project.midi.clone();
                let (midi_links, midi_skipped) = crate::midi_in::restore(e, saved_midi.as_ref());
                // A successfully opened song owns its own checkpoints. Never
                // leave snapshots from the previous project in the picker.
                e.snapshots.clear();
                let mut loaded = 0;
                let mut snapshots_skipped = 0;
                if let Ok(t) = std::fs::read_to_string(snapshots_path(&path)) {
                    if let Ok(Value::Array(v)) = serde_json::from_str::<Value>(&t) {
                        e.snapshots.clear();
                        for s in v {
                            if let (Some(n), Ok(mut pr)) = (s["name"].as_str(), serde_json::from_value::<Project>(s["project"].clone())) {
                                if pr.validate_effects().is_err() {
                                    snapshots_skipped += 1;
                                    continue;
                                }
                                pr.sanitize_timing();
                                e.snapshots.push((n.to_string(), crate::engine::Snapshot { project: pr, note: s["note"].as_str().unwrap_or("").to_string(), revision: s["revision"].as_u64().unwrap_or(0) }));
                                loaded += 1;
                            }
                        }
                    }
                }
                let mut out = summary(&e.project);
                out["snapshots_loaded"] = json!(loaded);
                if snapshots_skipped > 0 {
                    out["snapshots_skipped_invalid_effects"] = json!(snapshots_skipped);
                }
                if saved_midi.is_some() {
                    out["midi"] = json!({"links": midi_links, "track": e.midi.track, "skipped": midi_skipped});
                }
                let missing = render::missing_clip_samples(&e.project, &e.bank);
                if !missing.is_empty() {
                    out["warnings"] = json!(missing.iter().map(|m| format!("audio clip {m}; renders fail until it is fixed")).collect::<Vec<_>>());
                }
                Ok(out)
            },
        },
        Tool {
            name: "set_tempo",
            description: "Set BPM and/or swing (0 straight .. 1 heavy shuffle on off-beat 16ths).",
            mutates: true,
            schema: || obj(json!({"bpm": {"type": "number"}, "swing": {"type": "number", "minimum": 0, "maximum": 1}}), &[]),
            run: |e, a| {
                if let Some(b) = f_opt(a, "bpm") {
                    e.project.bpm = b.clamp(20.0, 400.0);
                }
                if let Some(s) = f_opt(a, "swing") {
                    e.project.swing = s.clamp(0.0, 1.0);
                }
                Ok(json!({"bpm": e.project.bpm, "swing": e.project.swing}))
            },
        },
        Tool {
            name: "set_key",
            description: "Set the song key used by roman-numeral progressions and generators.",
            mutates: true,
            schema: || obj(json!({"root": {"type": "string"}, "scale": {"type": "string"}}), &["root"]),
            run: |e, a| {
                let r = s_req(a, "root")?;
                theory::pitch_class(&r)?;
                e.project.key_root = r;
                if let Some(s) = s_opt(a, "scale") {
                    theory::scale_intervals(&s)?;
                    e.project.scale = s;
                }
                Ok(json!({"key": format!("{} {}", e.project.key_root, e.project.scale)}))
            },
        },
        Tool {
            name: "undo",
            description: "Undo the last change, or the last `steps` changes.",
            mutates: false,
            schema: || obj(json!({"steps": {"type": "integer", "minimum": 1, "maximum": 100, "description": "How many changes to undo (default 1)."}}), &[]),
            run: |e, a| {
                let n = a.get("steps").and_then(|v| v.as_u64()).unwrap_or(1).clamp(1, 100);
                let mut done = 0;
                while done < n && e.undo() {
                    done += 1;
                }
                Ok(json!({"undone": done > 0, "steps": done, "remaining": e.undo_depth().0}))
            },
        },
        Tool {
            name: "redo",
            description: "Redo the last undone change, or the next `steps` undone changes.",
            mutates: false,
            schema: || obj(json!({"steps": {"type": "integer", "minimum": 1, "maximum": 100, "description": "How many undone changes to redo (default 1)."}}), &[]),
            run: |e, a| {
                let n = a.get("steps").and_then(|v| v.as_u64()).unwrap_or(1).clamp(1, 100);
                let mut done = 0;
                while done < n && e.redo() {
                    done += 1;
                }
                Ok(json!({"redone": done > 0, "steps": done, "remaining": e.undo_depth().1}))
            },
        },
        Tool {
            name: "history",
            description: "The change history in plain words (\"Added clip · Drums · bar 5\"): `done` lists changes undo can take back, newest first; `undone` lists changes redo can bring back, the next redo last. undo/redo with `steps` jump to any line.",
            mutates: false,
            schema: || obj(json!({"limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Most lines per list (default 20)."}}), &[]),
            run: |e, a| {
                let n = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).clamp(1, 100) as usize;
                let (done, undone) = e.history();
                let ul = undone.len();
                let undone: Vec<String> = undone.into_iter().skip(ul.saturating_sub(n)).collect();
                Ok(json!({"done": done.into_iter().take(n).collect::<Vec<_>>(), "undone": undone, "can_undo": e.undo_depth().0, "can_redo": e.undo_depth().1}))
            },
        },
        // ----- tracks -----
        Tool {
            name: "add_track",
            description: "Add a track with an instrument preset (see list_presets) or a full instrument object {type: synth|fm|drum|pluck|bass808|sampler, ...params}. Without volume_db the fader is CALIBRATED so the preset sits at its role level (kick, bass, snare, hats, lead, keys, pad, fx); level_hints checks levels without rendering.",
            mutates: true,
            schema: || obj(json!({
                "name": {"type": "string"},
                "preset": {"type": "string"},
                "instrument": {"type": "object"},
                "volume_db": {"type": "number"},
                "pan": {"type": "number", "minimum": -1, "maximum": 1}
            }), &["name"]),
            run: |e, a| {
                let name = s_req(a, "name")?;
                if e.project.track_index(&name).is_ok() {
                    bail!("track '{name}' already exists");
                }
                let inst = instrument_from(a)?.unwrap_or_else(|| instruments::preset(&name).unwrap_or_else(|| instruments::preset("pluck_lead").unwrap()));
                if e.project.bus_index(&name).is_ok() || name.eq_ignore_ascii_case("master") {
                    bail!("'{name}' is already used by a bus/master; pick another track name");
                }
                // calibrated start: the fader puts the preset at its role's level
                let (cal, role, _) = crate::tools_mix::calibrated_volume(&name, &inst, &e.bank);
                let vol = f_opt(a, "volume_db").unwrap_or(cal);
                e.project.tracks.push(Track {
                    volume_db: vol,
                    pan: f_or(a, "pan", 0.0).clamp(-1.0, 1.0),
                    ..Track::new(&name, inst)
                });
                Ok(json!({"added": name, "tracks": e.project.tracks.len(), "role": role, "volume_db": vol, "calibrated": f_opt(a, "volume_db").is_none()}))
            },
        },
        Tool {
            name: "remove_track",
            description: "Delete a track and its notes in every pattern.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}}), &["track"]),
            run: |e, a| {
                let i = e.project.track_index(&s_req(a, "track")?)?;
                let t = e.project.tracks.remove(i);
                for p in e.project.patterns.iter_mut() {
                    p.clips.remove(&t.name.to_lowercase());
                }
                let lanes = crate::tools_studio::drop_owner_controls(&mut e.project, &t.name, &t.id);
                Ok(json!({"removed": t.name, "automation_lanes_removed": lanes}))
            },
        },
        Tool {
            name: "rename_track",
            description: "Rename a channel. Notes, automation, playlist clips, macros and sidechain sources that point at it follow the new name. {track, name}.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "name": {"type": "string"}}), &["track", "name"]),
            run: |e, a| {
                let i = e.project.track_index(&s_req(a, "track")?)?;
                let new = s_req(a, "name")?;
                let old = e.project.rename_track(i, &new)?;
                Ok(json!({"renamed": old, "name": e.project.tracks[i].name}))
            },
        },
        Tool {
            name: "set_instrument",
            description: "Replace a track's instrument with a preset or instrument object.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "preset": {"type": "string"}, "instrument": {"type": "object"}}), &["track"]),
            run: |e, a| {
                let i = e.project.track_index(&s_req(a, "track")?)?;
                let inst = instrument_from(a)?.ok_or_else(|| anyhow!("give a preset or instrument"))?;
                e.project.tracks[i].instrument = inst;
                Ok(serde_json::to_value(&e.project.tracks[i].instrument)?)
            },
        },
        Tool {
            name: "tweak_instrument",
            description: "Change some instrument parameters, keeping the rest. E.g. {track:'bass', params:{cutoff:300, resonance:0.8, amp_env:{release:0.4}}}. Returns the full instrument.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "params": {"type": "object"}}), &["track", "params"]),
            run: |e, a| {
                let i = e.project.track_index(&s_req(a, "track")?)?;
                let mut v = serde_json::to_value(&e.project.tracks[i].instrument)?;
                let patch = a.get("params").ok_or_else(|| anyhow!("missing params"))?;
                merge(&mut v, patch);
                e.project.tracks[i].instrument = serde_json::from_value(v).context("invalid parameter value")?;
                let eff = serde_json::to_value(&e.project.tracks[i].instrument)?;
                reject_unknown(patch, &eff, "tweak_instrument")?;
                Ok(eff)
            },
        },
        Tool {
            name: "solo_track_exclusive",
            description: "Solo exactly one track (name or stable ID), clearing every other track solo flag in one Undo step. Preserves track/bus mutes and all faders, pan, routing, sends, effects, automation and material/IDs. A muted selected track remains muted; this is not an unmute action or a bus solo. Missing track rejects atomically. Already exclusive is a no-op with no history entry.",
            mutates: true,
            schema: || obj(json!({"track":{"type":"string"}}), &["track"]),
            run: solo_track_exclusive,
        },
        Tool {
            name: "set_track_insert_bypass",
            description: "Set bypass on EVERY insert of one track, by name or stable ID, in one Undo step. bypass=true bypasses all; false enables ALL, including previously bypassed inserts, and is NOT a restore of a previous mixed state. Use Undo for exact restoration. Keeps insert IDs/order/settings, track fader/pan/mute/solo, sends/routing, automation, other tracks and bus/master chains unchanged. Track-only; rejects buses/master/missing track and invalid bypass atomically. Empty/already-matching chain is a no-op. Bypass can change loudness.",
            mutates: true,
            schema: || obj(json!({"track":{"type":"string"},"bypass":{"type":"boolean"}}), &["track","bypass"]),
            run: set_track_insert_bypass,
        },
        Tool {
            name: "offset_track_faders",
            description: "Move ALL track faders by the same delta_db (-12 to 12), preserving their relative fader balance in one Undo step. Includes muted/soloed tracks. Bus/master faders, pan, mutes/solos, sends, routing, effects, automation and notes unchanged. Rejects the WHOLE edit if a track has active nonempty volume automation or any fader would exceed -60 to +12 dB; never clamps individual tracks. This is a fader edit, not a guaranteed output-level or quality change with nonlinear effects/routing. Zero delta or no tracks is a no-op.",
            mutates: true,
            schema: || obj(json!({"delta_db":{"type":"number","minimum":-12,"maximum":12}}), &["delta_db"]),
            run: offset_track_faders,
        },
        Tool {
            name: "clear_track_solos",
            description: "Clear every track's solo flag in one Undo step, so hidden/offscreen solos do not isolate the mix. Preserves track and bus mutes, faders, pan, sends/routing, effects, automation and all note material/IDs. Does not guarantee all tracks will sound: muted tracks/buses stay muted. A no-op creates no history entry.",
            mutates: true,
            schema: || obj(json!({}), &[]),
            run: clear_track_solos,
        },
        Tool {
            name: "set_mixer",
            description: "Set a track's (or bus') volume_db, pan (-1..1), mute and solo. Use track='master' for the master volume.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string"},
                "volume_db": {"type": "number"},
                "pan": {"type": "number"},
                "mute": {"type": "boolean"},
                "solo": {"type": "boolean"}
            }), &["track"]),
            run: |e, a| {
                let name = s_req(a, "track")?;
                if name.eq_ignore_ascii_case("master") {
                    if let Some(v) = f_opt(a, "volume_db") {
                        e.project.master_volume_db = v.clamp(-60.0, 12.0);
                    }
                    return Ok(json!({"master_volume_db": e.project.master_volume_db}));
                }
                if e.project.track_index(&name).is_err() {
                    if let Ok(bi) = e.project.bus_index(&name) {
                        let b = &mut e.project.buses[bi];
                        if let Some(v) = f_opt(a, "volume_db") {
                            b.volume_db = v.clamp(-60.0, 12.0);
                        }
                        if let Some(v) = f_opt(a, "pan") {
                            b.pan = v.clamp(-1.0, 1.0);
                        }
                        if let Some(v) = a.get("mute").and_then(|v| v.as_bool()) {
                            b.mute = v;
                        }
                        return Ok(json!({"bus": b.name, "volume_db": b.volume_db, "pan": b.pan, "mute": b.mute}));
                    }
                }
                let i = e.project.track_index(&name)?;
                let t = &mut e.project.tracks[i];
                if let Some(v) = f_opt(a, "volume_db") {
                    t.volume_db = v.clamp(-60.0, 12.0);
                }
                if let Some(v) = f_opt(a, "pan") {
                    t.pan = v.clamp(-1.0, 1.0);
                }
                if let Some(v) = a.get("mute").and_then(|v| v.as_bool()) {
                    t.mute = v;
                }
                if let Some(v) = a.get("solo").and_then(|v| v.as_bool()) {
                    t.solo = v;
                }
                let mut out = json!({"track": t.name, "volume_db": t.volume_db, "pan": t.pan, "mute": t.mute, "solo": t.solo});
                let tname = t.name.clone();
                let mut warn = Vec::new();
                for (k, param) in [("volume_db", "volume"), ("pan", "pan")] {
                    if a.get(k).is_some() && e.project.automation.iter().any(|l| l.enabled && !l.points.is_empty() && l.is_target(&tname, param)) {
                        warn.push(format!("automation on '{param}' overrides this fader: edit the lane (set_automation_points / clear_automation {{track: '{tname}', param: '{param}'}}) instead"));
                    }
                }
                if !warn.is_empty() { out["warnings"] = json!(warn); }
                Ok(out)
            },
        },
        // ----- effects -----
        Tool {
            name: "add_effect",
            description: "Append an effect to a track's chain (or 'master'). Give `type` plus any params, e.g. {track:'pad', type:'reverb', params:{size:0.85, mix:0.35}} or {track:'bass', type:'sidechain', params:{source:'kick', amount:0.8}}. parametric_eq bands: {kind, freq, gain_db, q, stages} with kind bell | low_shelf | high_shelf | low_cut (= high-pass) | high_cut (= low-pass) | notch, e.g. {type:'parametric_eq', params:{bands:[{kind:'low_cut', freq:100}, {kind:'bell', freq:3200, gain_db:3}]}}. describe_effect lists every type's params.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string", "description": "Track name or 'master'"},
                "type": {"type": "string", "enum": fx::EFFECT_TYPES.iter().map(|e| e.0).collect::<Vec<_>>()},
                "params": {"type": "object"}
            }), &["track", "type"]),
            run: |e, a| {
                let t = s_req(a, "type")?;
                let mut v = json!({"type": t.to_lowercase()});
                if let Some(p) = a.get("params") {
                    merge(&mut v, p);
                }
                let fx: Effect = serde_json::from_value(v).with_context(|| format!("bad effect '{t}'"))?;
                let effective = serde_json::to_value(&fx)?;
                if let Some(p) = a.get("params") {
                    reject_unknown(p, &effective, &format!("add_effect {t}"))?;
                }
                fx.validate().map_err(|m| anyhow!("add_effect {t}: {m}. Nothing was changed."))?;
                let track = s_req(a, "track")?;
                if let Effect::Sidechain(sc) = &fx {
                    e.project.track_index(&sc.source).context("sidechain source track")?;
                }
                let chain = effects_of(&mut e.project, &track)?;
                chain.push(fx);
                Ok(json!({"track": track, "index": chain.len() - 1, "chain": chain.iter().map(|x| x.type_name()).collect::<Vec<_>>(), "effective": effective}))
            },
        },
        Tool {
            name: "tweak_effect",
            description: "Change parameters of an effect at `index` in a track's chain (or 'master').",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "index": {"type": ["integer", "string"], "description": "Effect position (0-based) or its stable id, e.g. \"reverb1\" (see get_effects)"}, "params": {"type": "object"}}), &["track", "index", "params"]),
            run: |e, a| {
                let track = s_req(a, "track")?;
                let idx = u_or(a, "index", 0) as usize;
                let chain = effects_of(&mut e.project, &track)?;
                let fx = chain.get_mut(idx).ok_or_else(|| anyhow!("no effect at index {idx}"))?;
                let mut v = serde_json::to_value(&*fx)?;
                let patch = a.get("params").ok_or_else(|| anyhow!("missing params"))?;
                merge(&mut v, patch);
                let new_fx: Effect = serde_json::from_value(v)?;
                let eff = serde_json::to_value(&new_fx)?;
                reject_unknown(patch, &eff, "tweak_effect")?;
                new_fx.validate().map_err(|m| anyhow!("tweak_effect: {m}. Nothing was changed."))?;
                *fx = new_fx;
                Ok(eff)
            },
        },
        Tool {
            name: "remove_effect",
            description: "Remove the effect at `index` from a track's chain (or 'master').",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "index": {"type": ["integer", "string"], "description": "Effect position (0-based) or its stable id, e.g. \"reverb1\" (see get_effects)"}}), &["track", "index"]),
            run: |e, a| {
                let track = s_req(a, "track")?;
                let idx = u_or(a, "index", 0) as usize;
                let chain = effects_of(&mut e.project, &track)?;
                if idx >= chain.len() {
                    bail!("no effect at index {idx}");
                }
                let fx = chain.remove(idx);
                let names = chain.iter().map(|x| x.type_name()).collect::<Vec<_>>();
                let lanes = crate::tools_studio::reindex_fx_lanes(&mut e.project, &track, idx);
                Ok(json!({"removed": fx.type_name(), "chain": names, "automation_lanes_removed": lanes}))
            },
        },
        Tool {
            name: "get_effects",
            description: "Show the full effect chain (with parameters) of a track or 'master'.",
            mutates: false,
            schema: || obj(json!({"track": {"type": "string"}}), &["track"]),
            run: |e, a| {
                let track = s_req(a, "track")?;
                let chain = effects_of(&mut e.project, &track)?;
                Ok(serde_json::to_value(&*chain)?)
            },
        },
        // ----- patterns & notes -----
        Tool {
            name: "add_pattern",
            description: "Create a pattern (a loopable section, 1-64 bars). copy_from duplicates an existing pattern so you can make variations (intro, break, drop).",
            mutates: true,
            schema: || obj(json!({"name": {"type": "string"}, "bars": {"type": "integer"}, "copy_from": {"type": "string"}}), &["name"]),
            run: |e, a| {
                let name = s_req(a, "name")?;
                if e.project.pattern_index(&name).is_ok() {
                    bail!("pattern '{name}' already exists");
                }
                let mut pat = match s_opt(a, "copy_from") {
                    Some(src) => {
                        let mut p = e.project.patterns[e.project.pattern_index(&src)?].clone();
                        p.name = name.clone();
                        p
                    }
                    None => Pattern::new(&name, 4),
                };
                if let Some(b) = a.get("bars").and_then(|v| v.as_u64()) {
                    pat.bars = (b as u32).clamp(1, 64);
                }
                let bars = pat.bars;
                e.project.patterns.push(pat);
                Ok(json!({"pattern": name, "bars": bars}))
            },
        },
        Tool {
            name: "duplicate_pattern",
            description: "Make an independent copy of a user pattern with identical notes, velocities, timing, length and meter, and fresh pattern/note IDs. Optional new_name is trimmed and must not conflict with another pattern or stable ID; otherwise choose 'name copy', 'name copy 2', etc. Does not place the copy in the song or change arrangement, playlist, markers, automation or audio clips. One Undo removes the copy.",
            mutates: true,
            schema: || obj(json!({"pattern":{"type":"string"},"new_name":{"type":"string"}}), &["pattern"]),
            run: duplicate_pattern,
        },
        Tool {
            name: "rename_pattern",
            description: "Rename one user pattern by name or stable ID, preserving its identity, notes and audio. Updates arrangement and playlist references, including stable-ID references. Rejects blank, conflicting or reserved pl: names and generated playlist patterns. Does not rename markers or historical generation records. Undo restores the name and references.",
            mutates: true,
            schema: || obj(json!({"pattern":{"type":"string"},"new_name":{"type":"string"}}), &["pattern","new_name"]),
            run: rename_pattern,
        },
        Tool {
            name: "remove_pattern",
            description: "Delete a pattern (also removes it from the arrangement). Accepts `pattern` or `name`.",
            mutates: true,
            schema: || obj(json!({"pattern": {"type": "string"}, "name": {"type": "string"}}), &[]),
            run: |e, a| {
                let key = s_opt(a, "pattern").or_else(|| s_opt(a, "name")).ok_or_else(|| anyhow!("missing required argument 'pattern'"))?;
                let i = e.project.pattern_index(&key)?;
                if e.project.patterns.len() == 1 {
                    bail!("can't remove the only pattern");
                }
                let p = e.project.patterns.remove(i);
                e.project.arrangement.retain(|s| !s.pattern.eq_ignore_ascii_case(&p.name));
                Ok(json!({"removed": p.name}))
            },
        },
        Tool {
            name: "set_pattern_length",
            description: "Change how many bars a pattern has (notes past the end are dropped).",
            mutates: true,
            schema: || obj(json!({"pattern": pattern_prop(), "bars": {"type": "integer"}}), &["bars"]),
            run: |e, a| {
                let i = pattern_idx(&e.project, a)?;
                let p = &mut e.project.patterns[i];
                p.bars = (u_or(a, "bars", 4) as u32).clamp(1, 64);
                let steps = p.steps() as f32;
                for v in p.clips.values_mut() {
                    v.retain(|n| n.start < steps);
                }
                Ok(json!({"pattern": p.name, "bars": p.bars}))
            },
        },
        Tool {
            name: "set_steps",
            description: "Program a drum (or any) track with a step string: X accent, x hit, o ghost, . rest. A 16-char string repeats every bar. E.g. {track:'kick', steps:'X...X...X...X...'}. Replaces the track's notes in that pattern.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string"},
                "steps": {"type": "string"},
                "pattern": pattern_prop(),
                "pitch": {"description": "Note for every hit (default 60 for drums / C4)"},
                "repeat": {"type": "boolean", "description": "Repeat the string to fill the pattern (default true)"},
                "note_len": {"type": "number", "description": "Length of each hit in steps (default 1)"}
            }), &["track", "steps"]),
            run: |e, a| {
                let track = e.project.tracks[e.project.track_index(&s_req(a, "track")?)?].name.clone();
                let pitch = a.get("pitch").map(pitch_of).transpose()?.unwrap_or(60);
                let pi = pattern_idx(&e.project, a)?;
                let steps = theory::parse_steps(&s_req(a, "steps")?);
                let pat = &mut e.project.patterns[pi];
                let total = pat.steps();
                let notes = theory::steps_to_notes(&steps, total, b_or(a, "repeat", true), pitch, f_or(a, "note_len", 1.0));
                let count = notes.len();
                *pat.notes_mut(&track) = notes;
                Ok(json!({"track": track, "pattern": pat.name, "hits": count, "grid": grid(pat.notes(&track), total)}))
            },
        },
        Tool {
            name: "toggle_step",
            description: "Toggle a single step on/off for a track (what clicking a pad does in the studio).",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "step": {"type": "integer"}, "pattern": pattern_prop(), "vel": {"type": "number"}, "pitch": {}}), &["track", "step"]),
            run: |e, a| {
                let track = e.project.tracks[e.project.track_index(&s_req(a, "track")?)?].name.clone();
                let pi = pattern_idx(&e.project, a)?;
                let step = u_or(a, "step", 0) as f32;
                let pitch = a.get("pitch").map(pitch_of).transpose()?.unwrap_or(60);
                let vel = f_or(a, "vel", 0.85);
                let pat = &mut e.project.patterns[pi];
                if step >= pat.steps() as f32 {
                    bail!("step {step} past end of pattern ({} steps)", pat.steps());
                }
                let notes = pat.notes_mut(&track);
                let before = notes.len();
                notes.retain(|n| !(n.start >= step && n.start < step + 1.0));
                let on = notes.len() == before;
                if on {
                    notes.push(Note::new(step, 1.0, pitch, vel));
                    notes.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
                }
                Ok(json!({"track": track, "step": step, "on": on}))
            },
        },
        Tool {
            name: "add_notes",
            description: "Write notes into a track: notes=[{start (steps), len (steps, default 1), pitch (MIDI or 'C4'), vel (0-1, default 0.8), prob (0-1 chance per pass, default 1), offset (microtiming in steps -0.5..0.5), slide_to (pitch the note glides into by its end: 808 / synth slides, glide time = instrument glide_ms)}]. Starts may be fractional (12.5 = a 32nd after step 12) (aliases: duration, velocity 0-127). replace=true clears the track in that pattern first. Use this to hand-write melodies, chords, basslines or drum hits.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string"},
                "pattern": pattern_prop(),
                "replace": {"type": "boolean"},
                "notes": {"type": "array", "items": {"type": "object", "properties": {
                    "start": {"type": "number"}, "len": {"type": "number"}, "pitch": {}, "vel": {"type": "number"},
                    "prob": {"type": "number", "minimum": 0, "maximum": 1}, "offset": {"type": "number", "minimum": -0.5, "maximum": 0.5}, "slide_to": {}
                }, "required": ["start", "pitch"]}}
            }), &["track", "notes"]),
            run: |e, a| {
                let track = e.project.tracks[e.project.track_index(&s_req(a, "track")?)?].name.clone();
                let pi = pattern_idx(&e.project, a)?;
                let arr = a.get("notes").and_then(|v| v.as_array()).ok_or_else(|| anyhow!("notes must be an array"))?;
                let mut new = Vec::new();
                for n in arr {
                    new.push(Note {
                        start: f_opt(n, "start").ok_or_else(|| anyhow!("note missing start"))?.max(0.0),
                        len: f_opt(n, "len").or(f_opt(n, "duration")).unwrap_or(1.0).max(0.05),
                        pitch: pitch_of(n.get("pitch").ok_or_else(|| anyhow!("note missing pitch"))?)?,
                        vel: f_opt(n, "vel")
                            .or(f_opt(n, "velocity").map(|v| if v > 1.0 { v / 127.0 } else { v }))
                            .unwrap_or(0.8)
                            .clamp(0.0, 1.0),
                        prob: f_or(n, "prob", 1.0).clamp(0.0, 1.0),
                        offset: f_or(n, "offset", 0.0).clamp(-0.5, 0.5),
                        slide_to: n.get("slide_to").map(pitch_of).transpose()?,
                        id: String::new(),
                    });
                }
                let pat = &mut e.project.patterns[pi];
                let pname = pat.name.clone();
                let steps = pat.steps() as f32;
                let dropped = new.iter().filter(|n| n.start >= steps).count();
                new.retain(|n| n.start < steps);
                let notes = pat.notes_mut(&track);
                if b_or(a, "replace", false) {
                    notes.clear();
                }
                notes.extend(new);
                notes.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
                Ok(json!({"track": track, "pattern": pname, "notes_now": notes.len(), "dropped_past_end": dropped}))
            },
        },
        Tool {
            name: "clear",
            description: "Clear notes: one track in a pattern, or the whole pattern if track is omitted.",
            mutates: true,
            schema: || obj(json!({"pattern": pattern_prop(), "track": {"type": "string"}}), &[]),
            run: |e, a| {
                let pi = pattern_idx(&e.project, a)?;
                match s_opt(a, "track") {
                    Some(t) => {
                        let name = e.project.tracks[e.project.track_index(&t)?].name.to_lowercase();
                        e.project.patterns[pi].clips.remove(&name);
                    }
                    None => e.project.patterns[pi].clips.clear(),
                }
                Ok(json!({"cleared": e.project.patterns[pi].name}))
            },
        },
        Tool {
            name: "get_pattern",
            description: "Read a pattern's notes. Drum tracks also get an ASCII step grid. Optionally filter to one track.",
            mutates: false,
            schema: || obj(json!({"pattern": pattern_prop(), "track": {"type": "string"}}), &[]),
            run: |e, a| {
                let p = &e.project;
                let pat = &p.patterns[pattern_idx(p, a)?];
                let filter = s_opt(a, "track").map(|t| t.to_lowercase());
                let mut tracks = Map::new();
                for t in &p.tracks {
                    let key = t.name.to_lowercase();
                    if filter.as_ref().map(|f| f != &key).unwrap_or(false) {
                        continue;
                    }
                    let notes = pat.notes(&t.name);
                    let v = if t.instrument.is_drum() {
                        json!({"grid": grid(notes, pat.steps()), "hits": notes.len()})
                    } else {
                        json!({"notes": notes_json(notes)})
                    };
                    tracks.insert(t.name.clone(), v);
                }
                Ok(json!({"pattern": pat.name, "bars": pat.bars, "steps": pat.steps(), "tracks": tracks}))
            },
        },
        Tool {
            name: "transpose",
            description: "Shift a track's notes in a pattern by semitones.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "semitones": {"type": "integer"}, "pattern": pattern_prop()}), &["track", "semitones"]),
            run: |e, a| {
                let track = e.project.tracks[e.project.track_index(&s_req(a, "track")?)?].name.clone();
                let pi = pattern_idx(&e.project, a)?;
                let st = a.get("semitones").and_then(|v| v.as_i64()).unwrap_or(0);
                for n in e.project.patterns[pi].notes_mut(&track).iter_mut() {
                    n.pitch = (n.pitch as i64 + st).clamp(0, 127) as u8;
                }
                Ok(json!({"track": track, "semitones": st}))
            },
        },
        Tool {
            name: "humanize",
            description: "Add human feel: random timing offset (in steps, e.g. 0.05) and velocity variation (0-1) to one track or all tracks in a pattern.",
            mutates: true,
            schema: || obj(json!({"pattern": pattern_prop(), "track": {"type": "string"}, "timing": {"type": "number"}, "velocity": {"type": "number"}, "seed": {"type": "integer"}}), &[]),
            run: |e, a| {
                let pi = pattern_idx(&e.project, a)?;
                let mut rng = Rng::new(seed_of(a));
                let (timing, vel) = (f_or(a, "timing", 0.04), f_or(a, "velocity", 0.12));
                let only = s_opt(a, "track").map(|t| t.to_lowercase());
                for (k, notes) in e.project.patterns[pi].clips.iter_mut() {
                    if only.as_ref().map(|o| o == k).unwrap_or(true) {
                        theory::humanize(notes, timing, vel, &mut rng);
                    }
                }
                Ok(json!({"humanized": e.project.patterns[pi].name}))
            },
        },
        // ----- generators -----
        Tool {
            name: "generate_drums",
            description: "Write a full drum groove in a style (house, techno, trap, drill, boom_bap, lofi, dnb, reggaeton, afrobeats, phonk, garage). Creates the drum tracks it needs, adds a fill in the last bar and humanizes. set_tempo=true also applies the style's BPM/swing.",
            mutates: true,
            schema: || obj(json!({
                "style": {"type": "string"},
                "pattern": pattern_prop(),
                "fill": {"type": "boolean"},
                "set_tempo": {"type": "boolean"},
                "seed": {"type": "integer"}
            }), &["style"]),
            run: |e, a| {
                let g = theory::groove(&s_req(a, "style")?)?;
                if b_or(a, "set_tempo", true) {
                    e.project.bpm = g.bpm;
                    e.project.swing = g.swing;
                }
                let mut rng = Rng::new(seed_of(a));
                let pi = pattern_idx(&e.project, a)?;
                let mut made = Vec::new();
                for (track, preset, steps) in g.parts {
                    let name = ensure_track(&mut e.project, track, preset, None)?;
                    let pat = &mut e.project.patterns[pi];
                    let total = pat.steps();
                    let mut notes = theory::steps_to_notes(&theory::parse_steps(steps), total, true, 60, 1.0);
                    if b_or(a, "fill", true) && (name.contains("snare") || name == "clap") && pat.bars >= 2 {
                        theory::add_fill(&mut notes, total, &mut rng, 0.6);
                    }
                    theory::humanize(&mut notes, 0.0, if name.contains("hat") || name.contains("shaker") { 0.18 } else { 0.06 }, &mut rng);
                    *pat.notes_mut(&name) = notes;
                    made.push(name);
                }
                // tasteful defaults: pan hats, quiet percs
                for t in e.project.tracks.iter_mut() {
                    match t.name.as_str() {
                        "hat" => t.pan = 0.25,
                        "open_hat" => { t.pan = -0.2; t.volume_db = -4.0; }
                        "shaker" => { t.pan = -0.35; t.volume_db = -5.0; }
                        "rim" | "cowbell" => { t.pan = 0.3; t.volume_db = -4.0; }
                        _ => {}
                    }
                }
                Ok(json!({"style": g.name, "bpm": e.project.bpm, "swing": e.project.swing, "tracks": made}))
            },
        },
        Tool {
            name: "generate_chords",
            description: "Write a chord progression with smooth voice leading. progression uses roman numerals relative to the key ('i VI III VII', 'ii7 V7 Imaj7', 'bVII') or chord names ('Cm7 Ab Eb Bb'). style: block, stabs, offbeat, pulse, arp_up, arp_down, arp_updown.",
            mutates: true,
            schema: || obj(json!({
                "progression": {"type": "string"},
                "pattern": pattern_prop(),
                "track": {"type": "string", "description": "default 'chords'"},
                "preset": {"type": "string", "description": "default warm_pad"},
                "style": {"type": "string"},
                "octave": {"type": "integer", "description": "default 4"},
                "bars_per_chord": {"type": "number", "description": "default 1"},
                "velocity": {"type": "number"},
                "voicing": {"type": "string", "enum": ["auto", "open", "close"], "description": "auto (default): open voicings above ~247 Hz with no low thirds and rootless 7ths when a bass/808 track plays in the pattern (keeps 200-500 Hz clear), close otherwise"}
            }), &["progression"]),
            run: |e, a| {
                let chords = theory::parse_progression(&s_req(a, "progression")?, key_pc(&e.project), &e.project.scale)?;
                let track = ensure_track(&mut e.project, &s_opt(a, "track").unwrap_or_else(|| "chords".into()), "warm_pad", instrument_from(a)?)?;
                let octave = u_or(a, "octave", 4) as i32;
                let open = match s_opt(a, "voicing").as_deref().unwrap_or("auto") {
                    "open" => true,
                    "close" => false,
                    "auto" => {
                        let pi = pattern_idx(&e.project, a)?;
                        let pat = &e.project.patterns[pi];
                        e.project.tracks.iter().any(|t| t.name != track && crate::tools_mix::role_of(&t.name, &t.instrument) == "bass" && !pat.notes(&t.name).is_empty())
                    }
                    other => bail!("voicing '{other}': use auto, open or close"),
                };
                let voiced = if open { theory::voice_chords_open(&chords, octave, 59) } else { theory::voice_chords(&chords, octave, true) };
                let pi = pattern_idx(&e.project, a)?;
                let pat = &mut e.project.patterns[pi];
                let notes = theory::chord_notes(&voiced, f_or(a, "bars_per_chord", 1.0), pat.steps(), &s_opt(a, "style").unwrap_or_else(|| "block".into()), f_or(a, "velocity", 0.75))?;
                *pat.notes_mut(&track) = notes;
                Ok(json!({
                    "track": track,
                    "voicing": if open { "open" } else { "close" },
                    "chords": chords.iter().zip(voiced.iter()).map(|(c, v)| json!({"chord": c.label, "notes": v.iter().map(|p| theory::note_name(*p)).collect::<Vec<_>>()})).collect::<Vec<_>>()
                }))
            },
        },
        Tool {
            name: "generate_bassline",
            description: "Write a bassline that follows a progression. style: root, eighths, octave, offbeat (house), syncopated, 808 (trap glide-style long notes), walking.",
            mutates: true,
            schema: || obj(json!({
                "progression": {"type": "string"},
                "pattern": pattern_prop(),
                "track": {"type": "string", "description": "default 'bass'"},
                "preset": {"type": "string", "description": "default 808 for style 808, else sub_bass"},
                "style": {"type": "string"},
                "octave": {"type": "integer", "description": "default 1 for 808, else 2"},
                "bars_per_chord": {"type": "number"},
                "seed": {"type": "integer"}
            }), &["progression"]),
            run: |e, a| {
                let style = s_opt(a, "style").unwrap_or_else(|| "root".into());
                let chords = theory::parse_progression(&s_req(a, "progression")?, key_pc(&e.project), &e.project.scale)?;
                let default_preset = if style == "808" { "808" } else { "sub_bass" };
                let track = ensure_track(&mut e.project, &s_opt(a, "track").unwrap_or_else(|| "bass".into()), default_preset, instrument_from(a)?)?;
                let pi = pattern_idx(&e.project, a)?;
                let pat = &mut e.project.patterns[pi];
                let oct = a.get("octave").and_then(|v| v.as_i64()).map(|v| v as i32).unwrap_or(if style == "808" { 1 } else { 2 });
                let mut notes = theory::bass_notes(&chords, f_or(a, "bars_per_chord", 1.0), pat.steps(), oct, &style, &mut Rng::new(seed_of(a)))?;
                // keep the bass in its own register: octave jumps above E3 (~165 Hz)
                // fold down, so the 200-500 Hz band stays for the chords and snare
                if oct <= 2 {
                    for n in notes.iter_mut() {
                        while n.pitch > 52 {
                            n.pitch -= 12;
                        }
                    }
                }
                let n = notes.len();
                *pat.notes_mut(&track) = notes;
                Ok(json!({"track": track, "style": style, "notes": n}))
            },
        },
        Tool {
            name: "generate_melody",
            description: "Write a scale-locked melody with motif repetition (call & response). If progression is given, strong beats land on chord tones. density 0-1, range in semitones, seed for variations.",
            mutates: true,
            schema: || obj(json!({
                "pattern": pattern_prop(),
                "track": {"type": "string", "description": "default 'lead'"},
                "preset": {"type": "string", "description": "default pluck_lead"},
                "octave": {"type": "integer", "description": "default 5"},
                "density": {"type": "number"},
                "range": {"type": "integer"},
                "motif_bars": {"type": "integer"},
                "progression": {"type": "string"},
                "bars_per_chord": {"type": "number"},
                "note_len": {"type": "number"},
                "seed": {"type": "integer"}
            }), &[]),
            run: |e, a| {
                let scale = theory::scale_intervals(&e.project.scale)?;
                let kp = key_pc(&e.project);
                let chords = match s_opt(a, "progression") {
                    Some(p) => Some(theory::parse_progression(&p, kp, &e.project.scale)?),
                    None => None,
                };
                let track = ensure_track(&mut e.project, &s_opt(a, "track").unwrap_or_else(|| "lead".into()), "pluck_lead", instrument_from(a)?)?;
                let pi = pattern_idx(&e.project, a)?;
                let pat = &mut e.project.patterns[pi];
                let bpc = f_or(a, "bars_per_chord", 1.0);
                let spec = MelodySpec {
                    key_pc: kp,
                    scale,
                    octave: a.get("octave").and_then(|v| v.as_i64()).unwrap_or(5) as i32,
                    range_semitones: a.get("range").and_then(|v| v.as_i64()).unwrap_or(12) as i32,
                    density: f_or(a, "density", 0.4).clamp(0.05, 1.0),
                    total_steps: pat.steps(),
                    motif_bars: (u_or(a, "motif_bars", 2) as u32).max(1),
                    chords: chords.as_deref().map(|c| (c, bpc)),
                    note_len: f_or(a, "note_len", 1.0),
                };
                let notes = theory::melody_notes(&spec, &mut Rng::new(seed_of(a)));
                let preview: Vec<String> = notes.iter().take(16).map(|n| theory::note_name(n.pitch)).collect();
                let n = notes.len();
                *pat.notes_mut(&track) = notes;
                Ok(json!({"track": track, "notes": n, "first_notes": preview}))
            },
        },
        Tool {
            name: "generate_beat",
            description: "One call, full song: sets key/tempo, writes drums, bass, chords and a lead in a style, adds mix FX (reverb, delay, sidechain where it fits) and arranges intro / main / break / main. Great starting point to then refine.",
            mutates: true,
            schema: || obj(json!({
                "style": {"type": "string"},
                "key": {"type": "string"},
                "scale": {"type": "string"},
                "progression": {"type": "string"},
                "bars": {"type": "integer", "description": "bars per section, default 4"},
                "seed": {"type": "integer"},
                "name": {"type": "string"}
            }), &["style"]),
            run: |e, a| {
                let style_in = s_req(a, "style")?;
                let g = theory::groove(&style_in)?;
                let prof = profile(g.name);
                let seed = seed_of(a);
                let bars = (u_or(a, "bars", 4) as u32).clamp(1, 16);
                let mut p = Project::new(&s_opt(a, "name").unwrap_or_else(|| format!("{} beat", g.name)), g.bpm);
                p.key_root = s_opt(a, "key").unwrap_or_else(|| ["A", "C", "F", "D", "G", "E"][(seed % 6) as usize].into());
                theory::pitch_class(&p.key_root)?;
                let desi = g.name == "desi_hiphop";
                p.scale = s_opt(a, "scale").unwrap_or_else(|| {
                    if desi {
                        // raag-flavoured thaats: Kafi (dorian colour), Asavari, Bhairav
                        ["kafi", "asavari", "bhairav"][((seed >> 3) % 3) as usize].into()
                    } else if g.name == "afrobeats" || g.name == "lofi" {
                        "major".into()
                    } else {
                        "minor".into()
                    }
                });
                theory::scale_intervals(&p.scale)?;
                p.patterns = vec![Pattern::new("main", bars)];
                e.project = p;
                let prog = s_opt(a, "progression").unwrap_or_else(|| {
                    if desi {
                        match e.project.scale.as_str() {
                            "kafi" => "i IV i VII",
                            "bhairav" => "I II iv I",
                            "bhairavi" => "i II i VII",
                            _ => "i VI VII i",
                        }
                        .to_string()
                    } else {
                        prof.progression.to_string()
                    }
                });
                let lead_preset = if desi { ["sitar", "bansuri"][((seed >> 5) % 2) as usize] } else { prof.lead_preset };
                let main = json!({"pattern": "main", "seed": seed});
                let with = |extra: Value| {
                    let mut m = main.clone();
                    merge(&mut m, &extra);
                    m
                };
                let call = |e: &mut Engine, name: &str, args: Value| -> Result<Value> { (find(name).unwrap().run)(e, &args) };
                call(e, "generate_drums", with(json!({"style": g.name})))?;
                call(e, "generate_bassline", with(json!({"progression": prog, "preset": prof.bass_preset, "style": prof.bass_style, "octave": prof.bass_octave})))?;
                call(e, "generate_chords", with(json!({"progression": prog, "preset": prof.chord_preset, "style": prof.chord_style, "octave": 4})))?;
                call(e, "generate_melody", with(json!({"progression": prog, "preset": lead_preset, "density": prof.lead_density, "octave": 5})))?;
                if desi {
                    desi_touches(e, seed, lead_preset == "bansuri")?;
                }
                // mix
                call(e, "set_mixer", json!({"track": "chords", "volume_db": -6.0}))?;
                call(e, "set_mixer", json!({"track": "lead", "volume_db": -5.0, "pan": 0.1}))?;
                call(e, "set_mixer", json!({"track": "bass", "volume_db": if prof.bass_preset == "808" { -2.0 } else { -6.0 }}))?;
                call(e, "set_mixer", json!({"track": "kick", "volume_db": -1.5}))?;
                call(e, "add_effect", json!({"track": "chords", "type": "reverb", "params": {"size": 0.8, "mix": 0.3}}))?;
                call(e, "add_effect", json!({"track": "chords", "type": "eq", "params": {"low_db": -6.0}}))?;
                call(e, "add_effect", json!({"track": "chords", "type": "chorus", "params": {"mix": 0.35}}))?;
                call(e, "add_effect", json!({"track": "chords", "type": "width", "params": {"amount": 1.6}}))?;
                call(e, "add_effect", json!({"track": "lead", "type": "delay", "params": {"steps": 3.0, "mix": 0.22, "feedback": 0.35}}))?;
                call(e, "add_effect", json!({"track": "lead", "type": "reverb", "params": {"size": 0.6, "mix": 0.2}}))?;
                if prof.sidechain || g.name == "trap" || g.name == "phonk" || g.name == "drill" {
                    call(e, "add_effect", json!({"track": "bass", "type": "sidechain", "params": {"source": "kick", "amount": if prof.sidechain { 0.8 } else { 0.5 }}}))?;
                }
                if prof.sidechain {
                    call(e, "add_effect", json!({"track": "chords", "type": "sidechain", "params": {"source": "kick", "amount": 0.6}}))?;
                }
                crate::carve::carve(e, &[]);
                if g.name == "lofi" || g.name == "boom_bap" {
                    call(e, "add_effect", json!({"track": "master", "type": "bitcrush", "params": {"bits": 12.0, "downsample": 2, "mix": 0.35}}))?;
                    e.project.master_effects.rotate_right(1);
                }
                // arrangement variants
                let drums: Vec<String> = g.parts.iter().map(|p| p.0.to_string()).collect();
                let mut intro = e.project.patterns[0].clone();
                intro.name = "intro".into();
                for d in &drums {
                    if d != "hat" && d != "shaker" {
                        intro.clips.remove(d.as_str());
                    }
                }
                intro.clips.remove("bass");
                let mut brk = e.project.patterns[0].clone();
                brk.name = "break".into();
                for d in &drums {
                    brk.clips.remove(d.as_str());
                }
                e.project.patterns.push(intro);
                e.project.patterns.push(brk);
                e.project.arrangement = vec![
                    Section { pattern: "intro".into(), repeats: 1 },
                    Section { pattern: "main".into(), repeats: 2 },
                    Section { pattern: "break".into(), repeats: 1 },
                    Section { pattern: "main".into(), repeats: 2 },
                ];
                Ok(summary(&e.project))
            },
        },
        // ----- theory helpers -----
        Tool {
            name: "theory_scale",
            description: "Notes of a scale, e.g. {root:'D', scale:'dorian', octave:4}.",
            mutates: false,
            schema: || obj(json!({"root": {"type": "string"}, "scale": {"type": "string"}, "octave": {"type": "integer"}}), &["root", "scale"]),
            run: |_, a| {
                let pc = theory::pitch_class(&s_req(a, "root")?)? as i32;
                let iv = theory::scale_intervals(&s_req(a, "scale")?)?;
                let oct = a.get("octave").and_then(|v| v.as_i64()).unwrap_or(4).clamp(-2, 8) as i32;
                let notes: Vec<String> = iv.iter().map(|i| theory::note_name(((oct + 1) * 12 + pc + *i as i32).clamp(0, 127) as u8)).collect();
                Ok(json!({"notes": notes, "intervals": iv}))
            },
        },
        Tool {
            name: "theory_chords",
            description: "Resolve a progression to actual notes in the project key (or a given key/scale) without writing anything.",
            mutates: false,
            schema: || obj(json!({"progression": {"type": "string"}, "key": {"type": "string"}, "scale": {"type": "string"}, "octave": {"type": "integer"}}), &["progression"]),
            run: |e, a| {
                let key = s_opt(a, "key").unwrap_or_else(|| e.project.key_root.clone());
                let scale = s_opt(a, "scale").unwrap_or_else(|| e.project.scale.clone());
                let chords = theory::parse_progression(&s_req(a, "progression")?, theory::pitch_class(&key)?, &scale)?;
                let voiced = theory::voice_chords(&chords, a.get("octave").and_then(|v| v.as_i64()).unwrap_or(4) as i32, true);
                Ok(Value::Array(chords.iter().zip(voiced.iter()).map(|(c, v)| json!({"chord": c.label, "notes": v.iter().map(|p| theory::note_name(*p)).collect::<Vec<_>>()})).collect()))
            },
        },
        // ----- arrangement -----
        Tool {
            name: "set_arrangement",
            description: "Set the song order: sections=[{pattern:'intro', repeats:1}, {pattern:'drop', repeats:2}, ...]. Empty list = loop the first pattern.",
            mutates: true,
            schema: || obj(json!({"sections": {"type": "array", "items": {"type": "object", "properties": {"pattern": {"type": "string"}, "repeats": {"type": "integer"}}, "required": ["pattern"]}}}), &["sections"]),
            run: |e, a| {
                let secs: Vec<Section> = serde_json::from_value(a.get("sections").cloned().unwrap_or(json!([])))?;
                for s in &secs {
                    e.project.pattern_index(&s.pattern)?;
                }
                e.project.arrangement = secs;
                Ok(json!({"arrangement": e.project.song_sections().iter().map(|s| format!("{} x{}", s.pattern, s.repeats)).collect::<Vec<_>>(), "song_seconds": e.project.song_seconds()}))
            },
        },
        // ----- samples -----
        Tool {
            name: "search_samples",
            description: "Search Freesound.org for sounds (drums, vocals chops, foley, risers, vinyl crackle, FX...). Needs FREESOUND_API_KEY. cc0_only (default true) keeps results royalty-free. Then call download_sample with an id.",
            mutates: false,
            schema: || obj(json!({"query": {"type": "string"}, "max": {"type": "integer"}, "cc0_only": {"type": "boolean"}, "max_duration": {"type": "number", "description": "seconds, default 10"}}), &["query"]),
            run: |_, a| samples::freesound_search(&s_req(a, "query")?, u_or(a, "max", 10) as usize, b_or(a, "cc0_only", true), f_or(a, "max_duration", 10.0)),
        },
        Tool {
            name: "download_sample",
            description: "Download a sound into the project: freesound_id (from search_samples) or any direct audio url (wav/mp3/flac/ogg). Registers it by name for add_sample_track.",
            mutates: true,
            schema: || obj(json!({"freesound_id": {"type": "integer"}, "url": {"type": "string"}, "name": {"type": "string"}}), &[]),
            run: |e, a| {
                let dir = e.samples_dir();
                let name = s_opt(a, "name");
                let info = if let Some(id) = a.get("freesound_id").and_then(|v| v.as_u64()) {
                    samples::freesound_download(id, name.as_deref(), &dir)?
                } else if let Some(url) = s_opt(a, "url") {
                    samples::url_download(&url, name.as_deref(), &dir)?
                } else {
                    bail!("give freesound_id or url");
                };
                register_sample(e, info)
            },
        },
        Tool {
            name: "import_sample",
            description: "Register a local audio file (wav/mp3/flac/ogg) as a sample. license / author / source (where it came from: a URL or a description) are kept for credits (list_samples, export_credits).",
            mutates: true,
            schema: || obj(json!({"path": {"type": "string"}, "name": {"type": "string"},
                "license": {"type": "string", "description": "e.g. CC0, CC-BY-4.0, own recording"},
                "author": {"type": "string", "description": "who made or recorded it"},
                "source": {"type": "string", "description": "where it came from (URL or description); default 'local file'"}}), &["path"]),
            run: |e, a| {
                let path = e.resolve(&s_req(a, "path")?);
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("sample").to_string();
                let info = SampleInfo {
                    name: samples::sample_name(&s_opt(a, "name").unwrap_or(stem)),
                    path: path.to_string_lossy().into(),
                    source: s_opt(a, "source").filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "local file".into()),
                    license: s_opt(a, "license").unwrap_or_default(),
                    author: s_opt(a, "author").unwrap_or_default(),
                    duration: 0.0,
                };
                register_sample(e, info)
            },
        },
        Tool {
            name: "list_samples",
            description: "List samples in the project with source, license and duration (for credits).",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| Ok(serde_json::to_value(&e.project.samples)?),
        },
        Tool {
            name: "add_sample_track",
            description: "Create a track that plays a sample. one_shot=true for drums/FX, false to play pitched notes for their length. root = MIDI note where the sample sounds at original pitch.",
            mutates: true,
            schema: || obj(json!({"name": {"type": "string"}, "sample": {"type": "string"}, "root": {}, "one_shot": {"type": "boolean"}, "volume_db": {"type": "number"}}), &["name", "sample"]),
            run: |e, a| {
                let name = s_req(a, "name")?;
                let sample = s_req(a, "sample")?;
                if !e.project.samples.iter().any(|s| s.name == sample) {
                    bail!("no sample '{sample}'. Samples: [{}]", e.project.samples.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "));
                }
                if e.project.track_index(&name).is_ok() {
                    bail!("track '{name}' already exists");
                }
                let root = a.get("root").map(pitch_of).transpose()?.unwrap_or(60);
                let inst = Instrument::Sampler(instruments::SamplerParams { sample, root, one_shot: b_or(a, "one_shot", true), ..Default::default() });
                e.project.tracks.push(Track { volume_db: f_or(a, "volume_db", 0.0), ..Track::new(&name, inst) });
                Ok(json!({"added": name}))
            },
        },
        // ----- output -----
        Tool {
            name: "render",
            description: "Render the arranged song (or a region: section / start_beat..end_beat) to a 16-bit WAV (44.1 kHz stereo) with `tail` seconds of ring-out (default 1.5). Optionally export every track as stems. Returns loudness/peak so you can sanity-check. For delivery formats use export_audio.",
            mutates: false,
            schema: || obj(json!({"path": {"type": "string", "description": "default renders/<project>.wav"}, "loops": {"type": "integer"}, "stems_dir": {"type": "string"},
                "section": {"description": "Render only this arrangement section (index or pattern name)", "type": ["string", "integer"]},
                "start_beat": {"type": "number", "description": "Region start in song beats"}, "end_beat": {"type": "number"},
                "tail": {"type": "number", "description": "Seconds of ring-out after the last bar (default 1.5, max 30)"},
                "allow_missing_clips": {"type": "boolean", "description": "render even if audio clips point at missing samples (they stay silent; default false = fail and name them)"}}), &[]),
            run: |e, a| {
                let path = match s_opt(a, "path") {
                    Some(p) => e.resolve(&p),
                    None => e.renders_dir().join(format!("{}.wav", samples::sample_name(&e.project.name))),
                };
                let loops = u_or(a, "loops", 1) as u32;
                let stems_dir = s_opt(a, "stems_dir").map(|s| e.resolve(&s));
                // region -> step range
                let sec = a.get("section").and_then(|v| v.as_str().map(String::from).or_else(|| v.as_u64().map(|n| n.to_string())));
                let range = if let Some(s) = sec {
                    let (b0, b1) = e.project.section_beats(&s)?;
                    Some(((b0 * 4.0).round() as u32, (b1 * 4.0).round() as u32))
                } else if f_opt(a, "start_beat").is_some() || f_opt(a, "end_beat").is_some() {
                    let b0 = f_opt(a, "start_beat").unwrap_or(0.0).max(0.0);
                    let b1 = f_opt(a, "end_beat").unwrap_or(e.project.song_beats());
                    if b1 <= b0 { bail!("end_beat must be after start_beat"); }
                    Some(((b0 * 4.0).round() as u32, (b1 * 4.0).round() as u32))
                } else { None };
                let tail = f_opt(a, "tail").unwrap_or(1.5).clamp(0.0, 30.0);
                e.bank.sync(&e.project.samples);
                let mix = render::render(&e.project, &e.bank, &RenderOptions { loops, keep_stems: stems_dir.is_some(), step_range: range, tail, allow_missing_clips: b_or(a, "allow_missing_clips", false), ..Default::default() })?;
                render::write_wav(&path, &mix.left, &mix.right)?;
                let mut stems = Vec::new();
                if let Some(d) = stems_dir {
                    let names = samples::distinct_export_names(&mix.stems.iter().map(|s| samples::sample_name(&s.name)).collect::<Vec<_>>());
                    for (s, name) in mix.stems.iter().zip(names) {
                        let sp = d.join(format!("{name}.wav"));
                        render::write_wav(&sp, &s.left, &s.right)?;
                        stems.push(sp);
                    }
                }
                let st = analysis::stats(&mix.left, &mix.right);
                Ok(json!({"path": path, "seconds": (mix.seconds * 10.0).round() / 10.0, "peak_dbfs": st.peak_dbfs, "rms_dbfs": st.rms_dbfs, "stems": stems,
                    "region_steps": range.map(|r| json!([r.0, r.1])), "tail_s": tail}))
            },
        },
        Tool {
            name: "analyze_mix",
            description: "Listen to the current mix (or a window: section / start_beat..end_beat / start_s..end_s): peak/RMS loudness, crest factor, spectral balance across sub/bass/low-mid/mid/presence/air, stereo correlation, per-track energy, a 0-100 mix score and concrete suggestions (which tool + params to fix each issue). Use after every big change.",
            mutates: false,
            schema: || obj(crate::tools_ears::region_props(), &[]),
            run: |e, a| {
                let windowed = ["section", "start_beat", "end_beat", "start_s", "end_s"].iter().any(|k| a.get(*k).is_some());
                if !windowed {
                    return Ok(serde_json::to_value(e.analyze()?)?);
                }
                let m = e.mix()?;
                let p = e.project.clone();
                let (s0, s1, label) = crate::tools_ears::region(&p, a, m.left.len().min(m.right.len()))?;
                let sub = crate::tools_ears::window_mix(&m, s0, s1);
                let mut v = serde_json::to_value(analysis::analyze(&sub))?;
                v["window"] = json!({"label": label, "start_s": s0 as f32 / crate::dsp::SR, "end_s": s1 as f32 / crate::dsp::SR});
                v["top_tracks"] = json!(crate::ears::top_tracks(&m.track_info, s0, s1, 5));
                Ok(v)
            },
        },
    ]
}

pub(crate) fn snapshots_path(p: &std::path::Path) -> std::path::PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".snapshots.json");
    std::path::PathBuf::from(s)
}

pub(crate) fn register_sample(e: &mut Engine, mut info: SampleInfo) -> Result<Value> {
    let data = samples::decode_file(std::path::Path::new(&info.path))?;
    info.duration = (data.len() as f32 / crate::dsp::SR * 100.0).round() / 100.0;
    let mut name = info.name.clone();
    let mut k = 2;
    while e
        .project
        .samples
        .iter()
        .any(|s| s.name.eq_ignore_ascii_case(&name))
    {
        name = format!("{}_{k}", info.name);
        k += 1;
    }
    info.name = name.clone();
    e.bank
        .insert_for_source(&name, std::path::Path::new(&info.path), data);
    e.project.samples.push(info.clone());
    Ok(
        json!({"sample": name, "duration": info.duration, "license": info.license, "source": info.source, "next": "add_sample_track {name, sample}"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eng() -> Engine {
        Engine::new(std::env::temp_dir().join("beatbox_tool_tests"))
    }

    #[test]
    fn names_unique_and_schemas_valid() {
        let mut seen = std::collections::HashSet::new();
        for t in registry() {
            assert!(seen.insert(t.name), "duplicate tool {}", t.name);
            let s = (t.schema)();
            assert_eq!(s["type"], "object", "{}", t.name);
            assert!(t.description.len() > 10);
        }
        assert!(registry().len() >= 40, "only {} tools", registry().len());
    }

    #[test]
    fn every_style_generates_a_full_beat_that_renders() {
        for g in theory::GROOVES {
            let mut e = eng();
            e.call(
                "generate_beat",
                &json!({"style": g.name, "bars": 2, "seed": 1}),
            )
            .unwrap();
            assert!(e.project.tracks.len() >= 6, "{}", g.name);
            let rep = e.analyze().unwrap();
            assert!(
                rep.master.rms_dbfs > -40.0,
                "{} too quiet: {}",
                g.name,
                rep.master.rms_dbfs
            );
            assert!(
                rep.master.peak_dbfs <= -0.9,
                "{} peak {}",
                g.name,
                rep.master.peak_dbfs
            );
            // the true-peak aware master limiter holds the ceiling between samples too
            assert!(
                rep.loudness.true_peak_dbtp <= -0.6,
                "{} true peak {}",
                g.name,
                rep.loudness.true_peak_dbtp
            );
            assert!(rep.loudness.integrated_lufs > -30.0, "{}", g.name);
        }
    }

    #[test]
    fn errors_roll_back_and_undo_works() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "kick", "preset": "kick"}))
            .unwrap();
        assert!(e
            .call(
                "add_effect",
                &json!({"track": "kick", "type": "sidechain", "params": {"source": "nope"}})
            )
            .is_err());
        assert!(e.project.tracks[0].effects.is_empty());
        e.call(
            "set_steps",
            &json!({"track": "kick", "steps": "X...X...X...X..."}),
        )
        .unwrap();
        assert_eq!(e.project.patterns[0].notes("kick").len(), 16);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project.patterns[0].notes("kick").len(), 0);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(e.project.patterns[0].notes("kick").len(), 16);
    }

    #[test]
    fn tweak_instrument_merges() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "bass", "preset": "acid_bass"}))
            .unwrap();
        let v = e
            .call(
                "tweak_instrument",
                &json!({"track": "bass", "params": {"cutoff": 222, "amp_env": {"release": 0.9}}}),
            )
            .unwrap();
        let close = |x: &Value, y: f64| (x.as_f64().unwrap() - y).abs() < 1e-4;
        assert!(close(&v["cutoff"], 222.0));
        assert!(close(&v["amp_env"]["release"], 0.9));
        assert!(close(&v["resonance"], 0.85));
    }

    #[test]
    fn add_notes_with_names() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "keys", "preset": "epiano"}))
            .unwrap();
        e.call("add_notes", &json!({"track": "keys", "notes": [{"start": 0, "len": 4, "pitch": "C4"}, {"start": 4, "pitch": 67, "vel": 0.5}]})).unwrap();
        let n = e.project.patterns[0].notes("keys");
        assert_eq!(n[0].pitch, 60);
        assert_eq!(n[1].pitch, 67);
    }

    #[test]
    fn transpose_extreme_values_clamp_instead_of_overflowing() {
        let mut e = eng();
        e.call(
            "generate_beat",
            &json!({"style": "trap", "bars": 1, "seed": 1}),
        )
        .unwrap();
        let track = e.project.tracks[0].name.clone();

        // legacy transpose: i64 extremes (JSON integers parse as i64)
        e.call(
            "transpose",
            &json!({"track": track, "semitones": 9000000000000000000i64}),
        )
        .unwrap();
        e.call(
            "transpose",
            &json!({"track": track, "semitones": -9000000000000000000i64}),
        )
        .unwrap();
        // edit_notes: huge floats saturate the f64 -> i32 cast
        e.call(
            "edit_notes",
            &json!({"track": track, "all_patterns": true, "transpose": 1e18}),
        )
        .unwrap();
        e.call(
            "edit_notes",
            &json!({"track": track, "all_patterns": true, "transpose": -1e18}),
        )
        .unwrap();
        // copy_notes: same via its own transpose arg
        let pat = e.project.patterns[0].name.clone();
        e.call(
            "copy_notes",
            &json!({"track": track, "from_pattern": pat, "to_pattern": pat, "to_bar": 0.5, "transpose": 1e18}),
        )
        .unwrap();

        for p in &e.project.patterns {
            for n in p.notes(&track) {
                assert!(n.pitch <= 127, "pitch out of range");
            }
        }
    }

    #[test]
    fn octave_extreme_values_clamp_instead_of_overflowing() {
        let mut e = eng();
        e.call(
            "generate_beat",
            &json!({"style": "trap", "bars": 1, "seed": 1}),
        )
        .unwrap();
        for oct in [9000000000000000000i64, -9000000000000000000, 2147483647] {
            e.call(
                "theory_scale",
                &json!({"root": "C", "scale": "major", "octave": oct}),
            )
            .unwrap();
            e.call(
                "theory_chords",
                &json!({"progression": "i-VI-III-VII", "octave": oct}),
            )
            .unwrap();
            e.call(
                "generate_bassline",
                &json!({"progression": "i-VI-III-VII", "octave": oct}),
            )
            .unwrap();
        }
    }

    #[test]
    fn load_project_clamps_hostile_timing_fields() {
        // bpm 0 / negative / huge and extreme swing used to reach raw-`bpm`
        // math in some voice paths and hang the render; load clamps them to
        // the same ranges the use sites already clamp to.
        let dir = std::env::temp_dir().join("bb_hostile_timing_load");
        std::fs::create_dir_all(&dir).unwrap();
        for (bpm, swing, want_bpm, want_swing) in [
            (0.0, 0.0, 20.0, 0.0),
            (-120.0, 5.0, 20.0, 1.0),
            (1e300, -1.0, 400.0, 0.0),
            (140.0, 0.4, 140.0, 0.4),
        ] {
            let path = dir.join("p.json");
            std::fs::write(
                &path,
                format!(
                    r#"{{"name":"h","bpm":{bpm},"swing":{swing},"key_root":"C","scale":"minor","tracks":[],"patterns":[],"arrangement":[],"master_effects":[],"master_volume_db":0.0,"samples":[],"schema_version":2,"id_seq":0,"seed_log":[]}}"#
                ),
            )
            .unwrap();
            let mut e = eng();
            e.call("load_project", &json!({"path": path.to_str().unwrap()}))
                .unwrap();
            assert_eq!(e.project.bpm, want_bpm, "bpm {bpm}");
            assert_eq!(e.project.swing, want_swing, "swing {swing}");
        }
    }

    #[test]
    fn save_and_load_roundtrip() {
        let mut e = eng();
        e.call("generate_beat", &json!({"style": "house", "bars": 1}))
            .unwrap();
        let before = e.project.clone();
        e.call("save_project", &json!({"path": "rt.beatbox.json"}))
            .unwrap();
        e.call("new_project", &json!({})).unwrap();
        e.call("load_project", &json!({"path": "rt.beatbox.json"}))
            .unwrap();
        assert_eq!(e.project, before);
    }

    #[test]
    fn rename_track_moves_every_reference_and_undoes() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "kick", "preset": "kick"}))
            .unwrap();
        e.call("add_track", &json!({"name": "bass", "preset": "acid_bass"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track": "bass", "notes": [{"start": 0, "len": 1, "pitch": 40}]}),
        )
        .unwrap();
        e.call(
            "add_automation",
            &json!({"track": "bass", "param": "volume", "points": [{"beat": 0, "value": -6}, {"beat": 4, "value": 0}]}),
        )
        .unwrap();
        e.call(
            "create_macro",
            &json!({"name": "m", "targets": [{"track": "bass", "param": "volume", "min": -12, "max": 0}]}),
        )
        .unwrap();
        e.call(
            "add_effect",
            &json!({"track": "bass", "type": "sidechain", "params": {"source": "kick", "amount": 0.5}}),
        )
        .unwrap();
        e.call(
            "place_pattern",
            &json!({"pattern": "A", "start_bar": 0, "tracks": ["bass"]}),
        )
        .unwrap();
        let before = e.project.clone();
        e.call(
            "rename_track",
            &json!({"track": "bass", "name": "Sub Bass"}),
        )
        .unwrap();
        e.call("rename_track", &json!({"track": "kick", "name": "drumz"}))
            .unwrap();
        let p = &e.project;
        assert_eq!(p.tracks[0].name, "drumz");
        assert_eq!(p.tracks[1].name, "Sub Bass");
        assert!(!p.tracks[1].id.is_empty());
        for pat in &p.patterns {
            assert!(!pat.clips.contains_key("bass"), "{}", pat.name);
        }
        assert_eq!(p.patterns[0].notes("Sub Bass").len(), 1);
        assert!(p.automation.iter().all(|l| l.target == "Sub Bass"));
        assert!(p.macros[0].targets.iter().all(|t| t.track == "Sub Bass"));
        assert!(!p.playlist.is_empty());
        assert!(p
            .playlist
            .iter()
            .all(|c| c.tracks.len() == 1 && c.tracks[0] == "Sub Bass"));
        let fx = serde_json::to_value(&p.tracks[1].effects).unwrap();
        assert_eq!(fx[0]["source"], "drumz");
        assert!(e
            .call(
                "rename_track",
                &json!({"track": "Sub Bass", "name": "DRUMZ"})
            )
            .is_err());
        assert!(e
            .call("rename_track", &json!({"track": "Sub Bass", "name": " "}))
            .is_err());
        assert!(e
            .call(
                "rename_track",
                &json!({"track": "Sub Bass", "name": "master"})
            )
            .is_err());
        e.call("undo", &json!({"steps": 2})).unwrap();
        assert_eq!(e.project.tracks[1].name, "bass");
        assert_eq!(
            e.project.patterns[0].notes("bass").len(),
            before.patterns[0].notes("bass").len()
        );
        e.call("redo", &json!({"steps": 2})).unwrap();
        assert_eq!(e.project.tracks[1].name, "Sub Bass");
    }
}

#[cfg(test)]
mod legacy_sample_name_tests {
    use serde_json::json;
    #[test]
    fn importing_against_mixed_case_legacy_name_keeps_both_buffers() {
        let dir = std::env::temp_dir().join(format!("beatbox-legacy-case-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        for (p, n) in [(&a, 1000), (&b, 2000)] {
            crate::render::write_wav(p, &vec![0.1; n], &vec![0.1; n]).unwrap();
        }
        let mut e = crate::Engine::new(dir.clone());
        e.call("import_sample", &json!({"path":a,"name":"voice"}))
            .unwrap();
        e.project.samples[0].name = "Voice".into();
        let result = e
            .call("import_sample", &json!({"path":b,"name":"voice"}))
            .unwrap();
        assert_eq!(result["sample"], "voice_2");
        assert_eq!(e.bank.get("Voice").unwrap().len(), 1000);
        assert_eq!(e.bank.get("voice_2").unwrap().len(), 2000);
        assert!(e.bank.sync(&e.project.samples).is_empty());
        assert_eq!(e.bank.get("Voice").unwrap().len(), 1000);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project.samples.len(), 1);
        e.call("redo", &json!({})).unwrap();
        e.call("save_project", &json!({"path":"case.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"case.json"}))
            .unwrap();
        assert_eq!(e.project.samples[0].name, "Voice");
        assert_eq!(e.project.samples[1].name, "voice_2");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod pattern_rename_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/pattern-rename-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e
    }
    #[test]
    fn pattern_rename_references_ids_history_reopen() {
        let mut e = fixture();
        let old = e.project.patterns[0].name.clone();
        let id = e.project.patterns[0].id.clone();
        e.call("set_arrangement",&json!({"sections":[{"pattern":old.to_uppercase(),"repeats":2},{"pattern":id,"repeats":1}]})).unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        let opts = beatbox::render::RenderOptions {
            tail: 0.,
            ..Default::default()
        };
        let before = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        e.call(
            "pattern",
            &json!({"action":"rename_pattern","params":{"pattern":id,"new_name":"  Hook A  "}}),
        )
        .unwrap();
        let mut x = b.clone();
        x["patterns"][0]["name"] = json!("Hook A");
        for s in x["arrangement"].as_array_mut().unwrap() {
            s["pattern"] = json!("Hook A");
        }
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        let after = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(before.left, after.left);
        assert_eq!(before.right, after.right);
        let n = e.history().0.len();
        e.call("rename_pattern", &json!({"pattern":id,"new_name":"Hook A"}))
            .unwrap();
        assert_eq!(n, e.history().0.len());
        e.call(
            "save_project",
            &json!({"path":"/tmp/pattern-rename-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/pattern-rename-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
    }
    #[test]
    fn pattern_rename_playlist_recompile_audio() {
        let mut e = fixture();
        let old = e.project.patterns[0].name.clone();
        let id = e.project.patterns[0].id.clone();
        e.call(
            "place_pattern",
            &json!({"pattern":old,"start_bar":1,"bars":2,"lane":0}),
        )
        .unwrap();
        let opts = beatbox::render::RenderOptions {
            tail: 0.,
            ..Default::default()
        };
        let b = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        let notes = e.project.patterns[0].clips.clone();
        let clips = e.project.playlist.clone();
        e.call("rename_pattern", &json!({"pattern":id,"new_name":"Verse"}))
            .unwrap();
        assert_eq!(e.project.patterns[0].clips, notes);
        assert_eq!(e.project.playlist[0].pattern, "Verse");
        assert_eq!(e.project.playlist[0].id, clips[0].id);
        beatbox::tools_fl::compile_playlist(&mut e.project).unwrap();
        let a = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(b.left, a.left);
        assert_eq!(b.right, a.right);
    }
    #[test]
    fn pattern_rename_rejects_atomically() {
        let mut e = fixture();
        let old = e.project.patterns[0].name.clone();
        e.call("add_pattern", &json!({"name":"Other"})).unwrap();
        let other = e.project.patterns[1].id.clone();
        let b = serde_json::to_value(&e.project).unwrap();
        let n = e.history().0.len();
        for a in [
            json!({"pattern":old,"new_name":" "}),
            json!({"pattern":old,"new_name":"oThEr"}),
            json!({"pattern":old,"new_name":other}),
            json!({"pattern":old,"new_name":"PL:generated"}),
            json!({"pattern":"missing","new_name":"Verse"}),
            json!({"pattern":old,"new_name":7}),
        ] {
            assert!(e.call("rename_pattern", &a).is_err(), "accepted {a}");
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
            assert_eq!(n, e.history().0.len());
        }
        e.project
            .patterns
            .push(beatbox::project::Pattern::new("pl:hidden", 1));
        assert!(e
            .call(
                "rename_pattern",
                &json!({"pattern":"pl:hidden","new_name":"Visible"})
            )
            .is_err());
    }
}

#[cfg(test)]
mod pattern_duplicate_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/duplicate-pattern-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e
    }
    #[test]
    fn duplicate_preserves_material_not_identity_or_song() {
        let mut e = fixture();
        e.project.patterns[0].meter = Some((7, 8));
        let b = serde_json::to_value(&e.project).unwrap();
        let old = e.project.patterns[0].clone();
        let audio = beatbox::render::render(&e.project, &e.bank, &Default::default()).unwrap();
        let response=e.call("pattern",&json!({"action":"duplicate_pattern","params":{"pattern":old.id,"new_name":"  Hook variation  "}})).unwrap();
        assert_eq!(response["pattern"], "Hook variation");
        assert_eq!(response["placed_in_song"], false);
        let c = &e.project.patterns[1];
        assert_ne!(old.id, c.id);
        assert_eq!(c.name, "Hook variation");
        assert_eq!(c.bars, old.bars);
        assert_eq!(c.meter, old.meter);
        let mut expected = old.clone();
        expected.name = c.name.clone();
        expected.id = c.id.clone();
        for (t, notes) in &mut expected.clips {
            for (n, cn) in notes.iter_mut().zip(&c.clips[t]) {
                assert_ne!(n.id, cn.id);
                assert!(!cn.id.is_empty());
                n.id = cn.id.clone();
            }
        }
        assert_eq!(&expected, c);
        let mut after = serde_json::to_value(&e.project).unwrap();
        after["patterns"].as_array_mut().unwrap().pop();
        let mut base = b.clone();
        base.as_object_mut().unwrap().remove("id_seq");
        after.as_object_mut().unwrap().remove("id_seq");
        for (k, v) in base.as_object().unwrap() {
            if after.get(k) != Some(v) {
                panic!("unexpected changed field: {}", k);
            }
        }
        let a = beatbox::render::render(&e.project, &e.bank, &Default::default()).unwrap();
        assert_eq!(audio.left, a.left);
        assert_eq!(audio.right, a.right);
        e.call(
            "save_project",
            &json!({"path":"/tmp/duplicate-pattern-reopen.json"}),
        )
        .unwrap();
        let saved = serde_json::to_value(&e.project).unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/duplicate-pattern-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), saved);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), saved);
    }
    #[test]
    fn duplicate_names_are_safe_and_copies_independent() {
        let mut e = fixture();
        let source = e.project.patterns[0].clone();
        for n in [
            format!("{} copy", source.name),
            format!("{} copy 2", source.name),
        ] {
            let a = e
                .call("duplicate_pattern", &json!({"pattern":source.id}))
                .unwrap();
            assert_eq!(a["pattern"], n);
        }
        let copy = e.project.patterns[1].clone();
        let track = copy.clips.keys().next().unwrap().clone();
        e.project.patterns[1].clips.get_mut(&track).unwrap()[0].vel = 0.123;
        assert_eq!(e.project.patterns[0], source);
        let count = e.project.patterns.len();
        let bads = [
            json!({"pattern":source.id,"new_name":" "}),
            json!({"pattern":source.id,"new_name":"PL:generated"}),
            json!({"pattern":source.id,"new_name":source.name.to_uppercase()}),
            json!({"pattern":source.id,"new_name":source.id.to_uppercase()}),
            json!({"pattern":source.id,"new_name":7}),
            json!({"pattern":"missing"}),
        ];
        let b = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        for a in bads {
            assert!(e.call("duplicate_pattern", &a).is_err(), "{a}");
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
            assert_eq!(h, e.history().0.len());
        }
        assert_eq!(count, e.project.patterns.len());
        e.project
            .patterns
            .push(beatbox::project::Pattern::new("pl:generated", 1));
        assert!(e
            .call("duplicate_pattern", &json!({"pattern":"pl:generated"}))
            .is_err());
    }
    #[test]
    fn duplicate_playlist_does_not_add_or_retarget_clips() {
        let mut e = fixture();
        let source = e.project.patterns[0].clone();
        e.call(
            "place_pattern",
            &json!({"pattern":source.name,"start_bar":1,"bars":2,"lane":0}),
        )
        .unwrap();
        let clips = e.project.playlist.clone();
        e.call("duplicate_pattern", &json!({"pattern":source.id}))
            .unwrap();
        assert_eq!(clips, e.project.playlist);
    }
}

#[cfg(test)]
mod clear_solo_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/clear-solo-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.call(
            "set_mixer",
            &json!({"track":"drums","solo":true,"mute":true,"volume_db":-8,"pan":0.25}),
        )
        .unwrap();
        e.call("set_mixer", &json!({"track":"piano","solo":true}))
            .unwrap();
        e.call("add_bus", &json!({"name":"Room"})).unwrap();
        e.call(
            "set_mixer",
            &json!({"track":"Room","mute":true,"volume_db":-4}),
        )
        .unwrap();
        e
    }
    #[test]
    fn clear_solo_scope_history_reopen() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        let r = e
            .call("mixer", &json!({"action":"clear_track_solos","params":{}}))
            .unwrap();
        assert_eq!(r["cleared"], json!(["drums", "piano"]));
        let mut x = b.clone();
        for t in x["tracks"].as_array_mut().unwrap() {
            t["solo"] = json!(false);
        }
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        let h = e.history().0.len();
        e.call("clear_track_solos", &json!({})).unwrap();
        assert_eq!(h, e.history().0.len());
        e.call(
            "save_project",
            &json!({"path":"/tmp/clear-solo-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/clear-solo-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
    }
    #[test]
    fn clear_solo_noop_and_mutes_preserved() {
        let mut e = fixture();
        e.project.tracks.iter_mut().for_each(|t| t.solo = false);
        let b = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        let rev = e.revision;
        let r = e.call("clear_track_solos", &json!({})).unwrap();
        assert_eq!(r["cleared"], json!([]));
        assert_eq!(b, serde_json::to_value(&e.project).unwrap());
        assert_eq!(h, e.history().0.len());
        assert_eq!(rev, e.revision);
        e.project.tracks.clear();
        let b = e.project.clone();
        e.call("clear_track_solos", &json!({})).unwrap();
        assert_eq!(b, e.project);
    }
    #[test]
    fn clear_solo_render_matches_manual_flags() {
        let mut e = fixture();
        e.project.tracks[0].solo = false;
        let opts = beatbox::render::RenderOptions {
            tail: 0.,
            ..Default::default()
        };
        let mut expected = e.project.clone();
        expected.tracks.iter_mut().for_each(|t| t.solo = false);
        let off = beatbox::render::render(&expected, &e.bank, &opts).unwrap();
        e.call("clear_track_solos", &json!({})).unwrap();
        assert!(e.project.tracks[0].mute);
        assert!(e.project.buses[0].mute);
        let actual = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(actual.left, off.left);
        assert_eq!(actual.right, off.right);
    }
}

#[cfg(test)]
mod exclusive_solo_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/exclusive-solo-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.call(
            "set_mixer",
            &json!({"track":"drums","solo":true,"mute":true,"volume_db":-8,"pan":0.25}),
        )
        .unwrap();
        e.call("set_mixer", &json!({"track":"piano","solo":true}))
            .unwrap();
        e.call("add_bus", &json!({"name":"Room"})).unwrap();
        e.call("set_mixer", &json!({"track":"Room","mute":true}))
            .unwrap();
        e
    }
    #[test]
    fn exclusive_scope_reopen_undo_noop() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        let id = e.project.tracks[0].id.clone();
        let r = e
            .call(
                "mixer",
                &json!({"action":"solo_track_exclusive","params":{"track":id}}),
            )
            .unwrap();
        assert_eq!(r["track_muted"], true);
        assert_eq!(r["cleared"], json!(["piano"]));
        let mut x = b.clone();
        x["tracks"][1]["solo"] = json!(false);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        let h = e.history().0.len();
        let rev = e.revision;
        e.call("solo_track_exclusive", &json!({"track":"DRUMS"}))
            .unwrap();
        assert_eq!(h, e.history().0.len());
        assert_eq!(rev, e.revision);
        e.call(
            "save_project",
            &json!({"path":"/tmp/exclusive-solo-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/exclusive-solo-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
    }
    #[test]
    fn exclusive_rejections_atomic_and_switch() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        for a in [
            json!({"track":"missing"}),
            json!({"track":"Room"}),
            json!({}),
            json!({"track":7}),
        ] {
            assert!(e.call("solo_track_exclusive", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
            assert_eq!(h, e.history().0.len());
        }
        e.call("clear_track_solos", &json!({})).unwrap();
        e.call("solo_track_exclusive", &json!({"track":"piano"}))
            .unwrap();
        assert!(!e.project.tracks[0].solo);
        assert!(e.project.tracks[1].solo);
        assert!(e.project.tracks[0].mute);
        e.project.tracks.clear();
        assert!(e
            .call("solo_track_exclusive", &json!({"track":"piano"}))
            .is_err());
    }
    #[test]
    fn exclusive_render_matches_manual() {
        let mut e = fixture();
        let opts = beatbox::render::RenderOptions {
            tail: 0.,
            ..Default::default()
        };
        let mut expected = e.project.clone();
        expected.tracks[0].solo = false;
        expected.tracks[1].solo = true;
        let off = beatbox::render::render(&expected, &e.bank, &opts).unwrap();
        e.call("solo_track_exclusive", &json!({"track":"piano"}))
            .unwrap();
        let a = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(off.left, a.left);
        assert_eq!(off.right, a.right);
    }
}

#[cfg(test)]
mod offset_faders_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/offset-faders-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.call(
            "set_mixer",
            &json!({"track":"drums","mute":true,"solo":true,"pan":0.3}),
        )
        .unwrap();
        e.call("add_bus", &json!({"name":"Room"})).unwrap();
        e
    }
    #[test]
    fn fader_scope_undo_reopen() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        let r = e
            .call(
                "mixer",
                &json!({"action":"offset_track_faders","params":{"delta_db":-3}}),
            )
            .unwrap();
        assert_eq!(r["changed_tracks"], 2);
        let mut x = b.clone();
        for t in x["tracks"].as_array_mut().unwrap() {
            t["volume_db"] = json!(t["volume_db"].as_f64().unwrap() - 3.0);
        }
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        assert_eq!(e.history().0.len(), h + 1);
        e.call(
            "save_project",
            &json!({"path":"/tmp/offset-faders-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/offset-faders-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
    }
    #[test]
    fn fader_reject_limits_automation_atomic() {
        let mut e = fixture();
        for a in [
            json!({}),
            json!({"delta_db":"1"}),
            json!({"delta_db":13}),
            json!({"delta_db":-13}),
        ] {
            let b = serde_json::to_value(&e.project).unwrap();
            let rev = e.revision;
            let h = e.history().0.len();
            assert!(e.call("offset_track_faders", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
            assert_eq!(e.revision, rev);
            assert_eq!(e.history().0.len(), h);
        }
        for v in [-60., 12.] {
            e.project.tracks[1].volume_db = v;
            let b = serde_json::to_value(&e.project).unwrap();
            assert!(e
                .call(
                    "offset_track_faders",
                    &json!({"delta_db":if v<0.{-1.}else{1.}})
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.project.tracks[1].volume_db = -3.;
        let mut lane = beatbox::automation::AutomationLane::new("DRUMS", "VOLUME");
        lane.points.push(beatbox::automation::AutoPoint {
            beat: 0.,
            value: -4.,
            curve: Default::default(),
        });
        e.project.automation.push(lane);
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call("offset_track_faders", &json!({"delta_db":-1}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.project.automation[0].enabled = false;
        e.call("offset_track_faders", &json!({"delta_db":-1}))
            .unwrap();
        e.project.automation[0].enabled = true;
        e.project.automation[0].points.clear();
        e.call("offset_track_faders", &json!({"delta_db":1}))
            .unwrap();
    }
    #[test]
    fn fader_noop_boundaries_render() {
        let mut e = fixture();
        for delta in [0., -0.] {
            let h = e.history().0.len();
            let rev = e.revision;
            e.call("offset_track_faders", &json!({"delta_db":delta}))
                .unwrap();
            assert_eq!(e.history().0.len(), h);
            assert_eq!(e.revision, rev);
        }
        let mut expected = e.project.clone();
        for t in &mut expected.tracks {
            t.volume_db -= 1.;
        }
        let opts = beatbox::render::RenderOptions {
            tail: 0.,
            ..Default::default()
        };
        let a = beatbox::render::render(&expected, &e.bank, &opts).unwrap();
        e.call("offset_track_faders", &json!({"delta_db":-1}))
            .unwrap();
        let b = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(a.left, b.left);
        assert_eq!(a.right, b.right);
        e.project.tracks[0].volume_db = -59.;
        e.call("offset_track_faders", &json!({"delta_db":-1}))
            .unwrap();
        assert_eq!(e.project.tracks[0].volume_db, -60.);
        e.project.tracks.clear();
        let h = e.history().0.len();
        let rev = e.revision;
        assert_eq!(
            e.call("offset_track_faders", &json!({"delta_db":12}))
                .unwrap()["changed_tracks"],
            0
        );
        assert_eq!(e.history().0.len(), h);
        assert_eq!(e.revision, rev);
    }
}

#[cfg(test)]
mod insert_bypass_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/insert-bypass-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.call(
            "add_effect",
            &json!({"track":"drums","type":"reverb","params":{"mix":0.12}}),
        )
        .unwrap();
        e.call(
            "bypass_effect",
            &json!({"track":"drums","index":1,"on":true}),
        )
        .unwrap();
        e.call("add_bus", &json!({"name":"Room"})).unwrap();
        e.call("add_effect", &json!({"track":"Room","type":"reverb"}))
            .unwrap();
        e
    }
    #[test]
    fn insert_scope_mixed_undo_reopen() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(!e.project.tracks[0].effects[0].bypassed());
        assert!(e.project.tracks[0].effects[1].bypassed());
        let id = e.project.tracks[0].id.clone();
        let h = e.history().0.len();
        let r = e
            .call(
                "effects",
                &json!({"action":"set_track_insert_bypass","params":{"track":id,"bypass":true}}),
            )
            .unwrap();
        assert_eq!(r["changed"], 1);
        let mut expected = e.project.clone();
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        for fx in &mut expected.tracks[0].effects {
            fx.set_bypass(false);
        }
        e.call(
            "set_track_insert_bypass",
            &json!({"track":"DRUMS","bypass":false}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&e.project).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(e.history().0.len(), h + 1);
        e.call(
            "save_project",
            &json!({"path":"/tmp/insert-bypass-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/insert-bypass-reopen.json"}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&f.project).unwrap(),
            serde_json::to_value(&e.project).unwrap()
        );
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
    #[test]
    fn insert_atomic_reject_noop_empty() {
        let mut e = fixture();
        for a in [
            json!({"track":"drums"}),
            json!({"track":"drums","bypass":1}),
            json!({"track":"Room","bypass":true}),
            json!({"track":"master","bypass":true}),
            json!({"track":"missing","bypass":true}),
        ] {
            let b = serde_json::to_value(&e.project).unwrap();
            let h = e.history().0.len();
            let rev = e.revision;
            assert!(e.call("set_track_insert_bypass", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
            assert_eq!(h, e.history().0.len());
            assert_eq!(rev, e.revision);
        }
        e.call(
            "set_track_insert_bypass",
            &json!({"track":"drums","bypass":true}),
        )
        .unwrap();
        let h = e.history().0.len();
        let rev = e.revision;
        assert_eq!(
            e.call(
                "set_track_insert_bypass",
                &json!({"track":"drums","bypass":true})
            )
            .unwrap()["changed"],
            0
        );
        assert_eq!(h, e.history().0.len());
        assert_eq!(rev, e.revision);
        e.project.tracks[0].effects.clear();
        assert_eq!(
            e.call(
                "set_track_insert_bypass",
                &json!({"track":"drums","bypass":false})
            )
            .unwrap()["changed"],
            0
        );
        assert_eq!(rev, e.revision);
    }
    #[test]
    fn insert_render_and_exact_scope() {
        let mut e = fixture();
        let mut expected = e.project.clone();
        for fx in &mut expected.tracks[0].effects {
            fx.set_bypass(true);
        }
        let opts = beatbox::render::RenderOptions {
            tail: 0.,
            ..Default::default()
        };
        let a = beatbox::render::render(&expected, &e.bank, &opts).unwrap();
        e.call(
            "set_track_insert_bypass",
            &json!({"track":"drums","bypass":true}),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&e.project).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        let b = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(a.left, b.left);
        assert_eq!(a.right, b.right);
        e.call("undo", &json!({})).unwrap();
        e.call("redo", &json!({})).unwrap();
        assert_eq!(
            serde_json::to_value(&e.project).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
    }
}

#[cfg(test)]
mod checkpoint_load_tests {
    use crate as beatbox;
    use serde_json::json;
    use std::fs;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/checkpoint-load-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.call("snapshot", &json!({"name":"previous-song"}))
            .unwrap();
        e
    }
    #[test]
    fn missing_and_bad_companion_clear_previous() {
        for (n, side) in [
            ("missing", None),
            ("bad", Some("not json")),
            ("wrongshape", Some("{}")),
            ("empty", Some("[]")),
        ] {
            let mut e = fixture();
            let path = format!("/tmp/checkpoint-load-{n}.json");
            fs::write(&path, serde_json::to_string(&e.project).unwrap()).unwrap();
            let companion = format!("{path}.snapshots.json");
            let _ = fs::remove_file(&companion);
            if let Some(s) = side {
                fs::write(&companion, s).unwrap();
            }
            let original = serde_json::to_value(&e.project).unwrap();
            e.call("load_project", &json!({"path":path})).unwrap();
            assert!(e.snapshots.is_empty(), "{n}");
            assert_eq!(serde_json::to_value(&e.project).unwrap(), original);
            assert!(e
                .call("restore_snapshot", &json!({"name":"previous-song"}))
                .is_err());
        }
    }
    #[test]
    fn valid_companion_only_new_song() {
        let mut e = fixture();
        let mut other = e.project.clone();
        other.name = "Second song".into();
        other.tracks[0].volume_db = -15.;
        let path = "/tmp/checkpoint-load-valid.json";
        fs::write(path, serde_json::to_string(&other).unwrap()).unwrap();
        fs::write(format!("{path}.snapshots.json"),serde_json::to_string(&json!([{"name":"second-song-before","note":"new song","revision":7,"project":other}])).unwrap()).unwrap();
        let r = e.call("load_project", &json!({"path":path})).unwrap();
        assert_eq!(r["snapshots_loaded"], 1);
        assert_eq!(e.snapshots.len(), 1);
        assert_eq!(e.snapshots[0].0, "second-song-before");
        e.call("set_mixer", &json!({"track":"drums","volume_db":-9}))
            .unwrap();
        e.call("restore_snapshot", &json!({"name":"second-song-before"}))
            .unwrap();
        assert_eq!(
            serde_json::to_value(&e.project).unwrap(),
            serde_json::to_value(&other).unwrap()
        );
    }
    #[test]
    fn failed_main_load_keeps_project_and_snapshots() {
        let mut e = fixture();
        let before = serde_json::to_value(&e.project).unwrap();
        let rev = e.revision;
        let h = e.history().0.len();
        let path = "/tmp/checkpoint-load-badmain.json";
        fs::write(path, "not a project").unwrap();
        for p in [path, "/tmp/checkpoint-load-nonexistent-214.json"] {
            assert!(e.call("load_project", &json!({"path":p})).is_err());
            assert_eq!(e.snapshots.len(), 1);
            assert_eq!(e.snapshots[0].0, "previous-song");
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
            assert_eq!(rev, e.revision);
            assert_eq!(h, e.history().0.len());
        }
    }
}
