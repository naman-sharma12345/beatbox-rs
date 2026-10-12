//! The MCP surface: a small, fixed set of CATEGORY tools.
//!
//! An AI client chooses far better from ~20 tools than from 300+, so
//! `tools/list` advertises one tool per category (project, pattern, notes,
//! mixer, effects, ...). Each takes an `action` (the operation) plus that
//! action's arguments, either inside `params` or flat next to `action`.
//! Every fine-grained engine tool is exactly one action of exactly one
//! category, and stays callable by its own name (`beatbox call <name>`,
//! `tools/call` with the old name, `beatbox mcp --legacy-tools`).
//!
//! Rule for the future: a new feature is a new ACTION in a category (add its
//! name to that category's list below), never a new top-level tool. Tests
//! fail if `tools/list` grows past [`MAX_TOOLS`] or if any engine tool is
//! missing from, or listed twice in, the table.

use crate::tools::{self, Tool};
use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

/// Hard ceiling on the advertised MCP tool count (tested).
pub const MAX_TOOLS: usize = 24;

/// The reserved action every category answers: the category's actions, or
/// one action's full schema and an example (`params.action`).
pub const HELP: &str = "help";

pub struct Category {
    pub name: &'static str,
    pub description: &'static str,
    pub actions: &'static [&'static str],
}

