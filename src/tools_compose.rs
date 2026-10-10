//! "One musical idea = one call" writing tools: chord-relative riffs,
//! rolls / ratchets, multi-bar phrases with varied repeats, note copying
//! between patterns, 808 slides, per-section mix moves, and `batch` (many
//! tool calls as one atomic undo step).

use crate::dsp::Rng;
use crate::midi_ops as mo;
use crate::project::{Note, Project};
use crate::theory;
use crate::tools::{
    self, b_or, f_opt, f_or, key_pc, obj, pitch_of, s_opt, s_req, seed_of, u_or, Tool,
};
use crate::tools_midi::steps_of;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

/// Pattern indices from `patterns` (array or comma list), `pattern`, or the first.
fn patterns_of(p: &Project, a: &Value) -> Result<Vec<usize>> {
    match a.get("patterns") {
        Some(Value::Array(v)) => v
            .iter()
            .map(|x| p.pattern_index(x.as_str().unwrap_or("")))
            .collect(),
        Some(Value::String(s)) if s == "all" || s == "*" => Ok((0..p.patterns.len()).collect()),
        Some(Value::String(s)) => s.split(',').map(|x| p.pattern_index(x.trim())).collect(),
        _ => Ok(vec![tools::pattern_idx(p, a)?]),
    }
}

fn patterns_prop() -> Value {
    json!({"description": "pattern names to write into (array, 'a,b', or 'all'); default `pattern` or the first", "items": {}})
}

