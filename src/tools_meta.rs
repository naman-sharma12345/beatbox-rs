//! MCP ergonomics for AI callers (small models first): tools grouped by
//! category, describe_tool with a ready-to-send example, a one-screen project
//! summary with next steps, and "did you mean" for unknown tool names.

use crate::engine::Engine;
use crate::tools::{self, obj, s_opt, s_req, Tool};
use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;

const DISCOVERY: [&str; 3] = ["get_guide", "list_presets", "list_tools"];

/// (category, one-line purpose) per tool module, in the order list_tools shows them.
fn modules() -> Vec<(&'static str, &'static str, Vec<Tool>)> {
    vec![
        ("start", "one call does it all: make_beat from a prompt or lyrics, produce_song from a recording", crate::tools_prompt::tools()),
        ("discover", "learn the engine: guide, catalog, describe a tool", { let mut v: Vec<Tool> = tools::core_tools().into_iter().filter(|t| DISCOVERY.contains(&t.name)).collect(); v.extend(tools()); v }),
        ("project", "new/load/save, tempo, key, tracks, notes, effects, undo/redo, snapshots", tools::core_tools().into_iter().filter(|t| !DISCOVERY.contains(&t.name)).collect()),
        ("studio", "transport, history, screenshot, live studio", crate::tools_studio::tools()),
        ("notes", "piano roll and MIDI: write, edit, quantize, arpeggiate, strum", crate::tools_midi::tools()),
        ("sound", "instruments and sound design", crate::tools_sound::tools()),
        ("compose", "generators and song structure", crate::tools_compose::tools()),
        ("listen", "ears: analysis, loudness, masking, spectrogram", { let mut v = crate::tools_ears::tools(); v.extend(crate::tools_listen::tools()); v.extend(crate::tools_ears_pro::tools()); v }),
        ("mix", "levels, balance, routing, buses", crate::tools_mix::tools()),
        ("fx", "effects: add, tweak, place, compare", crate::tools_fx2::tools()),
        ("playlist", "FL-style playlist, automation clips, patterns", { let mut v = crate::tools_parity::tools(); v.extend(crate::tools_fl::tools()); v }),
        ("produce", "the producer loop: plan, critique, revise", crate::tools_producer::tools()),
        ("creative", "variations, wildcards, contrast", crate::tools_creative::tools()),
        ("samples", "palettes, kits, sample search and flips", crate::tools_palette::tools()),
        ("vocal", "vocals: transcribe, tune, vocal_to_song, audio clips", crate::tools_vocal::tools()),
        ("groove", "groove extraction and drum tuning", crate::tools_groove::tools()),
        ("export", "render to wav/flac/mp3, stems, mastering", crate::tools_delivery::tools()),
    ]
}

fn cat_index() -> &'static BTreeMap<&'static str, &'static str> {
    static C: OnceLock<BTreeMap<&'static str, &'static str>> = OnceLock::new();
    C.get_or_init(|| {
        let mut m = BTreeMap::new();
        for (c, _, ts) in modules() {
            for t in ts {
                m.entry(t.name).or_insert(c);
            }
        }
        m
    })
}

/// The category a tool belongs to ("other" when a module is not mapped yet).
pub fn category(name: &str) -> &'static str {
    cat_index().get(name).copied().unwrap_or("other")
}

