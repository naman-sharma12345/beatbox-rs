//! Piano-roll / MIDI tools (midi namespace), Standard MIDI File import and
//! export, and arrangement helpers: song structure from a brief, section
//! variations, and transitions (risers, impacts, reverse cymbals, fills,
//! filter builds, stutters, tape stops, drop gaps).

use crate::automation::{self, AutoPoint, Curve, Shape};
use crate::dsp::{Adsr, FilterMode, Rng, Wave};
use crate::engine::Engine;
use crate::fx::{Effect, FilterFx, ReverbFx, StutterFx, StutterMode};
use crate::instruments::{self, DrumKind, Instrument, SynthParams};
use crate::midi_ops::{self as mo, EditSpec, Selection};
use crate::project::{Note, Pattern, Project, Section, Track};
use crate::smf;
use crate::theory;
use crate::tools::{
    b_or, f_opt, f_or, grid, instrument_from, key_pc, notes_json, obj, pattern_idx, pattern_prop,
    pitch_of, s_opt, s_req, seed_of, u_or, Tool,
};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

// ---------------- shared helpers ----------------

/// Musical role of a track, inferred from its instrument and name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Kick,
    Snare,
    Hat,
    Perc,
    Bass,
    Chords,
    Lead,
    Fx,
}

pub fn role_of(t: &Track) -> Role {
    let n = t.name.to_lowercase();
    let has = |k: &str| n.contains(k);
    if has("riser") || has("impact") || has("sweep") || has("fx_") || has("cymbal_rev") {
        return Role::Fx;
    }
    if let Instrument::Drum(d) = &t.instrument {
        return match d.kind {
            DrumKind::Kick => Role::Kick,
            DrumKind::Snare | DrumKind::Clap | DrumKind::Rim => Role::Snare,
            DrumKind::ClosedHat | DrumKind::OpenHat | DrumKind::Shaker => Role::Hat,
            _ => Role::Perc,
        };
    }
    if has("kick") {
        return Role::Kick;
    }
    if has("snare") || has("clap") {
        return Role::Snare;
    }
    if has("hat") || has("shaker") {
        return Role::Hat;
    }
    if t.instrument.is_drum() || has("perc") || has("tom") || has("drum") {
        return Role::Perc;
    }
    if has("bass") || has("808") || matches!(t.instrument, Instrument::Bass808(_)) {
        return Role::Bass;
    }
    if has("chord") || has("pad") || has("key") || has("piano") || has("string") || has("organ") {
        return Role::Chords;
    }
    Role::Lead
}

pub fn is_drum_role(r: Role) -> bool {
    matches!(r, Role::Kick | Role::Snare | Role::Hat | Role::Perc)
}

/// Note selection from tool arguments.
pub fn selection_of(a: &Value) -> Result<Selection> {
    let mut s = Selection::default();
    if let Some(f) = f_opt(a, "from") {
        s.from = f;
    }
    if let Some(t) = f_opt(a, "to") {
        s.to = t;
    }
    if let Some(Value::Array(b)) = a.get("bars") {
        if let (Some(b0), Some(b1)) = (
            b.first().and_then(|v| v.as_f64()),
            b.get(1).and_then(|v| v.as_f64()),
        ) {
            s.from = b0 as f32 * 16.0;
            s.to = b1 as f32 * 16.0;
        }
    }
    if let Some(v) = a.get("pitch") {
        let p = pitch_of(v)?;
        s.pitches = vec![p];
    }
    if let Some(Value::Array(ps)) = a.get("pitches") {
        s.pitches = ps.iter().map(pitch_of).collect::<Result<_>>()?;
    }
    if let Some(v) = a.get("pitch_min") {
        s.pitch_min = pitch_of(v)?;
    }
    if let Some(v) = a.get("pitch_max") {
        s.pitch_max = pitch_of(v)?;
    }
    if let Some(v) = f_opt(a, "vel_min") {
        s.vel_min = v;
    }
    if let Some(v) = f_opt(a, "vel_max") {
        s.vel_max = v;
    }
    Ok(s)
}

pub fn selection_props() -> Value {
    json!({
        "from": {"type": "number", "description": "Select notes starting at/after this step (16 per bar)"},
        "to": {"type": "number", "description": "...and before this step"},
        "bars": {"type": "array", "items": {"type": "number"}, "description": "[first_bar, end_bar) 0-based, alternative to from/to"},
        "pitch": {"description": "Only this pitch (MIDI or 'C4')"},
        "pitches": {"type": "array", "items": {}},
        "pitch_min": {}, "pitch_max": {},
        "vel_min": {"type": "number"}, "vel_max": {"type": "number"}
    })
}

/// Merge `extra` object properties into a base properties object.
fn props(base: Value, extra: Value) -> Value {
    let mut b = base;
    if let (Value::Object(m), Value::Object(x)) = (&mut b, extra) {
        m.extend(x);
    }
    b
}

fn target_props() -> Value {
    json!({
        "track": {"type": "string"},
        "pattern": pattern_prop(),
        "all_patterns": {"type": "boolean", "description": "Apply in every pattern (default: just `pattern` / the first)"}
    })
}

/// Pattern indices a note edit applies to.
fn target_patterns(p: &Project, a: &Value) -> Result<Vec<usize>> {
    if b_or(a, "all_patterns", false) {
        Ok((0..p.patterns.len()).collect())
    } else {
        Ok(vec![pattern_idx(p, a)?])
    }
}

/// Run `f` on the target track's notes in each target pattern.
fn with_notes(
    e: &mut Engine,
    a: &Value,
    mut f: impl FnMut(&mut Vec<Note>, f32, &Track) -> Result<usize>,
) -> Result<Value> {
    let ti = e.project.track_index(&s_req(a, "track")?)?;
    let track = e.project.tracks[ti].clone();
    let pats = target_patterns(&e.project, a)?;
    let mut total = 0;
    let mut out = Vec::new();
    for pi in pats {
        let pat = &mut e.project.patterns[pi];
        let steps = pat.steps() as f32;
        let notes = pat.notes_mut(&track.name);
        let k = f(notes, steps, &track)?;
        notes.retain(|n| n.start < steps);
        mo::sort(notes);
        total += k;
        let preview = if track.instrument.is_drum() {
            json!(grid(notes, pat_steps(steps)))
        } else {
            json!(notes.len())
        };
        out.push(json!({"pattern": pat.name, "affected": k, "notes": preview}));
    }
    Ok(json!({"track": track.name, "affected": total, "patterns": out}))
}

fn pat_steps(s: f32) -> u32 {
    s as u32
}

/// Grid / rate in steps: number, or "1/16", "1/8", "1/8t", "1/4.", "1 bar".
pub fn steps_of(v: Option<&Value>, default: f32) -> Result<f32> {
    match v {
        None => Ok(default),
        Some(x) => Ok(crate::tools_studio::parse_rate(Some(x))? * 4.0),
    }
}

fn scale_of(p: &Project) -> Result<&'static [u8]> {
    theory::scale_intervals(&p.scale)
}

// ---------------- song walking (for export) ----------------

/// Every note of the song in absolute steps, per track, honouring
/// arrangement, repeats, probability and microtiming like the renderer.
pub fn song_notes(p: &Project) -> Vec<Vec<(f32, Note)>> {
    let mut out = vec![Vec::new(); p.tracks.len()];
    let swing = p.swing.clamp(0.0, 1.0) * 0.5;
    let mut off = 0u32;
    for sec in p.song_sections() {
        let Ok(pi) = p.pattern_index(&sec.pattern) else {
            continue;
        };
        let pat = &p.patterns[pi];
        for _ in 0..sec.repeats.max(1) {
            for (ti, t) in p.tracks.iter().enumerate() {
                for (ni, n) in pat.notes(&t.name).iter().enumerate() {
                    if n.prob < 1.0 && !crate::render::chance_hit(ti, ni, off, n.prob) {
                        continue;
                    }
                    let mut s = n.start;
                    if s.fract() == 0.0 && (s as i64) % 2 == 1 {
                        s += swing;
                    }
                    s += n.offset;
                    out[ti].push(((off as f32 + s).max(0.0), n.clone()));
                }
            }
            off += pat.steps();
        }
    }
    out
}

fn gm_drum_note(t: &Track) -> Option<u8> {
    let kind = match &t.instrument {
        Instrument::Drum(d) => d.kind,
        Instrument::Layer(l) => match l.layers.first().map(|x| &x.instrument) {
            Some(Instrument::Drum(d)) => d.kind,
            _ => return None,
        },
        _ => return None,
    };
    Some(match kind {
        DrumKind::Kick => 36,
        DrumKind::Snare => 38,
        DrumKind::Clap => 39,
        DrumKind::ClosedHat => 42,
        DrumKind::OpenHat => 46,
        DrumKind::Rim => 37,
        DrumKind::Tom => 45,
        DrumKind::Cowbell => 56,
        DrumKind::Shaker => 70,
        DrumKind::Crash => 49,
    })
}

fn gm_program(t: &Track) -> u8 {
    match role_of(t) {
        Role::Bass => 38,
        Role::Chords => match &t.instrument {
            Instrument::Fm(_) => 4,
            _ => 89,
        },
        Role::Lead => match &t.instrument {
            Instrument::Pluck(_) => 25,
            Instrument::Fm(_) => 11,
            _ => 81,
        },
        _ => 0,
    }
}

fn drum_preset_for_gm(n: u8) -> (&'static str, &'static str) {
    match n {
        35 | 36 => ("kick", "kick"),
        38 | 40 => ("snare", "snare"),
        39 => ("clap", "clap"),
        37 => ("rim", "rim"),
        42 | 44 => ("hat", "hat"),
        46 => ("open_hat", "open_hat"),
        41 | 43 | 45 | 47 | 48 | 50 => ("tom", "tom"),
        49 | 52 | 55 | 57 => ("crash", "crash"),
        51 | 53 | 59 => ("ride", "hat"),
        56 => ("cowbell", "cowbell"),
        69 | 70 | 82 => ("shaker", "shaker"),
        _ => ("perc", "rim"),
    }
}