/// Parse a rhythm string: X accent, x hit, o soft, '-' or '_' extend the
/// previous hit, '.' rest. Returns (step, len, vel factor).
pub fn rhythm_hits(r: &str) -> Vec<(usize, f32, f32)> {
    let cells: Vec<char> = r
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '|')
        .collect();
    let mut out: Vec<(usize, f32, f32)> = Vec::new();
    for (i, c) in cells.iter().enumerate() {
        match c {
            'X' => out.push((i, 1.0, 1.0)),
            'x' => out.push((i, 1.0, 0.8)),
            'o' | 'O' => out.push((i, 1.0, 0.55)),
            '-' | '_' | '=' => {
                if let Some(last) = out.last_mut() {
                    if last.0 as f32 + last.1 >= i as f32 - 0.01 {
                        last.1 += 1.0;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn rhythm_len(r: &str) -> usize {
    r.chars()
        .filter(|c| !c.is_whitespace() && *c != '|')
        .count()
        .max(1)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "write_riff",
            description: "Write a chord-relative riff / ostinato over a progression in one call: rhythm (step string, X accent x hit o soft - hold . rest; repeats over each chord, e.g. 3-3-2 'x..x..x.') and degrees (chord-relative scale degrees cycled across the hits: 1 = chord root, 3 = third, 5 = fifth, 8 = octave, 2/4/6 passing tones, 0/-1 below). Follows each chord of the progression (bars_per_chord), in the project key. patterns=['verse','hook'] writes every section at once; replace=false layers onto existing notes. Use for arps, guitar counter-lines, piano ostinatos, stabs.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string"}, "preset": {"type": "string"}, "instrument": {"type": "object"},
                "pattern": {"type": "string"}, "patterns": patterns_prop(),
                "progression": {"type": "string"}, "bars_per_chord": {"type": "number"},
                "rhythm": {"type": "string"}, "degrees": {"type": "array", "items": {"type": "integer"}},
                "octave": {"type": "integer"}, "gate": {"type": "number", "description": "note length factor 0.1-1 (1 = legato)"},
                "velocity": {"type": "number"}, "vel_accent": {"type": "number", "description": "extra velocity on X hits and chord changes"},
                "replace": {"type": "boolean"}
            }), &["track", "progression", "rhythm"]),
            run: |e, a| {
                let key = key_pc(&e.project);
                let scale = theory::scale_intervals(&e.project.scale)?.to_vec();
                let chords = theory::parse_progression(&s_req(a, "progression")?, key, &e.project.scale)?;
                let rhythm = s_req(a, "rhythm")?;
                let hits = rhythm_hits(&rhythm);
                if hits.is_empty() { bail!("rhythm has no hits (use X, x or o)"); }
                let rlen = rhythm_len(&rhythm);
                let degrees: Vec<i32> = a.get("degrees").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_i64().map(|d| d as i32)).collect()).unwrap_or_else(|| vec![1, 3, 5, 8]);
                if degrees.is_empty() { bail!("degrees is empty"); }
                let octave = u_or(a, "octave", 4) as i32;
                let gate = f_or(a, "gate", 0.9).clamp(0.1, 1.0);
                let vel = f_or(a, "velocity", 0.75);
                let acc = f_or(a, "vel_accent", 0.12);
                let track = tools::ensure_track(&mut e.project, &s_req(a, "track")?, "pluck_lead", tools::instrument_from(a)?)?;
                let span = (f_or(a, "bars_per_chord", 1.0) * 16.0).max(1.0);
                let mut written = Vec::new();
                for pi in patterns_of(&e.project, a)? {
                    let steps = e.project.patterns[pi].steps() as f32;
                    let mut notes = Vec::new();
                    let mut hit_i = 0usize;
                    let mut ci = 0usize;
                    let mut t0 = 0.0f32;
                    while t0 < steps - 0.01 {
                        let ch = &chords[ci % chords.len()];
                        let root = (octave + 1) * 12 + ch.root_pc as i32;
                        // snap the chord root into the key's scale for diatonic steps
                        let rootp = root.clamp(0, 127) as u8;
                        let mut off = 0.0f32;
                        while off < span - 0.01 && t0 + off < steps - 0.01 {
                            for (s, len, vf) in &hits {
                                let start = t0 + off + *s as f32;
                                if *s as f32 + off >= span || start >= steps { continue; }
                                let d = degrees[hit_i % degrees.len()];
                                hit_i += 1;
                                let pitch = match d {
                                    1 => rootp,
                                    3 if ch.intervals.len() > 1 => (root + ch.intervals[1] as i32).clamp(0, 127) as u8,
                                    5 if ch.intervals.len() > 2 => (root + ch.intervals[2] as i32).clamp(0, 127) as u8,
                                    7 if ch.intervals.len() > 3 => (root + ch.intervals[3] as i32).clamp(0, 127) as u8,
                                    8 => (root + 12).clamp(0, 127) as u8,
                                    d => mo::diatonic_shift(mo::snap_to_scale(rootp, key, &scale), d - 1, key, &scale),
                                };
                                let first = off == 0.0 && *s == hits[0].0;
                                let v = (vel * vf + if *vf >= 1.0 || first { acc } else { 0.0 }).clamp(0.05, 1.0);
                                notes.push(Note::new(start, (len * gate).max(0.1).min(steps - start), pitch, v));
                            }
                            off += rlen as f32;
                        }
                        t0 += span;
                        ci += 1;
                    }
                    let pat = &mut e.project.patterns[pi];
                    let pname = pat.name.clone();
                    let dst = pat.notes_mut(&track);
                    if b_or(a, "replace", true) { dst.clear(); }
                    written.push(json!({"pattern": pname, "notes": notes.len()}));
                    dst.extend(notes);
                    mo::sort(dst);
                }
                Ok(json!({"track": track, "written": written, "chords": chords.iter().map(|c| c.label.clone()).collect::<Vec<_>>()}))
            },
        },
        Tool {
            name: "add_roll",
            description: "Write a roll / ratchet / build in one call: hits every `rate` ('1/32', '1/16t', '1/64', '1/16' or steps) from at_step for length_steps, velocity ramping vel_from -> vel_to and optional pitch glide pitch_from -> pitch_to (pitched 808 / tom rolls). replace=true (default) clears the track's notes in that range first. Hat rolls: track 'hat', at_step 28, length_steps 4, rate '1/32'. Snare build: length_steps 16, rate '1/16' then another call with '1/32' for the last beat.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string"}, "pattern": {"type": "string"}, "patterns": patterns_prop(),
                "at_step": {"type": "number"}, "length_steps": {"type": "number"}, "rate": {"description": "'1/32' | '1/16t' | '1/64' | steps"},
                "vel_from": {"type": "number"}, "vel_to": {"type": "number"}, "pitch_from": {}, "pitch_to": {}, "replace": {"type": "boolean"}
            }), &["track", "at_step", "length_steps"]),
            run: |e, a| {
                let ti = e.project.track_index(&s_req(a, "track")?)?;
                let name = e.project.tracks[ti].name.clone();
                let at = f_or(a, "at_step", 0.0).max(0.0);
                let len = f_or(a, "length_steps", 4.0).max(0.125);
                let rate = steps_of(a.get("rate"), 0.5)?.max(0.0625);
                let (v0, v1) = (f_or(a, "vel_from", 0.45), f_or(a, "vel_to", 0.95));
                let p0 = a.get("pitch_from").map(pitch_of).transpose()?;
                let p1 = a.get("pitch_to").map(pitch_of).transpose()?;
                let default_pitch = if e.project.tracks[ti].instrument.is_drum() { 60 } else { 48 };
                let count = ((len / rate).round() as usize).max(1);
                let mut out = Vec::new();
                for pi in patterns_of(&e.project, a)? {
                    let pat = &mut e.project.patterns[pi];
                    let steps = pat.steps() as f32;
                    let notes = pat.notes_mut(&name);
                    if b_or(a, "replace", true) { notes.retain(|n| n.start < at - 1e-3 || n.start >= at + len - 1e-3); }
                    let mut k = 0;
                    for i in 0..count {
                        let t = at + i as f32 * rate;
                        if t >= steps { break; }
                        let f = if count > 1 { i as f32 / (count - 1) as f32 } else { 1.0 };
                        let pitch = match (p0, p1) {
                            (Some(a0), Some(a1)) => (a0 as f32 + (a1 as f32 - a0 as f32) * f).round() as u8,
                            (Some(a0), None) => a0,
                            _ => notes.iter().rev().find(|n| n.start < at).map(|n| n.pitch).unwrap_or(default_pitch),
                        };
                        notes.push(Note::new(t, (rate * 0.9).max(0.05), pitch, (v0 + (v1 - v0) * f).clamp(0.02, 1.0)));
                        k += 1;
                    }
                    mo::sort(notes);
                    out.push(json!({"pattern": pat.name, "hits": k}));
                }
                Ok(json!({"track": name, "rate_steps": rate, "written": out}))
            },
        },
        Tool {
            name: "write_phrase",
            description: "Write a multi-bar phrase once and have it repeated across a pattern (and across several patterns) with musical variation: notes = the motif [{start, len, pitch, vel}] in steps within `bars` bars; it repeats to fill each pattern. variation 0..1 varies repeats 2+ (kind melodic|rhythmic|both, the last repeat varies most, like a turnaround), transpose_repeats=[0,0,5,0] shifts repeats diatonically by scale steps (sequence), answer=true makes every other repeat end on a different chord tone. Saves dozens of add_notes calls.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string"}, "preset": {"type": "string"}, "instrument": {"type": "object"},
                "pattern": {"type": "string"}, "patterns": patterns_prop(),
                "notes": {"type": "array", "items": {"type": "object"}}, "bars": {"type": "number"},
                "variation": {"type": "number"}, "kind": {"type": "string", "enum": ["melodic", "rhythmic", "both"]},
                "transpose_repeats": {"type": "array", "items": {"type": "integer"}}, "answer": {"type": "boolean"}, "seed": {"type": "integer"}, "replace": {"type": "boolean"}
            }), &["track", "notes"]),
            run: |e, a| {
                let arr = a.get("notes").and_then(|v| v.as_array()).ok_or_else(|| anyhow!("notes must be an array"))?;
                let mut motif = Vec::new();
                for n in arr {
                    motif.push(Note {
                        start: f_opt(n, "start").ok_or_else(|| anyhow!("note missing start"))?.max(0.0),
                        len: f_opt(n, "len").or(f_opt(n, "duration")).unwrap_or(1.0).max(0.05),
                        pitch: pitch_of(n.get("pitch").ok_or_else(|| anyhow!("note missing pitch"))?)?,
                        vel: f_or(n, "vel", 0.8).clamp(0.0, 1.0),
                        slide_to: n.get("slide_to").map(pitch_of).transpose()?,
                        ..Default::default()
                    });
                }
                if motif.is_empty() { bail!("notes is empty"); }
                let span = match f_opt(a, "bars") { Some(b) => b * 16.0, None => ((motif.iter().map(|n| n.end()).fold(0.0f32, f32::max) / 16.0).ceil() * 16.0).max(16.0) };
                let key = key_pc(&e.project);
                let scale = theory::scale_intervals(&e.project.scale)?.to_vec();
                let trans: Vec<i32> = a.get("transpose_repeats").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_i64().map(|d| d as i32)).collect()).unwrap_or_default();
                let var = f_or(a, "variation", 0.0).clamp(0.0, 1.0);
                let kind = s_opt(a, "kind").unwrap_or_else(|| "melodic".into());
                let answer = b_or(a, "answer", false);
                let track = tools::ensure_track(&mut e.project, &s_req(a, "track")?, "pluck_lead", tools::instrument_from(a)?)?;
                let is_drum = e.project.tracks[e.project.track_index(&track)?].instrument.is_drum();
                let mut rng = Rng::new(seed_of(a));
                let mut out = Vec::new();
                for pi in patterns_of(&e.project, a)? {
                    let steps = e.project.patterns[pi].steps() as f32;
                    let reps = ((steps / span).ceil() as usize).max(1);
                    let mut notes = Vec::new();
                    for r in 0..reps {
                        let mut m: Vec<Note> = motif.clone();
                        if let Some(&t) = trans.get(r % trans.len().max(1)).filter(|_| !trans.is_empty()) {
                            if t != 0 && !is_drum {
                                for n in m.iter_mut() { n.pitch = mo::diatonic_shift(mo::snap_to_scale(n.pitch, key, &scale), t, key, &scale); }
                            }
                        }
                        if answer && r % 2 == 1 && !is_drum {
                            if let Some(last) = m.iter_mut().max_by(|x, y| x.start.partial_cmp(&y.start).unwrap_or(std::cmp::Ordering::Equal)) {
                                last.pitch = mo::diatonic_shift(mo::snap_to_scale(last.pitch, key, &scale), -2, key, &scale);
                            }
                        }
                        if var > 0.0 && r > 0 {
                            let amt = if r == reps - 1 { (var * 1.5).min(1.0) } else { var * 0.6 };
                            m = mo::variation(&m, &kind, amt, key, &scale, span, is_drum, &mut rng)?;
                        }
                        for n in m {
                            let s = n.start + r as f32 * span;
                            if s < steps { notes.push(Note { start: s, len: n.len.min(steps - s), ..n }); }
                        }
                    }
                    let pat = &mut e.project.patterns[pi];
                    let pname = pat.name.clone();
                    let dst = pat.notes_mut(&track);
                    if b_or(a, "replace", true) { dst.clear(); }
                    out.push(json!({"pattern": pname, "repeats": reps, "notes": notes.len()}));
                    dst.extend(notes);
                    mo::sort(dst);
                }
                Ok(json!({"track": track, "phrase_steps": span, "written": out}))
            },
        },
        Tool {
            name: "copy_notes",
            description: "Copy one track's notes between patterns (unlike add_pattern copy_from, which copies every track): from_pattern -> to_pattern(s) (to_patterns array or 'all'), optional from_bar + bars (a slice), to_bar (destination offset), transpose (semitones), velocity_scale, tile=true repeats the slice to fill the destination, replace (default true clears the destination range first).",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string"}, "from_pattern": {"type": "string"}, "to_pattern": {"type": "string"}, "to_patterns": patterns_prop(),
                "from_bar": {"type": "number"}, "bars": {"type": "number"}, "to_bar": {"type": "number"},
                "transpose": {"type": "integer"}, "velocity_scale": {"type": "number"}, "tile": {"type": "boolean"}, "replace": {"type": "boolean"}
            }), &["track", "from_pattern"]),
            run: |e, a| {
                let ti = e.project.track_index(&s_req(a, "track")?)?;
                let name = e.project.tracks[ti].name.clone();
                let src_i = e.project.pattern_index(&s_req(a, "from_pattern")?)?;
                let src_steps = e.project.patterns[src_i].steps() as f32;
                let f0 = f_or(a, "from_bar", 0.0) * 16.0;
                let f1 = f_opt(a, "bars").map(|b| f0 + b * 16.0).unwrap_or(src_steps);
                let slice: Vec<Note> = e.project.patterns[src_i].notes(&name).iter().filter(|n| n.start >= f0 - 1e-3 && n.start < f1 - 1e-3).map(|n| Note { start: n.start - f0, ..n.clone() }).collect();
                if slice.is_empty() { bail!("'{name}' has no notes in that part of '{}'", e.project.patterns[src_i].name); }
                let dests: Vec<usize> = match (a.get("to_patterns"), s_opt(a, "to_pattern")) {
                    (Some(_), _) => { let mut b = a.clone(); b["patterns"] = a["to_patterns"].clone(); patterns_of(&e.project, &b)? }
                    (None, Some(t)) => vec![e.project.pattern_index(&t)?],
                    _ => bail!("give to_pattern or to_patterns"),
                };
                let to0 = f_or(a, "to_bar", 0.0) * 16.0;
                let tr = f_or(a, "transpose", 0.0) as i32;
                let vs = f_or(a, "velocity_scale", 1.0);
                let len = f1 - f0;
                let mut out = Vec::new();
                for di in dests {
                    if di == src_i && to0 == f0 { continue; }
                    let pat = &mut e.project.patterns[di];
                    let steps = pat.steps() as f32;
                    let end = if b_or(a, "tile", false) { steps } else { (to0 + len).min(steps) };
                    let dst = pat.notes_mut(&name);
                    if b_or(a, "replace", true) { dst.retain(|n| n.start < to0 - 1e-3 || n.start >= end - 1e-3); }
                    let mut k = 0;
                    let mut off = to0;
                    while off < end - 1e-3 {
                        for n in &slice {
                            let s = n.start + off;
                            if s < end - 1e-3 {
                                dst.push(Note { start: s, pitch: (n.pitch as i32 + tr).clamp(0, 127) as u8, vel: (n.vel * vs).clamp(0.02, 1.0), ..n.clone() });
                                k += 1;
                            }
                        }
                        off += len.max(1.0);
                        if !b_or(a, "tile", false) { break; }
                    }
                    mo::sort(dst);
                    out.push(json!({"pattern": pat.name, "notes": k}));
                }
                Ok(json!({"track": name, "copied": out}))
            },
        },
        Tool {
            name: "add_slides",
            description: "Make an 808 / bass line glide: for consecutive notes whose pitch changes (within max_interval semitones), with probability `amount` (1 = every change), the earlier note is extended to the next one and gets slide_to = its pitch, so it bends into it. glide_ms sets the instrument's glide time (808 and synth). Undo-safe; remove with clear=true.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "pattern": {"type": "string"}, "patterns": patterns_prop(), "amount": {"type": "number"}, "max_interval": {"type": "integer"}, "glide_ms": {"type": "number"}, "clear": {"type": "boolean"}, "seed": {"type": "integer"}}), &["track"]),
            run: |e, a| {
                let ti = e.project.track_index(&s_req(a, "track")?)?;
                let name = e.project.tracks[ti].name.clone();
                if let Some(g) = f_opt(a, "glide_ms") {
                    match &mut e.project.tracks[ti].instrument {
                        crate::instruments::Instrument::Bass808(p) => p.glide_ms = g.clamp(5.0, 2000.0),
                        crate::instruments::Instrument::Synth(p) => p.glide_ms = g.clamp(5.0, 2000.0),
                        other => bail!("{} instruments don't glide (use 808 or synth)", other.kind_name()),
                    }
                }
                let amount = f_or(a, "amount", 0.5).clamp(0.0, 1.0);
                let maxi = u_or(a, "max_interval", 12) as i32;
                let clear = b_or(a, "clear", false);
                let mut rng = Rng::new(seed_of(a));
                let mut total = 0;
                for pi in patterns_of(&e.project, a)? {
                    let notes = e.project.patterns[pi].notes_mut(&name);
                    mo::sort(notes);
                    for i in 0..notes.len() {
                        if clear { notes[i].slide_to = None; continue; }
                        if i + 1 >= notes.len() { break; }
                        let (p0, p1) = (notes[i].pitch as i32, notes[i + 1].pitch as i32);
                        if p0 != p1 && (p1 - p0).abs() <= maxi && rng.chance(amount) {
                            notes[i].slide_to = Some(p1 as u8);
                            notes[i].len = (notes[i + 1].start - notes[i].start).max(notes[i].len);
                            total += 1;
                        }
                    }
                }
                Ok(json!({"track": name, "slides": total}))
            },
        },
        Tool {
            name: "set_section_mix",
            description: "Set a track's / bus's level just for one arrangement section (verse/hook contrast, a quieter bridge): writes stepped volume automation so the section plays at volume_db and the rest of the song keeps the fader level. section = arrangement index or pattern name (every occurrence with all_occurrences=true).",
            mutates: true,
            schema: || obj(json!({"section": {"type": "string"}, "track": {"type": "string", "description": "track, bus or master"}, "volume_db": {"type": "number"}, "all_occurrences": {"type": "boolean"}}), &["section", "track", "volume_db"]),
            run: |e, a| {
                let owner = crate::tools_studio::owner_of(&e.project, &s_req(a, "track")?)?;
                let sec = s_req(a, "section")?;
                let v = f_or(a, "volume_db", 0.0).clamp(-100.0, 12.0);
                let base = crate::tools_studio::static_value(&e.project, &owner, "volume")?;
                let secs = e.project.song_sections();
                let mut ranges = Vec::new();
                if let Ok(i) = sec.parse::<usize>() { ranges.push(e.project.section_beats(&i.to_string())?); } else {
                    for (i, s) in secs.iter().enumerate() {
                        if s.pattern.eq_ignore_ascii_case(&sec) {
                            ranges.push(e.project.section_beats(&i.to_string())?);
                            if !b_or(a, "all_occurrences", true) { break; }
                        }
                    }
                }
                if ranges.is_empty() { bail!("no section '{sec}'"); }
                use crate::automation::{AutoPoint, Curve};
                let lane = crate::tools_studio::lane_mut(&mut e.project, &owner, "volume");
                if lane.points.is_empty() { lane.points.push(AutoPoint { beat: 0.0, value: base, curve: Curve::Step }); }
                for (s0, s1) in &ranges {
                    let before = lane.value_at(*s0 - 0.01).unwrap_or(base);
                    let after = lane.value_at(*s1 + 0.01).unwrap_or(base);
                    lane.points.retain(|p| p.beat < *s0 - 1e-3 || p.beat > *s1 + 1e-3);
                    lane.points.push(AutoPoint { beat: (*s0 - 0.01).max(0.0), value: before, curve: Curve::Step });
                    lane.points.push(AutoPoint { beat: *s0, value: v, curve: Curve::Step });
                    lane.points.push(AutoPoint { beat: *s1, value: after, curve: Curve::Step });
                }
                lane.enabled = true;
                lane.sort();
                Ok(json!({"track": owner, "volume_db": v, "sections": ranges.iter().map(|r| json!([r.0, r.1])).collect::<Vec<_>>()}))
            },
        },
        Tool {
            name: "batch",
            description: "Run many tool calls in one round trip and ONE undo step: calls=[{tool, args}]. Every call goes through the same validation as a standalone call (unknown arguments are rejected, stable effect ids like 'fx.<id>.<param>' resolve against the project as it is after the earlier calls). atomic=true (default): the first failing call rolls back the whole batch and the error names it. atomic=false (partial success): each failing call is rolled back on its own, the others are kept, and results list ok/error per call; the kept calls are still one undo step. batch/undo/redo/load_project/new_project/produce_track can't be nested.",
            mutates: true,
            schema: || obj(json!({"calls": {"type": "array", "items": {"type": "object", "properties": {"tool": {"type": "string"}, "args": {"type": "object"}}, "required": ["tool"]}}, "atomic": {"type": "boolean"}}), &["calls"]),
            run: |e, a| {
                let calls = a.get("calls").and_then(|v| v.as_array()).ok_or_else(|| anyhow!("calls must be an array"))?;
                if calls.len() > 500 { bail!("max 500 calls per batch"); }
                let atomic = b_or(a, "atomic", true);
                // structural problems are checked before anything runs
                let mut plan = Vec::with_capacity(calls.len());
                for (i, c) in calls.iter().enumerate() {
                    let name = c.get("tool").or_else(|| c.get("name")).and_then(|v| v.as_str()).ok_or_else(|| anyhow!("call {i} has no tool"))?;
                    if matches!(name, "batch" | "undo" | "redo" | "load_project" | "new_project" | "produce_track") { bail!("call {i}: '{name}' can't run inside a batch"); }
                    let args = c.get("args").or_else(|| c.get("arguments")).cloned().unwrap_or(json!({}));
                    plan.push((name, args));
                }
                let mut results = Vec::new();
                let mut errors = 0;
                for (i, (name, args)) in plan.into_iter().enumerate() {
                    // the same validated path as a standalone call; it rolls back
                    // its own failure, the outer call owns undo and revision
                    match e.call_nested(name, &args, "batch") {
                        Ok(v) => results.push(json!({"index": i, "tool": name, "ok": true, "result": v})),
                        Err(err) => {
                            if atomic { bail!("batch call {i} ({name}) failed, nothing applied: {err:#}"); }
                            errors += 1;
                            results.push(json!({"index": i, "tool": name, "ok": false, "error": format!("{err:#}")}));
                        }
                    }
                }
                Ok(json!({"calls": results.len(), "applied": results.len() - errors, "errors": errors, "atomic": atomic, "results": results}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    fn eng() -> Engine {
        Engine::new(std::env::temp_dir().join("beatbox_compose_tests"))
    }

    #[test]
    fn rhythm_strings() {
        assert_eq!(
            rhythm_hits("x..x..x."),
            vec![(0, 1.0, 0.8), (3, 1.0, 0.8), (6, 1.0, 0.8)]
        );
        assert_eq!(rhythm_hits("X--.o"), vec![(0, 3.0, 1.0), (4, 1.0, 0.55)]);
    }

    #[test]
    fn riff_roll_phrase_copy_slides_batch() {
        let mut e = eng();
        e.call("new_project", &json!({"bpm": 90, "key": "D", "scale": "harmonic_minor", "patterns": [{"name": "verse", "bars": 4}, {"name": "hook", "bars": 4}]})).unwrap();
        let r = e.call("write_riff", &json!({"track": "piano", "preset": "felt_piano", "patterns": ["verse", "hook"], "progression": "i VI iv V", "rhythm": "x..x..x.x..x..x.", "degrees": [5, 1, 3, 5, 2, 1], "octave": 4})).unwrap();
        assert_eq!(r["written"].as_array().unwrap().len(), 2);
        let v = e.project.pattern_index("verse").unwrap();
        assert_eq!(e.project.patterns[v].notes("piano").len(), 4 * 6);
        // every riff note is in D harmonic minor
        let sc = theory::scale_intervals("harmonic_minor").unwrap();
        assert!(e.project.patterns[v]
            .notes("piano")
            .iter()
            .all(|n| sc.contains(&(((n.pitch as i32 - 2).rem_euclid(12)) as u8))));
        e.call("add_track", &json!({"name": "hat", "preset": "hat"}))
            .unwrap();
        e.call("add_roll", &json!({"track": "hat", "patterns": "all", "at_step": 60, "length_steps": 4, "rate": "1/32", "vel_from": 0.3, "vel_to": 1.0})).unwrap();
        assert_eq!(e.project.patterns[v].notes("hat").len(), 8);
        e.call("write_phrase", &json!({"track": "lead", "patterns": ["verse"], "bars": 1, "variation": 0.5, "transpose_repeats": [0, 0, 2, 0], "notes": [{"start": 0, "len": 2, "pitch": "D5"}, {"start": 4, "len": 2, "pitch": "F5"}, {"start": 8, "len": 4, "pitch": "A5"}]})).unwrap();
        assert!(e.project.patterns[v].notes("lead").len() >= 10);
        e.call("copy_notes", &json!({"track": "lead", "from_pattern": "verse", "to_pattern": "hook", "bars": 1, "tile": true, "transpose": 12})).unwrap();
        let h = e.project.pattern_index("hook").unwrap();
        assert_eq!(e.project.patterns[h].notes("lead").len(), 12);
        e.call("add_track", &json!({"name": "808", "preset": "808"}))
            .unwrap();
        e.call("add_notes", &json!({"track": "808", "pattern": "verse", "notes": [{"start": 0, "len": 4, "pitch": "D1"}, {"start": 8, "len": 4, "pitch": "A1"}, {"start": 16, "pitch": "F1", "len": 4}]})).unwrap();
        let s = e
            .call(
                "add_slides",
                &json!({"track": "808", "pattern": "verse", "amount": 1.0, "glide_ms": 120}),
            )
            .unwrap();
        assert_eq!(s["slides"], 2);
        assert!(e.analyze().unwrap().master.rms_dbfs > -40.0);
        // batch: atomic failure rolls back everything; success is one undo step
        let before = e.project.clone();
        assert!(e
            .call(
                "batch",
                &json!({"calls": [{"tool": "set_tempo", "args": {"bpm": 100}}, {"tool": "nope"}]})
            )
            .is_err());
        assert_eq!(e.project, before);
        e.call("batch", &json!({"calls": [{"tool": "set_tempo", "args": {"bpm": 100}}, {"tool": "set_mixer", "args": {"track": "hat", "volume_db": -6}}]})).unwrap();
        assert_eq!(e.project.bpm, 100.0);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project, before);
        e.call(
            "set_arrangement",
            &json!({"sections": [{"pattern": "verse"}, {"pattern": "hook"}]}),
        )
        .unwrap();
        e.call(
            "set_section_mix",
            &json!({"section": "hook", "track": "hat", "volume_db": 3}),
        )
        .unwrap();
        let lane = e
            .project
            .automation
            .iter()
            .find(|l| l.target == "hat")
            .unwrap();
        // outside the hook the lane holds the track's own (calibrated) fader
        let base = e
            .project
            .tracks
            .iter()
            .find(|t| t.name == "hat")
            .unwrap()
            .volume_db;
        assert_eq!(lane.value_at(8.0), Some(base));
        assert_eq!(lane.value_at(24.0), Some(3.0));
    }

    // --- batch / standalone parity (external review, finding 1) ---

    fn tempo_eng() -> Engine {
        let mut e = eng();
        e.call(
            "new_project",
            &json!({"bpm": 120, "patterns": [{"name": "a", "bars": 1}]}),
        )
        .unwrap();
        e.call("add_track", &json!({"name": "pad", "preset": "warm_pad"}))
            .unwrap();
        e
    }

    #[test]
    fn batch_rejects_unknown_args_like_a_standalone_call() {
        let mut e = tempo_eng();
        let bad = json!({"bpm": 144, "typo": 1});
        let solo = e.call("set_tempo", &bad).unwrap_err().to_string();
        assert!(solo.contains("typo"), "{solo}");
        let before = e.project.clone();
        let (undo0, _) = e.undo_depth();
        let rev0 = e.revision;
        // atomic: the whole batch fails and nothing is applied
        let err = e
            .call("batch", &json!({"calls": [{"tool": "set_mixer", "args": {"track": "pad", "volume_db": -3}}, {"tool": "set_tempo", "args": bad}]}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("typo") && err.contains("call 1"), "{err}");
        assert_eq!(e.project, before);
        assert_eq!(e.project.bpm, 120.0);
        assert_eq!(e.undo_depth().0, undo0);
        assert_eq!(e.revision, rev0);
        // non-atomic: the typo call fails on its own, with the same message
        let r = e
            .call(
                "batch",
                &json!({"atomic": false, "calls": [{"tool": "set_tempo", "args": bad}]}),
            )
            .unwrap();
        assert_eq!(r["errors"], 1, "{r}");
        assert_eq!(r["results"][0]["error"].as_str().unwrap(), solo);
        assert_eq!(e.project.bpm, 120.0);
    }

    #[test]
    fn batch_resolves_stable_effect_ids() {
        let mut e = tempo_eng();
        // ids assigned by an earlier call in the same batch resolve for later ones
        e.call("batch", &json!({"calls": [
            {"tool": "add_effect", "args": {"track": "pad", "type": "filter"}},
            {"tool": "add_effect", "args": {"track": "pad", "type": "reverb"}},
            {"tool": "tweak_effect", "args": {"track": "pad", "index": "reverb1", "params": {"mix": 0.5}}},
            {"tool": "add_automation", "args": {"track": "pad", "param": "fx.filter1.cutoff", "points": [[0, 200], [4, 4000]]}}
        ]}))
        .unwrap();
        let fx = &e
            .project
            .tracks
            .iter()
            .find(|t| t.name == "pad")
            .unwrap()
            .effects;
        let ids: Vec<&str> = fx.iter().map(|f| f.id()).collect();
        assert_eq!(ids, vec!["filter1", "reverb1"]);
        let rev = serde_json::to_value(&fx[1]).unwrap();
        assert_eq!(rev["mix"].as_f64().unwrap(), 0.5, "{rev}");
        // the filter (position 0) was not tweaked by a mis-resolved index
        assert!(serde_json::to_value(&fx[0])
            .unwrap()
            .get("mix")
            .map(|m| m.as_f64() != Some(0.5))
            .unwrap_or(true));
        assert!(
            e.project
                .automation
                .iter()
                .any(|l| l.target == "pad" && l.param.starts_with("fx.0.")),
            "fx.filter1 -> fx.0"
        );
        // an unknown id fails in a batch exactly as standalone
        let solo = e
            .call(
                "tweak_effect",
                &json!({"track": "pad", "index": "chorus9", "params": {"mix": 0.1}}),
            )
            .unwrap_err()
            .to_string();
        let r = e
            .call("batch", &json!({"atomic": false, "calls": [{"tool": "tweak_effect", "args": {"track": "pad", "index": "chorus9", "params": {"mix": 0.1}}}]}))
            .unwrap();
        assert_eq!(r["results"][0]["error"].as_str().unwrap(), solo);
    }

    #[test]
    fn batch_failure_rolls_back_and_is_one_undo_step() {
        let mut e = tempo_eng();
        let before = e.project.clone();
        let (undo0, _) = e.undo_depth();
        // atomic: a late failure undoes the earlier successful calls
        assert!(e
            .call(
                "batch",
                &json!({"calls": [
                    {"tool": "set_tempo", "args": {"bpm": 150}},
                    {"tool": "add_effect", "args": {"track": "pad", "type": "reverb"}},
                    {"tool": "set_mixer", "args": {"track": "no_such_track", "volume_db": -6}}
                ]})
            )
            .is_err());
        assert_eq!(e.project, before);
        assert_eq!(e.undo_depth().0, undo0);
        // success: many calls, one undo step
        e.call(
            "batch",
            &json!({"calls": [
                {"tool": "set_tempo", "args": {"bpm": 150}},
                {"tool": "add_effect", "args": {"track": "pad", "type": "reverb"}}
            ]}),
        )
        .unwrap();
        assert_eq!(e.undo_depth().0, undo0 + 1);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project, before);
        // nested meta tools are refused before anything runs
        assert!(e
            .call(
                "batch",
                &json!({"calls": [{"tool": "set_tempo", "args": {"bpm": 99}}, {"tool": "undo"}]})
            )
            .is_err());
        assert_eq!(e.project, before);
    }

    #[test]
    fn batch_partial_success_semantics() {
        let mut e = tempo_eng();
        let before = e.project.clone();
        let (undo0, _) = e.undo_depth();
        let r = e
            .call(
                "batch",
                &json!({"atomic": false, "calls": [
                    {"tool": "set_tempo", "args": {"bpm": 140}},
                    {"tool": "set_tempo", "args": {"bpm": 144, "typo": 1}},
                    {"tool": "nope"},
                    {"tool": "set_mixer", "args": {"track": "pad", "volume_db": -4}}
                ]}),
            )
            .unwrap();
        // documented: failing calls are skipped and rolled back individually,
        // the rest are kept, per-call status is reported, one undo step total
        assert_eq!(r["calls"], 4, "{r}");
        assert_eq!(r["applied"], 2, "{r}");
        assert_eq!(r["errors"], 2, "{r}");
        let ok: Vec<bool> = r["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["ok"].as_bool().unwrap())
            .collect();
        assert_eq!(ok, vec![true, false, false, true]);
        assert_eq!(e.project.bpm, 140.0);
        let pad = e.project.tracks.iter().find(|t| t.name == "pad").unwrap();
        assert_eq!(pad.volume_db, -4.0);
        assert_eq!(e.undo_depth().0, undo0 + 1);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project, before);
        // a non-atomic batch where every call fails changes nothing and adds no undo step
        let r = e
            .call("batch", &json!({"atomic": false, "calls": [{"tool": "set_tempo", "args": {"bpm": 1, "typo": 2}}]}))
            .unwrap();
        assert_eq!(r["applied"], 0);
        assert_eq!(e.project, before);
        assert_eq!(e.undo_depth().0, undo0);
    }
}
