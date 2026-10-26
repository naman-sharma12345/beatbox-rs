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
    match a.get("ids") {
        Some(Value::Array(v)) => {
            s.ids = v
                .iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .collect()
        }
        Some(Value::String(v)) => {
            s.ids = v
                .split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        }
        _ => {}
    }
    Ok(s)
}

pub fn selection_props() -> Value {
    json!({
        "from": {"type": "number", "description": "Select notes starting at/after this sixteenth-note step"},
        "to": {"type": "number", "description": "...and before this step"},
        "bars": {"type": "array", "items": {"type": "number"}, "description": "[first_bar, end_bar) 0-based, alternative to from/to, follows each pattern meter"},
        "pitch": {"description": "Only this pitch (MIDI or 'C4')"},
        "pitches": {"type": "array", "items": {}},
        "pitch_min": {}, "pitch_max": {},
        "vel_min": {"type": "number"}, "vel_max": {"type": "number"},
        "ids": {"type": ["array", "string"], "items": {"type": "string"}, "description": "Only the notes with these stable ids (from get_pattern / lookup_id)"}
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

/// Meter-aware selection for note operations. Bar ranges override from/to.
fn with_selected_notes(
    e: &mut Engine,
    a: &Value,
    mut f: impl FnMut(&mut Vec<Note>, &Selection, f32, f32) -> Result<usize>,
) -> Result<Value> {
    let ti = e.project.track_index(&s_req(a, "track")?)?;
    let name = e.project.tracks[ti].name.clone();
    let mut total = 0;
    let mut out = Vec::new();
    for pi in target_patterns(&e.project, a)? {
        let pat = &mut e.project.patterns[pi];
        let bar_steps = pat.steps_per_bar() as f32;
        let steps = pat.steps() as f32;
        let sel = selection_in_meter(a, bar_steps)?;
        let count = f(pat.notes_mut(&name), &sel, steps, bar_steps)?;
        total += count;
        let notes = pat.notes(&name);
        let preview = if e.project.tracks[ti].instrument.is_drum() {
            json!(grid(notes, pat_steps(steps)))
        } else {
            json!(notes.len())
        };
        out.push(json!({"pattern":pat.name,"affected":count,"notes":preview}));
    }
    Ok(json!({"track":name,"affected":total,"patterns":out}))
}

fn with_legacy_selected_notes(
    e: &mut Engine,
    a: &Value,
    mut f: impl FnMut(&mut Vec<Note>, &Selection, f32, f32) -> Result<usize>,
) -> Result<Value> {
    with_selected_notes(e, a, |notes, sel, steps, bar| {
        let count = f(notes, sel, steps, bar)?;
        notes.retain(|n| n.start < steps);
        mo::sort(notes);
        Ok(count)
    })
}

fn selection_in_meter(a: &Value, bar_steps: f32) -> Result<Selection> {
    let mut sel = selection_of(a)?;
    if let Some(bars) = a.get("bars") {
        let bounds = bars
            .as_array()
            .filter(|v| v.len() == 2)
            .ok_or_else(|| anyhow!("bars must be [first_bar, end_bar)"))?;
        let first = bounds[0]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or_else(|| anyhow!("first bar must be nonnegative"))?;
        let end = bounds[1]
            .as_f64()
            .filter(|v| v.is_finite() && *v > first)
            .ok_or_else(|| anyhow!("end bar must be after first bar"))?;
        sel.from = (first * bar_steps as f64) as f32;
        sel.to = (end * bar_steps as f64) as f32;
        if !sel.from.is_finite() || !sel.to.is_finite() {
            bail!("bar range is too large");
        }
    }
    Ok(sel)
}

fn pat_steps(s: f32) -> u32 {
    s as u32
}

/// Grid / rate in steps: number, or "1/16", "1/8", "1/8t", "1/4.", "1 bar".
pub fn steps_of(v: Option<&Value>, default: f32) -> Result<f32> {
    match v {
        None => Ok(default),
        Some(x) if x.is_number() => {
            let steps = x.as_f64().unwrap() as f32;
            if !steps.is_finite() || steps <= 0.0 {
                bail!("steps must be finite and positive");
            }
            Ok(steps)
        }
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
        DrumKind::Tabla => 60,
        DrumKind::Bayan => 61,
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
                    ..Default::default()
                }))
            },
            instruments::Layer {
                gain_db: -6.0,
                ..instruments::Layer::of(Instrument::Drum(instruments::DrumParams {
                    kind: DrumKind::Crash,
                    tune: -2.0,
                    decay: 2.0,
                    drive: 0.0,
                    ..Default::default()
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

/// One LFO sample in -1..1 at `pos` periods (fractional part is the phase).
/// tension > 1 sharpens the peaks, < 1 rounds them.
pub fn lfo_value(shape: &str, pos: f32, tension: f32, rng: &mut Rng) -> f32 {
    let ph = pos - pos.floor();
    let v = match shape {
        "triangle" => 1.0 - 4.0 * (ph - 0.5).abs(),
        "saw_up" => 2.0 * ph - 1.0,
        "saw_down" => 1.0 - 2.0 * ph,
        "square" => {
            if ph < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        "random" => rng.bipolar(),
        _ => (2.0 * std::f32::consts::PI * ph).sin(),
    };
    v.signum() * v.abs().powf(tension)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "remove_zero_notes",
            description: "Remove selected notes whose velocity or trigger chance is exactly zero. Not a threshold: very quiet and low-chance notes remain. Preserves surviving note fields, IDs and order, not muted tracks. Deletion changes note indices and can change seeded chance outcomes on remaining notes. Defaults to all notes; bar filters follow each pattern meter. One Undo step; no-op adds no history.",
            mutates: true,
            schema: || obj(props(target_props(),selection_props()), &["track"]),
            run: |e,a| {
                let mut result=with_selected_notes(e,a,|notes,sel,_,_|Ok(mo::remove_zero_notes(notes,sel)))?;
                result["removed"]=result["affected"].clone();Ok(result)
            },
        },
        Tool {
            name: "remove_duplicate_notes",
            description: "Remove exact musical duplicates among selected notes, keeping the first selected copy and its stable ID. Exact match of start, length, pitch, velocity, chance, microtiming and slide destination required. Notes with different expression are preserved. Unselected notes never change and do not suppress selected copies. Default all notes. Bar filters follow each pattern meter. One undo step.",
            mutates: true,
            schema: || obj(props(target_props(),selection_props()), &["track"]),
            run: |e,a| {
                let mut result=with_selected_notes(e,a,|notes,sel,_,_|Ok(mo::remove_exact_duplicates(notes,sel)))?;
                result["removed"]=result["affected"].clone();Ok(result)
            },
        },
        Tool {
            name:"set_note_offset",
            description:"Set selected per-note microtiming offsets in sixteenth steps (-0.5..0.5, negative=early, positive=late). Changes only offset, not nominal starts, note order, IDs, pitch, length, velocity, chance or slide targets. Global swing stays. Playback/export apply the offset with their normal song-start clamp. Same targets/selections as edit_notes; strict finite range. One Undo step; no-op adds no history.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"offset":{"type":"number","minimum":-0.5,"maximum":0.5}})), &["track","offset"]),
            run: |e,a|{let offset=a.get("offset").and_then(Value::as_f64).filter(|v|v.is_finite()&&(-0.5..=0.5).contains(v)).ok_or_else(||anyhow!("offset must be a finite number between-0.5 and0.5"))? as f32;with_selected_notes(e,a,|notes,sel,_,_|mo::set_note_offset(notes,sel,offset))},
        },
        Tool {
            name:"ramp_note_velocities",
            description:"Linear MIDI velocity ramp between selected earliest and latest nominal starts, with strict finite start_velocity/end_velocity0..1, including true0. Shared-onset notes get equal velocity; a single onset uses start_velocity. Changes velocity only, preserving starts, lengths, order, IDs and expression. Each targeted pattern gets its own ramp. Same targets/selections as edit_notes. One Undo step; no-op adds no history.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"start_velocity":{"type":"number","minimum":0,"maximum":1},"end_velocity":{"type":"number","minimum":0,"maximum":1}})), &["track","start_velocity","end_velocity"]),
            run: |e,a|{let read=|key:&str|->Result<f32>{Ok(a.get(key).and_then(Value::as_f64).filter(|v|v.is_finite()&&(0.0..=1.0).contains(v)).ok_or_else(||anyhow!("velocity endpoints must be finite between0 and1"))? as f32)};let start=read("start_velocity")?;let end=read("end_velocity")?;with_selected_notes(e,a,|notes,sel,_,_|mo::ramp_velocities(notes,sel,start,end))},
        },
        Tool {
            name:"offset_note_velocities",
            description:"Add a signed MIDI velocity amount (-1..1) to selected notes, clamping only velocity to0..1, including true silence. Preserves note order, IDs, starts, lengths, pitch and expression. Same targets/selections as edit_notes. Finite delta required. One Undo step; unchanged edits add no history.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"delta":{"type":"number","minimum":-1,"maximum":1}})), &["track","delta"]),
            run: |e,a| {
                let delta=a.get("delta").and_then(Value::as_f64).filter(|v|v.is_finite()&&(-1.0..=1.0).contains(v)).ok_or_else(||anyhow!("delta must be finite between-1 and1"))? as f32;
                with_selected_notes(e,a,|notes,sel,_,_| {
                    if notes.iter().filter(|n|sel.matches(n)).any(|n|!n.vel.is_finite()||!(0.0..=1.0).contains(&n.vel)) {bail!("selected velocities must be finite between0 and1");}
                    let mut changed=0;
                    for n in notes.iter_mut().filter(|n|sel.matches(n)) {let value=(n.vel+delta).clamp(0.0,1.0);if value!=n.vel {n.vel=value;changed+=1;}}
                    Ok(changed)
                })
            },
        },
        Tool {
            name:"set_note_velocity",
            description:"Set selected MIDI velocity0..1, including true zero (silence). Changes velocity only: no normalization of lengths and no changes to starts, order, IDs, pitches, chance, offsets or slide targets. Same targets/selections as edit_notes. Strict finite range; no-change edits add no history. One Undo step.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"velocity":{"type":"number","minimum":0,"maximum":1}})), &["track","velocity"]),
            run: |e,a|{let velocity=a.get("velocity").and_then(Value::as_f64).filter(|v|v.is_finite()&&(0.0..=1.0).contains(v)).ok_or_else(||anyhow!("velocity must be a finite number between0 and1"))? as f32;with_selected_notes(e,a,|notes,sel,_,_|mo::set_velocity(notes,sel,velocity))},
        },
        Tool {
            name:"ramp_note_chance",
            description:"Linearly ramp selected per-note trigger chance between earliest/latest nominal starts. Strict finite start_chance/end_chance0..1;0=never,1=always. Shared-onset notes get equal chance; a single onset uses start_chance. Only probability changes; no deletion, order, velocity, timing or other expression changes. Each target pattern gets its own ramp. Same targets/selections as edit_notes. Seeded probability is not a guaranteed hit count. One Undo step; no-op adds no history.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"start_chance":{"type":"number","minimum":0,"maximum":1},"end_chance":{"type":"number","minimum":0,"maximum":1}})), &["track","start_chance","end_chance"]),
            run: |e,a| {let read=|key:&str|->Result<f32>{Ok(a.get(key).and_then(Value::as_f64).filter(|v|v.is_finite()&&(0.0..=1.0).contains(v)).ok_or_else(||anyhow!("chance endpoints must be finite between0 and1"))? as f32)};let start=read("start_chance")?;let end=read("end_chance")?;with_selected_notes(e,a,|notes,sel,_,_|mo::ramp_note_chance(notes,sel,start,end))},
        },
        Tool {
            name:"set_note_chance",
            description:"Set selected per-note trigger probability (chance0..1,0=never,1=always). Changes only probability: no velocity/length normalization and no changes to starts, order, IDs, pitch, timing offsets or slide targets. Same targets/selections as edit_notes. Strict finite range; no-op edits add no history. One Undo step.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"chance":{"type":"number","minimum":0,"maximum":1}})), &["track","chance"]),
            run: |e,a|{let chance=a.get("chance").and_then(Value::as_f64).filter(|v|v.is_finite()&&(0.0..=1.0).contains(v)).ok_or_else(||anyhow!("chance must be a finite number between0 and1"))? as f32;with_selected_notes(e,a,|notes,sel,_,_|mo::set_note_chance(notes,sel,chance))},
        },
        Tool {
            name:"reset_note_humanization",
            description:"Reset selected per-note timing offsets to0, chance to100%, or both. Does not change note starts, order, pitch, length, velocity, IDs or slide targets. Does not reset global swing or other groove settings. Same selection/targets as edit_notes, with strict mode timing|chance|both (default both). Empty/no-change edits do not add history. One Undo step.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"mode":{"type":"string","enum":["timing","chance","both"],"default":"both"}})), &["track"]),
            run: |e,a|{let mode=match a.get("mode"){None=>"both",Some(v)=>v.as_str().ok_or_else(||anyhow!("mode must be timing, chance or both"))?};let(timing,chance)=match mode{"timing"=>(true,false),"chance"=>(false,true),"both"=>(true,true),_=>bail!("mode must be timing, chance or both")};with_selected_notes(e,a,|notes,sel,_,_|mo::reset_humanization(notes,sel,timing,chance))},
        },
        Tool {
            name:"rotate_note_lengths",
            description:"Rotate existing selected MIDI durations through nominal onset order: shift1 moves lengths one note later with wraparound, shift-1 earlier. Changes lengths only, preserving starts, order, IDs, pitch and expression. Requires one selected note per onset. Rejects atomically if a resulting note exceeds the pattern or has invalid length. Strict signed32bit integer shift. Same targets/selections as edit_notes; one Undo step.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"shift":{"type":"integer"}})), &["track","shift"]),
            run: |e,a|{let shift=a.get("shift").and_then(Value::as_i64).and_then(|n|i32::try_from(n).ok()).ok_or_else(||anyhow!("shift must be a signed32bit integer"))?;with_selected_notes(e,a,|notes,sel,steps,_|mo::rotate_lengths(notes,sel,shift,steps))},
        },
        Tool {
            name:"invert_note_lengths",
            description:"Mirror selected MIDI durations within their current min/max range: new length=min+max-old. Changes lengths only, preserving starts, order, IDs, pitches and expression. Supports chords. Rejects the entire edit if a resulting length exceeds its pattern or a selected note has invalid bounds. Same targets/selections as edit_notes; one Undo step. Empty or equal-length selections add no history.",
            mutates:true,
            schema: || obj(props(target_props(),selection_props()), &["track"]),
            run: |e,a|with_selected_notes(e,a,|notes,sel,steps,_|mo::invert_lengths(notes,sel,steps)),
        },
        Tool {
            name:"balance_chord_lengths",
            description:"Give selected MIDI notes at each shared nominal onset that chord's mean duration. Changes lengths only, preserving starts, order, IDs, pitches and expression. Solo onsets stay unchanged. Partial selection balances only selected voices. Rejects the whole edit if selected bounds or resulting lengths do not fit the pattern. Same targets/selections as edit_notes; one Undo step. No-change edits add no history.",
            mutates:true,
            schema: || obj(props(target_props(),selection_props()), &["track"]),
            run: |e,a|with_selected_notes(e,a,|notes,sel,steps,_|mo::balance_chord_lengths(notes,sel,steps)),
        },
        Tool {
            name:"set_note_lengths",
            description:"Set one fixed duration in sixteenth steps (minimum0.05) for selected MIDI notes. Changes lengths only, preserving starts, order, IDs, pitches and expression. Rejects the entire edit if any selected note has invalid bounds or the new length cannot fit its pattern. Same targets/selections as edit_notes; strict finite length; one Undo step; no-op adds no history.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"length":{"type":"number","minimum":0.05}})), &["track","length"]),
            run: |e,a|{let v=a.get("length").and_then(Value::as_f64).filter(|v|v.is_finite()&&*v>=0.05&&*v<=f32::MAX as f64).ok_or_else(||anyhow!("length must be finite and at least0.05 steps"))? as f32;with_selected_notes(e,a,|notes,sel,steps,_|mo::set_lengths(notes,sel,v,steps))},
        },
        Tool {
            name:"scale_note_lengths",
            description:"Scale selected MIDI durations only by factor0.0625..16, without moving starts or normalizing velocity. Preserves order, IDs, pitch and all expression. Supports chords. Rejects the whole edit if any resulting length is below0.05 steps, exceeds its pattern or has invalid bounds. Same targets/selections as edit_notes; one Undo step. No-change edits add no history.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"factor":{"type":"number","minimum":0.0625,"maximum":16}})), &["track","factor"]),
            run: |e,a|{let factor=a.get("factor").and_then(Value::as_f64).filter(|v|v.is_finite()&&(0.0625..=16.).contains(v)).ok_or_else(||anyhow!("factor must be a finite number between0.0625 and16"))? as f32;with_selected_notes(e,a,|notes,sel,steps,_|mo::scale_lengths(notes,sel,factor,steps))},
        },
        Tool {
            name:"reverse_note_lengths",
            description:"Reverse selected MIDI note durations in nominal onset order without moving notes. Changes lengths only, preserving order, IDs, starts, pitches, velocity and expression. Requires one selected note per onset (within0.0001 steps); partial selection can choose one voice of a chord. Rejects atomically if any resulting note exceeds its pattern end or length is invalid. Same target and selection controls as edit_notes. One Undo step.",
            mutates:true,
            schema: || obj(props(target_props(),selection_props()), &["track"]),
            run: |e,a|with_selected_notes(e,a,|notes,sel,steps,_|mo::reverse_lengths(notes,sel,steps)),
        },
        Tool {
            name:"trim_notes_to_pattern",
            description:"Shorten selected MIDI notes that sustain beyond their pattern's end. Leaves shorter notes alone, never lengthens/deletes notes, never changes starts/IDs/pitches/velocity/chance/microtiming/slides. Rejects atomically if an affected note has less than0.05 steps before the end. Uses nominal MIDI starts/lengths, not swing or microtiming. Same targets/selection as edit_notes; one Undo step.",
            mutates:true,
            schema: || obj(props(target_props(),selection_props()), &["track"]),
            run: |e,a|with_selected_notes(e,a,|notes,sel,steps,_|mo::trim_pattern_ends(notes,sel,steps)),
        },
        Tool {
            name: "trim_note_overlaps",
            description: "Shorten selected notes at the next start of the same pitch when they overlap. Other pitches/chords are untouched. Unselected same-pitch notes act as boundaries but are never changed. Never lengthens/deletes notes or changes IDs, velocity, chance, slides or microtiming. Equal-start doubles and gaps below0.05 steps are skipped and counted. Uses nominal MIDI starts, not swing/microtiming; one undo step.",
            mutates: true,
            schema: || obj(props(target_props(), selection_props()), &["track"]),
            run: |e,a| {
                let mut skipped=0;
                let mut result=with_selected_notes(e,a,|notes,sel,_,_| {
                    let (changed,s)=mo::trim_overlaps(notes,sel);skipped+=s;Ok(changed)
                })?;
                result["shortened"]=result["affected"].clone();result["skipped"]=json!(skipped);Ok(result)
            },
        },
        Tool {
            name: "rotate_notes",
            description: "Rotate selected note starts inside their own bar with wraparound: shift1 moves a sixteenth later, shift-1 earlier. Follows each pattern's meter, keeps notes in their original bar and preserves lengths, IDs, pitch, velocity and expression. Same selection/target controls as edit_notes. Notes near the bar end may sustain across the boundary because lengths are unchanged. One undo step.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "shift": {"type":"number","description":"signed shift in sixteenth-note steps"}
            })), &["track","shift"]),
            run: |e,a| {
                let shift=f_opt(a,"shift").ok_or_else(||anyhow!("shift is required"))?;
                if !shift.is_finite() { bail!("shift must be finite"); }
                let mut result=with_selected_notes(e,a,|notes,sel,_,bar_steps|mo::rotate_rhythm(notes,sel,shift,bar_steps))?;
                result["shift"]=json!(shift);Ok(result)
            },
        },
        Tool {
            name:"fit_note_velocity_range",
            description:"Fit selected MIDI velocities linearly into low..high (0..1), preserving relative dynamics. Chords supported. Equal-valued selections use low. Changes only velocity; true zero is allowed. Same targets/selections as edit_notes; each targeted pattern fits independently. Strict finite low <= high. One Undo step; no-op adds no history.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"low":{"type":"number","minimum":0,"maximum":1},"high":{"type":"number","minimum":0,"maximum":1}})), &["track","low","high"]),
            run: |e,a| { let read=|key:&str|->Result<f32>{Ok(a.get(key).and_then(Value::as_f64).filter(|v|v.is_finite()&&(0.0..=1.0).contains(v)).ok_or_else(||anyhow!("velocity bounds must be finite between0 and1"))? as f32)};let low=read("low")?;let high=read("high")?;with_selected_notes(e,a,|notes,sel,_,_|mo::fit_velocity_range(notes,sel,low,high)) },
        },
        Tool {
            name:"balance_chord_velocities",
            description:"Equalize selected MIDI note velocities separately at each shared nominal onset (within0.0001 steps), preserving that chord's average velocity. Single selected notes are unchanged. Partial chords balance only selected notes. Changes no rhythm, pitch, length, IDs, probability, microtiming or slide targets. Same targets/selection as edit_notes; one Undo step.",
            mutates:true,
            schema: || obj(props(target_props(),selection_props()), &["track"]),
            run: |e,a|with_selected_notes(e,a,|notes,sel,_,_|mo::balance_chord_velocities(notes,sel)),
        },
        Tool {
            name:"invert_note_velocities",
            description:"Mirror selected MIDI velocities within their current softest-to-loudest range: loud becomes soft and soft becomes loud, preserving the range. Supports chords. Changes only velocity, not order, rhythm, pitches, length, ID or other expression. Equal velocities are a no-op. Same targets/selection as edit_notes; one Undo step.",
            mutates:true,
            schema: || obj(props(target_props(),selection_props()), &["track"]),
            run: |e,a|with_selected_notes(e,a,|notes,sel,_,_|mo::invert_velocities(notes,sel)),
        },
        Tool {
            name:"reverse_note_velocities",
            description:"Reverse the existing selected velocity sequence in chronological single-note onset order. Changes only velocity, not rhythm, pitches, lengths, IDs, probability, microtiming or slide targets. Keeps the exact original velocity values. Simultaneous selected notes are rejected atomically. Same target/selection as edit_notes; one undo step.",
            mutates:true,
            schema: || obj(props(target_props(),selection_props()), &["track"]),
            run: |e,a|with_selected_notes(e,a,|notes,sel,_,_|mo::reverse_velocities(notes,sel)),
        },
        Tool {
            name:"rotate_note_velocities",
            description:"Cycle existing selected MIDI velocities through chronological single-note onsets. shift+1 moves accents one note later with wraparound; -1 moves earlier. Keeps the exact velocity values and changes no rhythm, pitches, lengths, IDs, chance, microtiming or slide targets. Select one note per onset; simultaneous selected notes are rejected atomically. Same target/selection as edit_notes; one undo step.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"shift":{"type":"integer","minimum":-2147483648_i64,"maximum":2147483647_i64}})), &["track","shift"]),
            run: |e,a| {let shift=a.get("shift").and_then(Value::as_i64).and_then(|n|i32::try_from(n).ok()).ok_or_else(||anyhow!("shift must be a signed 32-bit integer"))?;with_selected_notes(e,a,|notes,sel,_,_|mo::rotate_velocities(notes,sel,shift))},
        },
        Tool {
            name:"reverse_melody_pitches",
            description:"Reverse selected pitches in chronological onset order without moving note starts, lengths, IDs or expression including slide targets. Unlike reverse-time, rhythm stays fixed; unlike pitch flip, pitches are not inverted. Rejects simultaneous selected chords atomically. Same target/selection as edit_notes; one undo step.",
            mutates:true,
            schema: || obj(props(target_props(),selection_props()), &["track"]),
            run: |e,a|with_selected_notes(e,a,|notes,sel,_,_|mo::reverse_pitches(notes,sel)),
        },
        Tool {
            name:"reverse_rhythm_gaps",
            description:"Reverse the order of gaps between selected onset groups, keeping first/last anchors and the note/pitch sequence fixed. Example0/1/4/8 becomes0/4/7/8. Chords move together with relative microspacing. Only starts change; IDs, lengths and expression stay unchanged. Needs three onset groups; rejects out-of-pattern results atomically. Same target/selection as edit_notes; one undo step.",
            mutates:true,
            schema: || obj(props(target_props(),selection_props()), &["track"]),
            run: |e,a|with_selected_notes(e,a,|notes,sel,steps,_|mo::reverse_rhythm_gaps(notes,sel,steps)),
        },
        Tool {
            name:"rotate_melody_pitches",
            description:"Cycle selected melody pitches through chronological single-note onsets. shift+1 moves pitches one note later with wraparound; -1 moves earlier. Only pitches change; starts, lengths, IDs, velocity and expression including slide targets stay unchanged. Rejects simultaneous selected notes rather than arbitrarily rearranging chords. Same target/selection as edit_notes; one undo step.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"shift":{"type":"integer","minimum":-2147483648_i64,"maximum":2147483647_i64}})), &["track","shift"]),
            run: |e,a| {let shift=a.get("shift").and_then(Value::as_i64).and_then(|n|i32::try_from(n).ok()).ok_or_else(||anyhow!("shift must be a signed 32-bit integer"))?;with_selected_notes(e,a,|notes,sel,_,_|mo::rotate_pitches(notes,sel,shift))},
        },
        Tool {
            name: "align_chord_ends",
            description: "Align selected chord releases to the longest existing release (default) or shortest existing release in each same-onset group. Only lengths change. Starts, IDs, pitches and expression remain unchanged; single notes and unselected notes are untouched. Rejects invalid bounds atomically. Same target/selection as edit_notes; one undo step.",
            mutates:true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"mode":{"type":"string","enum":["longest","shortest"]}})), &["track"]),
            run: |e,a| {let longest=match a.get("mode") {None=>true,Some(v) if v.as_str()==Some("longest")=>true,Some(v) if v.as_str()==Some("shortest")=>false,_=>bail!("mode must be longest or shortest")};with_selected_notes(e,a,|notes,sel,steps,_|mo::align_chord_ends(notes,sel,longest,steps))},
        },
        Tool {
            name: "distribute_note_rhythm",
            description: "Evenly space selected onset groups between their existing first and last positions. Chords stay together; endpoints, note lengths, IDs, pitches and expression stay unchanged. Requires at least three selected onset groups. Rejects the whole edit if a moved note would leave the pattern. Same target/selection as edit_notes; one undo step.",
            mutates: true,
            schema: || obj(props(target_props(),selection_props()), &["track"]),
            run: |e,a| with_selected_notes(e,a,|notes,sel,steps,_|mo::distribute_rhythm(notes,sel,steps)),
        },
        Tool {
            name: "fold_note_register",
            description: "Fold selected MIDI pitches into low..high using the smallest whole-octave move, preserving pitch classes. Notes already inside stay unchanged. Rejects the whole edit if any selected pitch class cannot fit. Only pitches change; slide targets and other expression stay unchanged. Same target/selection as edit_notes; one undo step.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({"low":{"type":"integer","minimum":0,"maximum":127},"high":{"type":"integer","minimum":0,"maximum":127}})), &["track","low","high"]),
            run: |e,a| {
                let bound=|key:&str|->Result<u8>{a.get(key).and_then(Value::as_u64).filter(|v|*v<=127).map(|v|v as u8).ok_or_else(||anyhow!("{key} must be an integer from 0 to 127"))};
                let (low,high)=(bound("low")?,bound("high")?);
                with_selected_notes(e,a,|notes,sel,_,_|mo::fold_register(notes,sel,low,high))
            },
        },
        Tool {
            name: "reduce_chord_voices",
            description: "Keep only the highest or lowest selected pitch at each simultaneous onset (within0.0001 steps). Retains same-pitch layers, single notes, unselected notes and all surviving IDs/expression. Groups onsets, not overlapping sustains; useful to extract a top line or bass line from chords. Same targets/selections as edit_notes; one undo step.",
            mutates: true,
            schema: || obj(props(props(target_props(),selection_props()),json!({"voice":{"type":"string","enum":["highest","lowest"],"default":"highest"}})), &["track"]),
            run: |e,a| {let voice=s_opt(a,"voice").unwrap_or_else(||"highest".into());if voice!="highest" && voice!="lowest" {bail!("voice must be highest or lowest");}let mut r=with_selected_notes(e,a,|ns,sel,_,_|mo::reduce_chords(ns,sel,voice=="highest"))?;r["removed"]=r["affected"].clone();Ok(r)},
        },
        Tool {
            name: "quantize_note_ends",
            description: "Quantize selected MIDI release positions only, leaving starts, microtiming and all expression unchanged. Grid in sixteenth steps; strength0..1. Rejects entire edit if any release makes a note shorter than0.05 steps or exceeds the pattern. Same targets/selections as edit_notes; one undo step.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()),json!({"grid":{"type":"number","minimum":0.0625},"strength":{"type":"number","minimum":0,"maximum":1,"default":1}})), &["track","grid"]),
            run: |e,a| {let g=f_opt(a,"grid").ok_or_else(||anyhow!("grid is required"))?;let st=f_or(a,"strength",1.0);with_selected_notes(e,a,|ns,sel,steps,_|mo::quantize_ends(ns,sel,g,st,steps))},
        },
        Tool {
            name: "transpose_phrase",
            description: "Transpose selected MIDI pitches together by integer semitones, preserving intervals. Rejects the whole edit if any selected pitch would leave 0..127. Changes only pitch, not slide targets, timing, IDs, velocity or other expression. Same target/selection as edit_notes; one undo step.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "semitones": {"type":"integer","minimum":-127,"maximum":127}
            })), &["track","semitones"]),
            run: |e,a| {
                let semitones=a.get("semitones").and_then(Value::as_i64).filter(|v|(-127..=127).contains(v)).ok_or_else(||anyhow!("semitones must be an integer from -127 to 127"))? as i32;
                with_selected_notes(e,a,|notes,sel,_,_|mo::transpose_phrase(notes,sel,semitones))
            },
        },
        Tool {
            name: "shift_phrase",
            description: "Move selected MIDI notes together by signed sixteenth steps, preserving spacing and lengths. Rejects the whole edit if any selected note would leave the pattern. Same target/selection as edit_notes; one undo step.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "shift": {"type":"number","description":"signed sixteenth steps"}
            })), &["track","shift"]),
            run: |e,a| {
                let shift=f_opt(a,"shift").ok_or_else(||anyhow!("shift is required"))?;
                with_selected_notes(e,a,|notes,sel,steps,_|mo::shift_phrase(notes,sel,shift,steps))
            },
        },
        Tool {
            name: "normalize_velocity",
            description: "Scale selected MIDI velocities so the loudest reaches target peak0.02..1 (default0.9), preserving relative velocity ratios rather than flattening dynamics. Changes no timing, pitches, probability, IDs or other expression. Same target/selection as edit_notes; one undo step.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "peak": {"type":"number","minimum":0.02,"maximum":1,"default":0.9}
            })), &["track"]),
            run: |e,a| {
                let peak=f_or(a,"peak",0.9);
                with_selected_notes(e,a,|notes,sel,_,_|mo::normalize_velocity(notes,sel,peak))
            },
        },
        Tool {
            name: "thin_notes",
            description: "Thin a selected rhythm by keeping one onset group in every N. Simultaneous selected chord notes stay together. every2..64; phase0..every-1 chooses which group survives. Preserves unselected notes and all kept IDs/expression. Same target/selection as edit_notes; one undo step.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "every": {"type":"integer","minimum":2,"maximum":64,"default":2},
                "phase": {"type":"integer","minimum":0,"maximum":63,"default":0}
            })), &["track"]),
            run: |e,a| {
                let every=match a.get("every") {Some(v)=>v.as_u64().filter(|n|(2..=64).contains(n)).ok_or_else(||anyhow!("every must be an integer from2 to64"))? as u32,None=>2};
                let phase=match a.get("phase") {Some(v)=>v.as_u64().filter(|n|*n<every as u64).ok_or_else(||anyhow!("phase must be an integer below every"))? as u32,None=>0};
                with_selected_notes(e,a,|notes,sel,_,_|mo::thin_rhythm(notes,sel,every,phase))
            },
        },
        Tool {
            name: "repeat_notes",
            description: "Add editable copies of a selected MIDI phrase without fading, changing expression, or moving originals. period is the distance between phrase starts in steps or a note value; repeats1..64. All copies must fit the pattern or the whole edit is rejected. Same selection/target as edit_notes; one undo step.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "period": {"type":["number","string"],"description":"positive sixteenth steps or note value"},
                "repeats": {"type":"integer","minimum":1,"maximum":64,"default":1}
            })), &["track","period"]),
            run: |e,a| {
                if a.get("period").is_none() { bail!("period is required"); }
                let period=steps_of(a.get("period"),16.0)?;
                let repeats=match a.get("repeats") {Some(v)=>v.as_u64().filter(|n|(1..=64).contains(n)).ok_or_else(||anyhow!("repeats must be an integer from1 to64"))? as u32,None=>1};
                with_selected_notes(e,a,|notes,sel,steps,_|mo::repeat_phrase(notes,sel,period,repeats,steps))
            },
        },
        Tool {
            name: "stretch_notes",
            description: "Scale selected MIDI note timing and lengths without changing song tempo, pitch, velocity or stable IDs. factor0.5 = twice as fast,2 = twice as slow. anchor is an absolute step, default earliest selected start. Rejects the whole edit if any selected note would leave the pattern or become shorter than0.05 steps. Same target/selection controls as edit_notes; one undo step.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "factor": {"type":"number","minimum":0.0625,"maximum":16},
                "anchor": {"type":"number","minimum":0}
            })), &["track","factor"]),
            run: |e,a| {
                let factor=f_opt(a,"factor").ok_or_else(|| anyhow!("factor is required"))?;
                let anchor=f_opt(a,"anchor");
                with_selected_notes(e,a,|notes,sel,steps,_|mo::stretch_phrase(notes,sel,factor,anchor,steps))
            },
        },
        Tool {
            name: "echo_notes",
            description: "Add editable MIDI echoes of selected notes, keeping originals unchanged. delay is steps or a note value such as1/8; repeats1..64; decay0..1 multiplies velocity per repeat; transpose is semitones per repeat. Uses the same selection/target as edit_notes. Copies beyond the pattern, MIDI pitch range or below2% velocity are omitted; lengths stop at the pattern boundary. One undo step. Not an audio effect.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "delay": {"type":["number","string"],"description":"positive steps or note value, default1/8"},
                "repeats": {"type":"integer","minimum":1,"maximum":64,"default":3},
                "decay": {"type":"number","minimum":0,"maximum":1,"default":0.6},
                "transpose": {"type":"integer","minimum":-127,"maximum":127,"default":0}
            })), &["track"]),
            run: |e, a| {
                let delay = steps_of(a.get("delay"), 2.0)?;
                let repeats = match a.get("repeats") {
                    Some(v) => v.as_u64().filter(|n| (1..=64).contains(n)).ok_or_else(|| anyhow!("repeats must be an integer from1 to64"))? as u32,
                    None => 3,
                };
                let transpose = match a.get("transpose") {
                    Some(v) => v.as_i64().filter(|n| (-127..=127).contains(n)).ok_or_else(|| anyhow!("transpose must be an integer from-127 to127"))? as i32,
                    None => 0,
                };
                let decay = f_or(a,"decay",0.6);
                with_selected_notes(e, a, |notes, sel, steps, _| mo::note_echoes(notes, sel, delay, repeats, decay, transpose, steps))
            },
        },
        Tool {
            name: "update_note_geometry",
            description: "Update start, length and pitch of existing notes by stable ID in one track/pattern. Intended for manual move/resize gestures. Preserves IDs, order, velocity, chance, offsets, slides and all unlisted notes. Requires unique existing IDs; finite nonnegative starts, positive lengths and MIDI pitches0..127. Does not prune notes outside the pattern. One Undo step; validates all updates before writing.",
            mutates: true,
            schema: || obj(json!({"track":{"type":"string"},"pattern":pattern_prop(),"updates":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"start":{"type":"number"},"len":{"type":"number"},"pitch":{"type":"integer"}},"required":["id","start","len","pitch"]}}}), &["track","updates"]),
            run: |e,a| {
                let name=e.project.tracks[e.project.track_index(&s_req(a,"track")?)?].name.clone();
                let pi=pattern_idx(&e.project,a)?;
                let updates=a["updates"].as_array().ok_or_else(||anyhow!("updates must be an array"))?;
                let notes=e.project.patterns[pi].notes(&name);
                let mut seen=std::collections::HashSet::new();let mut plan=Vec::new();
                for u in updates {
                    let id=s_req(u,"id")?;
                    if id.is_empty() || !seen.insert(id.clone()) {bail!("update ids must be nonempty and unique");}
                    let matches:Vec<_>=notes.iter().enumerate().filter(|(_,n)|n.id==id).map(|(i,_)|i).collect();
                    if matches.len()!=1 {bail!("note id '{id}' must identify exactly one note in this clip");}
                    let start=u["start"].as_f64().filter(|v|v.is_finite()&&*v>=0.0&&*v<=f32::MAX as f64).ok_or_else(||anyhow!("start must be finite and nonnegative"))? as f32;
                    let len=u["len"].as_f64().filter(|v|v.is_finite()&&*v>0.0&&*v<=f32::MAX as f64).ok_or_else(||anyhow!("length must be finite and positive"))? as f32;
                    if len<=0.0 || !(start+len).is_finite() {bail!("invalid note bounds");}
                    let pitch=u["pitch"].as_u64().filter(|p|*p<=127).ok_or_else(||anyhow!("pitch must be an integer0..127"))? as u8;
                    plan.push((matches[0],start,len,pitch));
                }
                let notes=e.project.patterns[pi].notes_mut(&name);let mut changed=0;
                for(i,start,len,pitch)in plan {let n=&mut notes[i];if (n.start,n.len,n.pitch)!=(start,len,pitch){n.start=start;n.len=len;n.pitch=pitch;changed+=1;}}
                Ok(json!({"track":name,"affected":changed}))
            },
        },
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
                with_legacy_selected_notes(e, a, |n, sel, steps, _| Ok(mo::edit(n, sel, &spec, steps)))
            },
        },
        Tool {
            name: "delete_notes",
            description: "Delete the selected notes of a track (same selection as edit_notes: from/to, bars, pitch, pitches, pitch_min/max, vel_min/max). With no selection, clears the track in the pattern.",
            mutates: true,
            schema: || obj(props(target_props(), selection_props()), &["track"]),
            run: |e, a| {
                with_selected_notes(e, a, |n, sel, _, _| {
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
                let g = steps_of(a.get("grid"), 1.0)?;
                let (st, sw, ends) = (f_or(a, "strength", 1.0), f_or(a, "swing", 0.0), b_or(a, "ends", false));
                with_legacy_selected_notes(e, a, |n, sel, _, _| Ok(mo::quantize(n, sel, g, st, sw, ends)))
            },
        },
        Tool {
            name: "split_notes",
            description: "Split selected notes into repeats every `every` steps (e.g. a held 808 into 8ths) or once at absolute step `at`.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({"every": {"description": "steps or '1/8'"}, "at": {"type": "number"}})), &["track"]),
            run: |e, a| {
                let every = a.get("every").map(|v| steps_of(Some(v), 1.0)).transpose()?;
                let at = f_opt(a, "at");
                if every.is_none() && at.is_none() {
                    bail!("give every (steps) or at (step)");
                }
                with_legacy_selected_notes(e, a, |n, sel, _, _| Ok(mo::split(n, sel, every, at)))
            },
        },
        Tool {
            name: "merge_notes",
            description: "Merge consecutive same-pitch selected notes into one long note when the gap between them is <= max_gap steps (default 0).",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({"max_gap": {"type": "number"}})), &["track"]),
            run: |e, a| {
                let g = f_or(a, "max_gap", 0.0);
                with_legacy_selected_notes(e, a, |n, sel, _, _| Ok(mo::merge(n, sel, g)))
            },
        },
        Tool {
            name: "legato",
            description: "Make selected notes legato: each note (or chord) is extended to the next one; overlap (steps, e.g. 0.25) makes them overlap for glides/slides on monophonic basses and leads.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({"overlap": {"type": "number"}})), &["track"]),
            run: |e, a| {
                let ov = f_or(a, "overlap", 0.0);
                with_legacy_selected_notes(e, a, |n, sel, steps, _| Ok(mo::legato(n, sel, ov, steps)))
            },
        },
        Tool {
            name: "articulate",
            description: "FL Articulate: set note lengths from the gap to the next note in one call. style staccatissimo (0.18), staccato (0.35), portato (0.7), tenuto (0.95), legato (1.0), or ratio (0.05-1.5) directly; max_len caps lengths in steps. Bouncy plucks = staccato, held pads = legato.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "style": {"type": "string", "enum": mo::ARTICULATIONS.iter().map(|x| x.0).collect::<Vec<_>>()},
                "ratio": {"type": "number", "description": "Length as a fraction of the gap to the next note (overrides style)"},
                "max_len": {"type": "number", "description": "Cap in steps (0 = none)"}
            })), &["track"]),
            run: |e, a| {
                let ratio = match (f_opt(a, "ratio"), s_opt(a, "style")) {
                    (Some(r), _) => r,
                    (None, Some(s)) => mo::ARTICULATIONS.iter().find(|x| x.0 == s).map(|x| x.1).ok_or_else(|| anyhow!("style '{s}' ({})", mo::ARTICULATIONS.iter().map(|x| x.0).collect::<Vec<_>>().join(", ")))?,
                    (None, None) => 0.35,
                };
                let max_len = f_or(a, "max_len", 0.0);
                with_legacy_selected_notes(e, a, |n, sel, steps, _| Ok(mo::articulate(n, sel, ratio, max_len, steps)))
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
                let pat = s_opt(a, "style").unwrap_or_else(|| "up".into());
                let rate = steps_of(a.get("rate"), 1.0)?;
                let oct = u_or(a, "octaves", 1) as u32;
                let gate = f_or(a, "gate", 0.8);
                let mut rng = Rng::new(seed_of(a));
                with_legacy_selected_notes(e, a, |n, sel, _, _| mo::arpeggiate(n, sel, &pat, rate, oct, gate, &mut rng))
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
                let (sp, dir, vt) = (f_or(a, "spread", 0.5), s_opt(a, "direction").unwrap_or_else(|| "down".into()), f_or(a, "vel_taper", 0.2));
                with_legacy_selected_notes(e, a, |n, sel, _, _| Ok(mo::strum(n, sel, sp, &dir, vt)))
            },
        },
        Tool {
            name: "flip_notes",
            description: "Flip selected notes (FL piano roll Flip). mode pitch: mirror pitches around axis (MIDI, default the middle of the selection) so a rising line falls; mode time: play the selected phrase backwards inside the span it occupies (lengths kept).",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "mode": {"type": "string", "enum": ["pitch", "time"], "default": "pitch"},
                "axis": {"type": "number", "minimum": 0, "maximum": 127, "description": "pitch mode: MIDI note to mirror around"}
            })), &["track"]),
            run: |e, a| {
                let mode = s_opt(a, "mode").unwrap_or_else(|| "pitch".into());
                let axis = f_opt(a, "axis");
                with_legacy_selected_notes(e, a, |n, sel, _, _| mo::flip(n, sel, &mode, axis))
            },
        },
        Tool {
            name: "chop_notes",
            description: "Chop selected notes with a rhythm (FL's pattern Chop): pattern is one character per `step` ('1/16' default) on the song grid, repeating: x = hit, X = accented hit, - = keep holding, . = rest. A held chord or 808 becomes that rhythm, e.g. 'x.x-x..x' for a stab pattern, 'x--x--x-' for a tresillo 808.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "pattern": {"type": "string", "description": "x hit, X accent, - hold, . rest; e.g. 'x.x-x..x'"},
                "target_pattern": {"type": "string", "description": "Song pattern name to edit (default: first pattern); pattern is the chop rhythm"},
                "step": {"type": "string", "description": "length of one character: '1/16' (default), '1/8', '1/8t', or steps as a number"}
            })), &["track", "pattern"]),
            run: |e, a| {
                let pat = s_req(a, "pattern")?;
                let step = steps_of(a.get("step"), 1.0)?;
                // `pattern` is the chop rhythm, not the target song pattern.
                let mut target = a.clone();
                target.as_object_mut().unwrap().remove("pattern");
                if let Some(name) = s_opt(a, "target_pattern") {
                    target["pattern"] = json!(name);
                }
                with_legacy_selected_notes(e, &target, |n, sel, _, _| mo::chop(n, sel, &pat, step))
            },
        },
        Tool {
            name: "limit_notes",
            description: "Keep selected notes inside a pitch range by moving them whole octaves (FL's Limit): lo and hi are MIDI notes at least an octave apart, e.g. a bass between 36 and 55.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "lo": {"type": "integer", "minimum": 0, "maximum": 127},
                "hi": {"type": "integer", "minimum": 0, "maximum": 127}
            })), &["track", "lo", "hi"]),
            run: |e, a| {
                let lo = u_or(a, "lo", 36).min(127) as u8;
                let hi = u_or(a, "hi", 72).min(127) as u8;
                with_legacy_selected_notes(e, a, |n, sel, _, _| mo::limit_range(n, sel, lo, hi))
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
                let mode = s_opt(a, "mode").unwrap_or_else(|| "roll".into());
                let (c, vs, ps) = (u_or(a, "count", 4) as u32, f_or(a, "vel_start", 0.4), f_or(a, "pitch_step", 0.0) as i32);
                with_legacy_selected_notes(e, a, |n, sel, _, _| mo::ratchet(n, sel, &mode, c, vs, ps))
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
                let mode = s_req(a, "mode")?;
                let inv = f_or(a, "inversion", 1.0) as i32;
                with_legacy_selected_notes(e, a, |n, sel, _, _| mo::voice(n, sel, &mode, inv))
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
                    let sel=selection_in_meter(a,pat.steps_per_bar() as f32)?;
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
                let shape = s_req(a, "shape")?;
                let (amt, acc, lo, hi) = (f_or(a, "amount", 0.5), s_opt(a, "accents").unwrap_or_default(), f_or(a, "min", 0.35), f_or(a, "max", 1.0));
                let mut rng = Rng::new(seed_of(a));
                with_legacy_selected_notes(e, a, |n, sel, _, _| mo::velocity_curve(n, sel, &shape, amt, &acc, lo, hi, &mut rng))
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
        Tool {
            name: "lfo_tool",
            description: "FL's LFO tool for the piano roll: shape a note property with a tempo-synced LFO across the selected notes. property velocity | gate (note length, times the current one) | timing (microtiming nudge in steps) | chance (probability). shape sine | triangle | saw_up | saw_down | square | random (held per note, seeded). rate '1/4', '1/8t', '1 bar', '2 bars' or beats; center and depth set the swing (velocity: center 0.7 depth 0.25 = 0.45..0.95); phase 0..1; tension bends the curve (>1 sharper peaks). For parameter LFOs (cutoff, pan, volume) use generate_automation shape lfo.",
            mutates: true,
            schema: || obj(props(props(target_props(), selection_props()), json!({
                "property": {"type": "string", "enum": ["velocity", "gate", "timing", "chance"]},
                "shape": {"type": "string", "enum": ["sine", "triangle", "saw_up", "saw_down", "square", "random"]},
                "rate": {"type": ["string", "number"], "description": "LFO period: '1/4', '1/8t', '1 bar', '2 bars' or beats (default 1 bar)"},
                "center": {"type": "number"}, "depth": {"type": "number"}, "phase": {"type": "number"}, "tension": {"type": "number"}, "seed": {"type": "integer"}
            })), &["track", "property"]),
            run: |e, a| {
                let prop = s_req(a, "property")?;
                let shape = s_opt(a, "shape").unwrap_or_else(|| "sine".into());
                if !["sine", "triangle", "saw_up", "saw_down", "square", "random"].contains(&shape.as_str()) { bail!("unknown shape '{shape}'"); }
                let rate = a.get("rate").cloned().unwrap_or_else(|| json!("1 bar"));
                let period_steps = (crate::tools_studio::parse_rate(Some(&rate))? * 4.0).max(0.25);
                let (dc, dd) = match prop.as_str() {
                    "velocity" => (0.7, 0.25),
                    "gate" => (1.0, 0.4),
                    "timing" => (0.0, 0.08),
                    "chance" => (0.8, 0.2),
                    p => bail!("unknown property '{p}' (velocity, gate, timing, chance)"),
                };
                let (center, depth) = (f_or(a, "center", dc), f_or(a, "depth", dd));
                let (phase, tension) = (f_or(a, "phase", 0.0), f_or(a, "tension", 1.0).clamp(0.1, 10.0));
                let mut rng = Rng::new(seed_of(a));
                with_legacy_selected_notes(e, a, |notes, sel, _, _| {
                    let mut k = 0;
                    for n in notes.iter_mut().filter(|n| sel.matches(n)) {
                        let v = lfo_value(&shape, n.start / period_steps + phase, tension, &mut rng);
                        let x = center + depth * v;
                        match prop.as_str() {
                            "velocity" => n.vel = x.clamp(0.01, 1.0),
                            "gate" => n.len = (n.len * x).max(0.05),
                            "timing" => n.offset = x.clamp(-0.5, 0.5),
                            _ => n.prob = x.clamp(0.0, 1.0),
                        }
                        k += 1;
                    }
                    Ok(k)
                })
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copies_expression_ids_and_undo() {
        let mut e = Engine::new("/tmp/repeat-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_notes",&json!({"track":"keys","notes":[{"start":0,"len":1,"pitch":60,"vel":0.7},{"start":3,"len":2,"pitch":64,"vel":0.8}]})).unwrap();
        let n = &mut e.project.patterns[0].notes_mut("keys")[0];
        n.prob = 0.5;
        n.offset = -0.1;
        n.slide_to = Some(62);
        let id = n.id.clone();
        let before = serde_json::to_value(&e.project).unwrap();
        let r = e
            .call(
                "repeat_notes",
                &json!({"track":"keys","period":16,"repeats":3,"ids":[id]}),
            )
            .unwrap();
        assert_eq!(r["affected"], 3);
        let ns = e.project.patterns[0].notes("keys");
        assert_eq!(ns.len(), 5);
        let orig = &ns[0];
        for (i, n) in ns.iter().filter(|n| n.pitch == 60).enumerate() {
            assert_eq!(n.start, 16. * i as f32);
            assert_eq!(n.prob, orig.prob);
            assert_eq!(n.offset, orig.offset);
            assert_eq!(n.slide_to, orig.slide_to);
            assert_eq!(n.vel, orig.vel);
            if i > 0 {
                assert_ne!(n.id, orig.id);
                assert!(!n.id.is_empty());
            }
        }
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(e.project.patterns[0].notes("keys").len(), 5);
    }
    #[test]
    fn rejects_without_partial_edit() {
        let mut e = Engine::new("/tmp/repeat-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_notes",&json!({"track":"keys","notes":[{"start":0,"len":1,"pitch":60},{"start":63,"len":1,"pitch":64}]})).unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"period":16,"repeats":1}),
            json!({"period":0}),
            json!({"period":-1}),
            json!({"period":1,"repeats":65}),
            json!({"period":1,"repeats":1.5}),
            json!({}),
        ] {
            let mut a = a;
            a["track"] = json!("keys");
            assert!(e.call("repeat_notes", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        }
    }

    #[test]
    fn numeric_midi_steps_match_note_values() {
        for (tool, key, num, text, extra) in [
            ("echo_notes", "delay", 2., "1/8", json!({"repeats":2})),
            ("quantize", "grid", 1., "1/16", json!({})),
            ("split_notes", "every", 1., "1/16", json!({})),
            ("arpeggiate", "rate", 1., "1/16", json!({"seed":42})),
            ("chop_notes", "step", 1., "1/16", json!({"pattern":"x.x."})),
        ] {
            let setup = || {
                let mut e = Engine::new("/tmp/numeric-steps".into());
                e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
                    .unwrap();
                e.call("add_notes",&json!({"track":"keys","notes":[{"start":0.3,"len":8,"pitch":60},{"start":0.3,"len":8,"pitch":64}]})).unwrap();
                e
            };
            let mut e = setup();
            let mut expected = setup();
            let mut a = extra.clone();
            a["track"] = json!("keys");
            a[key] = json!(num);
            e.call(tool, &a).unwrap();
            a[key] = json!(text);
            expected.call(tool, &a).unwrap();
            assert_eq!(
                serde_json::to_value(&e.project.patterns).unwrap(),
                serde_json::to_value(&expected.project.patterns).unwrap(),
                "{tool}"
            );
        }
        for n in [0., -1.] {
            assert!(crate::tools_midi::steps_of(Some(&json!(n)), 1.).is_err());
        }
        assert_eq!(
            crate::tools_midi::steps_of(Some(&json!(3)), 1.).unwrap(),
            3.
        );
    }

    #[test]
    fn exact_duplicate_cleanup_preserves_expression_selection_and_undo() {
        let mut e = Engine::new("/tmp/dedup-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track":"keys","notes":[{"start":0,"len":1,"pitch":60,"vel":0.8}]}),
        )
        .unwrap();
        let base = e.project.patterns[0].notes("keys")[0].clone();
        let mut copies = vec![base.clone()];
        for field in 0..7 {
            let mut n = base.clone();
            n.id = format!("distinct{field}");
            match field {
                0 => n.vel = 0.7,
                1 => n.len = 2.,
                2 => n.prob = 0.5,
                3 => n.offset = 0.1,
                4 => n.slide_to = Some(64),
                5 => n.pitch = 61,
                _ => n.start = 1.,
            };
            copies.push(n);
        }
        let mut duplicate = base.clone();
        duplicate.id = "duplicate".into();
        copies.push(duplicate);
        *e.project.patterns[0].notes_mut("keys") = copies;
        let before = serde_json::to_value(&e.project).unwrap();
        let r = e
            .call("remove_duplicate_notes", &json!({"track":"keys"}))
            .unwrap();
        assert_eq!(r["removed"], 1);
        assert_eq!(e.project.patterns[0].notes("keys").len(), 8);
        assert_eq!(e.project.patterns[0].notes("keys")[0].id, base.id);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        let r = e
            .call(
                "remove_duplicate_notes",
                &json!({"track":"keys","ids":["duplicate"]}),
            )
            .unwrap();
        assert_eq!(r["removed"], 0);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        let r = e
            .call(
                "remove_duplicate_notes",
                &json!({"track":"keys","ids":[base.id,"duplicate"]}),
            )
            .unwrap();
        assert_eq!(r["removed"], 1);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        let mut notes = vec![Note::new(0., 1., 60, 0.8), Note::new(-0., 1., 60, 0.8)];
        assert_eq!(
            crate::midi_ops::remove_exact_duplicates(&mut notes, &Default::default()),
            1
        );
    }

    #[test]
    fn legacy_midi_bar_filters_match_meter_step_ranges() {
        for (tool, extra) in [
            ("edit_notes", json!({"vel_add":-0.1})),
            ("delete_notes", json!({})),
            ("quantize", json!({"grid":2})),
            ("split_notes", json!({"every":1})),
            ("merge_notes", json!({"max_gap":2})),
            ("legato", json!({})),
            ("articulate", json!({"style":"staccato"})),
            ("arpeggiate", json!({"seed":42})),
            ("strum", json!({})),
            ("flip_notes", json!({"mode":"pitch"})),
            ("chop_notes", json!({"pattern":"x.x."})),
            ("limit_notes", json!({"lo":48,"hi":72})),
            ("roll_notes", json!({"count":3})),
            ("chord_voicing", json!({"mode":"inversion"})),
            ("harmonize", json!({"interval":"3rd"})),
            ("velocity_curve", json!({"shape":"set","max":0.5,"seed":42})),
            ("lfo_tool", json!({"property":"velocity","seed":42})),
        ] {
            for meter in ["7/8", "3/4", "4/4"] {
                let bar = match meter {
                    "7/8" => 14,
                    "3/4" => 12,
                    _ => 16,
                };
                let setup = || {
                    let mut e = Engine::new("/tmp/legacy-bars".into());
                    e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
                        .unwrap();
                    e.call("set_time_signature", &json!({"time_signature":meter}))
                        .unwrap();
                    e.call("add_notes",&json!({"track":"keys","notes":[{"start":0,"len":4,"pitch":45},{"start":12,"len":4,"pitch":60},{"start":14,"len":4,"pitch":64},{"start":16,"len":4,"pitch":67},{"start":16,"len":4,"pitch":72},{"start":20,"len":4,"pitch":60}]})).unwrap();
                    e
                };
                let mut e = setup();
                let mut expected = setup();
                let before = serde_json::to_value(&e.project).unwrap();
                let mut a = extra.clone();
                a["track"] = json!("keys");
                a["bars"] = json!([1, 2]);
                e.call(tool, &a).unwrap();
                a.as_object_mut().unwrap().remove("bars");
                a["from"] = json!(bar);
                a["to"] = json!(bar * 2);
                expected.call(tool, &a).unwrap();
                assert_eq!(
                    serde_json::to_value(&e.project.patterns).unwrap(),
                    serde_json::to_value(&expected.project.patterns).unwrap(),
                    "{tool} {meter}"
                );
                if serde_json::to_value(&e.project).unwrap() != before {
                    e.call("undo", &json!({})).unwrap();
                }
                assert_eq!(
                    serde_json::to_value(&e.project).unwrap(),
                    before,
                    "undo {tool} {meter}"
                );
            }
        }
    }

    #[test]
    fn new_note_tools_select_bars_per_pattern_meter() {
        for (tool, extra) in [
            ("rotate_notes", json!({"shift":1})),
            ("stretch_notes", json!({"factor":0.5})),
            ("echo_notes", json!({"delay":1,"repeats":1})),
            ("trim_note_overlaps", json!({})),
        ] {
            let mut e = Engine::new("/tmp/bar-select".into());
            e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
                .unwrap();
            e.call("add_pattern", &json!({"name":"odd","bars":2}))
                .unwrap();
            e.call(
                "set_time_signature",
                &json!({"pattern":"odd","time_signature":"7/8"}),
            )
            .unwrap();
            for pat in ["A", "odd"] {
                e.call("add_notes",&json!({"track":"keys","pattern":pat,"notes":[{"start":0,"len":8,"pitch":60},{"start":14,"len":8,"pitch":60},{"start":15,"len":8,"pitch":60},{"start":16,"len":8,"pitch":60},{"start":20,"len":8,"pitch":60}]})).unwrap();
            }
            let before = serde_json::to_value(&e.project).unwrap();
            let mut a = extra.clone();
            a["track"] = json!("keys");
            a["all_patterns"] = json!(true);
            a["bars"] = json!([1, 2]);
            let r = e.call(tool, &a).unwrap();
            if tool == "echo_notes" {
                assert_eq!(r["affected"], 6);
            } else if tool == "trim_note_overlaps" {
                assert_eq!(r["affected"], 4);
            } else {
                assert_eq!(r["affected"], 6);
            }
            for pi in 0..2 {
                for original in before["patterns"][pi]["clips"]["keys"].as_array().unwrap() {
                    let start = original["start"].as_f64().unwrap();
                    if start < if pi == 0 { 16. } else { 14. } {
                        let id = original["id"].as_str().unwrap();
                        let n = e.project.patterns[pi]
                            .notes("keys")
                            .iter()
                            .find(|n| n.id == id)
                            .unwrap();
                        assert_eq!(
                            serde_json::to_value(n).unwrap(),
                            *original,
                            "{tool}, pattern{pi}"
                        );
                    }
                }
            }
            e.call("undo", &json!({})).unwrap();
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
            for bad in [json!([1]), json!([-1, 2]), json!([2, 1]), json!([0, "bad"])] {
                a["bars"] = bad;
                assert!(e.call(tool, &a).is_err());
                assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
            }
        }
    }

    #[test]
    fn trim_overlaps_respects_pitch_selection_identity_and_undo() {
        let mut e = Engine::new("/tmp/trim-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_notes",&json!({"track":"keys","notes":[{"start":0,"len":8,"pitch":60,"vel":0.8},{"start":2,"len":1,"pitch":64,"vel":0.7},{"start":4,"len":8,"pitch":60,"vel":0.6},{"start":8,"len":1,"pitch":60,"vel":0.5}]})).unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let orig = e.project.patterns[0].notes("keys").to_vec();
        let id = orig[0].id.clone();
        let result = e
            .call("trim_note_overlaps", &json!({"track":"keys","ids":[id]}))
            .unwrap();
        assert_eq!(result["shortened"], 1);
        for n in e.project.patterns[0].notes("keys") {
            let mut expected = orig.iter().find(|v| v.id == n.id).unwrap().clone();
            if n.id == id {
                expected.len = 4.;
            }
            assert_eq!(
                serde_json::to_value(n).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        }
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        let result = e
            .call("trim_note_overlaps", &json!({"track":"keys"}))
            .unwrap();
        assert_eq!(result["shortened"], 2);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("add_notes",&json!({"track":"keys","notes":[{"start":0,"len":3,"pitch":60},{"start":2.02,"len":1,"pitch":64}]})).unwrap();
        let result = e
            .call("trim_note_overlaps", &json!({"track":"keys"}))
            .unwrap();
        assert_eq!(result["skipped"], 3);
        let stable = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call("trim_note_overlaps", &json!({"track":"missing"}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), stable);
    }

    #[test]
    fn rhythm_rotation_wraps_with_meter_and_preserves_identity() {
        let mut e = Engine::new("/tmp/rotate-test".into());
        e.call("add_track", &json!({"name":"hat","preset":"hat"}))
            .unwrap();
        e.call("set_time_signature", &json!({"time_signature":"7/8"}))
            .unwrap();
        e.call("add_notes",&json!({"track":"hat","notes":[{"start":0,"len":1,"pitch":60,"vel":0.8},{"start":13,"len":0.5,"pitch":64,"vel":0.7},{"start":14,"len":1,"pitch":67,"vel":0.6}]})).unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let notes = e.project.patterns[0].notes("hat").to_vec();
        let ids = vec![notes[0].id.clone(), notes[2].id.clone()];
        e.call("rotate_notes", &json!({"track":"hat","ids":ids,"shift":-1}))
            .unwrap();
        for (i, start) in [(0, 13.), (1, 13.), (2, 27.)] {
            let n = e.project.patterns[0]
                .notes("hat")
                .iter()
                .find(|n| n.id == notes[i].id)
                .unwrap();
            let mut expected = notes[i].clone();
            expected.start = start;
            assert_eq!(
                serde_json::to_value(n).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        }
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("rotate_notes", &json!({"track":"hat","shift":29.5}))
            .unwrap();
        for (i, start) in [(0, 1.5), (1, 0.5), (2, 15.5)] {
            assert_eq!(
                e.project.patterns[0]
                    .notes("hat")
                    .iter()
                    .find(|n| n.id == notes[i].id)
                    .unwrap()
                    .start,
                start
            );
        }
        e.call("undo", &json!({})).unwrap();
        assert!(e
            .call("rotate_notes", &json!({"track":"missing","shift":1}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }

    #[test]
    fn stretch_phrase_preserves_notes_and_rejects_overflow() {
        let mut e = Engine::new("/tmp/stretch-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_notes",&json!({"track":"keys","notes":[{"start":2,"len":1,"pitch":60,"vel":0.8},{"start":6,"len":2,"pitch":64,"vel":0.7},{"start":12,"len":1,"pitch":67,"vel":0.6}]})).unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let notes = e.project.patterns[0].notes("keys").to_vec();
        let ids = vec![notes[0].id.clone(), notes[1].id.clone()];
        e.call(
            "stretch_notes",
            &json!({"track":"keys","ids":ids,"factor":0.5}),
        )
        .unwrap();
        let out = e.project.patterns[0].notes("keys");
        assert_eq!(out[0].start, 2.);
        assert_eq!(out[0].len, 0.5);
        assert_eq!(out[1].start, 4.);
        assert_eq!(out[1].len, 1.);
        assert_eq!(
            serde_json::to_value(&out[2]).unwrap(),
            serde_json::to_value(&notes[2]).unwrap()
        );
        for i in 0..3 {
            assert_eq!(out[i].id, notes[i].id);
            assert_eq!(out[i].pitch, notes[i].pitch);
            assert_eq!(out[i].vel, notes[i].vel);
        }
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        for args in [
            json!({"track":"keys","factor":16}),
            json!({"track":"keys","factor":0}),
            json!({"track":"keys","factor":0.5,"anchor":-1}),
        ] {
            assert!(e.call("stretch_notes", &args).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        }
        e.call(
            "stretch_notes",
            &json!({"track":"keys","ids":ids,"factor":2.,"anchor":0}),
        )
        .unwrap();
        let out = e.project.patterns[0].notes("keys");
        assert!(out.iter().any(|n| n.start == 4. && n.len == 2.));
        assert!(out.iter().any(|n| n.start == 12. && n.len == 4.));
    }

    #[test]
    fn note_echoes_are_selective_bounded_and_undoable() {
        let mut e = Engine::new("/tmp/echo-engine-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_notes",&json!({"track":"keys","notes":[{"start":0,"len":1,"pitch":60,"vel":0.8},{"start":2,"len":1,"pitch":64,"vel":0.9}]})).unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let id = e.project.patterns[0].notes("keys")[0].id.clone();
        e.call(
            "echo_notes",
            &json!({"track":"keys","ids":[id],"delay":"1/8","repeats":3,"decay":0.5,"transpose":2}),
        )
        .unwrap();
        let out = e.project.patterns[0].notes("keys");
        assert_eq!(out.len(), 5);
        for (k, start, pitch, vel) in [(1, 2., 62, 0.4), (2, 4., 64, 0.2), (3, 6., 66, 0.1)] {
            let n = out
                .iter()
                .find(|n| n.start == start && n.pitch == pitch && (n.vel - vel).abs() < 1e-6)
                .unwrap();
            assert!(!n.id.is_empty());
            assert_ne!(n.id, id);
            assert!(k > 0);
        }
        for n in &before["patterns"][0]["clips"]["keys"]
            .as_array()
            .unwrap()
            .clone()
        {
            assert!(out.iter().any(|x| serde_json::to_value(x).unwrap() == *n));
        }
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        for args in [
            json!({"track":"keys","delay":0}),
            json!({"track":"keys","repeats":65}),
            json!({"track":"keys","decay":1.1}),
            json!({"track":"keys","transpose":128}),
        ] {
            assert!(e.call("echo_notes", &args).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        }
        e.call("add_notes",&json!({"track":"keys","replace":true,"notes":[{"start":14,"len":4,"pitch":126,"vel":0.8}]})).unwrap();
        let steps = e.project.patterns[0].steps() as f32;
        e.call(
            "echo_notes",
            &json!({"track":"keys","delay":1,"repeats":3,"decay":0.5,"transpose":1}),
        )
        .unwrap();
        assert!(e.project.patterns[0]
            .notes("keys")
            .iter()
            .all(|n| n.pitch <= 127));
        assert_eq!(e.project.patterns[0].notes("keys").len(), 2);
        assert!(e.project.patterns[0]
            .notes("keys")
            .iter()
            .all(|n| n.start < steps));
    }

    #[test]
    fn chop_rhythm_and_target_pattern_are_independent() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_chop_target_test"));
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"verse","bars":2}))
            .unwrap();
        for pattern in ["A", "verse"] {
            e.call(
                "add_notes",
                &json!({"track":"keys","pattern":pattern,"notes":[
                    {"start":0,"len":8,"pitch":60,"vel":0.8},
                    {"start":8,"len":4,"pitch":64,"vel":0.7}
                ]}),
            )
            .unwrap();
        }
        let original = serde_json::to_value(&e.project).unwrap();
        let main = serde_json::to_value(&e.project.patterns[0]).unwrap();
        let vi = e.project.pattern_index("verse").unwrap();
        let ids: Vec<String> = e.project.patterns[vi]
            .notes("keys")
            .iter()
            .map(|n| n.id.clone())
            .collect();
        let untouched = serde_json::to_value(&e.project.patterns[vi].notes("keys")[1]).unwrap();
        e.call(
            "chop_notes",
            &json!({"track":"keys","pattern":"x.x.x.x.","target_pattern":"verse","ids":[ids[0]]}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project.patterns[0]).unwrap(), main);
        let out = e.project.patterns[vi].notes("keys");
        assert_eq!(
            out.iter()
                .filter(|n| n.pitch == 60)
                .map(|n| n.start)
                .collect::<Vec<_>>(),
            vec![0.0, 2.0, 4.0, 6.0]
        );
        assert_eq!(
            serde_json::to_value(out.iter().find(|n| n.id == ids[1]).unwrap()).unwrap(),
            untouched
        );
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), original);
        assert!(e
            .call(
                "chop_notes",
                &json!({"track":"keys","pattern":"x.x.x.x.","target_pattern":"missing"})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), original);
        let verse = serde_json::to_value(&e.project.patterns[vi]).unwrap();
        e.call("chop_notes", &json!({"track":"keys","pattern":"x.x.x.x."}))
            .unwrap();
        assert_eq!(
            serde_json::to_value(&e.project.patterns[vi]).unwrap(),
            verse
        );
        assert_ne!(serde_json::to_value(&e.project.patterns[0]).unwrap(), main);
    }

    fn eng() -> Engine {
        Engine::new(std::env::temp_dir().join("beatbox_midi_tests"))
    }

    #[test]
    fn lfo_tool_shapes_velocity_and_gate() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_midi_lfo"));
        e.call("add_track", &json!({"name": "hat", "preset": "hat"}))
            .unwrap();
        let notes: Vec<Value> = (0..16)
            .map(|i| json!({"start": i, "len": 1, "pitch": 60}))
            .collect();
        e.call("add_notes", &json!({"track": "hat", "notes": notes}))
            .unwrap();
        let r = e.call("lfo_tool", &json!({"track": "hat", "property": "velocity", "shape": "sine", "rate": "1 bar", "center": 0.6, "depth": 0.3})).unwrap();
        assert_eq!(r["affected"], 16);
        let v: Vec<f32> = e.project.patterns[0]
            .notes("hat")
            .iter()
            .map(|n| n.vel)
            .collect();
        assert!(
            (v[0] - 0.6).abs() < 1e-3 && (v[4] - 0.9).abs() < 1e-3 && (v[12] - 0.3).abs() < 1e-3,
            "{v:?}"
        );
        e.call("lfo_tool", &json!({"track": "hat", "property": "gate", "shape": "saw_down", "rate": 4, "center": 1.0, "depth": 0.5, "from": 8})).unwrap();
        let l: Vec<f32> = e.project.patterns[0]
            .notes("hat")
            .iter()
            .map(|n| n.len)
            .collect();
        assert!(
            (l[0] - 1.0).abs() < 1e-3
                && (l[8] - 1.0).abs() < 1e-3
                && (l[9] - 0.9375).abs() < 1e-3
                && l[15] < l[9],
            "{l:?}"
        );
        assert!(e
            .call("lfo_tool", &json!({"track": "hat", "property": "pan"}))
            .is_err());
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

#[cfg(test)]
mod thin_rhythm_tests {
    use crate::{
        midi_ops::{thin_rhythm, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn chord_groups_selection_and_phase() {
        let mut ns = vec![
            Note::new(4., 1., 60, 0.7),
            Note::new(0., 1., 60, 0.7),
            Note::new(0., 2., 64, 0.8),
            Note::new(1., 1., 62, 0.7),
            Note::new(2., 1., 65, 0.7),
            Note::new(3., 1., 67, 0.7),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.3;
            n.offset = -0.1;
            n.slide_to = Some(70);
        }
        let before = ns.clone();
        assert_eq!(
            thin_rhythm(&mut ns, &Selection::default(), 2, 0).unwrap(),
            2
        );
        assert_eq!(
            ns,
            vec![
                before[0].clone(),
                before[1].clone(),
                before[2].clone(),
                before[4].clone()
            ]
        );
        let mut ns = before.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n3".into(), "n4".into()],
            ..Default::default()
        };
        assert_eq!(thin_rhythm(&mut ns, &sel, 2, 1).unwrap(), 2);
        assert!(ns.iter().any(|n| n.id == "n4"));
        for id in ["n1", "n2", "n5"] {
            assert!(ns.iter().any(|n| n.id == id));
        }
        for (e, p) in [(0, 0), (1, 0), (65, 0), (2, 2)] {
            let mut ns = before.clone();
            assert!(thin_rhythm(&mut ns, &Selection::default(), e, p).is_err());
            assert_eq!(ns, before);
        }
    }
    #[test]
    fn engine_save_undo_redo_and_reject() {
        let mut e = Engine::new("/tmp/thin-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_notes",&json!({"track":"keys","notes":[{"start":0,"pitch":60},{"start":0,"pitch":64},{"start":1,"pitch":62},{"start":2,"pitch":65},{"start":3,"pitch":67}]})).unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        let r = e
            .call("thin_notes", &json!({"track":"keys","every":2,"phase":0}))
            .unwrap();
        assert_eq!(r["affected"], 2);
        assert_eq!(e.project.patterns[0].notes("keys").len(), 3);
        let a = serde_json::to_value(&e.project).unwrap();
        e.call("save_project", &json!({"path":"thin.json"}))
            .unwrap();
        let mut loaded = Engine::new("/tmp/thin-test".into());
        loaded
            .call("load_project", &json!({"path":"thin.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&loaded.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        for x in [
            json!({"every":1}),
            json!({"every":2.5}),
            json!({"phase":-1}),
            json!({"every":2,"phase":2}),
        ] {
            let mut x = x;
            x["track"] = json!("keys");
            assert!(e.call("thin_notes", &x).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        }
    }

    #[test]
    fn all_patterns_meter_and_empty_selection() {
        let mut e = Engine::new("/tmp/thin-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":2}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"pitch":60},{"start":0,"pitch":64},{"start":1,"pitch":62},{"start":2,"pitch":65}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        e.call(
            "thin_notes",
            &json!({"track":"keys","all_patterns":true,"every":2}),
        )
        .unwrap();
        for p in &e.project.patterns {
            assert_eq!(p.notes("keys").len(), 3);
        }
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("thin_notes", &json!({"track":"keys","ids":["missing"]}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod normalize_velocity_tests {
    use crate::{
        midi_ops::{normalize_velocity, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn ratios_expression_selection_and_rejection() {
        let mut ns = vec![
            Note::new(0., 1., 60, 0.2),
            Note::new(1., 2., 64, 0.4),
            Note::new(2., 1., 65, 0.8),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.3;
            n.offset = 0.1;
            n.slide_to = Some(72);
        }
        let b = ns.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into()],
            ..Default::default()
        };
        assert_eq!(normalize_velocity(&mut ns, &sel, 0.9).unwrap(), 2);
        assert!((ns[0].vel - 0.45).abs() < 1e-6);
        assert!((ns[1].vel - 0.9).abs() < 1e-6);
        let mut expected = b.clone();
        expected[0].vel = 0.45;
        expected[1].vel = 0.9;
        assert_eq!(ns, expected);
        for peak in [0., -1., 1.1, f32::NAN] {
            let mut n = b.clone();
            assert!(normalize_velocity(&mut n, &sel, peak).is_err());
            assert_eq!(n, b);
        }
        let mut n = b.clone();
        n[1].vel = 0.;
        let z = n.clone();
        assert!(normalize_velocity(&mut n, &sel, 0.9).is_err());
        assert_eq!(n, z);
    }
    #[test]
    fn engine_all_patterns_undo_redo_roundtrip() {
        let mut e = Engine::new("/tmp/normalize-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":2}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"pitch":60,"vel":0.2},{"start":1,"pitch":64,"vel":0.4}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        e.call(
            "normalize_velocity",
            &json!({"track":"keys","all_patterns":true,"peak":0.9}),
        )
        .unwrap();
        for p in &e.project.patterns {
            assert_eq!(p.notes("keys")[0].vel, 0.45);
            assert_eq!(p.notes("keys")[1].vel, 0.9);
        }
        let a = serde_json::to_value(&e.project).unwrap();
        e.call("save_project", &json!({"path":"n.json"})).unwrap();
        let mut l = Engine::new("/tmp/normalize-test".into());
        l.call("load_project", &json!({"path":"n.json"})).unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.call(
            "normalize_velocity",
            &json!({"track":"keys","ids":["missing"]}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
    }
}

#[cfg(test)]
mod combined_midi_feature_tests {
    use crate::Engine;
    use serde_json::json;
    #[test]
    fn chain_roundtrip_and_entire_history() {
        let mut e = Engine::new("/tmp/feature-chain".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":4}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"len":1,"pitch":60,"vel":0.2},{"start":2,"len":1,"pitch":64,"vel":0.4},{"start":4,"len":1,"pitch":67,"vel":0.8}]})).unwrap();
        }
        let mut states = vec![serde_json::to_value(&e.project).unwrap()];
        for (t, mut a) in [
            ("repeat_notes", json!({"period":8,"repeats":1})),
            ("echo_notes", json!({"delay":1,"repeats":1})),
            ("stretch_notes", json!({"factor":0.75})),
            ("rotate_notes", json!({"shift":-1.5})),
            ("thin_notes", json!({"every":2,"phase":0})),
            ("normalize_velocity", json!({"peak":0.9})),
            ("trim_note_overlaps", json!({})),
            ("remove_duplicate_notes", json!({})),
        ] {
            a["track"] = json!("keys");
            a["all_patterns"] = json!(true);
            e.call(t, &a).unwrap();
            let s = serde_json::to_value(&e.project).unwrap();
            if s != *states.last().unwrap() {
                states.push(s);
            }
        }
        let last = states.last().unwrap().clone();
        e.call("save_project", &json!({"path":"chain.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/feature-chain".into());
        l.call("load_project", &json!({"path":"chain.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), last);
        for expected in states.iter().rev().skip(1) {
            e.call("undo", &json!({})).unwrap();
            assert_eq!(&serde_json::to_value(&e.project).unwrap(), expected);
        }
        for expected in states.iter().skip(1) {
            e.call("redo", &json!({})).unwrap();
            assert_eq!(&serde_json::to_value(&e.project).unwrap(), expected);
        }
        println!(
            "{} actual edits undone and redone exactly",
            states.len() - 1
        );
    }
}

#[cfg(test)]
mod shift_phrase_tests {
    use crate::{
        midi_ops::{shift_phrase, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn rigid_selected_move_and_atomic_bounds() {
        let mut ns = vec![
            Note::new(4., 1., 60, 0.2),
            Note::new(8., 2., 64, 0.4),
            Note::new(12., 1., 67, 0.8),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.offset = 0.1;
            n.prob = 0.4;
            n.slide_to = Some(70);
        }
        let b = ns.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into()],
            ..Default::default()
        };
        assert_eq!(shift_phrase(&mut ns, &sel, -2.5, 16.).unwrap(), 2);
        let mut e = b.clone();
        e[0].start = 1.5;
        e[1].start = 5.5;
        assert_eq!(ns, e);
        for shift in [-5., 10., f32::NAN] {
            let mut ns = b.clone();
            assert!(shift_phrase(&mut ns, &sel, shift, 16.).is_err());
            assert_eq!(ns, b);
        }
        let mut ns = b.clone();
        assert_eq!(
            shift_phrase(
                &mut ns,
                &Selection {
                    ids: vec!["missing".into()],
                    ..Default::default()
                },
                1.,
                16.
            )
            .unwrap(),
            0
        );
        assert_eq!(ns, b);
    }
    #[test]
    fn grouped_move_undo_roundtrip_and_crosspattern_rollback() {
        let mut e = Engine::new("/tmp/shift-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":4,"len":1,"pitch":60},{"start":12,"len":1,"pitch":64}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "shift_phrase",
                &json!({"track":"keys","all_patterns":true,"shift":2})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call(
            "notes",
            &json!({"action":"shift_phrase","track":"keys","all_patterns":true,"shift":-2.5}),
        )
        .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        e.call("save_project", &json!({"path":"shift.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/shift-test".into());
        l.call("load_project", &json!({"path":"shift.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
    }
}

#[cfg(test)]
mod transpose_phrase_tests {
    #[test]
    fn transpose_preserves_unsorted_note_order_short_silent_and_off_pattern_notes() {
        let mut e = crate::Engine::new(std::env::temp_dir());
        e.call(
            "add_track",
            &serde_json::json!({"name":"keys","preset":"epiano"}),
        )
        .unwrap();
        for (i, start) in [80., 8., 0., 4.].iter().enumerate() {
            e.project.patterns[0]
                .notes_mut("keys")
                .push(crate::project::Note {
                    start: *start,
                    len: 0.02,
                    pitch: 60 + i as u8,
                    vel: 0.,
                    prob: 0.7,
                    offset: 0.1,
                    slide_to: Some(72),
                    id: format!("n{i}"),
                });
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "transpose_phrase",
            &serde_json::json!({"track":"keys","semitones":12}),
        )
        .unwrap();
        let mut expected = before.clone();
        for n in expected["patterns"][0]["clips"]["keys"]
            .as_array_mut()
            .unwrap()
        {
            n["pitch"] = serde_json::json!(n["pitch"].as_u64().unwrap() + 12);
        }
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        e.call("undo", &serde_json::json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }

    use crate::{
        midi_ops::{transpose_phrase, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn selected_intervals_expression_and_atomic_rejection() {
        let mut ns = vec![
            Note::new(0., 1., 2, 0.2),
            Note::new(4., 2., 64, 0.4),
            Note::new(8., 1., 125, 0.8),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.offset = 0.1;
            n.prob = 0.4;
            n.slide_to = Some(70);
        }
        let b = ns.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into()],
            ..Default::default()
        };
        assert_eq!(transpose_phrase(&mut ns, &sel, 7).unwrap(), 2);
        let mut e = b.clone();
        e[0].pitch = 9;
        e[1].pitch = 71;
        assert_eq!(ns, e);
        for semitones in [-3, 127, i32::MAX] {
            let mut ns = b.clone();
            assert!(transpose_phrase(&mut ns, &sel, semitones).is_err());
            assert_eq!(ns, b);
        }
        let mut ns = b.clone();
        assert!(transpose_phrase(&mut ns, &Selection::default(), 3).is_err());
        assert_eq!(ns, b);
        assert_eq!(
            transpose_phrase(
                &mut ns,
                &Selection {
                    ids: vec!["missing".into()],
                    ..Default::default()
                },
                12
            )
            .unwrap(),
            0
        );
        assert_eq!(ns, b);
    }
    #[test]
    fn grouped_transpose_roundtrip_undo_and_rollback() {
        let mut e = Engine::new("/tmp/transpose-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for (p, pitch) in [("A", 60), ("odd", 125)] {
            e.call(
                "add_notes",
                &json!({"track":"keys","pattern":p,"notes":[{"start":4,"len":1,"pitch":pitch}]}),
            )
            .unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        for arg in [
            json!({"semitones":3}),
            json!({"semitones":1.5}),
            json!({"semitones":"12"}),
            json!({}),
        ] {
            let mut a = json!({"track":"keys","all_patterns":true});
            a.as_object_mut()
                .unwrap()
                .extend(arg.as_object().unwrap().clone());
            assert!(e.call("transpose_phrase", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.call("notes",&json!({"action":"transpose_phrase","track":"keys","all_patterns":true,"semitones":-12})).unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        e.call("save_project", &json!({"path":"transpose.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/transpose-test".into());
        l.call("load_project", &json!({"path":"transpose.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
    }
}

#[cfg(test)]
mod safe_melodic_integration_tests {
    use crate::Engine;
    use serde_json::json;
    #[test]
    fn melodic_move_transpose_render_and_full_history() {
        let mut e = Engine::new("/tmp/safe-melodic".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":2}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"len":1,"pitch":60,"vel":0.2},{"start":0,"len":2,"pitch":64,"vel":0.4},{"start":3,"len":1,"pitch":67,"vel":0.8},{"start":8,"len":1,"pitch":72,"vel":0.6}]})).unwrap();
        }
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":"odd","repeats":1}]}),
        )
        .unwrap();
        let r = e
            .call("render", &json!({"section":"odd","path":"before.wav"}))
            .unwrap();
        println!("before render: {r}");
        assert!(r["rms_dbfs"].as_f64().unwrap() > -90.);
        let mut states = vec![serde_json::to_value(&e.project).unwrap()];
        for (name, a) in [
            (
                "shift_phrase",
                json!({"track":"keys","all_patterns":true,"shift":1.5}),
            ),
            (
                "transpose_phrase",
                json!({"track":"keys","all_patterns":true,"semitones":7}),
            ),
            (
                "quantize_note_ends",
                json!({"track":"keys","all_patterns":true,"grid":1,"strength":1}),
            ),
            (
                "align_chord_ends",
                json!({"track":"keys","all_patterns":true,"mode":"longest"}),
            ),
            (
                "fold_note_register",
                json!({"track":"keys","all_patterns":true,"low":48,"high":72}),
            ),
            (
                "reverse_rhythm_gaps",
                json!({"track":"keys","all_patterns":true}),
            ),
            (
                "distribute_note_rhythm",
                json!({"track":"keys","all_patterns":true}),
            ),
            (
                "reduce_chord_voices",
                json!({"track":"keys","all_patterns":true,"voice":"highest"}),
            ),
            (
                "rotate_melody_pitches",
                json!({"track":"keys","all_patterns":true,"shift":1}),
            ),
            (
                "reverse_melody_pitches",
                json!({"track":"keys","all_patterns":true}),
            ),
            (
                "rotate_note_velocities",
                json!({"track":"keys","all_patterns":true,"shift":1}),
            ),
            (
                "normalize_velocity",
                json!({"track":"keys","all_patterns":true,"peak":0.9}),
            ),
        ] {
            e.call(name, &a).unwrap();
            let state = serde_json::to_value(&e.project).unwrap();
            assert_ne!(
                states.last(),
                Some(&state),
                "{name} must perform a real edit"
            );
            states.push(state);
        }
        for p in &e.project.patterns {
            if let Some(ns) = p.clips.get("keys") {
                assert_eq!(
                    ns.iter().map(|n| n.pitch).collect::<Vec<_>>(),
                    vec![62, 71, 67]
                );
                assert_eq!(
                    ns.iter().map(|n| n.start).collect::<Vec<_>>(),
                    vec![1.5, 5.5, 9.5]
                );
            }
        }
        assert_eq!(states.len(), 13);
        let r = e
            .call("render", &json!({"section":"odd","path":"after.wav"}))
            .unwrap();
        println!("after render: {r}");
        assert!(r["rms_dbfs"].as_f64().unwrap() > -90.);
        e.call("save_project", &json!({"path":"after.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/safe-melodic".into());
        l.call("load_project", &json!({"path":"after.json"}))
            .unwrap();
        assert_eq!(
            serde_json::to_value(&l.project).unwrap(),
            *states.last().unwrap()
        );
        for expected in states.iter().rev().skip(1) {
            e.call("undo", &json!({})).unwrap();
            assert_eq!(&serde_json::to_value(&e.project).unwrap(), expected);
        }
        for expected in states.iter().skip(1) {
            e.call("redo", &json!({})).unwrap();
            assert_eq!(&serde_json::to_value(&e.project).unwrap(), expected);
        }
    }
}

#[cfg(test)]
mod quantize_ends_tests {
    use crate::{
        midi_ops::{quantize_ends, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn ends_only_strength_selection_and_atomic_bounds() {
        let mut ns = vec![
            Note::new(0.25, 1.1, 60, 0.2),
            Note::new(4.2, 1.1, 64, 0.4),
            Note::new(8., 1.2, 67, 0.8),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.offset = 0.1;
            n.prob = 0.4;
            n.slide_to = Some(70);
        }
        let b = ns.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into()],
            ..Default::default()
        };
        assert_eq!(quantize_ends(&mut ns, &sel, 1., 0.5, 16.).unwrap(), 2);
        let mut expected = b.clone();
        expected[0].len = 0.925;
        expected[1].len = 0.95;
        for (i, n) in ns.iter().enumerate() {
            assert!((n.len - expected[i].len).abs() < 1e-5);
            expected[i].len = n.len;
        }
        assert_eq!(ns, expected);
        for (g, st) in [(0., 1.), (1., -0.1), (1., 1.1), (f32::NAN, 1.)] {
            let mut ns = b.clone();
            assert!(quantize_ends(&mut ns, &sel, g, st, 16.).is_err());
            assert_eq!(ns, b);
        }
        let mut ns = vec![Note::new(0., 1.2, 60, 0.5), Note::new(15.7, 0.2, 64, 0.5)];
        let b = ns.clone();
        assert!(quantize_ends(&mut ns, &Selection::default(), 3., 1., 16.).is_err());
        assert_eq!(ns, b);
        let mut ns = vec![Note::new(0.97, 0.1, 60, 0.5)];
        let b = ns.clone();
        assert!(quantize_ends(&mut ns, &Selection::default(), 1., 1., 16.).is_err());
        assert_eq!(ns, b);
    }
    #[test]
    fn grouped_ends_roundtrip_rollback_and_history() {
        let mut e = Engine::new("/tmp/ends-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0.25,"len":3.1,"pitch":60,"offset":0.1},{"start":12,"len":1.5,"pitch":64}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "quantize_note_ends",
                &json!({"track":"keys","all_patterns":true,"grid":5})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("notes",&json!({"action":"quantize_note_ends","track":"keys","all_patterns":true,"grid":1,"strength":0.5})).unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        e.call("save_project", &json!({"path":"ends.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/ends-test".into());
        l.call("load_project", &json!({"path":"ends.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
    }
}

#[cfg(test)]
mod reduce_chord_tests {
    use crate::{
        midi_ops::{reduce_chords, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn chord_extremes_ties_selection_and_expression() {
        let mut ns = vec![
            Note::new(0., 1., 60, 0.2),
            Note::new(0., 2., 64, 0.4),
            Note::new(0., 1., 67, 0.8),
            Note::new(0., 3., 67, 0.9),
            Note::new(4., 1., 72, 0.6),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.offset = 0.1;
            n.prob = 0.4;
            n.slide_to = Some(70);
        }
        let b = ns.clone();
        assert_eq!(
            reduce_chords(&mut ns, &Selection::default(), true).unwrap(),
            2
        );
        assert_eq!(ns, vec![b[2].clone(), b[3].clone(), b[4].clone()]);
        let mut ns = b.clone();
        assert_eq!(
            reduce_chords(&mut ns, &Selection::default(), false).unwrap(),
            3
        );
        assert_eq!(ns, vec![b[0].clone(), b[4].clone()]);
        let mut ns = b.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into()],
            ..Default::default()
        };
        assert_eq!(reduce_chords(&mut ns, &sel, true).unwrap(), 1);
        assert_eq!(ns, b[1..].to_vec());
    }
    #[test]
    fn grouped_reduce_meters_save_and_history() {
        let mut e = Engine::new("/tmp/reduce-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"len":1,"pitch":60},{"start":0,"len":2,"pitch":64},{"start":0,"len":1,"pitch":67},{"start":4,"len":1,"pitch":72}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "reduce_chord_voices",
                &json!({"track":"keys","voice":"middle"})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("notes",&json!({"action":"reduce_chord_voices","track":"keys","all_patterns":true,"voice":"highest"})).unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        e.call("save_project", &json!({"path":"reduce.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/reduce-test".into());
        l.call("load_project", &json!({"path":"reduce.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
    }
}

#[cfg(test)]
mod fold_register_tests {
    use crate::{
        midi_ops::{fold_register, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn octave_classes_selection_expression_and_atomic_rejection() {
        let mut ns = vec![
            Note::new(0., 1., 36, 0.2),
            Note::new(2., 2., 60, 0.4),
            Note::new(4., 1., 91, 0.8),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("r{i}");
            n.offset = 0.1;
            n.prob = 0.6;
            n.slide_to = Some(70);
        }
        let b = ns.clone();
        assert_eq!(
            fold_register(&mut ns, &Selection::default(), 48, 72).unwrap(),
            2
        );
        let mut expected = b.clone();
        expected[0].pitch = 48;
        expected[2].pitch = 67;
        assert_eq!(ns, expected);
        assert_eq!(
            fold_register(&mut ns, &Selection::default(), 48, 72).unwrap(),
            0
        );
        let mut ns = b.clone();
        let sel = Selection {
            ids: vec!["r2".into()],
            ..Default::default()
        };
        fold_register(&mut ns, &sel, 48, 72).unwrap();
        let mut expected = b.clone();
        expected[2].pitch = 67;
        assert_eq!(ns, expected);
        let mut ns = b.clone();
        assert!(fold_register(&mut ns, &Selection::default(), 48, 48).is_err());
        assert_eq!(ns, b);
        assert!(fold_register(&mut ns, &sel, 72, 48).is_err());
        assert_eq!(ns, b);
    }
    #[test]
    fn grouped_register_mixed_meters_roundtrip_and_history() {
        let mut e = Engine::new("/tmp/register-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"len":1,"pitch":36},{"start":2,"len":1,"pitch":91}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        for a in [
            json!({"low":48,"high":48}),
            json!({"low":48.5,"high":72}),
            json!({"low":-1,"high":72}),
        ] {
            let mut a = a;
            a["track"] = json!("keys");
            a["all_patterns"] = json!(true);
            assert!(e.call("fold_note_register", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.call("notes",&json!({"action":"fold_note_register","track":"keys","all_patterns":true,"low":48,"high":72})).unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        for p in &e.project.patterns {
            if let Some(ns) = p.clips.get("keys") {
                assert_eq!(ns.iter().map(|n| n.pitch).collect::<Vec<_>>(), vec![48, 67]);
            }
        }
        e.call("save_project", &json!({"path":"register.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/register-test".into());
        l.call("load_project", &json!({"path":"register.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
    }
}

#[cfg(test)]
mod distribute_rhythm_tests {
    use crate::{
        midi_ops::{distribute_rhythm, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn chords_selection_endpoints_expression_and_atomic_bounds() {
        let mut ns = vec![
            Note::new(0., 1., 60, 0.2),
            Note::new(1., 2., 64, 0.4),
            Note::new(1., 1., 67, 0.8),
            Note::new(3., 1., 70, 0.6),
            Note::new(8., 1., 72, 0.7),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("d{i}");
            n.offset = 0.1;
            n.prob = 0.6;
            n.slide_to = Some(74);
        }
        let b = ns.clone();
        let sel = Selection {
            ids: vec!["d0".into(), "d1".into(), "d2".into(), "d4".into()],
            ..Default::default()
        };
        assert_eq!(distribute_rhythm(&mut ns, &sel, 16.).unwrap(), 2);
        let mut expected = b.clone();
        expected[1].start = 4.;
        expected[2].start = 4.;
        assert_eq!(ns, expected);
        assert_eq!(distribute_rhythm(&mut ns, &sel, 16.).unwrap(), 0);
        let mut ns = b.clone();
        ns[1].len = 13.;
        let before = ns.clone();
        assert!(distribute_rhythm(&mut ns, &sel, 16.).is_err());
        assert_eq!(ns, before);
        let sel = Selection {
            ids: vec!["d0".into(), "d4".into()],
            ..Default::default()
        };
        assert_eq!(distribute_rhythm(&mut ns, &sel, 16.).unwrap(), 0);
        assert_eq!(ns, before);
    }
    #[test]
    fn grouped_mixed_meters_roundtrip_history_and_rollback() {
        let mut e = Engine::new("/tmp/distribute-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"len":1,"pitch":60},{"start":1,"len":1,"pitch":64},{"start":8,"len":1,"pitch":67}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        e.call(
            "notes",
            &json!({"action":"distribute_note_rhythm","track":"keys","all_patterns":true}),
        )
        .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        for p in &e.project.patterns {
            if let Some(ns) = p.clips.get("keys") {
                assert_eq!(
                    ns.iter().map(|n| n.start).collect::<Vec<_>>(),
                    vec![0., 4., 8.]
                );
            }
        }
        e.call("save_project", &json!({"path":"distribute.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/distribute-test".into());
        l.call("load_project", &json!({"path":"distribute.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.project.patterns[1].notes_mut("keys")[1].start = 1.;
        e.project.patterns[1].notes_mut("keys")[1].len = 11.;
        e.project.patterns[0].notes_mut("keys")[1].start = 1.;
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "distribute_note_rhythm",
                &json!({"track":"keys","all_patterns":true})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod align_chord_tests {
    use crate::{
        midi_ops::{align_chord_ends, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn longest_shortest_selection_expression_atomic_and_noop() {
        let mut ns = vec![
            Note::new(0., 1., 60, 0.2),
            Note::new(0., 3., 64, 0.8),
            Note::new(0., 2., 67, 0.6),
            Note::new(4., 1., 72, 0.4),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("c{i}");
            n.offset = 0.12;
            n.prob = 0.6;
            n.slide_to = Some(75);
        }
        let b = ns.clone();
        let sel = Selection {
            ids: vec!["c0".into(), "c1".into(), "c3".into()],
            ..Default::default()
        };
        assert_eq!(align_chord_ends(&mut ns, &sel, true, 16.).unwrap(), 1);
        let mut expected = b.clone();
        expected[0].len = 3.;
        assert_eq!(ns, expected);
        assert_eq!(align_chord_ends(&mut ns, &sel, true, 16.).unwrap(), 0);
        ns = b.clone();
        assert_eq!(align_chord_ends(&mut ns, &sel, false, 16.).unwrap(), 1);
        expected = b.clone();
        expected[1].len = 1.;
        assert_eq!(ns, expected);
        ns = b.clone();
        ns[3].len = 20.;
        let invalid = ns.clone();
        assert!(align_chord_ends(&mut ns, &sel, true, 16.).is_err());
        assert_eq!(ns, invalid);
        ns = b;
        ns[1].start = 0.00009;
        ns[0].len = 0.05;
        let b = ns.clone();
        assert!(align_chord_ends(&mut ns, &sel, false, 16.).is_err());
        assert_eq!(ns, b);
    }
    #[test]
    fn grouped_meters_roundtrip_history_and_mode_validation() {
        let mut e = Engine::new("/tmp/chord-align-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"len":1,"pitch":60},{"start":0,"len":3,"pitch":64},{"start":4,"len":2,"pitch":72}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        for bad in [json!("bad"), json!(5), json!(null)] {
            assert!(e
                .call(
                    "align_chord_ends",
                    &json!({"track":"keys","all_patterns":true,"mode":bad})
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.call(
            "notes",
            &json!({"action":"align_chord_ends","track":"keys","all_patterns":true}),
        )
        .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        for p in &e.project.patterns {
            if let Some(ns) = p.clips.get("keys") {
                assert_eq!(
                    ns.iter().map(|n| n.len).collect::<Vec<_>>(),
                    vec![3., 3., 2.]
                );
            }
        }
        e.call("save_project", &json!({"path":"aligned.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/chord-align-test".into());
        l.call("load_project", &json!({"path":"aligned.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.project.patterns[0].notes_mut("keys")[0].len = 1.;
        e.project.patterns[1].notes_mut("keys")[1].len = 20.;
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "align_chord_ends",
                &json!({"track":"keys","all_patterns":true})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod rotate_pitch_tests {
    use crate::{
        midi_ops::{rotate_pitches, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn chronological_selection_expression_wrap_and_chord_rejection() {
        let mut ns = vec![
            Note::new(8., 1., 72, 0.8),
            Note::new(0., 2., 60, 0.2),
            Note::new(2., 1., 64, 0.4),
            Note::new(2., 1., 65, 0.6),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("p{i}");
            n.prob = 0.7;
            n.offset = 0.1;
            n.slide_to = Some(74);
        }
        let b = ns.clone();
        let sel = Selection {
            ids: vec!["p0".into(), "p1".into(), "p2".into()],
            ..Default::default()
        };
        assert_eq!(rotate_pitches(&mut ns, &sel, 1).unwrap(), 3);
        let mut expected = b.clone();
        expected[0].pitch = 64;
        expected[1].pitch = 72;
        expected[2].pitch = 60;
        assert_eq!(ns, expected);
        assert_eq!(rotate_pitches(&mut ns, &sel, -1).unwrap(), 3);
        assert_eq!(ns, b);
        assert_eq!(rotate_pitches(&mut ns, &sel, 3).unwrap(), 0);
        assert_eq!(ns, b);
        assert!(rotate_pitches(&mut ns, &Selection::default(), 1).is_err());
        assert_eq!(ns, b);
        let mut ns = b.clone();
        assert!(rotate_pitches(&mut ns, &Selection::default(), i32::MIN).is_err());
        assert_eq!(ns, b);
        let mut ns = vec![
            Note::new(0., 1., 60, 0.5),
            Note::new(4., 1., 64, 0.5),
            Note::new(8., 1., 67, 0.5),
        ];
        assert_eq!(
            rotate_pitches(&mut ns, &Selection::default(), i32::MIN).unwrap(),
            3
        );
    }
    #[test]
    fn grouped_meters_save_history_invalid_mode_and_rollback() {
        let mut e = Engine::new("/tmp/rotate-pitch-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"len":1,"pitch":60},{"start":2,"len":2,"pitch":64},{"start":8,"len":1,"pitch":72}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        for shift in [json!(1.5), json!("1"), json!(2147483648_i64), json!(null)] {
            assert!(e
                .call(
                    "rotate_melody_pitches",
                    &json!({"track":"keys","all_patterns":true,"shift":shift})
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        e.call(
            "notes",
            &json!({"action":"rotate_melody_pitches","track":"keys","all_patterns":true,"shift":1}),
        )
        .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        for p in &e.project.patterns {
            if let Some(ns) = p.clips.get("keys") {
                assert_eq!(
                    ns.iter().map(|n| n.pitch).collect::<Vec<_>>(),
                    vec![72, 60, 64]
                );
            }
        }
        e.call("save_project", &json!({"path":"cycle.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/rotate-pitch-test".into());
        l.call("load_project", &json!({"path":"cycle.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.project.patterns[1].notes_mut("keys")[1].start = 0.;
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "rotate_melody_pitches",
                &json!({"track":"keys","all_patterns":true,"shift":1})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod reverse_gap_tests {
    use crate::{
        midi_ops::{reverse_rhythm_gaps, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn order_endpoints_chords_selection_expression_involution_and_bounds() {
        let mut ns = vec![
            Note::new(0., 1., 60, 0.2),
            Note::new(1., 1., 64, 0.4),
            Note::new(1., 2., 67, 0.8),
            Note::new(3., 1., 70, 0.5),
            Note::new(4., 1., 72, 0.6),
            Note::new(8., 1., 76, 0.7),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("g{i}");
            n.offset = 0.1;
            n.prob = 0.6;
            n.slide_to = Some(79);
        }
        let b = ns.clone();
        let sel = Selection {
            ids: vec![
                "g0".into(),
                "g1".into(),
                "g2".into(),
                "g4".into(),
                "g5".into(),
            ],
            ..Default::default()
        };
        assert_eq!(reverse_rhythm_gaps(&mut ns, &sel, 16.).unwrap(), 3);
        let mut expected = b.clone();
        expected[1].start = 4.;
        expected[2].start = 4.;
        expected[4].start = 7.;
        assert_eq!(ns, expected);
        assert_eq!(reverse_rhythm_gaps(&mut ns, &sel, 16.).unwrap(), 3);
        assert_eq!(ns, b);
        ns[4].len = 10.;
        let b = ns.clone();
        assert!(reverse_rhythm_gaps(&mut ns, &sel, 16.).is_err());
        assert_eq!(ns, b);
        let mut ns = vec![
            Note::new(0., 1., 60, 0.5),
            Note::new(2., 1., 64, 0.5),
            Note::new(4., 1., 67, 0.5),
        ];
        let b = ns.clone();
        assert_eq!(
            reverse_rhythm_gaps(&mut ns, &Selection::default(), 16.).unwrap(),
            0
        );
        assert_eq!(ns, b);
    }
    #[test]
    fn grouped_meters_roundtrip_history_and_atomic_rollback() {
        let mut e = Engine::new("/tmp/reverse-gap-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"len":1,"pitch":60},{"start":1,"len":1,"pitch":64},{"start":4,"len":1,"pitch":67},{"start":8,"len":1,"pitch":72}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        e.call(
            "notes",
            &json!({"action":"reverse_rhythm_gaps","track":"keys","all_patterns":true}),
        )
        .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        for p in &e.project.patterns {
            if let Some(ns) = p.clips.get("keys") {
                assert_eq!(
                    ns.iter().map(|n| n.start).collect::<Vec<_>>(),
                    vec![0., 4., 7., 8.]
                );
            }
        }
        e.call("save_project", &json!({"path":"gaps.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/reverse-gap-test".into());
        l.call("load_project", &json!({"path":"gaps.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        e.project.patterns[1].notes_mut("keys")[2].len = 9.;
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "reverse_rhythm_gaps",
                &json!({"track":"keys","all_patterns":true})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod reverse_pitch_tests {
    use crate::{
        midi_ops::{reverse_pitches, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn chronological_selection_expression_involution_and_chord_rejection() {
        let mut ns = vec![
            Note::new(8., 1., 72, 0.8),
            Note::new(0., 2., 60, 0.2),
            Note::new(2., 1., 64, 0.4),
            Note::new(2., 1., 65, 0.6),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("p{i}");
            n.prob = 0.7;
            n.offset = 0.1;
            n.slide_to = Some(74);
        }
        let b = ns.clone();
        let sel = Selection {
            ids: vec!["p0".into(), "p1".into(), "p2".into()],
            ..Default::default()
        };
        assert_eq!(reverse_pitches(&mut ns, &sel).unwrap(), 2);
        let mut expected = b.clone();
        expected[0].pitch = 60;
        expected[1].pitch = 72;
        expected[2].pitch = 64;
        assert_eq!(ns, expected);
        assert_eq!(reverse_pitches(&mut ns, &sel).unwrap(), 2);
        assert_eq!(ns, b);
        assert!(reverse_pitches(&mut ns, &Selection::default()).is_err());
        assert_eq!(ns, b);
        let mut ns = b.clone();
        assert!(reverse_pitches(&mut ns, &Selection::default()).is_err());
        assert_eq!(ns, b);
        let mut ns = vec![
            Note::new(0., 1., 60, 0.5),
            Note::new(4., 1., 64, 0.5),
            Note::new(8., 1., 67, 0.5),
        ];
        assert_eq!(reverse_pitches(&mut ns, &Selection::default()).unwrap(), 2);
    }
    #[test]
    fn grouped_meters_save_history_and_atomic_rollback() {
        let mut e = Engine::new("/tmp/reverse-pitch-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"len":1,"pitch":60},{"start":2,"len":2,"pitch":64},{"start":8,"len":1,"pitch":72}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        e.call(
            "notes",
            &json!({"action":"reverse_melody_pitches","track":"keys","all_patterns":true}),
        )
        .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        for p in &e.project.patterns {
            if let Some(ns) = p.clips.get("keys") {
                assert_eq!(
                    ns.iter().map(|n| n.pitch).collect::<Vec<_>>(),
                    vec![72, 64, 60]
                );
            }
        }
        e.call("save_project", &json!({"path":"reverse.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/reverse-pitch-test".into());
        l.call("load_project", &json!({"path":"reverse.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.project.patterns[1].notes_mut("keys")[1].start = 0.;
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "reverse_melody_pitches",
                &json!({"track":"keys","all_patterns":true})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod rotate_velocity_tests {
    use crate::{
        midi_ops::{rotate_velocities, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn exact_values_chronology_selection_expression_and_atomic_chords() {
        let mut ns = vec![
            Note::new(8., 1., 72, 0.8),
            Note::new(0., 2., 60, 0.2),
            Note::new(2., 1., 64, 0.4),
            Note::new(2., 1., 67, 0.6),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("v{i}");
            n.prob = 0.7;
            n.offset = -0.1;
            n.slide_to = Some(74);
        }
        let b = ns.clone();
        let sel = Selection {
            ids: vec!["v0".into(), "v1".into(), "v2".into()],
            ..Default::default()
        };
        assert_eq!(rotate_velocities(&mut ns, &sel, 1).unwrap(), 3);
        let mut expected = b.clone();
        expected[0].vel = 0.4;
        expected[1].vel = 0.8;
        expected[2].vel = 0.2;
        assert_eq!(ns, expected);
        assert_eq!(rotate_velocities(&mut ns, &sel, -1).unwrap(), 3);
        assert_eq!(ns, b);
        assert_eq!(rotate_velocities(&mut ns, &sel, 3).unwrap(), 0);
        assert_eq!(ns, b);
        assert!(rotate_velocities(&mut ns, &Selection::default(), 1).is_err());
        assert_eq!(ns, b);
        assert_eq!(rotate_velocities(&mut ns, &sel, i32::MIN).unwrap(), 3);
        let mut expected = b.clone();
        expected[0].vel = 0.4;
        expected[1].vel = 0.8;
        expected[2].vel = 0.2;
        assert_eq!(ns, expected);
        let mut single = vec![Note::new(0., 1., 60, 0.5)];
        assert_eq!(
            rotate_velocities(&mut single, &Selection::default(), 1).unwrap(),
            0
        );
        let mut equal = vec![Note::new(0., 1., 60, 0.5), Note::new(2., 1., 64, 0.5)];
        assert_eq!(
            rotate_velocities(&mut equal, &Selection::default(), 1).unwrap(),
            0
        );
        let mut bad = b.clone();
        bad[1].vel = 1.1;
        let copy = bad.clone();
        let bad_sel = Selection {
            vel_max: 2.0,
            ..sel.clone()
        };
        assert!(rotate_velocities(&mut bad, &bad_sel, 1).is_err());
        assert_eq!(bad, copy);
    }
    #[test]
    fn mixed_meters_discovery_save_history_and_rollback() {
        let mut e = Engine::new("/tmp/velocity-cycle-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"len":1,"pitch":60,"vel":0.2},{"start":2,"len":2,"pitch":64,"vel":0.4},{"start":8,"len":1,"pitch":72,"vel":0.8}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        for shift in [json!(1.5), json!("1"), json!(2147483648_i64), json!(null)] {
            assert!(e
                .call(
                    "rotate_note_velocities",
                    &json!({"track":"keys","all_patterns":true,"shift":shift})
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        }
        let h = e
            .call(
                "notes",
                &json!({"action":"help","params":{"action":"rotate_note_velocities"}}),
            )
            .unwrap();
        assert_eq!(h["params_schema"]["properties"]["shift"]["type"], "integer");
        e.call("notes",&json!({"action":"rotate_note_velocities","track":"keys","all_patterns":true,"shift":1})).unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        let mut expected = b.clone();
        for p in expected["patterns"].as_array_mut().unwrap() {
            if let Some(ns) = p["clips"]["keys"].as_array_mut() {
                for (n, v) in ns.iter_mut().zip([0.8_f32, 0.2, 0.4]) {
                    n["vel"] = json!(v);
                }
            }
        }
        assert_eq!(a, expected);
        e.call("save_project", &json!({"path":"velocity-cycle.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/velocity-cycle-test".into());
        l.call("load_project", &json!({"path":"velocity-cycle.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.project.patterns[1].notes_mut("keys")[1].start = 0.;
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "rotate_note_velocities",
                &json!({"track":"keys","all_patterns":true,"shift":1})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod reverse_velocity_tests {
    use crate::{
        midi_ops::{reverse_velocities, Selection},
        project::Note,
        Engine,
    };
    use serde_json::json;
    #[test]
    fn chronology_exact_selection_expression_involution_and_chords() {
        let mut ns = vec![
            Note::new(8., 1., 72, 0.8),
            Note::new(0., 2., 60, 0.2),
            Note::new(2., 1., 64, 0.4),
            Note::new(2., 1., 67, 0.6),
        ];
        for (i, n) in ns.iter_mut().enumerate() {
            n.id = format!("r{i}");
            n.prob = 0.7;
            n.offset = -0.1;
            n.slide_to = Some(74);
        }
        let b = ns.clone();
        let sel = Selection {
            ids: vec!["r0".into(), "r1".into(), "r2".into()],
            ..Default::default()
        };
        assert_eq!(reverse_velocities(&mut ns, &sel).unwrap(), 2);
        let mut expected = b.clone();
        expected[0].vel = 0.2;
        expected[1].vel = 0.8;
        assert_eq!(ns, expected);
        assert_eq!(reverse_velocities(&mut ns, &sel).unwrap(), 2);
        assert_eq!(ns, b);
        assert!(reverse_velocities(&mut ns, &Selection::default()).is_err());
        assert_eq!(ns, b);
        let mut single = vec![Note::new(0., 1., 60, 0.5)];
        assert_eq!(
            reverse_velocities(&mut single, &Selection::default()).unwrap(),
            0
        );
        let mut equal = vec![Note::new(0., 1., 60, 0.5), Note::new(2., 1., 64, 0.5)];
        assert_eq!(
            reverse_velocities(&mut equal, &Selection::default()).unwrap(),
            0
        );
        let mut bad = b.clone();
        bad[1].vel = 1.1;
        let copy = bad.clone();
        let sel = Selection {
            vel_max: 2.0,
            ..sel
        };
        assert!(reverse_velocities(&mut bad, &sel).is_err());
        assert_eq!(bad, copy);
    }
    #[test]
    fn mixed_meters_discovery_save_history_and_rollback() {
        let mut e = Engine::new("/tmp/velocity-reverse-test".into());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        for p in ["A", "odd"] {
            e.call("add_notes",&json!({"track":"keys","pattern":p,"notes":[{"start":0,"len":1,"pitch":60,"vel":0.2},{"start":2,"len":2,"pitch":64,"vel":0.4},{"start":8,"len":1,"pitch":72,"vel":0.8}]})).unwrap();
        }
        let b = serde_json::to_value(&e.project).unwrap();
        let h = e
            .call(
                "notes",
                &json!({"action":"help","params":{"action":"reverse_note_velocities"}}),
            )
            .unwrap();
        assert!(h["params_schema"]["properties"]["track"].is_object());
        e.call(
            "notes",
            &json!({"action":"reverse_note_velocities","track":"keys","all_patterns":true}),
        )
        .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        let mut expected = b.clone();
        for p in expected["patterns"].as_array_mut().unwrap() {
            if let Some(ns) = p["clips"]["keys"].as_array_mut() {
                for (n, v) in ns.iter_mut().zip([0.8_f32, 0.4, 0.2]) {
                    n["vel"] = json!(v);
                }
            }
        }
        assert_eq!(a, expected);
        e.call("save_project", &json!({"path":"reverse-velocities.json"}))
            .unwrap();
        let mut l = Engine::new("/tmp/velocity-reverse-test".into());
        l.call("load_project", &json!({"path":"reverse-velocities.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&l.project).unwrap(), a);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.project.patterns[1].notes_mut("keys")[1].start = 0.;
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "reverse_note_velocities",
                &json!({"track":"keys","all_patterns":true})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod invert_velocity_engine_tests {
    use super::*;
    #[test]
    fn invert_velocity_selection_category_history_and_save() {
        let dir = std::env::temp_dir().join("invert-velocity-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for name in ["A", "odd"] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":1}))
                    .unwrap();
                e.call(
                    "set_time_signature",
                    &json!({"pattern":name,"time_signature":"7/8"}),
                )
                .unwrap();
            }
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":1,"pitch":60,"vel":0.2},{"start":0,"len":1,"pitch":64,"vel":0.4},{"start":4,"len":1,"pitch":67,"vel":0.8}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "notes",
            &json!({"action":"invert_note_velocities","track":"keys","all_patterns":true}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        for p in &e.project.patterns {
            for (n, v) in p.notes("keys").iter().zip([0.8, 0.6, 0.2]) {
                assert!((n.vel - v).abs() < 1e-6);
            }
        }
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let id = e.project.patterns[0].notes("keys")[0].id.clone();
        e.call(
            "invert_note_velocities",
            &json!({"track":"keys","pattern":"A","ids":[id]}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
    }
}

#[cfg(test)]
mod balance_chord_velocity_engine_tests {
    use super::*;
    #[test]
    fn balance_chords_category_meters_history_and_save() {
        let dir = std::env::temp_dir().join("balance-chords-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for name in ["A", "odd"] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":1}))
                    .unwrap();
                e.call(
                    "set_time_signature",
                    &json!({"pattern":name,"time_signature":"7/8"}),
                )
                .unwrap();
            }
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":1,"pitch":60,"vel":0.2},{"start":0,"len":2,"pitch":64,"vel":0.8},{"start":4,"len":1,"pitch":67,"vel":0.9}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "notes",
            &json!({"action":"balance_chord_velocities","track":"keys","all_patterns":true}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for p in expected["patterns"].as_array_mut().unwrap() {
            for n in p["clips"]["keys"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .take(2)
            {
                n["vel"] = json!(0.5);
            }
        }
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "balance_chord_velocities",
            &json!({"track":"keys","all_patterns":true}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        e.call("save_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
    }
}

#[cfg(test)]
mod trim_pattern_end_engine_tests {
    use super::*;
    #[test]
    fn mixed_meter_edges_category_undo_save_and_rollback() {
        let dir = std::env::temp_dir().join("trim-pattern-ends-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter, start) in [("A", "4/4", 12), ("odd", "7/8", 12)] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":1}))
                    .unwrap();
            } else {
                e.call("set_pattern_length", &json!({"pattern":name,"bars":1}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":start,"len":8,"pitch":60,"vel":0.8}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "notes",
            &json!({"action":"trim_notes_to_pattern","track":"keys","all_patterns":true}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        expected["patterns"][0]["clips"]["keys"][0]["len"] = json!(4.);
        expected["patterns"][1]["clips"]["keys"][0]["len"] = json!(2.);
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "trim_notes_to_pattern",
            &json!({"track":"keys","all_patterns":true}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        e.project.patterns[0].notes_mut("keys")[0].len = 8.;
        e.project.patterns[1].notes_mut("keys")[0].start = 13.98;
        let before = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "trim_notes_to_pattern",
                &json!({"track":"keys","all_patterns":true})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }
}

#[cfg(test)]
mod reverse_length_engine_tests {
    use super::*;
    #[test]
    fn meters_history_save_noop_and_atomic_rollback() {
        let dir = std::env::temp_dir().join("reverse-lengths-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter) in [("A", "4/4"), ("odd", "7/8")] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":1}))
                    .unwrap();
            } else {
                e.call("set_pattern_length", &json!({"pattern":name,"bars":1}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":4,"pitch":60,"vel":0.3},{"start":8,"len":1,"pitch":67,"vel":0.8}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "notes",
            &json!({"action":"reverse_note_lengths","track":"keys","all_patterns":true}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for i in 0..2 {
            expected["patterns"][i]["clips"]["keys"][0]["len"] = json!(1.);
            expected["patterns"][i]["clips"]["keys"][1]["len"] = json!(4.);
        }
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"reverse-lengths.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"reverse-lengths.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "reverse_note_lengths",
            &json!({"track":"keys","all_patterns":true,"pitches":[60]}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        e.project.patterns[1].notes_mut("keys")[1].start = 13.5;
        let before = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "reverse_note_lengths",
                &json!({"track":"keys","all_patterns":true})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }
}

#[cfg(test)]
mod reset_humanization_engine_tests {
    use super::*;
    #[test]
    fn targets_modes_history_save_and_invalid_atomicity() {
        let dir = std::env::temp_dir().join("reset-humanization-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter, start) in [("A", "4/4", 16.), ("odd", "7/8", 14.)] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":2}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":2,"pitch":60,"vel":0.4},{"start":start,"len":3,"pitch":67,"vel":0.8}]})).unwrap();
            for n in e
                .project
                .patterns
                .iter_mut()
                .find(|p| p.name == name)
                .unwrap()
                .notes_mut("keys")
            {
                n.prob = 0.5;
                n.offset = 0.2;
            }
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call("notes",&json!({"action":"reset_note_humanization","track":"keys","all_patterns":true,"bars":[1,2]})).unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for i in 0..2 {
            expected["patterns"][i]["clips"]["keys"][1]
                .as_object_mut()
                .unwrap()
                .remove("offset");
            expected["patterns"][i]["clips"]["keys"][1]
                .as_object_mut()
                .unwrap()
                .remove("prob");
        }
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"resetfeel.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"resetfeel.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "reset_note_humanization",
            &json!({"track":"keys","all_patterns":true,"bars":[1,2]}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        for mode in [json!("bogus"), json!(3), json!(null)] {
            assert!(e
                .call(
                    "reset_note_humanization",
                    &json!({"track":"keys","mode":mode})
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        }
        for (mode, key) in [("timing", "offset"), ("chance", "prob")] {
            e.call("load_project", &json!({"path":"resetfeel.json"}))
                .unwrap();
            e.call(
                "reset_note_humanization",
                &json!({"track":"keys","all_patterns":true,"mode":mode}),
            )
            .unwrap();
            let mut expected = after.clone();
            for i in 0..2 {
                expected["patterns"][i]["clips"]["keys"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove(key);
            }
            assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        }
    }
}

#[cfg(test)]
mod set_note_chance_engine_tests {
    use super::*;
    #[test]
    fn targets_history_save_validation_and_endpoints() {
        let dir = std::env::temp_dir().join("set-note-chance-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter, start) in [("A", "4/4", 16.), ("odd", "7/8", 14.)] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":2}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":2,"pitch":60,"vel":0.4},{"start":start,"len":3,"pitch":67,"vel":0.8}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call("notes",&json!({"action":"set_note_chance","track":"keys","all_patterns":true,"bars":[1,2],"chance":0.25})).unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for i in 0..2 {
            expected["patterns"][i]["clips"]["keys"][1]["prob"] = json!(0.25);
        }
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"chance.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"chance.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "set_note_chance",
            &json!({"track":"keys","all_patterns":true,"bars":[1,2],"chance":0.25}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        for chance in [json!(-0.1), json!(1.1), json!(null), json!("0.5")] {
            assert!(e
                .call("set_note_chance", &json!({"track":"keys","chance":chance}))
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        }
        for value in [0., 1.] {
            e.call(
                "set_note_chance",
                &json!({"track":"keys","all_patterns":true,"chance":value}),
            )
            .unwrap();
            for p in &e.project.patterns {
                assert!(p.notes("keys").iter().all(|n| n.prob == value as f32));
            }
        }
    }
}

#[cfg(test)]
mod set_note_offset_engine_tests {
    use super::*;
    #[test]
    fn targets_history_save_validation_and_endpoints() {
        let dir = std::env::temp_dir().join("set-note-chance-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter, start) in [("A", "4/4", 16.), ("odd", "7/8", 14.)] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":2}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":2,"pitch":60,"vel":0.4},{"start":start,"len":3,"pitch":67,"vel":0.8}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call("notes",&json!({"action":"set_note_offset","track":"keys","all_patterns":true,"bars":[1,2],"offset":-0.25})).unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for i in 0..2 {
            expected["patterns"][i]["clips"]["keys"][1]["offset"] = json!(-0.25);
        }
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"offset.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"offset.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "set_note_offset",
            &json!({"track":"keys","all_patterns":true,"bars":[1,2],"offset":-0.25}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        for chance in [json!(-0.51), json!(0.51), json!(null), json!("0.5")] {
            assert!(e
                .call("set_note_offset", &json!({"track":"keys","offset":chance}))
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        }
        for value in [-0.5, 0.5] {
            e.call(
                "set_note_offset",
                &json!({"track":"keys","all_patterns":true,"offset":value}),
            )
            .unwrap();
            for p in &e.project.patterns {
                assert!(p.notes("keys").iter().all(|n| n.offset == value as f32));
            }
        }
    }
}

#[cfg(test)]
mod rotate_length_engine_tests {
    use super::*;
    #[test]
    fn meters_history_save_noop_and_atomic_rollback() {
        let dir = std::env::temp_dir().join("rotate-lengths-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter) in [("A", "4/4"), ("odd", "7/8")] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":1}))
                    .unwrap();
            } else {
                e.call("set_pattern_length", &json!({"pattern":name,"bars":1}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":4,"pitch":60,"vel":0.3},{"start":8,"len":1,"pitch":67,"vel":0.8}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "notes",
            &json!({"action":"rotate_note_lengths","track":"keys","shift":1,"all_patterns":true}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for i in 0..2 {
            expected["patterns"][i]["clips"]["keys"][0]["len"] = json!(1.);
            expected["patterns"][i]["clips"]["keys"][1]["len"] = json!(4.);
        }
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"rotate-lengths.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"rotate-lengths.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "rotate_note_lengths",
            &json!({"track":"keys","shift":1,"all_patterns":true,"pitches":[60]}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        e.project.patterns[1].notes_mut("keys")[1].start = 13.5;
        let before = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "rotate_note_lengths",
                &json!({"track":"keys","shift":1,"all_patterns":true})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }
}

#[cfg(test)]
mod invert_length_engine_tests {
    use super::*;
    #[test]
    fn meters_history_save_noop_and_atomic_rollback() {
        let dir = std::env::temp_dir().join("invert-lengths-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter) in [("A", "4/4"), ("odd", "7/8")] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":1}))
                    .unwrap();
            } else {
                e.call("set_pattern_length", &json!({"pattern":name,"bars":1}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":4,"pitch":60,"vel":0.3},{"start":8,"len":1,"pitch":67,"vel":0.8}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "notes",
            &json!({"action":"invert_note_lengths","track":"keys","all_patterns":true}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for i in 0..2 {
            expected["patterns"][i]["clips"]["keys"][0]["len"] = json!(1.);
            expected["patterns"][i]["clips"]["keys"][1]["len"] = json!(4.);
        }
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"invert-lengths.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"invert-lengths.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "invert_note_lengths",
            &json!({"track":"keys","all_patterns":true,"pitches":[60]}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        e.project.patterns[1].notes_mut("keys")[1].start = 13.5;
        let before = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "invert_note_lengths",
                &json!({"track":"keys","all_patterns":true})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }
}

#[cfg(test)]
mod balance_chord_length_engine_tests {
    use super::*;
    #[test]
    fn meters_history_save_noop_and_atomic_rollback() {
        let dir = std::env::temp_dir().join("mean-lengths-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter) in [("A", "4/4"), ("odd", "7/8")] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":1}))
                    .unwrap();
            } else {
                e.call("set_pattern_length", &json!({"pattern":name,"bars":1}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":4,"pitch":60,"vel":0.3},{"start":0,"len":1,"pitch":67,"vel":0.8}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "notes",
            &json!({"action":"balance_chord_lengths","track":"keys","all_patterns":true}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for i in 0..2 {
            expected["patterns"][i]["clips"]["keys"][0]["len"] = json!(2.5);
            expected["patterns"][i]["clips"]["keys"][1]["len"] = json!(2.5);
        }
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"mean-lengths.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"mean-lengths.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "balance_chord_lengths",
            &json!({"track":"keys","all_patterns":true,"pitches":[60]}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        e.project.patterns[1].notes_mut("keys")[1].start = 13.5;
        let before = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "balance_chord_lengths",
                &json!({"track":"keys","all_patterns":true})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }
}

#[cfg(test)]
mod scale_length_engine_tests {
    use super::*;
    #[test]
    fn meters_history_save_noop_and_atomic_rollback() {
        let dir = std::env::temp_dir().join("scale-lengths-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter) in [("A", "4/4"), ("odd", "7/8")] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":1}))
                    .unwrap();
            } else {
                e.call("set_pattern_length", &json!({"pattern":name,"bars":1}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":4,"pitch":60,"vel":0.3},{"start":8,"len":1,"pitch":67,"vel":0.8}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "notes",
            &json!({"action":"scale_note_lengths","factor":0.5,"track":"keys","all_patterns":true}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for i in 0..2 {
            expected["patterns"][i]["clips"]["keys"][0]["len"] = json!(2.);
            expected["patterns"][i]["clips"]["keys"][1]["len"] = json!(0.5);
        }
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"scale-lengths.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"scale-lengths.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "scale_note_lengths",
            &json!({"track":"keys","all_patterns":true,"factor":1,"pitches":[60]}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        e.project.patterns[1].notes_mut("keys")[1].start = 13.5;
        let before = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "scale_note_lengths",
                &json!({"track":"keys","all_patterns":true,"factor":2})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }
}

#[cfg(test)]
mod fixed_velocity_engine_tests {
    use super::*;
    #[test]
    fn targets_history_save_validation_and_endpoints() {
        let dir = std::env::temp_dir().join("set-note-velocity-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter, start) in [("A", "4/4", 16.), ("odd", "7/8", 14.)] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":2}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":2,"pitch":60,"vel":0.4},{"start":start,"len":3,"pitch":67,"vel":0.8}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call("notes",&json!({"action":"set_note_velocity","track":"keys","all_patterns":true,"bars":[1,2],"velocity":0.25})).unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for i in 0..2 {
            expected["patterns"][i]["clips"]["keys"][1]["vel"] = json!(0.25);
        }
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"velocity.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"velocity.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "set_note_velocity",
            &json!({"track":"keys","all_patterns":true,"bars":[1,2],"velocity":0.25}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        for velocity in [json!(-0.1), json!(1.1), json!(null), json!("0.5")] {
            assert!(e
                .call(
                    "set_note_velocity",
                    &json!({"track":"keys","velocity":velocity})
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        }
        for value in [0., 1.] {
            e.call(
                "set_note_velocity",
                &json!({"track":"keys","all_patterns":true,"velocity":value}),
            )
            .unwrap();
            for p in &e.project.patterns {
                assert!(p.notes("keys").iter().all(|n| n.vel == value as f32));
            }
        }
    }
}

#[cfg(test)]
mod zero_note_cleanup_engine_tests {
    use super::*;
    #[test]
    fn target_meter_category_history_and_save() {
        let dir = std::env::temp_dir().join("remove-zero-notes-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call("add_track", &json!({"name":"bass","preset":"epiano"}))
            .unwrap();
        for (name, meter, start) in [("A", "4/4", 16.), ("odd", "7/8", 14.)] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":2}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":2,"pitch":60},{"start":start,"len":1,"pitch":64},{"start":start+1.,"len":1,"pitch":67}]})).unwrap();
            let pi = e.project.pattern_index(name).unwrap();
            let ns = e.project.patterns[pi].notes_mut("keys");
            ns[0].vel = 0.;
            ns[1].prob = 0.;
            ns[2].vel = 0.00001;
            ns[2].prob = 0.00001;
        }
        let b = serde_json::to_value(&e.project).unwrap();
        let result=e.call("notes",&json!({"action":"remove_zero_notes","track":"keys","all_patterns":true,"bars":[1,2]})).unwrap();
        assert_eq!(result["removed"], 2);
        let mut expected = b.clone();
        for i in 0..2 {
            expected["patterns"][i]["clips"]["keys"]
                .as_array_mut()
                .unwrap()
                .remove(1);
        }
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        e.call("save_project", &json!({"path":"zero.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"zero.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        let h = e.history().0.len();
        let result = e
            .call(
                "remove_zero_notes",
                &json!({"track":"keys","all_patterns":true,"bars":[1,2]}),
            )
            .unwrap();
        assert_eq!(result["removed"], 0);
        assert_eq!(e.history().0.len(), h);
        for args in [
            json!({"track":"missing"}),
            json!({"track":"keys","bars":[2,1]}),
        ] {
            assert!(e.call("remove_zero_notes", &args).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        }
    }
}
#[cfg(test)]
mod fixed_length_engine_tests {
    use super::*;
    #[test]
    fn meters_history_save_noop_and_atomic_rollback() {
        let dir = std::env::temp_dir().join("fixed-lengths-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter) in [("A", "4/4"), ("odd", "7/8")] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":1}))
                    .unwrap();
            } else {
                e.call("set_pattern_length", &json!({"pattern":name,"bars":1}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":4,"pitch":60,"vel":0.3},{"start":8,"len":1,"pitch":67,"vel":0.8}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "notes",
            &json!({"action":"set_note_lengths","length":2,"track":"keys","all_patterns":true}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for i in 0..2 {
            expected["patterns"][i]["clips"]["keys"][0]["len"] = json!(2.);
            expected["patterns"][i]["clips"]["keys"][1]["len"] = json!(2.);
        }
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"fixed-lengths.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"fixed-lengths.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "set_note_lengths",
            &json!({"track":"keys","all_patterns":true,"length":2,"pitches":[60]}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        e.project.patterns[1].notes_mut("keys")[1].start = 13.5;
        let before = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call(
                "set_note_lengths",
                &json!({"track":"keys","all_patterns":true,"length":2})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }
}
#[cfg(test)]
mod velocity_ramp_engine_tests {
    use super::*;
    #[test]
    fn ramp_preserves_unsorted_short_and_beyond_pattern_notes_and_undo() {
        let mut e = Engine::new(std::env::temp_dir().join("ramp-preserve-notes"));
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        let notes = e.project.patterns[0].notes_mut("keys");
        for (i, start) in [80.0, 8.0, 0.0, 4.0].iter().enumerate() {
            notes.push(Note {
                start: *start,
                len: 0.02,
                pitch: 60 + i as u8,
                vel: 0.6,
                prob: 0.7,
                offset: 0.1,
                slide_to: Some(72),
                id: format!("preserve{i}"),
            });
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "ramp_note_velocities",
            &json!({"track":"keys","start_velocity":0.35,"end_velocity":1.0}),
        )
        .unwrap();
        let mut expected = before.clone();
        for (i, start) in [80.0f32, 8.0, 0.0, 4.0].iter().enumerate() {
            expected["patterns"][0]["clips"]["keys"][i]["vel"] =
                json!(0.35f32 + (1.0f32 - 0.35f32) * (*start / 80.0));
        }
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }
    #[test]
    fn targets_history_save_validation_and_endpoints() {
        let dir = std::env::temp_dir().join("velocity-ramp-engine");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter, start) in [("A", "4/4", 16.), ("odd", "7/8", 14.)] {
            if name == "odd" {
                e.call("add_pattern", &json!({"name":name,"bars":2}))
                    .unwrap();
            }
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"track":"keys","pattern":name,"notes":[{"start":0,"len":2,"pitch":60,"vel":0.4},{"start":start,"len":3,"pitch":67,"vel":0.8}]})).unwrap();
        }
        let before = serde_json::to_value(&e.project).unwrap();
        e.call("notes",&json!({"action":"ramp_note_velocities","track":"keys","all_patterns":true,"bars":[1,2],"start_velocity":0.25,"end_velocity":0.75})).unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        for i in 0..2 {
            expected["patterns"][i]["clips"]["keys"][1]["vel"] = json!(0.25);
        }
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        e.call("save_project", &json!({"path":"ramp.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"ramp.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let count = e.history().0.len();
        e.call(
            "ramp_note_velocities",
            &json!({"track":"keys","all_patterns":true,"bars":[1,2],"start_velocity":0.25,"end_velocity":0.75}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), count);
        for velocity in [json!(-0.1), json!(1.1), json!(null), json!("0.5")] {
            assert!(e
                .call(
                    "ramp_note_velocities",
                    &json!({"track":"keys","start_velocity":velocity,"end_velocity":0.75})
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        }
        for value in [0., 1.] {
            e.call(
                "ramp_note_velocities",
                &json!({"track":"keys","all_patterns":true,"start_velocity":value,"end_velocity":value}),
            )
            .unwrap();
            for p in &e.project.patterns {
                assert!(p.notes("keys").iter().all(|n| n.vel == value as f32));
            }
        }
    }
}

#[cfg(test)]
mod geometry_engine_tests {
    use crate::{project::Note, Engine};
    use serde_json::json;
    #[test]
    fn geometry_ids_fields_atomicity_history() {
        let mut e = Engine::new(std::env::temp_dir());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (i, start) in [80., 4., 0.].iter().enumerate() {
            e.project.patterns[0].notes_mut("keys").push(Note {
                start: *start,
                len: 0.02,
                pitch: 60 + i as u8,
                vel: 0.,
                prob: 0.7,
                offset: 0.1,
                slide_to: Some(72),
                id: format!("n{i}"),
            });
        }
        let before = serde_json::to_value(&e.project).unwrap();
        let call = json!({"track":"keys","updates":[{"id":"n1","start":6.,"len":2.,"pitch":70}]});
        e.call(
            "notes",
            &json!({"action":"update_note_geometry","params":call}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        expected["patterns"][0]["clips"]["keys"][1]["start"] = json!(6.);
        expected["patterns"][0]["clips"]["keys"][1]["len"] = json!(2.);
        expected["patterns"][0]["clips"]["keys"][1]["pitch"] = json!(70);
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        let h = e.history().0.len();
        e.call("update_note_geometry", &call).unwrap();
        assert_eq!(e.history().0.len(), h);
        for bad in [
            json!({"id":"missing","start":0,"len":1,"pitch":60}),
            json!({"id":"n2","start":-1,"len":1,"pitch":60}),
            json!({"id":"n2","start":0,"len":0,"pitch":60}),
            json!({"id":"n2","start":0,"len":1,"pitch":128}),
            json!({"id":"n2","start":0,"len":1,"pitch":60.5}),
        ] {
            assert!(e.call("update_note_geometry",&json!({"track":"keys","updates":[{"id":"n1","start":9,"len":1,"pitch":62},bad]})).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        }
        assert!(e.call("update_note_geometry",&json!({"track":"keys","updates":[{"id":"n1","start":9,"len":1,"pitch":62},{"id":"n1","start":4,"len":1,"pitch":62}]})).is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
    }
}

#[cfg(test)]
mod delete_preservation_tests {
    use crate::{project::Note, Engine};
    use serde_json::json;
    #[test]
    fn delete_only_selected_ids_keeps_survivors_order_and_no_match_history() {
        let mut e = Engine::new(std::env::temp_dir());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (i, start) in [80., 8., 0., 4.].iter().enumerate() {
            e.project.patterns[0].notes_mut("keys").push(Note {
                start: *start,
                len: 0.02,
                pitch: 60 + i as u8,
                vel: 0.6,
                prob: 0.7,
                offset: 0.1,
                slide_to: Some(72),
                id: format!("n{i}"),
            });
        }
        let before = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        e.call("delete_notes", &json!({"track":"keys","ids":["missing"]}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        assert_eq!(e.history().0.len(), h);
        let r = e
            .call(
                "notes",
                &json!({"action":"delete_notes","params":{"track":"keys","ids":["n3"]}}),
            )
            .unwrap();
        assert_eq!(r["affected"], json!(1));
        let mut expected = before.clone();
        expected["patterns"][0]["clips"]["keys"]
            .as_array_mut()
            .unwrap()
            .remove(3);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        e.call("delete_notes", &json!({"track":"keys"})).unwrap();
        assert!(e.project.patterns[0].notes("keys").is_empty());
    }
}

#[cfg(test)]
mod velocity_offset_tests {
    use crate::{project::Note, Engine};
    use serde_json::json;
    #[test]
    fn velocity_offset_preserves_fields_selection_clamp_and_history() {
        let mut e = Engine::new(std::env::temp_dir());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (i, (start, vel)) in [(80., 0.), (8., 0.95), (0., 0.04), (4., 0.6)]
            .iter()
            .enumerate()
        {
            e.project.patterns[0].notes_mut("keys").push(Note {
                start: *start,
                len: 0.02,
                pitch: 60 + i as u8,
                vel: *vel,
                prob: 0.7,
                offset: 0.1,
                slide_to: Some(72),
                id: format!("n{i}"),
            });
        }
        let before = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        e.call(
            "offset_note_velocities",
            &json!({"track":"keys","delta":0.}),
        )
        .unwrap();
        assert_eq!(e.history().0.len(), h);
        for delta in [-1.01, 1.01] {
            assert!(e
                .call(
                    "offset_note_velocities",
                    &json!({"track":"keys","delta":delta})
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        }
        e.call("notes",&json!({"action":"offset_note_velocities","params":{"track":"keys","ids":["n1","n2"],"delta":0.1}})).unwrap();
        let mut expected = before.clone();
        expected["patterns"][0]["clips"]["keys"][1]["vel"] = json!(1.);
        expected["patterns"][0]["clips"]["keys"][2]["vel"] = json!(0.04f32 + 0.1f32);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        e.call(
            "offset_note_velocities",
            &json!({"track":"keys","ids":["n2"],"delta":-1.}),
        )
        .unwrap();
        expected["patterns"][0]["clips"]["keys"][2]["vel"] = json!(0.);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
    }
}

#[cfg(test)]
mod fit_velocity_range_engine_tests {
    use serde_json::json;
    #[test]
    fn routed_fit_range_preserves_fields_and_history() {
        let dir = std::env::temp_dir().join(format!("beatbox-fit-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = crate::Engine::new(dir.clone());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        let notes: Vec<_> = [0., 0.5, 1., 0.7]
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let mut n = crate::project::Note::new((i / 2) as f32 * 4., 0.02, 60 + i as u8, *v);
                n.id = format!("fit{i}");
                n.prob = 0.5;
                n.offset = 0.1;
                n.slide_to = Some(72);
                n
            })
            .collect();
        e.project.patterns[0]
            .clips
            .insert("keys".into(), notes.clone());
        let before = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        let args = json!({"track":"keys","ids":["fit0","fit1","fit2"],"low":0.2,"high":0.8});
        let r = e
            .call(
                "notes",
                &json!({"action":"fit_note_velocity_range","params":args}),
            )
            .unwrap();
        assert_eq!(r["affected"], 2);
        let mut expected = before.clone();
        for (i, v) in [0.2f32, 0.5, 0.8].iter().enumerate() {
            expected["patterns"][0]["clips"]["keys"][i]["vel"] = json!(*v);
        }
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        assert_eq!(e.history().0.len(), h + 1);
        e.call("fit_note_velocity_range", &args).unwrap();
        assert_eq!(e.history().0.len(), h + 1);
        for bad in [
            json!({"track":"keys","low":0.9,"high":0.1}),
            json!({"track":"keys","low":"0.2","high":0.8}),
            json!({"track":"keys","low":0,"high":1.1}),
        ] {
            assert!(e.call("fit_note_velocity_range", &bad).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        }
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        e.call("save_project", &json!({"path":"fit.json"})).unwrap();
        e.call("load_project", &json!({"path":"fit.json"})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod chance_ramp_engine_tests {
    use serde_json::json;
    #[test]
    fn routed_chance_ramp_preserves_fields_and_history() {
        let dir = std::env::temp_dir().join(format!("beatbox-fit-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = crate::Engine::new(dir.clone());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        let notes: Vec<_> = [0., 0.5, 1., 0.7]
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let mut n = crate::project::Note::new((i / 2) as f32 * 4., 0.02, 60 + i as u8, *v);
                n.id = format!("fit{i}");
                n.prob = 0.5;
                n.offset = 0.1;
                n.slide_to = Some(72);
                n
            })
            .collect();
        e.project.patterns[0]
            .clips
            .insert("keys".into(), notes.clone());
        let before = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        let args = json!({"track":"keys","ids":["fit0","fit1","fit2"],"start_chance":0.2,"end_chance":0.8});
        let r = e
            .call("notes", &json!({"action":"ramp_note_chance","params":args}))
            .unwrap();
        assert_eq!(r["affected"], 3);
        let mut expected = before.clone();
        for (i, v) in [0.2f32, 0.2, 0.8].iter().enumerate() {
            expected["patterns"][0]["clips"]["keys"][i]["prob"] = json!(*v);
        }
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        assert_eq!(e.history().0.len(), h + 1);
        e.call("ramp_note_chance", &args).unwrap();
        assert_eq!(e.history().0.len(), h + 1);
        for bad in [
            json!({"track":"keys","start_chance":-0.1,"end_chance":0.1}),
            json!({"track":"keys","start_chance":"0.2","end_chance":0.8}),
            json!({"track":"keys","start_chance":0,"end_chance":1.1}),
        ] {
            assert!(e.call("ramp_note_chance", &bad).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        }
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        e.call("save_project", &json!({"path":"fit.json"})).unwrap();
        e.call("load_project", &json!({"path":"fit.json"})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
