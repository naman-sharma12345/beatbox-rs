//! Naman, 2026-10-11: "The gui should not have any hint of ai, the gui of
//! beatbox-rs is fully manual ... The whole beatbox-rs should be
//! controllable through mcp." These checks keep it that way.

const SOURCES: [(&str, &str); 19] = [
    ("analyze_view.rs", include_str!("analyze_view.rs")),
    ("audio_check.rs", include_str!("audio_check.rs")),
    ("browser_view.rs", include_str!("browser_view.rs")),
    ("channel_rack.rs", include_str!("channel_rack.rs")),
    ("console.rs", include_str!("console.rs")),
    ("export_view.rs", include_str!("export_view.rs")),
    ("mod.rs", include_str!("mod.rs")),
    ("piano.rs", include_str!("piano.rs")),
    ("player.rs", include_str!("player.rs")),
    ("playlist.rs", include_str!("playlist.rs")),
    ("rack_view.rs", include_str!("rack_view.rs")),
    ("render_job.rs", include_str!("render_job.rs")),
    ("rhythm_trial.rs", include_str!("rhythm_trial.rs")),
    ("shortcuts.rs", include_str!("shortcuts.rs")),
    ("theme.rs", include_str!("theme.rs")),
    ("views.rs", include_str!("views.rs")),
    ("vocal_view.rs", include_str!("vocal_view.rs")),
    ("widgets.rs", include_str!("widgets.rs")),
    ("workspace.rs", include_str!("workspace.rs")),
];

/// String literals in Rust source (comments skipped), with their line.
fn literals(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let mut start_line = 0;
    for (ln, line) in src.lines().enumerate() {
        let ch: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < ch.len() {
            let c = ch[i];
            if let Some(s) = cur.as_mut() {
                if c == '\\' && i + 1 < ch.len() {
                    s.push(ch[i + 1]);
                    i += 2;
                    continue;
                }
                if c == '"' {
                    out.push((start_line + 1, cur.take().unwrap()));
                } else {
                    s.push(c);
                }
            } else {
                if c == '/' && ch.get(i + 1) == Some(&'/') {
                    break;
                }
                // a char literal like '"'
                if c == '\'' && ch.get(i + 2) == Some(&'\'') {
                    i += 3;
                    continue;
                }
                if c == '"' {
                    cur = Some(String::new());
                    start_line = ln;
                }
            }
            i += 1;
        }
        if let Some(s) = cur.as_mut() {
            s.push('\n');
        }
    }
    out
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

// This prefix rule requires all production code to precede the final test module.
fn production_source(src: &str) -> &str {
    src.split("#[cfg(test)]").next().unwrap_or(src)
}

#[test]
fn wording_scan_covers_rhythm_trial_production_labels() {
    let (_, src) = SOURCES
        .iter()
        .find(|(file, _)| *file == "rhythm_trial.rs")
        .expect("rhythm trial must remain in the wording scan");
    let scanned = literals(production_source(src));
    for label in ["Rhythm trial", "Open matched A/B"] {
        assert!(
            scanned.iter().any(|(_, lit)| lit == label),
            "wording scan misses production label: {label}"
        );
    }
}

#[test]
fn gui_text_has_no_ai_wording() {
    let banned = crate::gui_words::BANNED;
    let mut bad = Vec::new();
    for (file, src) in SOURCES {
        // a file's own tests are not on screen
        let src = production_source(src);
        for (line, lit) in literals(src) {
            // tool names and JSON keys are identifiers, not words a user reads
            if is_ident(&lit) {
                continue;
            }
            // default file names (a saved project, a part file, a numbered
            // "beat-{n}.beatbox.json") are names on disk, not wording on screen
            let is_file = lit.contains('.')
                && !lit.contains(' ')
                && lit
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-/{}".contains(c));
            if is_file {
                continue;
            }
            let low = lit.to_lowercase();
            let words: Vec<&str> = low
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|w| !w.is_empty())
                .collect();
            if words.iter().any(|w| banned.contains(w))
                || low.contains("make with")
                || low.contains("tool call")
                || low.contains("auto-produce")
                || low.contains("auto produce")
            {
                bad.push(format!("{file}:{line}: \"{lit}\""));
            }
        }
    }
    assert!(bad.is_empty(), "AI wording in the GUI:\n{}", bad.join("\n"));
}