fn preset_for_program(p: Option<u8>, low: bool) -> &'static str {
    match p.unwrap_or(0) {
        _ if low => "sub_bass",
        0..=7 => "epiano",
        8..=15 => "fm_bell",
        16..=23 => "organ",
        24..=31 => "guitar_pluck",
        32..=39 => "sub_bass",
        40..=55 => "strings",
        56..=63 => "brass_stab",
        80..=87 => "pluck_lead",
        88..=95 => "warm_pad",
        _ => "epiano",
    }
}

// ---------------- arrangement helpers ----------------

/// Make arrangement section `si` play a pattern used by no other section,
/// splitting off the last (`last=true`) or first repeat. Returns the new
/// section index and pattern index.
pub fn unique_section(
    p: &mut Project,
    si: usize,
    last: bool,
    suffix: &str,
) -> Result<(usize, usize)> {
    if p.arrangement.is_empty() {
        p.arrangement = p.song_sections();
    }
    let sec = p
        .arrangement
        .get(si)
        .cloned()
        .ok_or_else(|| anyhow!("no arrangement section {si}"))?;
    let pi = p.pattern_index(&sec.pattern)?;
    let shared = sec.repeats > 1
        || p.arrangement
            .iter()
            .enumerate()
            .any(|(i, s)| i != si && s.pattern.eq_ignore_ascii_case(&sec.pattern));
    if !shared {
        return Ok((si, pi));
    }
    let mut np = p.patterns[pi].clone();
    let mut name = format!("{}_{suffix}", sec.pattern);
    let mut k = 2;
    while p.pattern_index(&name).is_ok() {
        name = format!("{}_{suffix}{k}", sec.pattern);
        k += 1;
    }
    np.name = name.clone();
    p.patterns.push(np);
    let npi = p.patterns.len() - 1;
    let one = Section {
        pattern: name,
        repeats: 1,
    };
    if sec.repeats > 1 {
        let rest = Section {
            pattern: sec.pattern.clone(),
            repeats: sec.repeats - 1,
        };
        if last {
            p.arrangement.splice(si..=si, [rest, one]);
            Ok((si + 1, npi))
        } else {
            p.arrangement.splice(si..=si, [one, rest]);
            Ok((si, npi))
        }
    } else {
        p.arrangement[si] = one;
        Ok((si, npi))
    }
}

