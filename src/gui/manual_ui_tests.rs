//! Naman, 2026-10-11: "The gui should not have any hint of ai, the gui of
//! beatbox-rs is fully manual ... The whole beatbox-rs should be
//! controllable through mcp." These checks keep it that way.

const SOURCES: [(&str, &str); 11] = [
    ("browser_view.rs", include_str!("browser_view.rs")),
    ("console.rs", include_str!("console.rs")),
    ("mod.rs", include_str!("mod.rs")),
    ("piano.rs", include_str!("piano.rs")),
    ("player.rs", include_str!("player.rs")),
    ("playlist.rs", include_str!("playlist.rs")),
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
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[test]
fn gui_text_has_no_ai_wording() {
    let banned = ["ai", "assistant", "generate", "generated", "generates", "generating", "generator", "smart", "prompt", "critique", "lyrics", "magic", "copilot", "llm", "gpt"];
    let mut bad = Vec::new();
    for (file, src) in SOURCES {
        for (line, lit) in literals(src) {
            // tool names and JSON keys are identifiers, not words a user reads
            if is_ident(&lit) {
                continue;
            }
            let low = lit.to_lowercase();
            let words: Vec<&str> = low.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
            if words.iter().any(|w| banned.contains(w)) || low.contains("make with") || low.contains("auto-produce") || low.contains("auto produce") {
                bad.push(format!("{file}:{line}: \"{lit}\""));
            }
        }
    }
    assert!(bad.is_empty(), "AI wording in the GUI:\n{}", bad.join("\n"));
}

#[test]
fn gui_has_only_manual_views() {
    for v in super::VIEWS {
        assert!(!matches!(v, "CREATE" | "CRITIQUE" | "AI" | "ASSISTANT"), "{v}");
    }
    // the one-call production tools are MCP-only
    for (file, src) in SOURCES {
        for t in ["make_beat", "produce_song", "critique_mix", "generate_beat", "sing_lyrics", "vocal_to_song", "produce_track"] {
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
                if is_ident(&name) && crate::tools::find(&name).is_none() && !matches!(name.as_str(), "time" | "pitch" | "staccato") {
                    missing.push(format!("{file}: {name}"));
                }
            }
        }
    }
    assert!(missing.is_empty(), "GUI actions with no MCP tool: {missing:?}");
}