/// The studio never shows its plumbing: no call names or raw arguments in
/// the history panel, no server port or call count in the top bar (the port
/// lives in About), no YOU / SYS / EXT source tags.
#[test]
fn gui_shows_no_plumbing() {
    for (file, src) in SOURCES {
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for needle in [
            "{args}\")",
            "{args} ",
            "registry().len()",
            "tools::registry()",
            "e.tool",
            ".summary",
            "e.source",
            "\"YOU\"",
            "\"SYS\"",
            "\"EXT\"",
            "\"PORT",
            "ACTIVITY",
            "to_string_pretty(",
            "summarize_args",
        ] {
            assert!(!code.contains(needle), "{file} shows plumbing: {needle}");
        }
    }
    // the port appears only inside the About window
    let src = include_str!("mod.rs");
    let about = src.find("About Beatbox Studio").expect("About window");
    for (i, _) in src.match_indices("rsplit(':')") {
        assert!(i > about && i - about < 800, "port shown outside About");
    }
}

#[test]
fn history_panel_reads_plainly() {
    let mut e = crate::Engine::new(std::env::temp_dir());
    let _ = e.call_from("set_tempo", &serde_json::json!({"bpm": 96}), "you");
    let _ = e.call_from(
        "add_track",
        &serde_json::json!({"name": "drums", "preset": "kick"}),
        "you",
    );
    let (done, _) = e.history();
    assert!(!done.is_empty());
    for l in &done {
        assert!(!l.contains(['{', '"', '_', ':']), "{l}");
        let low = l.to_lowercase();
        assert!(
            !low.split(|c: char| !c.is_alphanumeric())
                .any(|w| crate::gui_words::BANNED.contains(&w)),
            "{l}"
        );
    }
    assert_eq!(done[0], "Added track · drums");
    // undo two steps from the panel, then redo one
    e.call_from("undo", &serde_json::json!({"steps": 2}), "you")
        .unwrap();
    let (d2, u2) = e.history();
    assert_eq!(u2.len(), 2);
    assert_eq!(d2.len(), done.len() - 2);
    e.call_from("redo", &serde_json::json!({"steps": 1}), "you")
        .unwrap();
    assert_eq!(e.history().0[0], "Tempo · 96 BPM");
}

#[test]
fn gui_has_only_manual_views() {
    for v in super::VIEWS {
        assert!(
            !matches!(v, "CREATE" | "CRITIQUE" | "AI" | "ASSISTANT"),
            "{v}"
        );
    }
    // the one-call production tools are MCP-only
    for (file, src) in SOURCES {
        for t in [
            "make_beat",
            "produce_song",
            "critique_mix",
            "generate_beat",
            "sing_lyrics",
            "vocal_to_song",
            "produce_track",
        ] {
            assert!(!src.contains(&format!("\"{t}\"")), "{file} calls {t}");
        }
    }
}

