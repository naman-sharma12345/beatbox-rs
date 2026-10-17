//! Naman, 2026-10-11: "The gui should not have any hint of ai, the gui of
//! beatbox-rs is fully manual ... The whole beatbox-rs should be
//! controllable through mcp." These checks keep it that way.

const SOURCES: [(&str, &str); 13] = [
    ("analyze_view.rs", include_str!("analyze_view.rs")),
    ("browser_view.rs", include_str!("browser_view.rs")),
    ("console.rs", include_str!("console.rs")),
    ("mod.rs", include_str!("mod.rs")),
    ("piano.rs", include_str!("piano.rs")),
    ("player.rs", include_str!("player.rs")),
    ("playlist.rs", include_str!("playlist.rs")),
    ("rack_view.rs", include_str!("rack_view.rs")),
    ("shortcuts.rs", include_str!("shortcuts.rs")),
    ("theme.rs", include_str!("theme.rs")),
    ("views.rs", include_str!("views.rs")),
    ("vocal_view.rs", include_str!("vocal_view.rs")),
    ("widgets.rs", include_str!("widgets.rs")),
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

#[test]
fn gui_text_has_no_ai_wording() {
    let banned = crate::gui_words::BANNED;
    let mut bad = Vec::new();
    for (file, src) in SOURCES {
        // a file's own tests are not on screen
        let src = src.split("#[cfg(test)]").next().unwrap_or(src);
        for (line, lit) in literals(src) {
            // tool names and JSON keys are identifiers, not words a user reads
            if is_ident(&lit) {
                continue;
            }
            // default file names (a saved project, a part file) are names, not wording
            let is_file = lit.contains('.')
                && !lit.contains(' ')
                && lit
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c));
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