/// Arrangement index of the section a transition goes *into*.
fn into_index(p: &Project, key: &str) -> Result<usize> {
    let secs = p.song_sections();
    if let Ok(i) = key.trim().parse::<usize>() {
        if i == 0 || i >= secs.len() {
            bail!(
                "'into' must be a section index 1..{} (the section after the boundary)",
                secs.len().saturating_sub(1)
            );
        }
        return Ok(i);
    }
    secs.iter()
        .enumerate()
        .skip(1)
        .find(|(_, s)| s.pattern.eq_ignore_ascii_case(key))
        .map(|(i, _)| i)
        .ok_or_else(|| {
            anyhow!(
                "no section '{key}' after the first. Arrangement: [{}]",
                secs.iter()
                    .map(|s| s.pattern.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn add_or_get_track(p: &mut Project, name: &str, inst: Instrument, vol: f32) -> String {
    match p.track_index(name) {
        Ok(i) => {
            p.tracks[i].instrument = inst;
            p.tracks[i].name.clone()
        }
        Err(_) => {
            let mut t = Track::new(name, inst);
            t.volume_db = vol;
            p.tracks.push(t);
            name.to_string()
        }
    }
}

/// Insert an effect into a chain at `idx`, shifting later fx automation lanes.
pub fn insert_effect(p: &mut Project, owner: &str, idx: usize, fx: Effect) -> Result<usize> {
    let chain = crate::tools::effects_of(p, owner)?;
    let idx = idx.min(chain.len());
    chain.insert(idx, fx);
    for l in p.automation.iter_mut() {
        if !l.target.eq_ignore_ascii_case(owner) {
            continue;
        }
        if let Some(automation::Target::Fx(i, path)) = automation::parse_target(&l.param) {
            if i >= idx {
                l.param = format!("fx.{}.{}", i + 1, path.join("."));
            }
        }
    }
    Ok(idx)
}

/// Index just before the master limiter (or the end).
fn pre_limiter_index(p: &Project) -> usize {
    p.master_effects
        .iter()
        .rposition(|e| matches!(e, Effect::Limiter(_)))
        .unwrap_or(p.master_effects.len())
}

fn write_lane(
    p: &mut Project,
    owner: &str,
    param: &str,
    pts: Vec<AutoPoint>,
    start: f32,
    end: f32,
) {
    let lane = crate::tools_studio::lane_mut(p, owner, param);
    lane.points
        .retain(|q| q.beat < start - 1e-4 || q.beat > end + 1e-4);
    lane.points.extend(pts);
    lane.enabled = true;
    lane.sort();
}

fn pt(beat: f32, value: f32, curve: Curve) -> AutoPoint {
    AutoPoint { beat, value, curve }
}

/// Drum fill over the last `beats` of a pattern on the given tracks.
fn write_fill(
    pat: &mut Pattern,
    tracks: &[Track],
    style: &str,
    beats: f32,
    intensity: f32,
    rng: &mut Rng,
) -> Result<Vec<String>> {
    let steps = pat.steps() as f32;
    let start = (steps - beats * 4.0).max(0.0);
    let mut touched = Vec::new();
    let find = |r: Role| {
        tracks
            .iter()
            .find(|t| role_of(t) == r)
            .map(|t| t.name.clone())
    };
    let snare = find(Role::Snare);
    let kick = find(Role::Kick);
    let hat = find(Role::Hat);
    let perc = tracks
        .iter()
        .find(|t| matches!(&t.instrument, Instrument::Drum(d) if d.kind == DrumKind::Tom))
        .map(|t| t.name.clone());
    let clear = |pat: &mut Pattern, name: &str| {
        pat.notes_mut(name).retain(|n| n.start < start);
    };
    match style {
        "snare_roll" | "roll" => {
            let s = snare.ok_or_else(|| anyhow!("no snare/clap track for a snare roll"))?;
            clear(pat, &s);
            let n = (beats * 4.0) as usize;
            for i in 0..n {
                let t = start + i as f32;
                let f = i as f32 / n.max(1) as f32;
                // 16ths, then 32nds in the last beat
                let sub = if t >= steps - 4.0 && intensity > 0.4 { 2 } else { 1 };
                for k in 0..sub {
                    pat.notes_mut(&s).push(Note::new(
                        t + k as f32 / sub as f32,
                        0.5,
                        60,
                        (0.35 + 0.65 * f).clamp(0.2, 1.0),
                    ));
                }
            }
            touched.push(s);
        }
        "tom_run" | "toms" => {
            let name = perc.or(snare).ok_or_else(|| anyhow!("no tom or snare track"))?;
            clear(pat, &name);
            let n = (beats * 4.0) as usize;
            for i in 0..n {
                if i % 4 == 3 && intensity < 0.7 {
                    continue;
                }
                let pitch = (72 - (i as i32 * 12 / n.max(1) as i32)).clamp(48, 84) as u8;
                pat.notes_mut(&name)
                    .push(Note::new(start + i as f32, 1.0, pitch, 0.7 + 0.3 * i as f32 / n as f32));
            }
            touched.push(name);
        }
        "hat_roll" => {
            let h = hat.ok_or_else(|| anyhow!("no hat track for a hat roll"))?;
            clear(pat, &h);
            let mut t = start;
            let mut i = 0;
            while t < steps - 0.01 {
                let div = if i % 8 < 4 { 2.0 } else { 3.0 };
                for k in 0..div as usize {
                    pat.notes_mut(&h).push(Note::new(
                        t + k as f32 / div,
                        0.3,
                        60,
                        0.45 + 0.4 * ((k as f32) / div),
                    ));
                }
                t += 1.0;
                i += 1;
            }
            touched.push(h);
        }
        "kick_build" => {
            let k = kick.ok_or_else(|| anyhow!("no kick track"))?;
            clear(pat, &k);
            let mut t = start;
            let mut step = 4.0;
            while t < steps - 0.01 {
                pat.notes_mut(&k).push(Note::new(t, 1.0, 60, 0.9));
                t += step;
                if t >= start + (steps - start) / 2.0 {
                    step = 2.0;
                }
                if t >= steps - 4.0 {
                    step = 1.0;
                }
            }
            touched.push(k);
        }
        "break" | "drop_out" => {
            for t in tracks.iter().filter(|t| is_drum_role(role_of(t))) {
                clear(pat, &t.name);
                touched.push(t.name.clone());
            }
        }
        "auto" | "classic" => {
            let s = snare.clone().ok_or_else(|| anyhow!("no snare/clap track for a fill"))?;
            clear(pat, &s);
            let n = (beats * 4.0) as usize;
            for i in 0..n {
                if rng.chance(0.35 + intensity * 0.6) || i + 4 >= n {
                    let v = 0.4 + 0.6 * i as f32 / n.max(1) as f32;
                    pat.notes_mut(&s).push(Note::new(start + i as f32, 0.5, 60, v));
                    if rng.chance(intensity * 0.5) {
                        pat.notes_mut(&s)
                            .push(Note::new(start + i as f32 + 0.5, 0.5, 60, v * 0.7));
                    }
                }
            }
            touched.push(s);
            if let Some(k) = kick {
                pat.notes_mut(&k).retain(|n| n.start < steps - 2.0);
                touched.push(k);
            }
        }
        other => bail!("unknown fill style '{other}'. Options: auto, snare_roll, tom_run, hat_roll, kick_build, break"),
    }
    for t in &touched {
        mo::sort(pat.notes_mut(t));
    }
    Ok(touched)
}

fn riser_synth(secs: f32) -> Instrument {
    Instrument::Synth(SynthParams {
        osc1: Wave::Noise,
        osc2: Wave::Saw,
        osc_mix: 0.25,
        unison: 3,
        filter_mode: FilterMode::Bandpass,
        cutoff: 1200.0,
        resonance: 0.35,
        pitch_env_semitones: -24.0,
        pitch_env_time: (secs / 2.5).max(0.05),
        amp_env: Adsr::new((secs * 0.8).max(0.05), 0.01, 1.0, 0.05),
        gain: 0.45,
        ..Default::default()
    })
}

fn reverse_cymbal_synth(secs: f32) -> Instrument {
    Instrument::Synth(SynthParams {
        osc1: Wave::Noise,
        osc2: Wave::Noise,
        filter_mode: FilterMode::Highpass,
        cutoff: 5500.0,
        resonance: 0.1,
        amp_env: Adsr::new(secs.max(0.05), 0.01, 1.0, 0.03),
        gain: 0.35,
        ..Default::default()
    })
}

fn impact_layer() -> Instrument {
    Instrument::Layer(instruments::LayerParams {
        layers: vec![
            instruments::Layer {
                transpose: -5.0,
                ..instruments::Layer::of(Instrument::Drum(instruments::DrumParams {
                    kind: DrumKind::Kick,
                    tune: -3.0,
                    decay: 2.5,
                    drive: 0.3,
                }))
            },
            instruments::Layer {
                gain_db: -6.0,
                ..instruments::Layer::of(Instrument::Drum(instruments::DrumParams {
                    kind: DrumKind::Crash,
                    tune: -2.0,
                    decay: 2.0,
                    drive: 0.0,
                }))
            },
        ],
    })
}

/// Section templates: which roles play and how dense, by section kind.
pub fn section_roles(kind: &str) -> (&'static [Role], f32) {
    match kind {
        "intro" => (&[Role::Chords, Role::Hat, Role::Fx], 0.3),
        "verse" => (
            &[
                Role::Kick,
                Role::Snare,
                Role::Hat,
                Role::Bass,
                Role::Chords,
                Role::Fx,
            ],
            0.6,
        ),
        "pre" | "prechorus" | "pre_chorus" | "build" | "buildup" => (
            &[Role::Snare, Role::Hat, Role::Chords, Role::Lead, Role::Fx],
            0.7,
        ),
        "hook" | "chorus" | "drop" => (
            &[
                Role::Kick,
                Role::Snare,
                Role::Hat,
                Role::Perc,
                Role::Bass,
                Role::Chords,
                Role::Lead,
                Role::Fx,
            ],
            1.0,
        ),
        "bridge" | "break" | "breakdown" => {
            (&[Role::Chords, Role::Lead, Role::Bass, Role::Fx], 0.45)
        }
        "outro" => (&[Role::Chords, Role::Lead, Role::Hat, Role::Fx], 0.3),
        _ => (
            &[
                Role::Kick,
                Role::Snare,
                Role::Hat,
                Role::Perc,
                Role::Bass,
                Role::Chords,
                Role::Lead,
                Role::Fx,
            ],
            0.8,
        ),
    }
}

fn section_kind(name: &str) -> String {
    let n = name.to_lowercase();
    let base: String = n
        .chars()
        .take_while(|c| c.is_ascii_alphabetic() || *c == '_')
        .collect();
    base.trim_end_matches('_').to_string()
}

fn default_bars(kind: &str) -> u32 {
    match kind {
        "intro" | "outro" | "bridge" | "break" | "build" | "pre" | "prechorus" => 4,
        _ => 8,
    }
}

/// The transition tool body, shared with produce_track.
pub fn add_transition(e: &mut Engine, a: &Value) -> Result<Value> {
    let kind = s_opt(a, "type")
        .unwrap_or_else(|| "riser".into())
        .to_lowercase();
    let mut into = into_index(&e.project, &s_req(a, "into")?)?;
    if e.project.arrangement.is_empty() {
        e.project.arrangement = e.project.song_sections();
    }
    let beats_len = f_or(
        a,
        "bars",
        if kind == "silence" || kind == "drop_gap" {
            0.25
        } else {
            2.0
        },
    )
    .clamp(0.25, 16.0)
        * 4.0;
    let (b0, _) = e.project.section_beats(&into.to_string())?;
    let start = (b0 - beats_len).max(0.0);
    let secs = beats_len * 60.0 / e.project.bpm;
    let mut created = Vec::new();
    let mut written = json!({});
    match kind.as_str() {
        "riser" | "reverse_cymbal" | "swell" => {
            // the note lives in the last bars of the previous section
            let (si, pi) = unique_section(&mut e.project, into - 1, true, "lead_in")?;
            into = si + 1;
            let pat_steps = e.project.patterns[pi].steps() as f32;
            let len_steps = (beats_len * 4.0).min(pat_steps);
            let (name, inst, pitch, vol) = if kind == "riser" {
                ("riser", riser_synth(secs), 60u8, -8.0)
            } else {
                ("cymbal_rev", reverse_cymbal_synth(secs), 60u8, -10.0)
            };
            let tn = add_or_get_track(&mut e.project, name, inst, vol);
            let pat = &mut e.project.patterns[pi];
            pat.notes_mut(&tn).retain(|n| n.start < pat_steps - len_steps);
            pat.notes_mut(&tn).push(Note::new(pat_steps - len_steps, len_steps, pitch, 0.9));
            if kind == "riser" {
                // bandpass sweep up via an automated filter on the riser track
                let ti = e.project.track_index(&tn)?;
                if !matches!(e.project.tracks[ti].effects.first(), Some(Effect::Filter(_))) {
                    e.project.tracks[ti].effects.insert(
                        0,
                        Effect::Filter(FilterFx {
                            mode: FilterMode::Highpass,
                            cutoff: 300.0,
                            resonance: 0.3,
                            ..Default::default()
                        }),
                    );
                    e.project.tracks[ti].effects.push(Effect::Reverb(ReverbFx {
                        size: 0.85,
                        mix: 0.35,
                        ..Default::default()
                    }));
                }
                let pts = automation::generate(Shape::SweepUp, start, b0, 150.0, 6000.0, 1.0, true);
                write_lane(&mut e.project, &tn, "fx.0.cutoff", pts, start, b0);
                written = json!({"automation": format!("{tn}:fx.0.cutoff 150 -> 6000 Hz")});
            }
            created.push(tn);
        }
        "impact" | "downlifter" => {
            let (si, pi) = unique_section(&mut e.project, into, false, "hit")?;
            into = si;
            let (name, inst, vol) = if kind == "impact" {
                ("impact", impact_layer(), -4.0)
            } else {
                let s = secs.max(0.5);
                (
                    "downlifter",
                    Instrument::Synth(SynthParams {
                        osc1: Wave::Noise,
                        osc2: Wave::Saw,
                        osc_mix: 0.2,
                        filter_mode: FilterMode::Bandpass,
                        cutoff: 1500.0,
                        resonance: 0.3,
                        pitch_env_semitones: 12.0,
                        pitch_env_time: s / 2.0,
                        filter_env_amount: 2.0,
                        filter_env: Adsr::new(0.001, s, 0.0, 0.1),
                        amp_env: Adsr::new(0.005, s, 0.0, 0.2),
                        gain: 0.4,
                        ..Default::default()
                    }),
                    -9.0,
                )
            };
            let tn = add_or_get_track(&mut e.project, name, inst, vol);
            let len = if kind == "impact" { 8.0 } else { (beats_len * 4.0).min(e.project.patterns[pi].steps() as f32) };
            let pat = &mut e.project.patterns[pi];
            pat.notes_mut(&tn).retain(|n| n.start > 0.01);
            pat.notes_mut(&tn).insert(0, Note::new(0.0, len, if kind == "impact" { 60 } else { 72 }, 1.0));
            created.push(tn);
        }
        "drum_fill" | "fill" => {
            let (si, pi) = unique_section(&mut e.project, into - 1, true, "fill")?;
            into = si + 1;
            let style = s_opt(a, "style").unwrap_or_else(|| "auto".into());
            let tracks = e.project.tracks.clone();
            let mut rng = Rng::new(seed_of(a));
            let t = write_fill(&mut e.project.patterns[pi], &tracks, &style, (beats_len).min(4.0), f_or(a, "intensity", 0.7), &mut rng)?;
            written = json!({"fill_tracks": t, "pattern": e.project.patterns[pi].name});
        }
        "filter_build" | "filter_sweep" | "stutter" | "tape_stop" | "half_time" => {
            let idx_existing = e.project.master_effects.iter().position(|fx| match (fx, kind.as_str()) {
                (Effect::Filter(f), "filter_build" | "filter_sweep") => f.mode == FilterMode::Highpass && f.cutoff <= 25.0,
                (Effect::Stutter(s), "stutter") => s.mode == StutterMode::Stutter,
                (Effect::Stutter(s), "tape_stop") => s.mode == StutterMode::TapeStop,
                (Effect::Stutter(s), "half_time") => s.mode == StutterMode::HalfTime,
                _ => false,
            });
            let idx = match idx_existing {
                Some(i) => i,
                None => {
                    let fx = match kind.as_str() {
                        "filter_build" | "filter_sweep" => Effect::Filter(FilterFx {
                            mode: FilterMode::Highpass,
                            cutoff: 20.0,
                            resonance: 0.25,
                            ..Default::default()
                        }),
                        "stutter" => Effect::Stutter(StutterFx {
                            mode: StutterMode::Stutter,
                            slice_steps: 1.0,
                            cycle_steps: 4.0,
                            mix: 0.0,
                            ..Default::default()
                        }),
                        "half_time" => Effect::Stutter(StutterFx {
                            mode: StutterMode::HalfTime,
                            cycle_steps: 16.0,
                            mix: 0.0,
                            ..Default::default()
                        }),
                        _ => Effect::Stutter(StutterFx {
                            mode: StutterMode::TapeStop,
                            cycle_steps: beats_len * 4.0,
                            mix: 0.0,
                            ..Default::default()
                        }),
                    };
                    let at = if matches!(kind.as_str(), "filter_build" | "filter_sweep") { 0 } else { pre_limiter_index(&e.project) };
                    insert_effect(&mut e.project, "master", at, fx)?
                }
            };
            if matches!(kind.as_str(), "filter_build" | "filter_sweep") {
                let mut pts = automation::generate(Shape::SweepUp, start, b0 - 0.01, 20.0, f_or(a, "max_hz", 800.0), 1.0, true);
                pts.push(pt(b0, 20.0, Curve::Step));
                write_lane(&mut e.project, "master", &format!("fx.{idx}.cutoff"), pts, start, b0);
                written = json!({"automation": format!("master:fx.{idx}.cutoff 20 -> {} Hz, snaps back at the boundary", f_or(a, "max_hz", 800.0))});
            } else {
                if kind == "tape_stop" {
                    // align the tape-stop cycle with the transition window
                    if let Some(Effect::Stutter(s)) = e.project.master_effects.get_mut(idx) {
                        s.cycle_steps = beats_len * 4.0;
                    }
                }
                let pts = vec![
                    pt(0.0, 0.0, Curve::Step),
                    pt(start, 1.0, Curve::Step),
                    pt(b0, 0.0, Curve::Step),
                ];
                let param = format!("fx.{idx}.mix");
                let lane = crate::tools_studio::lane_mut(&mut e.project, "master", &param);
                if lane.points.is_empty() {
                    lane.points = pts;
                } else {
                    lane.points.retain(|q| q.beat < start - 1e-4 || q.beat > b0 + 1e-4);
                    lane.points.push(pt(start, 1.0, Curve::Step));
                    lane.points.push(pt(b0, 0.0, Curve::Step));
                }
                lane.enabled = true;
                lane.sort();
                written = json!({"automation": format!("master:{param} on for beats {start}..{b0}")});
            }
            created.push(format!("master fx {idx}"));
        }
        "silence" | "drop_gap" => {
            let cur = e.project.master_volume_db;
            let pts = vec![
                pt(start - 0.01, cur, Curve::Step),
                pt(start, -80.0, Curve::Step),
                pt(b0, cur, Curve::Step),
            ];
            let lane = crate::tools_studio::lane_mut(&mut e.project, "master", "volume");
            if lane.points.is_empty() {
                lane.points.push(pt(0.0, cur, Curve::Step));
            }
            lane.points.retain(|q| q.beat < start - 0.02 || q.beat > b0 + 1e-4);
            lane.points.extend(pts);
            lane.enabled = true;
            lane.sort();
            written = json!({"automation": format!("master:volume muted beats {start}..{b0}")});
        }
        other => bail!("unknown transition '{other}'. Options: riser, reverse_cymbal, impact, downlifter, drum_fill, filter_build, stutter, tape_stop, half_time, silence"),
    }
    let secs_after: Vec<String> = e
        .project
        .song_sections()
        .iter()
        .map(|s| format!("{} x{}", s.pattern, s.repeats))
        .collect();
    Ok(
        json!({"transition": kind, "into_section": into, "beats": [start, b0], "created": created, "written": written, "arrangement": secs_after}),
    )
}

/// build_structure body (shared with produce_track).
pub fn build_structure(e: &mut Engine, a: &Value) -> Result<Value> {
    let base_name = match s_opt(a, "from") {
        Some(n) => n,
        None => {
            // the busiest pattern is the "full" version
            e.project
                .patterns
                .iter()
                .max_by_key(|p| p.clips.values().map(|v| v.len()).sum::<usize>())
                .map(|p| p.name.clone())
                .ok_or_else(|| anyhow!("no patterns to build from"))?
        }
    };
    let base_i = e.project.pattern_index(&base_name)?;
    let base = e.project.patterns[base_i].clone();
    let sections: Vec<(String, u32)> = match a.get("sections") {
        Some(Value::Array(v)) => v
            .iter()
            .map(|s| {
                if let Some(n) = s.as_str() {
                    let k = section_kind(n);
                    Ok((n.to_string(), default_bars(&k)))
                } else {
                    let n = s
                        .get("name")
                        .and_then(|x| x.as_str())
                        .ok_or_else(|| anyhow!("section needs a name"))?;
                    let b = s
                        .get("bars")
                        .and_then(|x| x.as_u64())
                        .map(|x| x as u32)
                        .unwrap_or_else(|| default_bars(&section_kind(n)));
                    Ok((n.to_string(), b))
                }
            })
            .collect::<Result<_>>()?,
        Some(Value::String(s)) => s
            .split([' ', ',', '-', '>'])
            .filter(|x| !x.is_empty())
            .map(|n| (n.to_string(), default_bars(&section_kind(n))))
            .collect(),
        _ => vec![
            ("intro".into(), 4),
            ("verse".into(), 8),
            ("hook".into(), 8),
            ("verse".into(), 8),
            ("hook".into(), 8),
            ("bridge".into(), 4),
            ("hook".into(), 8),
            ("outro".into(), 4),
        ],
    };
    if sections.is_empty() || sections.len() > 32 {
        bail!("sections must list 1-32 sections");
    }
    let tracks = e.project.tracks.clone();
    let mut rng = Rng::new(seed_of(a));
    let vary = b_or(a, "vary_repeats", true);
    let key = key_pc(&e.project);
    let scale = scale_of(&e.project)?.to_vec();
    let mut arrangement: Vec<Section> = Vec::new();
    let mut made: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    for (name, bars) in &sections {
        let kind = section_kind(name);
        let count = seen.entry(name.to_lowercase()).or_insert(0);
        *count += 1;
        let occurrence = *count;
        let pname = if occurrence > 1
            && vary
            && matches!(kind.as_str(), "verse" | "hook" | "chorus" | "drop")
        {
            format!("{name}{occurrence}")
        } else {
            name.clone()
        };
        if e.project.pattern_index(&pname).is_err() {
            let (roles, density) = section_roles(&kind);
            let mut pat = Pattern::new(&pname, *bars);
            let base_steps = base.steps().max(1);
            for t in &tracks {
                let r = role_of(t);
                if !roles.contains(&r) || r == Role::Fx {
                    continue;
                }
                let src = base.notes(&t.name);
                if src.is_empty() {
                    continue;
                }
                let mut notes = Vec::new();
                // tile the base pattern to the section length
                let mut off = 0;
                while off < pat.steps() {
                    for n in src {
                        let s = n.start + off as f32;
                        if s < pat.steps() as f32 {
                            notes.push(Note {
                                start: s,
                                ..n.clone()
                            });
                        }
                    }
                    off += base_steps;
                }
                // thin out low-energy sections: drop weak hits / lead notes
                if density < 0.75 && matches!(r, Role::Hat | Role::Perc | Role::Lead) {
                    notes.retain(|n| (n.start % 4.0).abs() < 1e-3 || rng.chance(density));
                }
                if kind == "intro" || kind == "outro" {
                    for n in notes.iter_mut() {
                        n.vel *= 0.8;
                    }
                }
                if occurrence > 1 && vary && matches!(r, Role::Lead | Role::Hat) {
                    notes = mo::variation(
                        &notes,
                        if r == Role::Hat {
                            "rhythmic"
                        } else {
                            "melodic"
                        },
                        0.25,
                        key,
                        &scale,
                        pat.steps() as f32,
                        is_drum_role(r),
                        &mut rng,
                    )?;
                }
                *pat.notes_mut(&t.name) = notes;
            }
            // builds end with a snare roll
            if matches!(
                kind.as_str(),
                "build" | "buildup" | "pre" | "prechorus" | "pre_chorus"
            ) {
                let _ = write_fill(
                    &mut pat,
                    &tracks,
                    "snare_roll",
                    (*bars as f32 * 2.0).min(8.0),
                    0.8,
                    &mut rng,
                );
            }
            made.push(json!({"pattern": pname, "kind": kind, "bars": bars, "tracks": pat.clips.iter().filter(|(_, v)| !v.is_empty()).map(|(k, _)| k.clone()).collect::<Vec<_>>()}));
            e.project.patterns.push(pat);
        }
        match arrangement.last_mut() {
            Some(last) if last.pattern == pname => last.repeats += 1,
            _ => arrangement.push(Section {
                pattern: pname,
                repeats: 1,
            }),
        }
    }
    e.project.arrangement = arrangement;
    let mut transitions = Vec::new();
    if b_or(a, "transitions", true) {
        let secs = e.project.arrangement.clone();
        // walk boundaries from the end so earlier indices stay valid
        for i in (1..secs.len()).rev() {
            let k = section_kind(&secs[i].pattern);
            let prev = section_kind(&secs[i - 1].pattern);
            let picks: &[&str] = match k.as_str() {
                "hook" | "chorus" | "drop" => &["riser", "drum_fill", "impact"],
                "verse" if prev == "intro" => &["reverse_cymbal"],
                "bridge" | "break" | "breakdown" => &["downlifter"],
                "outro" => &["reverse_cymbal"],
                _ => &[],
            };
            let mut into = i;
            for t in picks {
                let bars = if *t == "drum_fill" { 1.0 } else { 2.0 };
                match add_transition(
                    e,
                    &json!({"into": into.to_string(), "type": t, "bars": bars, "seed": seed_of(a)}),
                ) {
                    Ok(v) => {
                        into = v["into_section"]
                            .as_u64()
                            .map(|x| x as usize)
                            .unwrap_or(into);
                        transitions.push(format!("{t} -> {}", secs[i].pattern));
                    }
                    Err(err) => {
                        transitions.push(format!("{t} -> {} skipped: {err}", secs[i].pattern))
                    }
                }
            }
        }
        transitions.reverse();
    }
    Ok(json!({
        "from": base_name,
        "patterns_created": made,
        "arrangement": e.project.song_sections().iter().map(|s| format!("{} x{}", s.pattern, s.repeats)).collect::<Vec<_>>(),
        "song_seconds": (e.project.song_seconds() * 10.0).round() / 10.0,
        "transitions": transitions,
    }))
}

// ---------------- the tools ----------------

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "edit_notes",
            description: "Edit selected notes of a track (select by from/to steps, bars, pitch(es), pitch range, velocity range; default all): move (steps), transpose (semitones), len_scale / set_len (steps), vel_scale / vel_add / set_vel, set_prob (chance 0-1 per pass: ghost notes that come and go) and set_offset (microtiming -0.5..0.5 steps: negative pushes, positive lays back). all_patterns applies everywhere.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "move": {"type": "number"}, "transpose": {"type": "integer"},
                "len_scale": {"type": "number"}, "set_len": {"type": "number"},
                "vel_scale": {"type": "number"}, "vel_add": {"type": "number"}, "set_vel": {"type": "number"},
                "set_prob": {"type": "number", "minimum": 0, "maximum": 1}, "set_offset": {"type": "number", "minimum": -0.5, "maximum": 0.5}
            })), &["track"]),
            run: |e, a| {
                let sel = selection_of(a)?;
                let spec = EditSpec {
                    move_steps: f_or(a, "move", 0.0),
                    transpose: f_or(a, "transpose", 0.0) as i32,
                    len_scale: f_or(a, "len_scale", 0.0),
                    set_len: f_opt(a, "set_len").map(|x| x.max(0.05)),
                    vel_scale: f_or(a, "vel_scale", 0.0),
                    vel_add: f_or(a, "vel_add", 0.0),
                    set_vel: f_opt(a, "set_vel"),
                    set_prob: f_opt(a, "set_prob"),
                    set_offset: f_opt(a, "set_offset"),
                };
                with_notes(e, a, |n, steps, _| Ok(mo::edit(n, &sel, &spec, steps)))
            },
        },
        Tool {
            name: "delete_notes",
            description: "Delete the selected notes of a track (same selection as edit_notes: from/to, bars, pitch, pitches, pitch_min/max, vel_min/max). With no selection, clears the track in the pattern.",
            mutates: true,
            schema: || obj(props(target_props(), selection_props()), &["track"]),
            run: |e, a| {
                let sel = selection_of(a)?;
                with_notes(e, a, |n, _, _| {
                    let before = n.len();
                    n.retain(|x| !sel.matches(x));
                    Ok(before - n.len())
                })
            },
        },
        Tool {
            name: "quantize",
            description: "Quantize note starts (and optionally ends) to a grid with a strength (0..1, 0.5 = halfway: tighten while keeping feel) and swing (0..1 delays every second grid line, 1 = triplet shuffle). grid: steps (1 = 16th) or '1/16', '1/8', '1/8t', '1/4'. Selection like edit_notes.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "grid": {"description": "steps or '1/16' | '1/8' | '1/8t' | '1/4'"},
                "strength": {"type": "number", "minimum": 0, "maximum": 1},
                "swing": {"type": "number", "minimum": 0, "maximum": 1},
                "ends": {"type": "boolean"}
            })), &["track"]),
            run: |e, a| {
                let sel = selection_of(a)?;
                let g = steps_of(a.get("grid"), 1.0)?;
                let (st, sw, ends) = (f_or(a, "strength", 1.0), f_or(a, "swing", 0.0), b_or(a, "ends", false));
                with_notes(e, a, |n, _, _| Ok(mo::quantize(n, &sel, g, st, sw, ends)))
            },
        },
        Tool {
            name: "split_notes",
            description: "Split selected notes into repeats every `every` steps (e.g. a held 808 into 8ths) or once at absolute step `at`.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({"every": {"description": "steps or '1/8'"}, "at": {"type": "number"}})), &["track"]),
            run: |e, a| {
                let sel = selection_of(a)?;
                let every = a.get("every").map(|v| steps_of(Some(v), 1.0)).transpose()?;
                let at = f_opt(a, "at");
                if every.is_none() && at.is_none() {
                    bail!("give every (steps) or at (step)");
                }
                with_notes(e, a, |n, _, _| Ok(mo::split(n, &sel, every, at)))
            },
        },
        Tool {
            name: "merge_notes",
            description: "Merge consecutive same-pitch selected notes into one long note when the gap between them is <= max_gap steps (default 0).",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({"max_gap": {"type": "number"}})), &["track"]),
            run: |e, a| {
                let sel = selection_of(a)?;
                let g = f_or(a, "max_gap", 0.0);
                with_notes(e, a, |n, _, _| Ok(mo::merge(n, &sel, g)))
            },
        },
        Tool {
            name: "legato",
            description: "Make selected notes legato: each note (or chord) is extended to the next one; overlap (steps, e.g. 0.25) makes them overlap for glides/slides on monophonic basses and leads.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({"overlap": {"type": "number"}})), &["track"]),
            run: |e, a| {
                let sel = selection_of(a)?;
                let ov = f_or(a, "overlap", 0.0);
                with_notes(e, a, |n, steps, _| Ok(mo::legato(n, &sel, ov, steps)))
            },
        },
        Tool {
            name: "arpeggiate",
            description: "Turn selected chords into arpeggios. style: up, down, updown, downup, random, converge, chord (rhythmic chord stabs). rate: steps or '1/16', '1/8', '1/8t'. octaves 1-4 spans extra octaves; gate 0.1-1 = note length vs rate.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "style": {"type": "string", "enum": mo::ARP_PATTERNS},
                "rate": {"description": "steps or '1/16'"},
                "octaves": {"type": "integer", "minimum": 1, "maximum": 4},
                "gate": {"type": "number"}, "seed": {"type": "integer"}
            })), &["track"]),
            run: |e, a| {
                let sel = selection_of(a)?;
                let pat = s_opt(a, "style").unwrap_or_else(|| "up".into());
                let rate = steps_of(a.get("rate"), 1.0)?;
                let oct = u_or(a, "octaves", 1) as u32;
                let gate = f_or(a, "gate", 0.8);
                let mut rng = Rng::new(seed_of(a));
                with_notes(e, a, |n, _, _| mo::arpeggiate(n, &sel, &pat, rate, oct, gate, &mut rng))
            },
        },
        Tool {
            name: "strum",
            description: "Strum selected chords: spread their notes over `spread` steps (0.3-1 is natural guitar/keys), direction up|down|alternate, vel_taper 0..1 softens later strings.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "spread": {"type": "number"}, "direction": {"type": "string", "enum": ["up", "down", "alternate"]}, "vel_taper": {"type": "number"}
            })), &["track"]),
            run: |e, a| {
                let sel = selection_of(a)?;
                let (sp, dir, vt) = (f_or(a, "spread", 0.5), s_opt(a, "direction").unwrap_or_else(|| "down".into()), f_or(a, "vel_taper", 0.2));
                with_notes(e, a, |n, _, _| Ok(mo::strum(n, &sel, sp, &dir, vt)))
            },
        },
        Tool {
            name: "roll_notes",
            description: "Drum performance edits on selected notes: roll (replace each note with `count` evenly spaced hits filling its length, velocity ramping from vel_start: trap hi-hat rolls, snare build-ups), ratchet (fast repeats keeping the accent), flam (quiet grace note just before). pitch_step moves each repeat (pitched 808/hat rolls). Tip: select the last beat with from/to, set_len it to 4 steps, then roll count 6 for a triplet roll.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "mode": {"type": "string", "enum": ["roll", "ratchet", "flam"]},
                "count": {"type": "integer", "minimum": 2, "maximum": 32},
                "vel_start": {"type": "number"}, "pitch_step": {"type": "integer"}
            })), &["track"]),
            run: |e, a| {
                let sel = selection_of(a)?;
                let mode = s_opt(a, "mode").unwrap_or_else(|| "roll".into());
                let (c, vs, ps) = (u_or(a, "count", 4) as u32, f_or(a, "vel_start", 0.4), f_or(a, "pitch_step", 0.0) as i32);
                with_notes(e, a, |n, _, _| mo::ratchet(n, &sel, &mode, c, vs, ps))
            },
        },
        Tool {
            name: "chord_voicing",
            description: "Revoice selected chords (notes starting together): inversion (inversion=+1/+2 up, -1 down), close (pack within an octave), drop2 / drop3 (jazz/neo-soul spread), spread (open voicing for pads).",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "mode": {"type": "string", "enum": ["inversion", "close", "drop2", "drop3", "spread"]},
                "inversion": {"type": "integer"}
            })), &["track", "mode"]),
            run: |e, a| {
                let sel = selection_of(a)?;
                let mode = s_req(a, "mode")?;
                let inv = f_or(a, "inversion", 1.0) as i32;
                with_notes(e, a, |n, _, _| mo::voice(n, &sel, &mode, inv))
            },
        },
        Tool {
            name: "harmonize",
            description: "Add a diatonic harmony voice in the project key to selected notes: interval 3rd, 6th, 5th, 4th, octave, 10th... above (or below=true). Writes into the same track, or into `to_track` (created with the same instrument) for a separate harmony part. vel_scale sets the harmony's level.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "interval": {"type": "string", "description": "3rd | 6th | 5th | 4th | octave | 10th | 2nd | 7th"},
                "below": {"type": "boolean"}, "to_track": {"type": "string"}, "vel_scale": {"type": "number"}
            })), &["track"]),
            run: |e, a| {
                let sel = selection_of(a)?;
                let steps = mo::interval_steps(&s_opt(a, "interval").unwrap_or_else(|| "3rd".into()))?;
                let (below, vs) = (b_or(a, "below", false), f_or(a, "vel_scale", 0.85));
                let key = key_pc(&e.project);
                let scale = scale_of(&e.project)?.to_vec();
                let ti = e.project.track_index(&s_req(a, "track")?)?;
                let src = e.project.tracks[ti].clone();
                let dest = match s_opt(a, "to_track") {
                    Some(n) => match e.project.track_index(&n) {
                        Ok(i) => e.project.tracks[i].name.clone(),
                        Err(_) => {
                            let mut t = Track::new(&n, src.instrument.clone());
                            t.volume_db = src.volume_db - 3.0;
                            t.effects = src.effects.clone();
                            e.project.tracks.push(t);
                            n
                        }
                    },
                    None => src.name.clone(),
                };
                let mut total = 0;
                for pi in target_patterns(&e.project, a)? {
                    let pat = &mut e.project.patterns[pi];
                    let h = mo::harmonize(pat.notes(&src.name), &sel, steps, below, key, &scale, vs);
                    total += h.len();
                    let d = pat.notes_mut(&dest);
                    d.extend(h);
                    mo::sort(d);
                    d.dedup_by(|x, y| (x.start - y.start).abs() < 1e-3 && x.pitch == y.pitch);
                }
                Ok(json!({"harmony_notes": total, "track": dest, "key": format!("{} {}", e.project.key_root, e.project.scale)}))
            },
        },
        Tool {
            name: "detect_key",
            description: "Detect the key of the project's notes (Krumhansl-Schmuckler on duration-weighted pitch classes; drums ignored). tracks limits it to some parts. apply=true sets the project key to the best match. Returns the top candidates with confidence.",
            mutates: true,
            schema: || obj(json!({"tracks": {"type": "array", "items": {"type": "string"}}, "apply": {"type": "boolean"}}), &[]),
            run: |e, a| {
                let want: Vec<String> = a.get("tracks").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect()).unwrap_or_default();
                let mut all = Vec::new();
                for t in &e.project.tracks {
                    if is_drum_role(role_of(t)) || role_of(t) == Role::Fx { continue; }
                    if !want.is_empty() && !want.contains(&t.name.to_lowercase()) { continue; }
                    for p in &e.project.patterns { all.extend_from_slice(p.notes(&t.name)); }
                }
                if all.is_empty() { bail!("no pitched notes to analyse"); }
                let ranked = mo::key_from_chroma(&mo::chroma_of(&all));
                let best = ranked[0];
                if b_or(a, "apply", false) {
                    e.project.key_root = theory::NOTE_NAMES[best.0 as usize].to_string();
                    e.project.scale = best.1.to_string();
                }
                Ok(json!({
                    "key": format!("{} {}", theory::NOTE_NAMES[best.0 as usize], best.1),
                    "confidence": (best.2 * 100.0).round() / 100.0,
                    "candidates": ranked.iter().take(4).map(|(pc, m, r)| json!({"key": format!("{} {m}", theory::NOTE_NAMES[*pc as usize]), "r": (r * 100.0).round() / 100.0})).collect::<Vec<_>>(),
                    "notes_analysed": all.len(),
                    "project_key": format!("{} {}", e.project.key_root, e.project.scale),
                }))
            },
        },
        Tool {
            name: "velocity_curve",
            description: "Shape velocities of selected notes: ramp_up / ramp_down between min and max (crescendo into a drop), accent with a step pattern ('X..x' X = max, x = medium, o = soft, . = min, repeating per 16th), compress (toward the average by amount 0..1, for even hats), expand (more dynamics), random (±amount), set (all to max).",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "shape": {"type": "string", "enum": ["ramp_up", "ramp_down", "accent", "compress", "expand", "random", "set"]},
                "amount": {"type": "number"}, "accents": {"type": "string"}, "min": {"type": "number"}, "max": {"type": "number"}, "seed": {"type": "integer"}
            })), &["track", "shape"]),
            run: |e, a| {
                let sel = selection_of(a)?;
                let shape = s_req(a, "shape")?;
                let (amt, acc, lo, hi) = (f_or(a, "amount", 0.5), s_opt(a, "accents").unwrap_or_default(), f_or(a, "min", 0.35), f_or(a, "max", 1.0));
                let mut rng = Rng::new(seed_of(a));
                with_notes(e, a, |n, _, _| mo::velocity_curve(n, &sel, &shape, amt, &acc, lo, hi, &mut rng))
            },
        },
        Tool {
            name: "apply_groove",
            description: "Apply a feel to drums (or any tracks) with per-note microtiming and probability: mpc (swing every off-16th by amount), lazy (snares/claps lay back, hats drift: Dilla-style), push (hats/percs slightly early: urgent), drunk (random timing), plus hat_chance (0..1) makes off-beat hats probabilistic so loops breathe. tracks default = every drum track.",
            mutates: true,
            schema: || obj(json!({
                "pattern": pattern_prop(), "all_patterns": {"type": "boolean"},
                "tracks": {"type": "array", "items": {"type": "string"}},
                "template": {"type": "string", "enum": ["mpc", "lazy", "push", "drunk", "straight"]},
                "amount": {"type": "number", "minimum": 0, "maximum": 1},
                "hat_chance": {"type": "number", "minimum": 0, "maximum": 1}, "seed": {"type": "integer"}
            }), &["template"]),
            run: |e, a| {
                let tmpl = s_req(a, "template")?;
                if !["mpc", "lazy", "push", "drunk", "straight"].contains(&tmpl.as_str()) { bail!("unknown template '{tmpl}'"); }
                let amt = f_or(a, "amount", 0.5).clamp(0.0, 1.0);
                let hat_chance = f_opt(a, "hat_chance");
                let want: Vec<String> = a.get("tracks").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect()).unwrap_or_default();
                let tracks: Vec<(String, Role)> = e.project.tracks.iter().filter(|t| if want.is_empty() { is_drum_role(role_of(t)) } else { want.contains(&t.name.to_lowercase()) }).map(|t| (t.name.clone(), role_of(t))).collect();
                if tracks.is_empty() { bail!("no tracks to groove (no drum tracks found; pass tracks)"); }
                let mut rng = Rng::new(seed_of(a));
                let mut touched = 0;
                for pi in target_patterns(&e.project, a)? {
                    for (name, role) in &tracks {
                        for n in e.project.patterns[pi].notes_mut(name).iter_mut() {
                            let off16 = (n.start.round() as i64) % 2 == 1 && (n.start - n.start.round()).abs() < 1e-3;
                            n.offset = match tmpl.as_str() {
                                "straight" => 0.0,
                                "mpc" => if off16 { amt * 0.33 } else { 0.0 },
                                "lazy" => match role {
                                    Role::Snare => 0.08 + 0.12 * amt,
                                    Role::Hat => rng.bipolar() * 0.08 * amt + 0.04 * amt,
                                    Role::Kick => rng.bipolar() * 0.03 * amt,
                                    _ => 0.05 * amt,
                                },
                                "push" => match role { Role::Hat | Role::Perc => -0.06 - 0.06 * amt, _ => 0.0 },
                                _ => rng.bipolar() * 0.15 * amt,
                            }.clamp(-0.5, 0.5);
                            if let (Some(c), Role::Hat) = (hat_chance, role) {
                                if (n.start % 2.0).abs() > 1e-3 || n.vel < 0.6 { n.prob = c.clamp(0.0, 1.0); }
                            }
                            touched += 1;
                        }
                    }
                }
                Ok(json!({"template": tmpl, "amount": amt, "tracks": tracks.iter().map(|t| t.0.clone()).collect::<Vec<_>>(), "notes": touched}))
            },
        },
        Tool {
            name: "generate_variation",
            description: "Write a variation of a track's part that keeps its identity and key: kind rhythmic (shift/split/drop/echo a few hits), melodic (move some notes by scale steps, downbeats mostly kept) or both; amount 0..1. Writes in place, or into `to_pattern` (created as a copy of the source pattern if missing) so you get a 'B' version to arrange.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string"}, "pattern": pattern_prop(), "to_pattern": {"type": "string"},
                "kind": {"type": "string", "enum": ["rhythmic", "melodic", "both"]},
                "amount": {"type": "number"}, "seed": {"type": "integer"}
            }), &["track"]),
            run: |e, a| {
                let ti = e.project.track_index(&s_req(a, "track")?)?;
                let t = e.project.tracks[ti].clone();
                let pi = pattern_idx(&e.project, a)?;
                let kind = s_opt(a, "kind").unwrap_or_else(|| "both".into());
                let amount = f_or(a, "amount", 0.3);
                let key = key_pc(&e.project);
                let scale = scale_of(&e.project)?.to_vec();
                let src = e.project.patterns[pi].notes(&t.name).to_vec();
                if src.is_empty() { bail!("'{}' has no notes in pattern '{}'", t.name, e.project.patterns[pi].name); }
                let steps = e.project.patterns[pi].steps() as f32;
                let mut rng = Rng::new(seed_of(a));
                let var = mo::variation(&src, &kind, amount, key, &scale, steps, is_drum_role(role_of(&t)), &mut rng)?;
                let dest = match s_opt(a, "to_pattern") {
                    Some(n) => match e.project.pattern_index(&n) {
                        Ok(i) => i,
                        Err(_) => { let mut np = e.project.patterns[pi].clone(); np.name = n; e.project.patterns.push(np); e.project.patterns.len() - 1 }
                    },
                    None => pi,
                };
                let changed = src.len().abs_diff(var.len()) + src.iter().zip(var.iter()).filter(|(x, y)| x != y).count();
                *e.project.patterns[dest].notes_mut(&t.name) = var.clone();
                Ok(json!({"track": t.name, "pattern": e.project.patterns[dest].name, "kind": kind, "notes_before": src.len(), "notes_after": var.len(), "changed_approx": changed, "notes": if t.instrument.is_drum() { json!(grid(&var, steps as u32)) } else { notes_json(&var) }}))
            },
        },
        Tool {
            name: "generate_fill",
            description: "Write a drum fill over the last `beats` (default 4 = one bar) of a pattern: auto (snare fill + kick drop), snare_roll (accelerating build), tom_run (descending toms), hat_roll (16th/triplet hat roll), kick_build (quarter -> 8th -> 16th kicks), break (drums drop out). Use on the bar before a hook/drop; to keep other repeats untouched, prefer add_transition type drum_fill (it makes a unique copy of that section).",
            mutates: true,
            schema: || obj(json!({
                "pattern": pattern_prop(),
                "style": {"type": "string", "enum": ["auto", "snare_roll", "tom_run", "hat_roll", "kick_build", "break"]},
                "beats": {"type": "number"}, "intensity": {"type": "number"}, "seed": {"type": "integer"}
            }), &[]),
            run: |e, a| {
                let pi = pattern_idx(&e.project, a)?;
                let tracks = e.project.tracks.clone();
                let mut rng = Rng::new(seed_of(a));
                let style = s_opt(a, "style").unwrap_or_else(|| "auto".into());
                let t = write_fill(&mut e.project.patterns[pi], &tracks, &style, f_or(a, "beats", 4.0).clamp(0.5, 16.0), f_or(a, "intensity", 0.7), &mut rng)?;
                let pat = &e.project.patterns[pi];
                Ok(json!({"pattern": pat.name, "style": style, "tracks": t.iter().map(|n| json!({"track": n, "grid": grid(pat.notes(n), pat.steps())})).collect::<Vec<_>>()}))
            },
        },
        Tool {
            name: "counter_melody",
            description: "Compose a counter-melody against a track's melody: consonant intervals (3rds/6ths) under what the melody holds, contrary motion, and call-and-response phrases in the melody's gaps, all in the project key. Creates `to_track` (default 'counter', preset default 'pluck_lead' or an instrument object) in the same pattern(s).",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string", "description": "the melody to answer"}, "pattern": pattern_prop(), "all_patterns": {"type": "boolean"},
                "to_track": {"type": "string"}, "preset": {"type": "string"}, "instrument": {"type": "object"},
                "register": {"description": "centre pitch (MIDI or 'G4'); default an octave below the melody"},
                "density": {"type": "number"}, "seed": {"type": "integer"}
            }), &["track"]),
            run: |e, a| {
                let ti = e.project.track_index(&s_req(a, "track")?)?;
                let src = e.project.tracks[ti].name.clone();
                let key = key_pc(&e.project);
                let scale = scale_of(&e.project)?.to_vec();
                let dest = s_opt(a, "to_track").unwrap_or_else(|| "counter".into());
                let inst = match instrument_from(a)? { Some(i) => i, None => instruments::preset("pluck_lead").unwrap() };
                let dest = crate::tools::ensure_track(&mut e.project, &dest, "pluck_lead", Some(inst))?;
                let mut rng = Rng::new(seed_of(a));
                let mut out = Vec::new();
                for pi in target_patterns(&e.project, a)? {
                    let mel = e.project.patterns[pi].notes(&src).to_vec();
                    if mel.is_empty() { continue; }
                    let avg = mel.iter().map(|n| n.pitch as i32).sum::<i32>() / mel.len() as i32;
                    let reg = match a.get("register") { Some(v) => pitch_of(v)? as i32, None => avg - 9 };
                    let steps = e.project.patterns[pi].steps() as f32;
                    let cm = mo::counter_melody(&mel, key, &scale, steps, reg, f_or(a, "density", 0.45), &mut rng);
                    out.push(json!({"pattern": e.project.patterns[pi].name, "notes": cm.len()}));
                    *e.project.patterns[pi].notes_mut(&dest) = cm;
                }
                if out.is_empty() { bail!("'{src}' has no notes in the chosen pattern(s)"); }
                if let Ok(i) = e.project.track_index(&dest) { if e.project.tracks[i].volume_db == 0.0 { e.project.tracks[i].volume_db = -8.0; e.project.tracks[i].pan = -0.2; } }
                Ok(json!({"track": dest, "against": src, "patterns": out}))
            },
        },
        Tool {
            name: "import_midi",
            description: "Import a Standard MIDI File (.mid) into a new pattern: each MIDI track becomes a project track (drums on channel 10 are split into kick/snare/hat/... tracks with synthesized drum voices; melodic tracks get a preset from their General MIDI program, or `preset`). Sets tempo from the file unless set_tempo=false. The pattern is added to the end of the arrangement.",
            mutates: true,
            schema: || obj(json!({"path": {"type": "string"}, "pattern": {"type": "string", "description": "new pattern name (default the file name)"}, "set_tempo": {"type": "boolean"}, "preset": {"type": "string"}, "max_bars": {"type": "integer"}}), &["path"]),
            run: |e, a| {
                let path = e.resolve(&s_req(a, "path")?);
                let data = std::fs::read(&path).map_err(|err| anyhow!("read {}: {err}", path.display()))?;
                let m = smf::read(&data)?;
                if m.tracks.is_empty() { bail!("the MIDI file has no notes"); }
                let tpq = m.division as f32;
                let to_steps = |t: u32| t as f32 / tpq * 4.0;
                let last = m.tracks.iter().flat_map(|t| t.notes.iter()).map(|n| to_steps(n.tick + n.len)).fold(0.0f32, f32::max);
                let bars = ((last / 16.0).ceil() as u32).clamp(1, u_or(a, "max_bars", 64).clamp(1, 64) as u32);
                let pname = s_opt(a, "pattern").unwrap_or_else(|| crate::samples::sample_name(&path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "midi".into())));
                if e.project.pattern_index(&pname).is_ok() { bail!("pattern '{pname}' already exists"); }
                let mut pat = Pattern::new(&pname, bars);
                let steps = pat.steps() as f32;
                let mut made: Vec<Value> = Vec::new();
                for (k, t) in m.tracks.iter().enumerate() {
                    let drums = t.notes.iter().all(|n| n.channel == 9) || t.name.to_lowercase().contains("drum");
                    if drums {
                        let mut by: std::collections::BTreeMap<&str, (&str, Vec<Note>)> = Default::default();
                        for n in &t.notes {
                            let (tn, preset) = drum_preset_for_gm(n.pitch);
                            let s = to_steps(n.tick);
                            if s >= steps { continue; }
                            by.entry(tn).or_insert((preset, Vec::new())).1.push(Note::new(s, to_steps(n.len).max(0.25), 60, n.vel as f32 / 127.0));
                        }
                        for (tn, (preset, notes)) in by {
                            let name = crate::tools::ensure_track(&mut e.project, tn, preset, None)?;
                            made.push(json!({"track": name, "notes": notes.len(), "from": "drums"}));
                            pat.notes_mut(&name).extend(notes);
                        }
                    } else {
                        let avg = t.notes.iter().map(|n| n.pitch as u32).sum::<u32>() / t.notes.len().max(1) as u32;
                        let preset = s_opt(a, "preset").unwrap_or_else(|| preset_for_program(t.program, avg < 48).to_string());
                        let mut name = if t.name.trim().is_empty() { format!("midi{}", k + 1) } else { crate::samples::sample_name(&t.name) };
                        if e.project.track_index(&name).is_ok() && !pat.notes(&name).is_empty() { name = format!("{name}_{k}"); }
                        let name = crate::tools::ensure_track(&mut e.project, &name, &preset, None)?;
                        let notes: Vec<Note> = t.notes.iter().filter(|n| to_steps(n.tick) < steps).map(|n| Note::new(to_steps(n.tick), to_steps(n.len).max(0.1), n.pitch, n.vel as f32 / 127.0)).collect();
                        made.push(json!({"track": name, "notes": notes.len(), "preset": preset}));
                        pat.notes_mut(&name).extend(notes);
                    }
                }
                for v in pat.clips.values_mut() { mo::sort(v); }
                if b_or(a, "set_tempo", true) { e.project.bpm = m.bpm.clamp(20.0, 400.0); }
                if e.project.arrangement.is_empty() && e.project.patterns.iter().all(|p| p.clips.values().all(|v| v.is_empty())) {
                    e.project.patterns.clear();
                }
                e.project.patterns.push(pat);
                if !e.project.arrangement.is_empty() { e.project.arrangement.push(Section { pattern: pname.clone(), repeats: 1 }); }
                Ok(json!({"pattern": pname, "bars": bars, "bpm": e.project.bpm, "time_signature": format!("{}/{}", m.time_sig.0, m.time_sig.1), "tracks": made}))
            },
        },
        Tool {
            name: "export_midi",
            description: "Export the song (scope 'song': arrangement with repeats, note probability rolled like the render, swing + microtiming baked in) or one pattern (scope 'pattern') as a Standard MIDI File (format 1, 480 PPQ): one MIDI track per project track, drums on channel 10 with General MIDI note numbers, programs guessed from roles. Opens in any DAW.",
            mutates: false,
            schema: || obj(json!({"path": {"type": "string", "description": "default renders/<project>.mid"}, "scope": {"type": "string", "enum": ["song", "pattern"]}, "pattern": pattern_prop(), "tracks": {"type": "array", "items": {"type": "string"}}}), &[]),
            run: |e, a| {
                let p = &e.project;
                let path = match s_opt(a, "path") { Some(x) => e.resolve(&x), None => e.renders_dir().join(format!("{}.mid", crate::samples::sample_name(&p.name))) };
                let want: Vec<String> = a.get("tracks").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect()).unwrap_or_default();
                let per_track: Vec<Vec<(f32, Note)>> = if s_opt(a, "scope").as_deref() == Some("pattern") {
                    let pi = pattern_idx(p, a)?;
                    p.tracks.iter().map(|t| p.patterns[pi].notes(&t.name).iter().map(|n| (n.start + n.offset, n.clone())).collect()).collect()
                } else { song_notes(p) };
                const TPQ: f32 = 480.0;
                let mut tracks = Vec::new();
                let mut melodic_ch = 0u8;
                for (t, notes) in p.tracks.iter().zip(per_track.iter()) {
                    if !want.is_empty() && !want.contains(&t.name.to_lowercase()) { continue; }
                    if notes.is_empty() { continue; }
                    let drum = gm_drum_note(t);
                    let ch = if drum.is_some() { 9 } else { let c = melodic_ch; melodic_ch = (melodic_ch + 1) % 15; if c >= 9 { c + 1 } else { c } };
                    tracks.push(smf::SmfTrack {
                        name: t.name.clone(),
                        program: if drum.is_some() { None } else { Some(gm_program(t)) },
                        notes: notes.iter().map(|(s, n)| smf::SmfNote {
                            tick: (s * TPQ / 4.0).round().max(0.0) as u32,
                            len: (n.len * TPQ / 4.0).round().max(1.0) as u32,
                            pitch: drum.unwrap_or(n.pitch),
                            vel: (n.vel * 127.0).round().clamp(1.0, 127.0) as u8,
                            channel: ch,
                        }).collect(),
                    });
                }
                if tracks.is_empty() { bail!("nothing to export (no notes)"); }
                let bytes = smf::write(&smf::Smf { division: TPQ as u16, bpm: p.bpm, time_sig: (4, 4), tracks: tracks.clone() });
                if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
                std::fs::write(&path, &bytes)?;
                Ok(json!({"path": path, "bytes": bytes.len(), "tracks": tracks.iter().map(|t| json!({"track": t.name, "notes": t.notes.len(), "channel": t.notes.first().map(|n| n.channel + 1)})).collect::<Vec<_>>(), "bpm": p.bpm}))
            },
        },
        Tool {
            name: "build_structure",
            description: "Build a full song structure from one 'full' pattern: sections like 'intro verse hook verse hook bridge hook outro' (string, or [{name, bars}]) each get their own pattern derived by role (intro = chords + hats, verse = drums + bass + chords, build = snare roll, hook/drop = everything, bridge = chords + lead + bass, outro = chords + lead), repeated sections get light variations (vary_repeats), and transitions (risers, fills, impacts, reverse cymbals, downlifters) are added at boundaries (transitions=false to skip). from = source pattern (default the busiest).",
            mutates: true,
            schema: || obj(json!({"sections": {"description": "'intro verse hook ...' or [{name, bars}]"}, "from": {"type": "string"}, "vary_repeats": {"type": "boolean"}, "transitions": {"type": "boolean"}, "seed": {"type": "integer"}}), &[]),
            run: build_structure,
        },
        Tool {
            name: "vary_section",
            description: "Duplicate a pattern as a new section with variation (spec: duplicate_section_with_variation): copies `pattern` to `name`, then varies `tracks` (default leads + hats + perc) with kind/amount, keeping key and groove. insert_after (arrangement index) places it in the song.",
            mutates: true,
            schema: || obj(json!({"pattern": {"type": "string"}, "name": {"type": "string"}, "tracks": {"type": "array", "items": {"type": "string"}}, "kind": {"type": "string", "enum": ["rhythmic", "melodic", "both"]}, "amount": {"type": "number"}, "insert_after": {"type": "integer"}, "seed": {"type": "integer"}}), &["pattern", "name"]),
            run: |e, a| {
                let pi = e.project.pattern_index(&s_req(a, "pattern")?)?;
                let name = s_req(a, "name")?;
                if e.project.pattern_index(&name).is_ok() { bail!("pattern '{name}' already exists"); }
                let mut np = e.project.patterns[pi].clone();
                np.name = name.clone();
                let want: Vec<String> = a.get("tracks").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect()).unwrap_or_default();
                let key = key_pc(&e.project);
                let scale = scale_of(&e.project)?.to_vec();
                let mut rng = Rng::new(seed_of(a));
                let kind = s_opt(a, "kind").unwrap_or_else(|| "both".into());
                let amount = f_or(a, "amount", 0.3);
                let mut varied = Vec::new();
                for t in &e.project.tracks {
                    let r = role_of(t);
                    let pick = if want.is_empty() { matches!(r, Role::Lead | Role::Hat | Role::Perc) } else { want.contains(&t.name.to_lowercase()) };
                    if !pick { continue; }
                    let src = np.notes(&t.name).to_vec();
                    if src.is_empty() { continue; }
                    let v = mo::variation(&src, &kind, amount, key, &scale, np.steps() as f32, is_drum_role(r), &mut rng)?;
                    *np.notes_mut(&t.name) = v;
                    varied.push(t.name.clone());
                }
                e.project.patterns.push(np);
                if let Some(i) = a.get("insert_after").and_then(|v| v.as_u64()) {
                    if e.project.arrangement.is_empty() { e.project.arrangement = e.project.song_sections(); }
                    let at = (i as usize + 1).min(e.project.arrangement.len());
                    e.project.arrangement.insert(at, Section { pattern: name.clone(), repeats: 1 });
                }
                Ok(json!({"pattern": name, "varied_tracks": varied, "arrangement": e.project.song_sections().iter().map(|s| format!("{} x{}", s.pattern, s.repeats)).collect::<Vec<_>>()}))
            },
        },
        Tool {
            name: "add_transition",
            description: "Add a transition at a section boundary. into = the arrangement index (or pattern name) of the section that STARTS at the boundary. type: riser (noise+saw swell with an automated filter sweep over `bars` before), reverse_cymbal (swell into the downbeat), impact (low boom + crash on the downbeat), downlifter (falling sweep after), drum_fill (fill in the bar before; style), filter_build (master high-pass sweep up, snaps back), stutter / tape_stop / half_time (master FX switched on just before), silence (drop gap: master muted for the last `bars`, default a beat). Shared patterns are split into a unique copy so only that boundary changes.",
            mutates: true,
            schema: || obj(json!({
                "into": {"type": "string", "description": "section index (1 = second section) or pattern name"},
                "type": {"type": "string", "enum": ["riser", "reverse_cymbal", "impact", "downlifter", "drum_fill", "filter_build", "stutter", "tape_stop", "half_time", "silence"]},
                "bars": {"type": "number"}, "style": {"type": "string"}, "intensity": {"type": "number"}, "max_hz": {"type": "number"}, "seed": {"type": "integer"}
            }), &["into", "type"]),
            run: add_transition,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eng() -> Engine {
        Engine::new(std::env::temp_dir().join("beatbox_midi_tests"))
    }

    #[test]
    fn note_tools_roundtrip_through_engine() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "keys", "preset": "epiano"}))
            .unwrap();
        e.call("add_notes", &json!({"track": "keys", "notes": [{"start": 0, "len": 8, "pitch": "C4"}, {"start": 0, "len": 8, "pitch": "E4"}, {"start": 0, "len": 8, "pitch": "G4"}]})).unwrap();
        e.call(
            "arpeggiate",
            &json!({"track": "keys", "style": "updown", "rate": "1/16"}),
        )
        .unwrap();
        assert_eq!(e.project.patterns[0].notes("keys").len(), 8);
        e.call(
            "quantize",
            &json!({"track": "keys", "grid": "1/8", "strength": 1.0}),
        )
        .unwrap();
        e.call(
            "edit_notes",
            &json!({"track": "keys", "from": 4, "set_prob": 0.5, "set_offset": 0.1}),
        )
        .unwrap();
        assert!(e.project.patterns[0]
            .notes("keys")
            .iter()
            .any(|n| n.prob == 0.5));
        e.call("delete_notes", &json!({"track": "keys", "pitch": "G4"}))
            .unwrap();
        assert!(e.project.patterns[0]
            .notes("keys")
            .iter()
            .all(|n| n.pitch != 67));
        // prob notes still render deterministically
        let a = e.analyze().unwrap().master.rms_dbfs;
        e.revision += 1;
        let b = e.analyze().unwrap().master.rms_dbfs;
        assert_eq!(a, b);
    }

    #[test]
    fn midi_export_import_roundtrip() {
        let mut e = eng();
        e.call(
            "generate_beat",
            &json!({"style": "house", "bars": 1, "seed": 4}),
        )
        .unwrap();
        let out = e
            .call(
                "export_midi",
                &json!({"path": "rt_test.mid", "scope": "pattern", "pattern": "main"}),
            )
            .unwrap();
        assert!(out["bytes"].as_u64().unwrap() > 100);
        let kicks = e
            .project
            .patterns
            .iter()
            .find(|p| p.name == "main")
            .unwrap()
            .notes("kick")
            .len();
        let mut f = eng();
        f.call(
            "import_midi",
            &json!({"path": "rt_test.mid", "pattern": "imp"}),
        )
        .unwrap();
        let pi = f.project.pattern_index("imp").unwrap();
        assert_eq!(f.project.patterns[pi].notes("kick").len(), kicks);
        assert!((f.project.bpm - e.project.bpm).abs() < 0.1);
        assert!(f.project.tracks.iter().any(|t| t.name == "chords"));
    }

    #[test]
    fn structure_transitions_and_fill() {
        let mut e = eng();
        e.call(
            "generate_beat",
            &json!({"style": "trap", "bars": 2, "seed": 2}),
        )
        .unwrap();
        let r = e
            .call(
                "build_structure",
                &json!({"sections": "intro verse hook verse hook outro", "seed": 3}),
            )
            .unwrap();
        assert!(r["arrangement"].as_array().unwrap().len() >= 6, "{r}");
        assert!(e.project.tracks.iter().any(|t| t.name == "riser"));
        assert!(e.project.tracks.iter().any(|t| t.name == "impact"));
        // verse has no lead, hook does
        let v = e.project.pattern_index("verse").unwrap();
        assert!(e.project.patterns[v].notes("lead").is_empty());
        let h = e.project.pattern_index("hook").unwrap();
        assert!(!e.project.patterns[h].notes("lead").is_empty());
        let rep = e.analyze().unwrap();
        assert!(
            rep.loudness.true_peak_dbtp <= -0.5,
            "{}",
            rep.loudness.true_peak_dbtp
        );
        // stutter + silence transitions write master automation
        e.call(
            "add_transition",
            &json!({"into": "2", "type": "stutter", "bars": 1}),
        )
        .unwrap();
        e.call("add_transition", &json!({"into": "3", "type": "silence"}))
            .unwrap();
        assert!(e
            .project
            .automation
            .iter()
            .any(|l| l.target == "master" && l.param.ends_with(".mix")));
        assert!(e
            .project
            .automation
            .iter()
            .any(|l| l.target == "master" && l.param == "volume"));
        e.revision += 1;
        assert!(e.analyze().unwrap().master.rms_dbfs > -40.0);
    }

    #[test]
    fn harmonize_counter_variation_groove() {
        let mut e = eng();
        e.call(
            "generate_beat",
            &json!({"style": "lofi", "bars": 2, "seed": 9}),
        )
        .unwrap();
        let before = e
            .project
            .patterns
            .iter()
            .find(|p| p.name == "main")
            .unwrap()
            .notes("lead")
            .len();
        e.call("harmonize", &json!({"track": "lead", "pattern": "main", "interval": "3rd", "to_track": "lead_harm"})).unwrap();
        let pi = e.project.pattern_index("main").unwrap();
        assert_eq!(e.project.patterns[pi].notes("lead_harm").len(), before);
        e.call(
            "counter_melody",
            &json!({"track": "lead", "pattern": "main"}),
        )
        .unwrap();
        assert!(!e.project.patterns[pi].notes("counter").is_empty());
        e.call(
            "generate_variation",
            &json!({"track": "lead", "pattern": "main", "to_pattern": "main_b", "amount": 0.6}),
        )
        .unwrap();
        assert!(e.project.pattern_index("main_b").is_ok());
        e.call(
            "apply_groove",
            &json!({"pattern": "main", "template": "lazy", "hat_chance": 0.7}),
        )
        .unwrap();
        assert!(
            e.project.patterns[pi]
                .notes("snare")
                .iter()
                .all(|n| n.offset > 0.0)
                || e.project.patterns[pi].notes("snare").is_empty()
        );
        let k = e.call("detect_key", &json!({})).unwrap();
        assert!(k["key"].as_str().unwrap().len() >= 7);
        e.call(
            "generate_fill",
            &json!({"pattern": "main", "style": "snare_roll", "beats": 2}),
        )
        .unwrap();
        e.call("roll_notes", &json!({"track": "hat", "pattern": "main", "from": 28, "to": 32, "mode": "roll", "count": 3})).unwrap();
        e.call(
            "chord_voicing",
            &json!({"track": "chords", "pattern": "main", "mode": "drop2"}),
        )
        .unwrap();
        e.call(
            "strum",
            &json!({"track": "chords", "pattern": "main", "spread": 0.5}),
        )
        .unwrap();
        e.call("legato", &json!({"track": "bass", "pattern": "main"}))
            .unwrap();
        e.call(
            "velocity_curve",
            &json!({"track": "hat", "pattern": "main", "shape": "accent", "accents": "X.x.o.x."}),
        )
        .unwrap();
        e.call(
            "split_notes",
            &json!({"track": "bass", "pattern": "main", "every": "1/8"}),
        )
        .unwrap();
        e.call("merge_notes", &json!({"track": "bass", "pattern": "main"}))
            .unwrap();
        e.call(
            "vary_section",
            &json!({"pattern": "main", "name": "main_c", "insert_after": 1}),
        )
        .unwrap();
        assert!(e.project.arrangement.iter().any(|s| s.pattern == "main_c"));
    }
}
