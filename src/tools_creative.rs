//! Sprint 9 tools: novelty_report, the blind A/B harness (create, judge,
//! rate, reveal) and author_midi (the AI-authored MIDI path into a plan).

use crate::blind_ab;
use crate::novelty::{self, Fingerprint};
use crate::project::Note;
use crate::tools::{b_or, obj, s_opt, s_req, Tool};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

fn current_fp(e: &mut crate::engine::Engine, with_audio: bool) -> Result<Fingerprint> {
    let mut f = novelty::fingerprint(&e.project);
    f.label = "current".into();
    if with_audio {
        let m = e.mix()?;
        let (c, s) = novelty::audio_summary(&m.left, &m.right, f.key_pc);
        f.chroma = c;
        f.spectrum = s;
    }
    Ok(f)
}

fn novelty_report(e: &mut crate::engine::Engine, a: &Value) -> Result<Value> {
    let audio = b_or(a, "audio", true);
    let mut o = novelty::NoveltyOpts::default();
    if let Some(p) = s_opt(a, "history_path") {
        o.history = Some(e.resolve(&p));
    }
    if let Some(t) = a["threshold"].as_f64() {
        o.threshold = t as f32;
    }
    if let Some(w) = a["window"].as_u64() {
        o.window = w as usize;
    }
    let mut f = match s_opt(a, "project") {
        Some(p) => novelty::fingerprint_file(&e.resolve(&p), audio)?,
        None => current_fp(e, b_or(a, "render", false))?,
    };
    if let Some(l) = s_opt(a, "label") {
        f.label = l;
    }
    let hp = novelty::history_path(e, &o);
    let hist = novelty::load_history(&hp, o.window);
    let near = novelty::nearest(&f, &hist);
    let mut out = json!({
        "fingerprint": novelty::summary(&f),
        "history": hp.to_string_lossy(), "history_entries": hist.len(), "threshold": o.threshold,
        "nearest": near.as_ref().map(|(h, d)| json!({"label": h.label, "id": h.id, "genre": h.genre, "seed": h.seed, "distance": novelty::round_d(d)})),
        "verdict": match &near {
            None => "no history to compare against".to_string(),
            Some((_, d)) if d.total < o.threshold => format!("too similar ({:.3} < {:.3}): a near-repeat of a recent beat", d.total, o.threshold),
            Some((_, d)) => format!("novel enough ({:.3} >= {:.3})", d.total, o.threshold),
        },
        "note": "Novelty only shows a beat is not a repeat; it is not a quality score. Blind listening (blind_ab_*) is the quality test.",
    });
    if let Some(list) = a["compare"].as_array() {
        let mut fps = vec![f.clone()];
        for x in list {
            let p = x
                .as_str()
                .ok_or_else(|| anyhow!("compare items are paths"))?;
            fps.push(novelty::fingerprint_file(&e.resolve(p), audio)?);
        }
        if a["include_self"].as_bool() == Some(false) {
            fps.remove(0);
        }
        out["matrix"] = novelty::matrix(&fps);
    }
    if b_or(a, "add_to_history", false) {
        novelty::append_history(&hp, &f)?;
        out["added_to_history"] = json!(true);
    }
    Ok(out)
}