pub const CATEGORIES: &[Category] = &[
    Category {
        name: "project",
        description: "Song file and global settings: new/load/save, tempo, key, time signature, tracks, project summary, ids, validation.",
        actions: &["new_project", "get_project", "save_project", "load_project", "get_project_summary", "set_tempo", "set_key", "add_track", "remove_track", "clear", "lookup_id", "list_ids", "validate_project", "diff_project", "compare_variants"],
    },
    Category {
        name: "history",
        description: "Undo/redo, the edit history, snapshots and restore.",
        actions: &["undo", "redo", "history", "get_history", "snapshot", "list_snapshots", "restore_snapshot"],
    },
    Category {
        name: "transport",
        description: "Play/stop/seek, render or preview the song, metronome click, record audio input, studio screenshot.",
        actions: &["transport", "render", "render_preview", "play_audio", "export_click", "list_audio_inputs", "start_recording", "stop_recording", "record_audio", "screenshot"],
    },
    Category {
        name: "pattern",
        description: "Patterns and drums: step sequencer, drum generators, rolls, fills, ghost notes, groove, swing, humanize.",
        actions: &["add_pattern", "remove_pattern", "set_pattern_length", "set_steps", "toggle_step", "get_pattern", "generate_drums", "add_roll", "generate_fill", "generate_ghost_notes", "humanize", "apply_groove", "extract_groove", "steal_groove", "tune_drums_to_key"],
    },
    Category {
        name: "notes",
        description: "Piano roll: add/edit/delete notes, quantize, transpose, chords, arpeggios, strum, slides, riffs, scales and theory.",
        actions: &["add_notes", "edit_notes", "delete_notes", "copy_notes", "transpose", "quantize", "split_notes", "merge_notes", "legato", "articulate", "arpeggiate", "strum", "flip_notes", "chop_notes", "limit_notes", "roll_notes", "chord_voicing", "harmonize", "velocity_curve", "add_slides", "write_riff", "write_phrase", "scale_snap", "theory_scale", "theory_chords", "detect_key", "lfo_tool", "author_midi"],
    },
    Category {
        name: "midi",
        description: "MIDI files and controllers: import/export .mid, live MIDI input and events.",
        actions: &["import_midi", "export_midi", "midi_input", "midi_event"],
    },
    Category {
        name: "instrument",
        description: "Instruments and sound design: set/tweak a synth, recorded instrument library (search, fetch, load, preview), multisample packs, macros, resynthesis.",
        actions: &["set_instrument", "tweak_instrument", "describe_instrument", "set_parameters", "list_presets", "design_sound", "layer_instrument", "create_macro", "set_macro", "list_instruments", "search_instruments", "pick_instrument_for_role", "fetch_instrument", "load_instrument", "preview_instrument", "audition_instrument", "list_installed", "remove_instrument", "install_instrument_pack", "add_multisample_track", "resynthesize_sample", "analyze_partials", "additive_sample", "match_timbre", "cache_limit_mb"],
    },
    Category {
        name: "sample",
        description: "Samples: search/download/import audio (Freesound, any URL), sample tracks, slice/stretch/edit, kits, palettes, flips, chops, stem separation, credits.",
        actions: &["search_samples", "download_sample", "import_sample", "list_samples", "add_sample_track", "freesound_search", "freesound_fetch", "import_from_url", "import_from_page", "list_imports", "export_credits", "slice_sample", "stretch_sample", "edit_sample", "denoise_sample", "sample_spectrogram", "spectral_edit_sample", "install_kit", "index_samples", "find_samples", "flip_sample", "vocal_chop", "list_palettes", "apply_palette", "install_palette_samples", "audition_palette", "separate_stems"],
    },
    Category {
        name: "arrange",
        description: "Song structure and playlist: arrangement, place patterns, clips (move/split/trim/fade), sections, transitions, markers, tempo changes, performance mode.",
        actions: &["set_arrangement", "build_structure", "vary_section", "add_transition", "set_section_mix", "place_pattern", "arrangement_to_playlist", "list_playlist", "playlist_clip", "remove_clip", "clear_playlist", "split_clip", "trim_clip", "move_clip", "duplicate_clip", "slip_clip", "fade_clip", "crossfade_clips", "consolidate_clips", "ripple_delete", "add_marker", "list_markers", "remove_marker", "add_tempo_point", "tempo_ramp", "list_tempo", "remove_tempo_point", "set_time_signature", "perform"],
    },
    Category {
        name: "automation",
        description: "Automation: draw or generate parameter curves (filter sweeps, risers, fades, LFOs) and automation clips.",
        actions: &["add_automation", "set_automation_points", "clear_automation", "list_automation", "generate_automation", "create_automation_clip", "place_automation_clip", "list_automation_clips"],
    },
    Category {
        name: "mixer",
        description: "Mixer: volume, pan, mute, buses, sends, routing, auto-balance, carve space, strip silence, drone notches.",
        actions: &["set_mixer", "add_bus", "remove_bus", "set_send", "route_track", "route_bus", "list_routing", "balance_mix", "level_hints", "carve_mix", "critique_mix", "notch_drones", "strip_silence"],
    },
    Category {
        name: "effects",
        description: "Effects: add/tweak/remove/bypass/reorder effects, FX chains, one-knob polish, A/B an effect.",
        actions: &["add_effect", "tweak_effect", "remove_effect", "get_effects", "bypass_effect", "reorder_effects", "describe_effect", "place_fx", "move_effect", "ab_compare", "list_fx_chains", "apply_fx_chain", "save_fx_chain", "copy_fx_chain", "soundgoodizer"],
    },
    Category {
        name: "vocals",
        description: "Vocals: add recordings as clips, tune, align to the grid, clean (denoise, de-hum, de-ess, gate), EQ and channel strip, edit, lyrics, vocal-to-song.",
        actions: &["add_audio_clip", "list_audio_clips", "remove_audio_clip", "vocal_to_song", "fit_beat_to_vocal", "tune_vocal", "get_vocal_map", "transcribe_lyrics", "analyze_vocal", "analyze_vocal_tone", "analyze_vocal_style", "detect_vocal_styles", "detect_vocal_onsets", "split_vocal_at_pauses", "align_vocal_to_grid", "align_take", "align_phrases", "place_vocal_phrases", "lock_vocal_to_beat", "measure_vocal_vs_beat", "learn_noise_profile", "reduce_noise", "denoise_voice", "remove_hum", "remove_clicks", "gate_vocal", "reduce_reverb", "extend_bandwidth", "exciter", "clarity_eq", "match_eq", "match_vocal_tone", "edit_vocal", "check_vocal_edits", "snap_clip_edges", "melodic_vocal_chain", "vocal_clean_chain", "eq_7band", "eq_1band", "expander_gate", "de_ess", "channel_strip", "score_voice_quality"],
    },
    Category {
        name: "analyze",
        description: "Listen and measure: mix analysis, loudness, spectrum, masking, stereo, punch, hooks, structure, artifacts, render diffs, blind A/B tests with a listener.",
        actions: &["analyze_mix", "analyze_track", "analyze_audio", "judge_audio", "auto_fix_mix", "analyze_sections", "ears_report", "loudness_report", "spectrum", "eq_curve", "render_spectrogram", "waveform_peaks", "masking_matrix", "stereo_image", "punch", "groove_analysis", "hook_analysis", "structure", "vocal_pocket", "detect_transients", "detect_artifacts", "diff_renders", "reference_match", "ab_listen", "novelty_report", "blind_ab_create", "blind_ab_rate", "blind_ab_judge", "blind_ab_reveal", "blind_ab_preferences"],
    },
    Category {
        name: "transcribe",
        description: "Write a recording down as MIDI: drums, bass, melody, chords, FX, arrangement map; identify and analyze a reference song.",
        actions: &["transcribe_drums", "transcribe_bass", "transcribe_melody", "transcribe_chords", "detect_fx", "arrangement_map", "score_sections", "arrange_transcription", "identify_song", "fetch_reference", "analyze_song"],
    },
    Category {
        name: "generate",
        description: "Make music in one call: make_beat from a prompt or lyrics, drill beats, full-song generators, chords, bass, melody, counter-melody, variations.",
        actions: &["make_beat", "make_drill_beat", "generate_beat", "generate_chords", "generate_bassline", "generate_melody", "counter_melody", "generate_variation", "read_prompt", "analyze_lyrics", "list_genres", "describe_genre"],
    },
    Category {
        name: "produce",
        description: "Whole songs. START HERE: song_create = one SongSpec in, finished checked song out; song_patch edits by JSON path. Also song from a recording, covers, reference songs, critique loop, QA.",
        actions: &["song_create", "song_patch", "song_get", "song_validate", "song_schema", "song_render", "song_critique", "song_improve", "produce_song", "cover_song", "make_song_from_reference", "make_song_from_vocal", "plan_cover", "qa_song", "produce_track", "plan_track", "apply_plan", "critique_track", "revise_track"],
    },
    Category {
        name: "export",
        description: "Master and export: wav/flac/mp3, stems, bounce a track, mastering assistant, check the master, compare to a reference.",
        actions: &["export_audio", "export_stems", "bounce_track", "master_assistant", "check_master", "analyze_reference", "compare_to_reference"],
    },
    Category {
        name: "help",
        description: "Guide and discovery: how to use this engine, search every action, describe one with an example, run several calls in one undo step.",
        actions: &["get_guide", "list_tools", "list_tool_groups", "search_tools", "describe_tool", "suggest_tools", "call_tool", "batch"],
    },
];