/// First sentence of a description, at most ~110 chars (compact listings).
pub fn short(desc: &str) -> String {
    let end = desc.find(". ").map(|i| i + 1).unwrap_or(desc.len());
    let s = &desc[..end];
    if s.chars().count() > 110 {
        let cut: String = s.chars().take(107).collect();
        format!("{cut}...")
    } else {
        s.to_string()
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for j in 0..b.len() {
            let cost = if ca == b[j] { 0 } else { 1 };
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Closest tool names to a misspelled or invented one.
pub fn suggest(name: &str, n: usize) -> Vec<&'static str> {
    let q = name.to_lowercase().replace(['-', ' '], "_");
    let mut scored: Vec<(usize, &'static str)> = tools::registry()
        .iter()
        .map(|t| {
            let mut d = levenshtein(&q, t.name);
            // shared words count: "make_song" ~ "make_beat", "eq" ~ "add_effect"
            if q.split('_').any(|w| w.len() > 2 && t.name.contains(w)) {
                d = d.saturating_sub(3);
            }
            (d, t.name)
        })
        .collect();
    scored.sort();
    scored.into_iter().take(n).map(|(_, s)| s).collect()
}

/// An example value for one JSON-schema property.
fn example_for(key: &str, p: &Value) -> Value {
    if let Some(d) = p.get("default") {
        return d.clone();
    }
    if let Some(e) = p.get("enum").and_then(|e| e.as_array()).and_then(|e| e.first()) {
        return e.clone();
    }
    if let Some(Value::Array(ex)) = p.get("examples") {
        if let Some(x) = ex.first() {
            return x.clone();
        }
    }
    let ty = match p.get("type") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).find(|s| *s != "null").unwrap_or("string").to_string(),
        _ => if p.get("oneOf").is_some() || p.get("anyOf").is_some() { "string".into() } else { "string".into() },
    };
    match ty.as_str() {
        "integer" | "number" => {
            let lo = p.get("minimum").and_then(|v| v.as_f64());
            let hi = p.get("maximum").and_then(|v| v.as_f64());
            let v = match (lo, hi) {
                (Some(a), Some(b)) => (a + b) / 2.0,
                (Some(a), None) => a.max(1.0),
                (None, Some(b)) => b.min(1.0),
                _ => match key {
                    "bpm" => 90.0,
                    "seed" => 7.0,
                    k if k.contains("db") => -6.0,
                    k if k.contains("bars") => 4.0,
                    _ => 1.0,
                },
            };
            if ty == "integer" { json!(v.round() as i64) } else { json!(v) }
        }
        "boolean" => json!(true),
        "array" => json!([]),
        "object" => json!({}),
        _ => json!(match key {
            "track" => "drums",
            "pattern" => "A",
            "prompt" => "dark 90 BPM hip-hop beat with a beat switch",
            "lyrics" => "line one of the verse\nline two of the verse",
            "path" => "take.wav",
            "key" => "C",
            "scale" => "minor",
            "name" => "my_name",
            _ => "...",
        }),
    }
}

/// A minimal call: every required argument, plus one useful optional one.
pub fn example_call(t: &Tool) -> Value {
    let s = (t.schema)();
    let props = s["properties"].as_object().cloned().unwrap_or_default();
    let req: Vec<String> = s["required"].as_array().map(|r| r.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let mut args = Map::new();
    for k in &req {
        if let Some(p) = props.get(k) {
            args.insert(k.clone(), example_for(k, p));
        }
    }
    if req.is_empty() {
        for k in ["prompt", "track", "path", "bpm", "detail"] {
            if let Some(p) = props.get(k) {
                args.insert(k.to_string(), example_for(k, p));
                break;
            }
        }
    }
    json!({"tool": t.name, "args": Value::Object(args)})
}

/// What a caller usually does next after a tool (compact hints, never required).
pub fn next_steps(name: &str) -> &'static [&'static str] {
    match name {
        "make_beat" | "produce_track" | "produce_song" | "generate_beat" => &["critique_track", "export_audio", "get_project_summary"],
        "new_project" => &["make_beat", "generate_drums", "set_tempo"],
        "load_project" => &["get_project_summary", "render"],
        "generate_drums" => &["generate_bassline", "generate_chords"],
        "generate_bassline" => &["generate_chords", "generate_melody"],
        "generate_chords" => &["generate_melody", "add_effect"],
        "generate_melody" => &["build_structure", "render"],
        "build_structure" | "set_arrangement" => &["render", "analyze_mix"],
        "render" => &["analyze_mix", "export_audio"],
        "analyze_mix" | "ears_report" | "critique_track" => &["revise_track", "balance_mix", "export_audio"],
        "add_effect" | "tweak_effect" => &["render_preview", "ab_compare"],
        "tune_vocal" | "transcribe_lyrics" | "analyze_vocal" => &["vocal_to_song"],
        "vocal_to_song" => &["critique_track", "export_audio"],
        _ => &[],
    }
}

pub fn list(a: &Value) -> Result<Value> {
    let want = s_opt(a, "category").map(|c| c.to_lowercase());
    let search = s_opt(a, "search").map(|c| c.to_lowercase());
    let mut cats: Map<String, Value> = Map::new();
    let mut about: Map<String, Value> = Map::new();
    let mut n = 0;
    for (c, purpose, _) in modules() {
        about.insert(c.into(), json!(purpose));
    }
    for t in tools::registry() {
        let c = category(t.name);
        if want.as_deref().is_some_and(|w| w != c) {
            continue;
        }
        if let Some(q) = &search {
            if !t.name.contains(q.as_str()) && !t.description.to_lowercase().contains(q.as_str()) {
                continue;
            }
        }
        n += 1;
        let e = cats.entry(c.to_string()).or_insert_with(|| json!([]));
        e.as_array_mut().unwrap().push(json!({"name": t.name, "does": short(t.description)}));
    }
    if let Some(w) = &want {
        if n == 0 {
            let names: Vec<&String> = about.keys().collect();
            return Err(anyhow!("no category '{w}'. Categories: {}", names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
        }
    }
    Ok(json!({
        "count": n,
        "total_tools": tools::registry().len(),
        "categories": about,
        "tools": cats,
        "tip": "describe_tool {name} gives the arguments and a ready example. New here? make_beat {prompt} makes a finished beat in one call.",
    }))
}

pub fn describe(name: &str) -> Result<Value> {
    let t = tools::find(name).ok_or_else(|| {
        anyhow!("unknown tool '{name}'. Did you mean: {}? (list_tools shows all)", suggest(name, 3).join(", "))
    })?;
    let s = (t.schema)();
    let props = s["properties"].as_object().cloned().unwrap_or_default();
    let req: Vec<&str> = s["required"].as_array().map(|r| r.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
    let args: Vec<Value> = props
        .iter()
        .map(|(k, p)| {
            let mut o = json!({"name": k, "required": req.contains(&k.as_str())});
            let ty = p.get("type").cloned().unwrap_or(json!(if p.get("enum").is_some() { "enum" } else { "any" }));
            o["type"] = ty;
            for f in ["description", "enum", "default", "minimum", "maximum"] {
                if let Some(v) = p.get(f) {
                    o[f] = v.clone();
                }
            }
            o
        })
        .collect();
    let c = category(t.name);
    let related: Vec<&str> = tools::registry().iter().filter(|x| x.name != t.name && category(x.name) == c).take(6).map(|x| x.name).collect();
    Ok(json!({
        "name": t.name,
        "category": c,
        "description": t.description,
        "changes_project": t.mutates,
        "args": args,
        "example": example_call(t),
        "next": next_steps(t.name),
        "related": related,
    }))
}

pub fn project_summary(e: &Engine) -> Value {
    let p = &e.project;
    let notes: usize = p.patterns.iter().map(|pt| pt.clips.values().map(|v| v.len()).sum::<usize>()).sum();
    let tracks: Vec<String> = p
        .tracks
        .iter()
        .map(|t| {
            let mut s = format!("{} ({}, {:+.1} dB", t.name, t.instrument.kind_name(), t.volume_db);
            if !t.effects.is_empty() {
                s.push_str(&format!(", {} fx", t.effects.len()));
            }
            if t.mute {
                s.push_str(", muted");
            }
            s.push(')');
            s
        })
        .collect();
    let mut next: Vec<&str> = Vec::new();
    if p.tracks.is_empty() {
        next.extend(["make_beat {prompt}", "generate_beat {style}"]);
    } else if notes == 0 {
        next.extend(["generate_drums", "generate_chords"]);
    } else if p.song_sections().len() <= 1 {
        next.extend(["build_structure", "render"]);
    } else {
        next.extend(["critique_track", "export_audio {path}"]);
    }
    json!({
        "name": p.name,
        "tempo_key": format!("{:.0} BPM, {} {}", p.bpm, p.key_root, p.scale),
        "seconds": (p.song_seconds() * 10.0).round() / 10.0,
        "tracks": tracks,
        "patterns": p.patterns.iter().map(|pt| format!("{} ({} bars)", pt.name, pt.bars)).collect::<Vec<_>>(),
        "song": p.song_sections().iter().map(|s| format!("{} x{}", s.pattern, s.repeats)).collect::<Vec<_>>().join(" > "),
        "notes": notes,
        "audio_clips": p.audio_clips.len(),
        "snapshots": e.snapshots.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
        "undo_steps": e.undo_depth().0,
        "revision": e.revision,
        "next": next,
    })
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "describe_tool",
            description: "One tool in full: what it does, every argument (type, range, default, required), a ready-to-send example call, what to call next, related tools. Use before calling a tool you have not used.",
            mutates: false,
            schema: || obj(json!({"name": {"type": "string", "description": "Tool name, e.g. make_beat"}}), &["name"]),
            run: |_, a| describe(&s_req(a, "name")?),
        },
        Tool {
            name: "get_project_summary",
            description: "The project on one screen: tempo/key, length, tracks (instrument, level, fx), patterns, song order, note count, snapshots, undo depth, and the next tools to call. Cheaper than get_project for small models.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| Ok(project_summary(e)),
        },
        Tool {
            name: "suggest_tools",
            description: "Plain-words goal in, the best tools out (e.g. 'make the vocal louder', 'add a riser', 'export mp3'). Returns names with one line each and an example call for the top match.",
            mutates: false,
            schema: || obj(json!({"goal": {"type": "string", "description": "What you want to do, in plain words"}, "limit": {"type": "integer", "minimum": 1, "maximum": 10, "default": 5}}), &["goal"]),
            run: |_, a| {
                let goal = s_req(a, "goal")?.to_lowercase();
                let lim = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
                let words: Vec<String> = goal
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| w.len() > 2 && !["the", "and", "make", "with", "for", "add", "more", "this", "that", "into", "from", "want"].contains(w))
                    .map(|w| w.trim_end_matches('s').to_string())
                    .collect();
                let mut scored: Vec<(i32, &Tool)> = tools::registry()
                    .iter()
                    .map(|t| {
                        let d = t.description.to_lowercase();
                        let mut s = 0;
                        for w in &words {
                            if t.name.contains(w.as_str()) { s += 5; }
                            if d.contains(w.as_str()) { s += 2; }
                        }
                        if goal.contains("beat") && t.name == "make_beat" { s += 4; }
                        if (goal.contains("export") || goal.contains("mp3") || goal.contains("wav")) && t.name == "export_audio" { s += 6; }
                        (s, t)
                    })
                    .filter(|(s, _)| *s > 0)
                    .collect();
                scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(b.1.name)));
                let top: Vec<Value> = scored.iter().take(lim).map(|(_, t)| json!({"name": t.name, "does": short(t.description), "category": category(t.name)})).collect();
                let example = scored.first().map(|(_, t)| example_call(t)).unwrap_or(json!(null));
                Ok(json!({"goal": goal, "tools": top, "example": example, "tip": if top.is_empty() { "nothing matched; try list_tools {category}" } else { "describe_tool {name} for the full arguments" }}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool's schema is something a small model can fill in.
    #[test]
    fn schema_lint() {
        let mut seen = std::collections::HashSet::new();
        let mut bad = Vec::new();
        for t in tools::registry() {
            if !seen.insert(t.name) { bad.push(format!("{}: duplicate name", t.name)); }
            if !t.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                bad.push(format!("{}: name not snake_case", t.name));
            }
            if t.description.trim().len() < 15 { bad.push(format!("{}: description too short", t.name)); }
            if t.description.len() > 1200 { bad.push(format!("{}: description over 1200 chars ({})", t.name, t.description.len())); }
            let s = (t.schema)();
            if s["type"] != "object" { bad.push(format!("{}: schema type is not object", t.name)); }
            let props = s["properties"].as_object().cloned().unwrap_or_default();
            if let Some(req) = s["required"].as_array() {
                for r in req {
                    let r = r.as_str().unwrap_or("");
                    if !props.contains_key(r) { bad.push(format!("{}: required '{r}' is not a property", t.name)); }
                }
            }
            for (k, p) in &props {
                let typed = ["type", "enum", "oneOf", "anyOf", "$ref", "const"].iter().any(|f| p.get(*f).is_some());
                if !typed { bad.push(format!("{}.{k}: no type", t.name)); }
            }
            if category(t.name) == "other" { bad.push(format!("{}: no category (add its module to tools_meta::modules)", t.name)); }
        }
        assert!(bad.is_empty(), "schema lint: {} problems:\n{}", bad.len(), bad.join("\n"));
    }

    #[test]
    fn every_example_parses_against_its_schema() {
        let mut e = Engine::new(std::env::temp_dir().join("bb_meta_ex"));
        // examples only need to pass argument validation (unknown keys, required)
        for t in tools::registry() {
            let ex = example_call(t);
            let args = &ex["args"];
            let s = (t.schema)();
            for k in args.as_object().unwrap().keys() {
                assert!(s["properties"].get(k).is_some(), "{}: example uses unknown '{k}'", t.name);
            }
        }
        let d = e.call("describe_tool", &json!({"name": "make_beat"})).unwrap();
        assert_eq!(d["category"], "start");
        assert!(d["example"]["args"]["prompt"].is_string());
    }

    #[test]
    fn listing_and_suggestions() {
        let mut e = Engine::new(std::env::temp_dir().join("bb_meta_ls"));
        let l = e.call("list_tools", &json!({})).unwrap();
        assert_eq!(l["count"], l["total_tools"]);
        assert!(l["tools"]["start"].as_array().unwrap().iter().any(|t| t["name"] == "make_beat"));
        let v = e.call("list_tools", &json!({"category": "vocal"})).unwrap();
        assert!(v["count"].as_u64().unwrap() > 2);
        assert!(e.call("list_tools", &json!({"category": "nope"})).is_err());
        let err = format!("{:#}", e.call("make_song", &json!({})).unwrap_err());
        assert!(err.contains("Did you mean"), "{err}");
        let err = format!("{:#}", e.call("describe_tool", &json!({"name": "add_efect"})).unwrap_err());
        assert!(err.contains("add_effect"), "{err}");
        let s = e.call("suggest_tools", &json!({"goal": "export an mp3"})).unwrap();
        assert_eq!(s["tools"][0]["name"], "export_audio");
        let ps = e.call("get_project_summary", &json!({})).unwrap();
        assert!(ps["next"].as_array().unwrap().len() >= 1);
    }
}