fn parse_notes(v: &Value) -> Result<Vec<Note>> {
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("notes must be an array of {{start, len, pitch, vel}}"))?;
    let mut out = Vec::new();
    for n in arr {
        let start = n["start"]
            .as_f64()
            .ok_or_else(|| anyhow!("note without start"))? as f32;
        let len = n["len"].as_f64().unwrap_or(1.0) as f32;
        let pitch = match &n["pitch"] {
            Value::Number(x) => x.as_u64().unwrap_or(60) as u8,
            Value::String(s) => crate::theory::parse_note(s)?,
            _ => bail!("note without pitch"),
        };
        let vel = n["vel"].as_f64().unwrap_or(0.8) as f32;
        if start < 0.0 || len <= 0.0 || pitch > 127 {
            bail!("bad note {n}");
        }
        let mut note = Note::new(start, len, pitch, vel.clamp(0.0, 1.0));
        if let Some(s) = n["slide_to"].as_u64() {
            note.slide_to = Some(s.min(127) as u8);
        }
        out.push(note);
    }
    Ok(out)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "novelty_report",
            description: "Compare a beat against Beatbox's recent outputs (novelty history) and, optionally, against other projects: drum onset grids per voice with genre conventions masked out, hat subdivision profile, transposition-invariant melodic contour (interval bigrams + shape), functional harmony (bass roots relative to the key), palette, section map, tempo/key and a key-relative chroma/spectrum summary. Returns the nearest beat with per-component distances, a verdict against the threshold, and with 'compare' a pairwise matrix. Novelty is not quality.",
            mutates: false,
            schema: || obj(json!({
                "project": {"type": "string", "description": "Project json (or a folder holding one, e.g. a produce_track out_dir); default the current project. Audio next to it (same stem .mp3/.wav) feeds the audio summary"},
                "compare": {"type": "array", "items": {"type": "string"}, "description": "Other project paths/folders for a pairwise matrix"},
                "include_self": {"type": "boolean", "description": "Put 'project' first in the matrix (default true)"},
                "audio": {"type": "boolean", "description": "Use the audio files next to projects (default true)"},
                "render": {"type": "boolean", "description": "For the current project: render it for the audio summary (default false)"},
                "history_path": {"type": "string"}, "threshold": {"type": "number"}, "window": {"type": "integer"},
                "label": {"type": "string"}, "add_to_history": {"type": "boolean"},
            }), &[]),
            run: novelty_report,
        },
        Tool {
            name: "blind_ab_create",
            description: "Creative critic, step 1: build a BLIND A/B session from 2-8 renders. Loudness-matches them (same LUFS, true peak under -1 dBTP), writes plain WAVs labelled A, B, ... in shuffled order with a rubric (identity, groove, development, memorability, emotion, production). No seeds, configs or generator names go into the session folder; the key is stored beside it and only blind_ab_reveal reads it.",
            mutates: false,
            schema: || obj(json!({
                "files": {"type": "array", "items": {"type": "string"}, "description": "Audio files (mp3/wav/flac)"},
                "names": {"type": "array", "items": {"type": "string"}, "description": "Private names for the key (default: file names)"},
                "out_dir": {"type": "string", "description": "Where sessions go (default blind_ab/)"},
                "target_lufs": {"type": "number", "description": "default -14 (lowered if a file would exceed -1 dBTP)"},
            }), &["files"]),
            run: |e, a| {
                let files: Vec<String> = a["files"].as_array().map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                let names: Vec<String> = a["names"].as_array().map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                let items: Vec<(std::path::PathBuf, String)> = files
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (e.resolve(f), names.get(i).cloned().unwrap_or_else(|| f.rsplit('/').next().unwrap_or(f).to_string())))
                    .collect();
                let out = e.resolve(&s_opt(a, "out_dir").unwrap_or_else(|| "blind_ab".into()));
                blind_ab::create(&out, &items, a["target_lufs"].as_f64().unwrap_or(-14.0) as f32)
            },
        },
        Tool {
            name: "blind_ab_rate",
            description: "Creative critic, human path: record a listener's BLIND preference and notes for a session (e.g. Naman's). preference = a label or 'tie'; notes = {label: {criterion: text}} with criteria identity, groove, development, memorability, emotion, production (or other). Does not reveal which beat is which.",
            mutates: false,
            schema: || obj(json!({
                "session": {"type": "string"}, "rater": {"type": "string"},
                "preference": {"type": "string"}, "strength": {"type": "integer", "description": "1 slight .. 3 strong"},
                "notes": {"type": "object"}, "comment": {"type": "string"},
            }), &["session", "preference"]),
            run: |e, a| {
                let d = blind_ab::session_dir(e, a)?;
                blind_ab::rate(&d, a)
            },
        },
        Tool {
            name: "blind_ab_judge",
            description: "Creative critic, model path: ask a pluggable listening-capable backend to judge a blind session with the same rubric. backend 'command' runs BEATBOX_AB_JUDGE_CMD (or 'command') with the session folder and reads JSON {preference, notes, comment}; 'stub' (the default when nothing is configured) explains how to plug one in. No paid API is called.",
            mutates: false,
            schema: || obj(json!({"session": {"type": "string"}, "backend": {"type": "string", "enum": ["stub", "command"]}, "command": {"type": "string"}}), &["session"]),
            run: |e, a| {
                let d = blind_ab::session_dir(e, a)?;
                blind_ab::judge(&d, a["backend"].as_str(), s_opt(a, "command"))
            },
        },
        Tool {
            name: "blind_ab_reveal",
            description: "Creative critic, last step: join every blind rating and model judgment to its source and turn the notes into revision hints (per beat, per criterion). Writes <session>.feedback.json beside the session.",
            mutates: false,
            schema: || obj(json!({"session": {"type": "string"}}), &["session"]),
            run: |e, a| {
                let d = blind_ab::session_dir(e, a)?;
                blind_ab::reveal(&d)
            },
        },
        Tool {
            name: "author_midi",
            description: "AI-authored MIDI: put your own notes into a producer plan. section = a section name (hook1), a kind (hook) or '*'; role = kick, snare, hat, open_hat, perc, bass, harmony, lead, counter, texture, tabla, bayan; notes = [{start (16th steps from the section start), len, pitch (MIDI or 'C4'), vel, slide_to?}]. The composer plays them verbatim in place of the generated part (generated parts fill the rest) and the plan's method becomes ai_authored. Then apply_plan, or pass the same object as 'authored' to produce_track.",
            mutates: false,
            schema: || obj(json!({"plan": {"type": "object"}, "section": {"type": "string"}, "role": {"type": "string"}, "notes": {"type": "array", "items": {"type": "object"}}}), &["plan", "section", "role", "notes"]),
            run: |_, a| {
                let mut plan = crate::producer::plan_from_value(&a["plan"])?;
                let section = s_req(a, "section")?;
                let role = s_req(a, "role")?;
                if section != "*" && !plan.sections.iter().any(|s| s.name == section || s.kind == section) {
                    bail!("no section or kind '{section}' in the plan ({})", plan.sections.iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(", "));
                }
                if !plan.palette.contains_key(&role) && role != "harmony" {
                    bail!("role '{role}' has no sound in this plan's palette ({})", plan.palette.keys().cloned().collect::<Vec<_>>().join(", "));
                }
                let notes = parse_notes(&a["notes"])?;
                let n = notes.len();
                plan.authored.entry(section.clone()).or_default().insert(role.clone(), notes);
                plan.method = "ai_authored".into();
                plan.decisions.push(crate::creative::Decision { level: "method".into(), what: "authored part".into(), choice: format!("{section}:{role} ({n} notes)"), why: "written by an agent through MCP".into() });
                Ok(json!({"plan": plan, "authored": format!("{section}:{role}"), "notes": n, "next": "apply_plan {plan}"}))
            },
        },
    ]
}