/// The category with this tool name.
pub fn find(name: &str) -> Option<&'static Category> {
    CATEGORIES.iter().find(|c| c.name == name)
}

/// The category an engine tool (action) belongs to.
pub fn category_of(action: &str) -> Option<&'static Category> {
    CATEGORIES.iter().find(|c| c.actions.contains(&action))
}

/// First sentence of a description, at most ~64 chars.
fn gist(desc: &str) -> String {
    let end = desc.find(". ").map(|i| i + 1).unwrap_or(desc.len());
    let s = desc[..end].trim_end_matches('.');
    if s.chars().count() <= 64 {
        return s.to_string();
    }
    let cut: String = s.chars().take(61).collect();
    let cut = cut.rsplit_once(' ').map(|(a, _)| a).unwrap_or(&cut);
    format!("{}…", cut.trim_end_matches([',', ';', ':', '(']))
}

/// Does this engine tool have its own `action` argument (transport,
/// perform, midi_input, playlist_clip)? In flat form it is passed as `op`.
fn has_own_action(t: &Tool) -> bool {
    (t.schema)().get("properties").and_then(|p| p.get("action")).is_some()
}

/// `name(required, [optional…]): what it does` - one line per action.
pub fn action_line(t: &Tool) -> String {
    let s = (t.schema)();
    let req: Vec<String> = s
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|x| if x == "action" { "op".to_string() } else { x.to_string() })).collect())
        .unwrap_or_default();
    let opt: Vec<String> = s
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|m| m.keys().filter(|k| !req.contains(k) && !(k.as_str() == "action" && req.iter().any(|r| r == "op"))).map(|k| if k == "action" { "op".to_string() } else { k.clone() }).collect())
        .unwrap_or_default();
    let mut parts = req.clone();
    if !opt.is_empty() {
        let more = if opt.len() > 4 { ", …" } else { "" };
        parts.push(format!("[{}{more}]", opt.iter().take(4).cloned().collect::<Vec<_>>().join(", ")));
    }
    format!("{}({}): {}", t.name, parts.join(", "), gist(t.description))
}

/// One category as an MCP tool.
pub fn tool_json(c: &Category) -> Value {
    let mut actions: Vec<Value> = c.actions.iter().map(|a| Value::from(*a)).collect();
    actions.push(Value::from(HELP));
    let lines: Vec<String> = c.actions.iter().filter_map(|a| tools::find(a)).map(action_line).collect();
    json!({
        "name": c.name,
        "description": c.description,
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": actions,
                    "description": format!("What to do. Arguments in (), optional in []:\n{}\nhelp(action): full schema and an example for one action.", lines.join("\n")),
                },
                "params": {
                    "type": "object",
                    "description": "The action's arguments, e.g. {\"action\":\"<name>\",\"params\":{...}}. Flat arguments next to action work too.",
                    "additionalProperties": true,
                },
            },
            "required": ["action"],
            "additionalProperties": true,
        },
    })
}