#[test]
fn every_gui_action_is_an_mcp_tool() {
    let mut missing = Vec::new();
    for (file, src) in SOURCES {
        let src = production_source(src);
        // call("tool", ..) / call_from("tool", ..) / op = Some(("tool", ..)) / Some(("tool".into(), ..))
        for pat in ["call(\"", "call_from(\"", "Some((\""] {
            let mut rest = src;
            while let Some(k) = rest.find(pat) {
                rest = &rest[k + pat.len()..];
                let name: String = rest.chars().take_while(|c| *c != '"').collect();
                if is_ident(&name)
                    && crate::tools::find(&name).is_none()
                    && !matches!(name.as_str(), "time" | "pitch" | "staccato")
                {
                    missing.push(format!("{file}: {name}"));
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "GUI actions with no MCP tool: {missing:?}"
    );
}

/// Every registered tool must be reachable from the manual GUI - a button,
/// menu item or shortcut somewhere in the views - or sit on this list with
/// a reason (MCP-only analysis, authoring and agent helpers). Adding a tool
/// without a GUI entry fails this test until the tool is wired into a view
/// or exempted here.
#[test]
fn every_tool_is_reachable_from_the_gui() {
    const NO_GUI: &[&str] = &[
        "ab_compare",
        "add_audio_clip",
        "add_marker",
        "add_multisample_track",
        "add_roll",
        "add_tempo_point",
        "add_transition",
        "additive_sample",
        "align_phrases",
        "align_take",
        "analyze_audio",
        "analyze_loudness",
        "analyze_lyrics",
        "analyze_mix",
        "analyze_partials",
        "analyze_reference",
        "analyze_sections",
        "analyze_song",
        "analyze_track",
        "analyze_vocal",
        "analyze_vocal_style",
        "analyze_vocal_tone",
        "apply_groove",
        "apply_plan",
        "arrangement_to_playlist",
        "audition_palette",
        "author_midi",
        "auto_fix_mix",
        "auto_judge",
        "batch",
        "blind_ab_create",
        "blind_ab_judge",
        "blind_ab_preferences",
        "blind_ab_rate",
        "blind_ab_reveal",
        "bounce_track",
        "build_structure",
        "bypass_effect",
        "cache_limit_mb",
        "call_tool",
        "carve_frequencies",
        "chase_reference",
        "check_vocal_edits",
        "clear",
        "clear_playlist",
        "compare_to_reference",
        "compare_variants",
        "copy_fx_chain",
        "copy_notes",
        "cover_song",
        "create_automation_clip",
        "create_macro",
        "critique_mix",
        "critique_track",
        "delete_notes",
        "denoise_voice",
        "describe_effect",
        "describe_genre",
        "describe_instrument",
        "describe_tool",
        "design_sound",
        "detect_key",
        "detect_transients",
        "detect_vocal_onsets",
        "detect_vocal_styles",
        "diff_project",
        "diff_renders",
        "download_sample",
        "ears_report",
        "edit_vocal",
        "extract_groove",
        "fetch_instrument",
        "fetch_reference",
        "find_samples",
        "fit_clip",
        "flip_sample",
        "freesound_fetch",
        "freesound_search",
        "gain_stage",
        "generate_bassline",
        "generate_beat",
        "generate_chords",
        "generate_drums",
        "get_effects",
        "get_guide",
        "get_history",
        "get_pattern",
        "get_project",
        "get_project_summary",
        "get_vocal_map",
        "groove_analysis",
        "group_takes",
        "harmonize_vocal",
        "history",
        "hook_analysis",
        "identify_song",
        "import_from_page",
        "import_sample",
        "index_samples",
        "install_instrument_pack",
        "install_kit",
        "install_palette_samples",
        "judge_audio",
        "layer_instrument",
        "limit_notes",
        "list_audio_clips",
        "list_audio_inputs",
        "list_automation",
        "list_automation_clips",
        "list_genres",
        "list_ids",
        "list_installed",
        "list_instruments",
        "list_markers",
        "list_playlist",
        "list_routing",
        "list_samples",
        "list_snapshots",
        "list_tempo",
        "list_tool_groups",
        "list_tools",
        "load_project",
        "lookup_id",
        "loudness_report",
        "make_beat",
        "make_drill_beat",
        "make_song_from_reference",
        "make_song_from_vocal",
        "match_timbre",
        "midi_event",
        "move_effect",
        "multiband_dynamics",
        "notch_drones",
        "novelty_report",
        "pick_instrument_for_role",
        "place_automation_clip",
        "place_fx",
        "place_vocal_phrases",
        "plan_cover",
        "plan_track",
        "play_audio",
        "produce_song",
        "produce_track",
        "qa_song",
        "rack_get",
        "read_prompt",
        "record_audio",
        "reference_match",
        "remove_bus",
        "remove_clip",
        "remove_instrument",
        "remove_marker",
        "remove_pattern",
        "remove_tempo_point",
        "render_spectrogram",
        "render_waveform_png",
        "reorder_effects",
        "restore_snapshot",
        "resynthesize_sample",
        "revise_track",
        "score_voice_quality",
        "screenshot",
        "search_samples",
        "search_tools",
        // Read-only arrangement summary is shown directly in the section-order panel.
        "get_arrangement",
        "set_arrangement",
        "set_macro",
        "set_parameters",
        "set_pattern_length",
        "set_section_mix",
        "set_steps",
        "set_time_signature",
        "sidechain_compress",
        "snap_clip_edges",
        "snapshot",
        "song_create",
        "song_critique",
        "song_get",
        "song_improve",
        "song_patch",
        "song_render",
        "song_schema",
        "song_validate",
        "spectrum",
        "steal_groove",
        "stretch_sample",
        "strip_silence",
        "structure",
        "suggest_tools",
        "tempo_ramp",
        "theory_chords",
        "theory_scale",
        "transcribe_lyrics",
        "transport",
        "true_peak_limit",
        "tune_drums_to_key",
        "validate_project",
        "vary_section",
        "vocal_chop",
        "vocal_to_song",
        "waveform_peaks",
        "write_phrase",
        "write_riff",
    ];
    let gui: String = SOURCES.iter().map(|(_, s)| *s).collect();
    let mut missing = Vec::new();
    for t in crate::tools::registry() {
        if !gui.contains(&format!("\"{}\"", t.name)) && !NO_GUI.contains(&t.name) {
            missing.push(t.name);
        }
    }
    assert!(
        missing.is_empty(),
        "tools with no GUI entry and not on the NO_GUI list: {missing:?}"
    );
}

#[test]
fn piano_drag_uses_press_origin_before_hit_testing() {
    // A note edge is only five pixels wide. The drag threshold may carry
    // the current pointer beyond it, so drag setup must use the press point.
    let piano = include_str!("piano.rs");
    let drag = piano
        .split("if !self.piano.gesture_cancelled && resp.drag_started() {")
        .nth(1)
        .unwrap();
    let setup = drag
        .split("if !self.piano.gesture_cancelled && resp.dragged() {")
        .next()
        .unwrap();
    assert!(setup.contains("i.pointer.press_origin()"));
    assert!(setup.contains("match hit(start)"));
    assert!(setup.contains("self.piano.band = Some(start)"));
}

#[test]
fn save_success_feedback_is_guarded_by_tool_result() {
    let studio = include_str!("mod.rs");
    let save = studio.split("fn save(&mut self) -> bool {").nth(1).unwrap();
    let save = save.split("fn render_wav").next().unwrap();
    // only this function: the next one (open) has its own Ok/Err arms
    let save = save.split("\n    fn ").next().unwrap();
    assert!(save.contains("match result"));
    let success = save.split("Ok(written_revision) => {").nth(1).unwrap();
    let success = success.split("Err(e) =>").next().unwrap();
    assert!(success.contains("self.project_path = Some(path.clone())"));
    assert!(success.contains("self.saved_rev = written_revision"));
    assert!(!success.contains("lock()"));
    assert!(save.contains("save_project_revision(&mut engine, &path)"));
    assert!(success.contains("format!(\"Saved {}\""));
    // the failure toast names the file and shows the real error, and never
    // reports success (the last argument, `true`, only appears under Ok)
    let failure = save.split("Err(e) =>").nth(1).unwrap();
    assert!(failure.contains("Could not save"));
    assert!(failure.contains("{e:#}"));
    assert!(failure.contains("false"));
    assert!(!failure.contains("Saved {}"));
    // a failed save must not mark the project as saved or point it at the file
    assert!(!failure.contains("project_path"));
    assert!(!failure.contains("saved_rev"));
    // and success is only reported from the Ok arm, with the success flag
    assert!(success.contains("0.0, true"));
}

#[test]
fn wording_scan_covers_audio_check_production_labels() {
    let (_, src) = SOURCES
        .iter()
        .find(|(file, _)| *file == "audio_check.rs")
        .expect("audio check registered");
    let scanned = literals(production_source(src));
    for label in [
        "Check audio",
        "Prepare excerpt",
        "Play excerpt",
        "Stop excerpt",
        "Excerpt boundary, song ending not checked",
        "End of returned event report",
    ] {
        assert!(
            scanned.iter().any(|(_, lit)| lit == label),
            "production label not scanned: {label}"
        );
    }
}

#[test]
fn leave_actions_use_shared_file_and_save_guards() {
    let studio = production_source(include_str!("mod.rs"));
    // New, selected Open and OS Close all ask the same live-state predicate.
    assert_eq!(studio.matches("self.needs_leave_warning()").count(), 3);
    // Their Save-first actions all gate continuation on this attempt's result.
    assert_eq!(studio.matches("self.save_before_leave()").count(), 3);
    let guard = studio
        .split("fn save_before_leave(&mut self)")
        .nth(1)
        .unwrap();
    let guard = guard.split("\n    fn ").next().unwrap();
    assert!(guard.contains("let saved = self.save()"));
    assert!(guard.contains(
        "save_allows_leave(saved, self.project_path.is_some(), revision, self.saved_rev)"
    ));
    assert!(!studio.contains("revision == self.saved_rev"));
}

#[test]
fn wording_scan_covers_all_studio_production_and_new_seams() {
    let studio = include_str!("mod.rs");
    let production = production_source(studio);
    for seam in [
        "fn needs_leave_warning(",
        "fn save_allows_leave(",
        "fn save_project_revision(",
        "impl QuitCloseState",
        "fn persist_screenshot(",
        "fn approve_completed_capture(",
        "fn fail_screenshot(",
        "fn save_before_leave(",
        "fn project_has_nothing_to_render(",
        "impl eframe::App for Studio",
    ] {
        assert!(production.contains(seam), "unscanned production: {seam}");
    }
    let scanned = literals(production);
    for label in [
        "Not saved",
        "Saved",
        "Save, then quit",
        "Save, then open",
        "Save, then start new",
        "Cancel",
        "Discard changes",
    ] {
        assert!(
            scanned.iter().any(|(_, lit)| lit == label),
            "unscanned label: {label}"
        );
    }
    // There must not be another production island after the first test module.
    let tests = &studio[production.len()..];
    assert!(!tests.contains("impl Studio"));
    assert!(!tests.contains("impl eframe::App"));
    assert!(!tests.contains("fn project_has_nothing_to_render("));
}

#[test]
fn leave_dialogs_keep_cancel_discard_and_undo_paths() {
    let studio = production_source(include_str!("mod.rs"));
    assert!(studio.contains("3 => Some(QuitChoice::Cancel)"));
    assert!(studio.contains("3 => self.pending_open = None"));
    assert!(studio.contains("3 => self.confirm_new = false"));
    assert!(studio.contains("2 => {\n                    self.pending_open = None;\n                    self.open_project_file(target);"));
    assert!(studio.contains("2 => {\n                    self.confirm_new = false;\n                    self.start_new_project();"));
    assert!(studio.contains("Undo brings the previous one back."));
    assert!(studio.contains("Undo only works until you close the app."));
    assert!(studio.contains("Action::Undo => self.call(\"undo\", json!({}))"));
}

#[test]
fn root_close_routes_through_tested_state_and_explicit_quit_approval() {
    let studio = production_source(include_str!("mod.rs"));
    assert!(studio.contains("quit_close: QuitCloseState"));
    assert!(studio.contains("quit_close: QuitCloseState::default()"));
    let route = studio
        .split("let close_requested = ctx.input")
        .nth(1)
        .unwrap();
    let route = route.split("if self.open_list {").next().unwrap();
    assert!(route.contains("i.viewport().close_requested()"));
    assert!(route.contains("let needs_warning = self.needs_leave_warning()"));
    let compact: String = route.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(compact.contains("ifself.quit_close.incoming_close(close_requested,needs_warning){ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose)"));
    assert!(route.contains("if self.quit_close.pending {"));
    assert!(route.contains("1 => Some(QuitChoice::Save(self.save_before_leave()))"));
    assert!(route.contains("2 => Some(QuitChoice::Discard)"));
    assert!(route.contains("3 => Some(QuitChoice::Cancel)"));
    assert!(route.contains("if self.quit_close.resolve(choice) {\n                    ctx.send_viewport_cmd(egui::ViewportCommand::Close)"));
    assert!(!route.contains("saved_rev ="));
    // New/Open keep independent questions and cannot grant quit approval.
    let new = studio.split(".button(\"New project\")").nth(1).unwrap();
    let new = new.split(".button(\"Open project...\")").next().unwrap();
    assert!(new.contains("self.confirm_new = true"));
    assert!(!new.contains("quit_close"));
    let open = studio.split("if let Some(f) = pick {").nth(1).unwrap();
    let open = open.split("if let Some(target)").next().unwrap();
    assert!(open.contains("self.pending_open = Some(f)"));
    assert!(!open.contains("quit_close"));
    assert!(!route.contains("self.confirm_new"));
    assert!(!route.contains("self.pending_open"));
    assert!(!studio.contains("confirm_quit"));
    assert_eq!(studio.matches("self.quit_close.resolve(").count(), 1);
    let compact_studio: String = studio.chars().filter(|c| !c.is_whitespace()).collect();
    assert_eq!(
        compact_studio
            .matches("self.quit_close.incoming_close(")
            .count(),
        1
    );
    assert_eq!(route.matches("ViewportCommand::Close)").count(), 1);
}

#[test]
fn screenshot_shutdown_requires_completed_writes_and_capture_specific_approval() {
    let studio = production_source(include_str!("mod.rs"));
    let capture = studio.split("// screenshot automation").nth(1).unwrap();
    let capture = capture
        .split("fn project_has_nothing_to_render")
        .next()
        .unwrap();
    assert!(capture.contains("if let Some(path) = self.screenshot.clone()"));
    assert!(capture.contains("if let Some(img) = shot"));
    assert!(capture.contains("let saved = persist_screenshot("));
    assert!(capture.contains("im.save(&path).map_err(|e| e.to_string())"));
    assert!(capture.contains("capture.manifest(&path, &self.audio_check, ctx, [w, h])"));
    assert!(capture.contains("Ok(receipt) => receipt"));
    assert!(
        capture.contains(".approve_completed_capture(self.screenshot.is_some(), Some(receipt))")
    );
    assert_eq!(capture.matches("ViewportCommand::Close)").count(), 1);
    assert_eq!(studio.matches(".approve_completed_capture(").count(), 1);
    assert!(capture.find("im.save(&path)").unwrap() < capture.find("capture.manifest(").unwrap());
    assert!(
        capture.find("capture.manifest(").unwrap()
            < capture.find(".approve_completed_capture(").unwrap()
    );
    assert!(
        capture.find(".approve_completed_capture(").unwrap()
            < capture.find("ViewportCommand::Close)").unwrap()
    );
    assert!(!capture.contains("QuitChoice::Discard"));
    assert!(!capture.contains("saved_rev ="));
    assert!(!capture.contains("project_path ="));
    assert_eq!(capture.matches("fail_screenshot(").count(), 3);
    assert!(capture.contains("Invalid screenshot pixel buffer"));
    assert!(capture.contains("fail_screenshot(&error)"));
    let failure = studio
        .split("fn fail_screenshot(error: &str) -> ! {")
        .nth(1)
        .unwrap();
    let failure = failure.split("\n}").next().unwrap();
    assert!(failure.contains("eprintln!(\"screenshot FAILED: {error}\")"));
    assert!(failure.contains("std::process::exit(1)"));
    assert!(!failure.contains("approved_once"));
    assert!(!failure.contains("ViewportCommand::Close"));
}
