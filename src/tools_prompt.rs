//! make_beat: the forgiving one-call beat tool. A free-text prompt and/or
//! lyrics become a producer plan that produce_track builds, listens to and
//! revises. analyze_lyrics shows how the lyrics were read.

use crate::engine::Engine;
use crate::prompt_beat::{analyze_lyrics, parse_prompt};
use crate::tools::{find, obj, s_opt, u_or, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

fn make_beat(e: &mut Engine, a: &Value) -> Result<Value> {
    let prompt = s_opt(a, "prompt").unwrap_or_default();
    let lyrics = s_opt(a, "lyrics").unwrap_or_default();
    if prompt.trim().is_empty() && lyrics.trim().is_empty() {
        bail!("say what you want: prompt (e.g. 'Bohemia type beat with a beat switch') and/or lyrics");
    }
    let p = parse_prompt(&prompt);
    let lr = (!lyrics.trim().is_empty()).then(|| analyze_lyrics(&lyrics));
    let mut reasons = p.reasons.clone();
    let mut args = Map::new();
    let brief = if prompt.is_empty() { format!("a beat for these {} lyrics", lr.as_ref().map(|l| l.delivery.as_str()).unwrap_or("")) } else { prompt.clone() };
    args.insert("brief".into(), json!(brief));
    // the prompt wins over what the lyrics suggest; lyrics fill the gaps
    let genre = p.genre.clone().or_else(|| lr.as_ref().map(|l| l.genre.clone()));
    if let Some(g) = &genre {
        args.insert("genre".into(), json!(g));
    }
    let bpm = p.bpm.or_else(|| lr.as_ref().map(|l| l.bpm));
    if let Some(b) = bpm {
        args.insert("bpm".into(), json!(b));
    }
    if let Some(k) = &p.key {
        args.insert("key".into(), json!(k));
    }
    if let Some(s) = &p.scale {
        args.insert("scale".into(), json!(s));
    }
    let mood = p.mood.clone().or_else(|| lr.as_ref().map(|l| l.mood.clone()));
    let mut intent = Map::new();
    if let Some(m) = &mood {
        args.insert("mood".into(), json!(m));
        intent.insert("mood".into(), json!(m));
    }
    let mut contrasts = p.contrasts.clone();
    let mut duration = p.duration_s;
    if let Some(l) = &lr {
        reasons.extend(l.reasons.iter().cloned());
        // rap leaves space for the flow; sung lyrics get fuller harmony
        if l.delivery == "rap" {
            intent.insert("density".into(), json!(p.density.clone().unwrap_or_else(|| "sparse".into())));
            intent.insert("hero".into(), json!("groove"));
        } else {
            intent.insert("density".into(), json!(p.density.clone().unwrap_or_else(|| "balanced".into())));
            intent.insert("hero".into(), json!("motif"));
        }
        if duration.is_none() {
            let bar_s = 240.0 / bpm.unwrap_or(l.bpm);
            duration = Some((l.total_bars as f32 * bar_s).clamp(30.0, 300.0));
        }
        if l.sections.iter().filter(|s| s.kind == "hook").count() >= 1 && !contrasts.iter().any(|c| c == "sparse_to_dense") {
            contrasts.push("sparse_to_dense".into());
        }
        // the arrangement follows the words: intro, one section per stanza, outro
        if l.sections.len() >= 2 && p.duration_s.is_none() {
            let mut form = vec![json!({"kind": "intro", "bars": 4})];
            for s in &l.sections {
                form.push(json!({"kind": s.kind, "bars": s.bars}));
            }
            form.push(json!({"kind": "outro", "bars": 4}));
            reasons.push(format!(
                "arrangement from the lyrics: intro 4 > {} > outro 4",
                l.sections.iter().map(|s| format!("{} {}", s.kind, s.bars)).collect::<Vec<_>>().join(" > ")
            ));
            intent.insert("form".into(), json!(form));
        }
    } else if let Some(d) = &p.density {
        intent.insert("density".into(), json!(d));
    }
    if let Some(en) = p.energy {
        intent.insert("energy".into(), json!(en));
    }
    // the quiet section goes in before the switch is placed (the switch comes out of it)
    contrasts.sort_by_key(|c| match c.as_str() {
        "quiet_section" => 0,
        "beat_switch" => 1,
        _ => 2,
    });
    intent.insert("target_lufs".into(), json!(a["target_lufs"].as_f64().unwrap_or(-14.0)));
    if !contrasts.is_empty() {
        intent.insert("contrasts".into(), json!(contrasts));
    }
    if !p.feel.is_empty() {
        let allowed = ["half_time", "backbeat", "triplet", "swing", "straight", "bounce", "driving", "rolling", "sparse_hats"];
        let f: Vec<&String> = p.feel.iter().filter(|x| allowed.contains(&x.as_str())).collect();
        if !f.is_empty() {
            intent.insert("rhythmic_feel".into(), json!(f));
        }
    }
    if !p.palette.is_empty() {
        intent.insert("palette".into(), Value::Object(p.palette.clone()));
    }
    args.insert("intent".into(), Value::Object(intent));
    if let Some(d) = duration {
        args.insert("duration_s".into(), json!(d.round()));
    }
    for k in ["seed", "out_dir", "max_iterations", "reference_path"] {
        if let Some(v) = a.get(k) {
            args.insert(k.into(), v.clone());
        }
    }
    if !args.contains_key("max_iterations") {
        args.insert("max_iterations".into(), json!(u_or(a, "quality", 2).clamp(1, 6)));
    }
    let run = find("produce_track").expect("produce_track").run;
    let res = run(e, &Value::Object(args.clone()))?;
    // compact answer for small models; the full producer result rides under "details"
    let summary = json!({
        "genre": genre.clone().unwrap_or_else(|| e.project.name.clone()),
        "bpm": e.project.bpm,
        "key": format!("{} {}", e.project.key_root, e.project.scale),
        "seconds": (e.project.song_seconds() * 10.0).round() / 10.0,
        "sections": e.project.song_sections().iter().map(|s| s.pattern.clone()).collect::<Vec<_>>(),
    });
    Ok(json!({
        "summary": summary,
        "how_i_read_it": reasons,
        "lyrics": lr,
        "plan_args": args,
        "files": res.get("files").cloned().unwrap_or(Value::Null),
        "score": res.get("score").cloned().or_else(|| res.get("best").cloned()).unwrap_or(Value::Null),
        "next": ["export_audio {path:'song.mp3'} to save it", "make_beat again with a different seed for another take", "set_mixer / generate_drums {pattern} to change a part"],
        "details": res,
    }))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "make_beat",
            description: "One call, finished beat. prompt: what you want in plain words ('Bohemia type beat with a beat switch and a quiet section', 'dark UK drill 144 bpm with piano'). lyrics: optional text; rap vs sung, mood and hooks shape the beat. Artist references, beat switches, quiet parts, drops, half-time, key changes, instruments, BPM, key and length are understood. Built, mixed, mastered and checked by the producer.",
            mutates: true,
            schema: || obj(json!({"prompt": {"type": "string"}, "lyrics": {"type": "string"}, "seed": {"type": "integer"}, "out_dir": {"type": "string"}, "quality": {"type": "integer", "description": "1-6 listen/revise rounds (default 2)"}, "target_lufs": {"type": "number", "description": "delivery loudness (default -14, streaming)"}}), &[]),
            run: make_beat,
        },
        Tool {
            name: "analyze_lyrics",
            description: "Read lyrics (text): rap or sung, syllables per line, rhyme density, mood, sections with the hook found from repeats, bar counts, and the genre/BPM they suggest.",
            mutates: false,
            schema: || obj(json!({"lyrics": {"type": "string"}}), &["lyrics"]),
            run: |_, a| Ok(serde_json::to_value(analyze_lyrics(&s_opt(a, "lyrics").unwrap_or_default()))?),
        },
        Tool {
            name: "read_prompt",
            description: "Show how a beat prompt is understood (genre, BPM, key, mood, contrasts like beat_switch/drum_dropout, instruments, length) without building anything.",
            mutates: false,
            schema: || obj(json!({"prompt": {"type": "string"}}), &["prompt"]),
            run: |_, a| Ok(serde_json::to_value(parse_prompt(&s_opt(a, "prompt").unwrap_or_default()))?),
        },
    ]
}