/// The `tools/list` array: the categories only.
pub fn tools_json() -> Value {
    Value::Array(CATEGORIES.iter().map(tool_json).collect())
}

/// The engine tool a call by `name` (a category or an engine tool) runs.
pub fn engine_tool(name: &str, args: &Value) -> Option<&'static Tool> {
    match find(name) {
        Some(c) => match resolve(c, args) {
            Ok(Resolved::Call(t, _)) | Ok(Resolved::Routed(t, _, _)) => tools::find(t),
            _ => None,
        },
        None => tools::find(name),
    }
}

/// What a category call resolves to.
pub enum Resolved {
    /// answered by the category itself (help / no action)
    Answer(Value),
    /// run this engine tool with these arguments
    Call(&'static str, Value),
    /// the action lives in another category: run it there (every engine tool
    /// is exactly one action, so this is never ambiguous) and say so
    Routed(&'static str, Value, String),
}

fn help(c: &Category, which: Option<&str>) -> Result<Value> {
    match which {
        Some(a) => {
            let t = tools::find(a).filter(|_| c.actions.contains(&a)).ok_or_else(|| wrong_action(c, a))?;
            let mut schema = (t.schema)();
            if has_own_action(t) {
                if let Some(p) = schema.get_mut("properties").and_then(|p| p.as_object_mut()) {
                    if let Some(v) = p.remove("action") {
                        p.insert("op".into(), v);
                    }
                }
                if let Some(r) = schema.get_mut("required").and_then(|r| r.as_array_mut()) {
                    for x in r.iter_mut() {
                        if x == "action" {
                            *x = Value::from("op");
                        }
                    }
                }
            }
            let mut ex = crate::tools_meta::example_call(t);
            if has_own_action(t) {
                if let Some(m) = ex.as_object_mut() {
                    if let Some(v) = m.remove("action") {
                        m.insert("op".into(), v);
                    }
                }
            }
            Ok(json!({"tool": c.name, "action": a, "does": t.description, "params_schema": schema,
                "example": {"action": a, "params": ex}}))
        }
        None => Ok(json!({"tool": c.name, "about": c.description,
            "actions": c.actions.iter().filter_map(|a| tools::find(a)).map(action_line).collect::<Vec<_>>(),
            "tip": format!("call {{\"action\":\"<name>\",\"params\":{{...}}}}; {{\"action\":\"help\",\"params\":{{\"action\":\"<name>\"}}}} shows one action's full schema")})),
    }
}

fn wrong_action(c: &Category, a: &str) -> anyhow::Error {
    if let Some(home) = category_of(a) {
        return anyhow!("'{a}' is an action of the '{}' tool, not '{}'. Call {} with {{\"action\":\"{a}\"}}.", home.name, c.name, home.name);
    }
    let mut near: Vec<(usize, &str)> = c.actions.iter().map(|x| (crate::tools_meta::distance(a, x), *x)).collect();
    near.sort();
    anyhow!("'{}' has no action '{a}'. Did you mean: {}? (action \"help\" lists them all)", c.name,
        near.iter().take(3).map(|(_, x)| *x).collect::<Vec<_>>().join(", "))
}

/// Turn a category call into the engine tool it names.
pub fn resolve(c: &'static Category, args: &Value) -> Result<Resolved> {
    let empty = Map::new();
    let m = args.as_object().unwrap_or(&empty);
    let action = m.get("action").and_then(|v| v.as_str());
    let legacy_same_name = tools::find(c.name).filter(|_| c.actions.contains(&c.name));
    let action = match action {
        Some(a) if a == HELP => {
            let which = m.get("params").and_then(|p| p.get("action")).or_else(|| m.get("of")).and_then(|v| v.as_str());
            return help(c, which).map(Resolved::Answer);
        }
        Some(a) if c.actions.contains(&a) => a,
        // a category that is also an engine tool (transport, history): its own
        // arguments pass straight through, so `transport {"action":"play"}`
        // and `history {}` keep meaning what they always did
        _ if legacy_same_name.is_some() => return Ok(Resolved::Call(legacy_same_name.unwrap().name, args.clone())),
        Some(a) => {
            if let Some(home) = category_of(a).filter(|h| h.name != c.name) {
                let note = format!("'{a}' lives in the '{}' tool; it ran there (call {} with {{\"action\":\"{a}\"}} next time)", home.name, home.name);
                return match resolve(home, args)? {
                    Resolved::Call(t, x) => Ok(Resolved::Routed(t, x, note)),
                    other => Ok(other),
                };
            }
            return Err(wrong_action(c, a));
        }
        None => return help(c, None).map(Resolved::Answer),
    };
    let tool = tools::find(action).ok_or_else(|| anyhow!("action '{action}' is not available in this build"))?;
    let only_params = m.keys().all(|k| k == "action" || k == "params");
    let mut inner: Map<String, Value> = match m.get("params") {
        Some(Value::Object(p)) if only_params => p.clone(),
        _ => m.iter().filter(|(k, _)| k.as_str() != "action").map(|(k, v)| (k.clone(), v.clone())).collect(),
    };
    if has_own_action(tool) {
        if let Some(op) = inner.remove("op") {
            inner.insert("action".into(), op);
        }
    }
    Ok(Resolved::Call(tool.name, Value::Object(inner)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use std::collections::HashMap;

    #[test]
    fn tools_list_stays_small() {
        let n = tools_json().as_array().unwrap().len();
        assert!(n <= MAX_TOOLS, "tools/list has {n} tools; the ceiling is {MAX_TOOLS}. Add an ACTION to a category instead of a new tool.");
        assert!(n >= 10, "{n}");
        for c in CATEGORIES {
            assert!(c.description.len() <= 200, "{}: description too long", c.name);
        }
    }

    #[test]
    fn every_engine_tool_is_exactly_one_category_action() {
        let mut seen: HashMap<&str, Vec<&str>> = HashMap::new();
        for c in CATEGORIES {
            for a in c.actions {
                seen.entry(a).or_default().push(c.name);
                assert!(tools::find(a).is_some(), "{}.{a}: no such engine tool", c.name);
                assert_ne!(*a, HELP);
            }
        }
        for t in tools::registry() {
            let homes = seen.get(t.name).cloned().unwrap_or_default();
            assert_eq!(homes.len(), 1, "engine tool '{}' must be an action of exactly one category, found in {:?} (add it to CATEGORIES in tools_categories.rs)", t.name, homes);
        }
        assert_eq!(seen.len(), tools::registry().len());
    }

    #[test]
    fn category_calls_reach_the_engine_tools() {
        let mut e = Engine::new(std::env::temp_dir().join("bb_categories"));
        e.call("project", &json!({"action": "new_project", "params": {}})).unwrap();
        e.call("project", &json!({"action": "set_tempo", "params": {"bpm": 97}})).unwrap();
        assert_eq!(e.project.bpm, 97.0);
        // flat arguments next to action
        e.call("project", &json!({"action": "set_tempo", "bpm": 101})).unwrap();
        assert_eq!(e.project.bpm, 101.0);
        // the same edit is one undo step, through the category or the old name
        e.call("history", &json!({"action": "undo"})).unwrap();
        assert_eq!(e.project.bpm, 97.0);
        // old names still work
        e.call("set_tempo", &json!({"bpm": 88})).unwrap();
        assert_eq!(e.project.bpm, 88.0);
        // a category that is also an old tool keeps its own arguments
        assert!(e.call("history", &json!({})).is_ok());
        // help: the list, and one action's schema + example
        let h = e.call("mixer", &json!({"action": "help"})).unwrap();
        assert!(h["actions"].as_array().unwrap().len() >= 5, "{h}");
        let h = e.call("mixer", &json!({"action": "help", "params": {"action": "set_mixer"}})).unwrap();
        assert!(h["params_schema"]["properties"]["volume_db"].is_object(), "{h}");
        // an action called on the wrong category runs in its own, and says so
        let r = e.call("mixer", &json!({"action": "set_tempo", "bpm": 90})).unwrap();
        assert_eq!(e.project.bpm, 90.0);
        assert!(r["note"].as_str().map(|n| n.contains("project")).unwrap_or(false), "{r}");
        // an action no category has is still a clear error with suggestions
        let err = e.call("mixer", &json!({"action": "set_mixr"})).unwrap_err().to_string();
        assert!(err.contains("set_mixer"), "{err}");
    }

    #[test]
    fn own_action_tools_take_op_in_flat_form() {
        let mut e = Engine::new(std::env::temp_dir().join("bb_categories_op"));
        e.call("project", &json!({"action": "new_project"})).unwrap();
        let r = e.call("transport", &json!({"action": "transport", "op": "status"}));
        assert!(r.is_ok(), "{r:?}");
        let r = e.call("transport", &json!({"action": "status"}));
        assert!(r.is_ok(), "{r:?}");
        let s = tools_json().to_string();
        assert!(s.len() < 60_000, "category list is {} bytes", s.len());
    }
}
