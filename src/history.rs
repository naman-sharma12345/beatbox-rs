//! Plain-language names for undo steps, the way a musician reads a
//! history list ("Added clip · Drums · bar 5"): no call names, no
//! arguments, no machine wording. The studio's history panel and the
//! `history` tool both read these.

use serde_json::Value;

/// Words that never reach a person's history list, with what is said instead.
const SWAP: [(&str, &str); 19] = [
    ("generate", "make"),
    ("generated", "made"),
    ("generates", "makes"),
    ("lyrics", "words"),
    ("prompt", "brief"),
    ("critique", "review"),
    ("ai", ""),
    ("mcp", ""),
    ("llm", ""),
    ("gpt", ""),
    ("smart", ""),
    ("json", ""),
    ("tool", ""),
    ("tools", ""),
    ("assistant", ""),
    ("agent", ""),
    ("bot", ""),
    ("args", ""),
    ("magic", ""),
];

fn clean_word(w: &str) -> String {
    let low = w.to_lowercase();
    for (from, to) in SWAP {
        if low == from {
            return to.to_string();
        }
    }
    match low.as_str() {
        "bpm" | "fx" | "eq" | "midi" | "lfo" | "adsr" => low.to_uppercase(),
        "db" => "dB".into(),
        _ => low,
    }
}

fn sentence(words: &str) -> String {
    let w: Vec<String> = words
        .split(['_', ' ', '-'])
        .filter(|s| !s.is_empty())
        .map(clean_word)
        .filter(|s| !s.is_empty())
        .collect();
    let mut s = w.join(" ");
    if let Some(f) = s.get(0..1) {
        s = f.to_uppercase() + &s[1..];
    }
    if s.is_empty() {
        "Change".into()
    } else {
        s
    }
}

/// A value as a person reads it: names spaced, numbers without trailing zeros.
fn text(v: &Value) -> Option<String> {
    let s = match v {
        Value::String(s) => s.replace('_', " "),
        Value::Number(n) => {
            let f = n.as_f64()?;
            if f.fract() == 0.0 {
                format!("{}", f as i64)
            } else {
                format!("{:.2}", f).trim_end_matches('0').to_string()
            }
        }
        Value::Bool(b) => (if *b { "on" } else { "off" }).to_string(),
        _ => return None,
    };
    let s: String = s
        .chars()
        .filter(|c| !matches!(c, '{' | '}' | '"' | '[' | ']'))
        .collect();
    let s = s.trim().to_string();
    if s.is_empty() {
        return None;
    }
    if s.chars().count() > 22 {
        Some(s.chars().take(21).collect::<String>() + "…")
    } else {
        Some(s)
    }
}

fn arg(a: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| a.get(*k).and_then(text))
}

/// The history line for one change made by `tool` with `args`.
pub fn label(tool: &str, args: &Value) -> String {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let verb: String = match tool {
        "place_pattern" => "Added clip".into(),
        "playlist_clip" | "audio_clip" | "clip" => match action {
            "move" => "Moved clip".into(),
            "split" => "Split clip".into(),
            "trim" => "Trimmed clip".into(),
            "extend" => "Extended clip".into(),
            "duplicate" => "Duplicated clip".into(),
            "delete" | "remove" => "Deleted clip".into(),
            "lane" => "Moved clip to lane".into(),
            _ => "Edited clip".into(),
        },
        "add_track" => "Added track".into(),
        "remove_track" | "delete_track" => "Removed track".into(),
        "add_notes" | "add_note" => "Added notes".into(),
        "remove_notes" | "clear_notes" => "Removed notes".into(),
        "add_effect" => "Added effect".into(),
        "rack_apply" => "Rack edit".into(),
        "remove_effect" => "Removed effect".into(),
        "set_instrument" => "Changed sound".into(),
        "set_mixer" => "Mixer".into(),
        "load_project" | "open_project" => "Opened project".into(),
        "new_project" => "New project".into(),
        "set_tempo" => {
            return match arg(args, &["bpm"]) {
                Some(b) => format!("Tempo · {b} BPM"),
                None => "Tempo".into(),
            }
        }
        "set_key" => {
            let k = [arg(args, &["root"]), arg(args, &["scale"])]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            return if k.is_empty() {
                "Key".into()
            } else {
                format!("Key · {k}")
            };
        }
        "batch" => {
            let n = args
                .get("calls")
                .and_then(|c| c.as_array())
                .map(|c| c.len())
                .unwrap_or(0);
            return if n > 1 {
                format!("{n} changes at once")
            } else {
                "Change".into()
            };
        }
        _ => {
            let s = sentence(tool);
            if !action.is_empty() && action.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
                format!("{s} · {}", sentence(action).to_lowercase())
            } else {
                s
            }
        }
    };
    let mut parts = vec![verb];
    let who = if tool == "add_track" {
        arg(args, &["name"])
    } else {
        arg(args, &["track", "pattern", "target", "name"])
    };
    if let Some(w) = who {
        parts.push(w);
    }
    if let Some(b) = arg(args, &["start_bar", "at_bar", "to_bar", "bar"]) {
        parts.push(format!("bar {b}"));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn plain(s: &str) -> bool {
        let low = s.to_lowercase();
        let words: Vec<&str> = low
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        !s.contains(['{', '}', '"', '_', '[', ']', ':'])
            && !words.iter().any(|w| crate::gui_words::BANNED.contains(w))
    }

    #[test]
    fn labels_read_like_a_history_list() {
        assert_eq!(
            label(
                "place_pattern",
                &json!({"pattern": "Drums", "start_bar": 5})
            ),
            "Added clip · Drums · bar 5"
        );
        assert_eq!(
            label("playlist_clip", &json!({"action": "split", "id": "pl4"})),
            "Split clip"
        );
        assert_eq!(label("set_tempo", &json!({"bpm": 92.0})), "Tempo · 92 BPM");
        assert_eq!(
            label("add_track", &json!({"name": "acid_bass"})),
            "Added track · acid bass"
        );
        assert_eq!(
            label("generate_lyrics_prompt", &json!({})),
            "Make words brief"
        );
    }

    #[test]
    fn every_tool_gets_a_plain_label() {
        let args = json!({"track": "Drums", "pattern": "main", "start_bar": 3, "action": "move", "name": "x", "calls": [{}, {}]});
        let mut bad = Vec::new();
        for t in crate::tools::registry() {
            for a in [json!({}), args.clone()] {
                let l = label(t.name, &a);
                if !plain(&l) || l.is_empty() {
                    bad.push(format!("{} -> {l}", t.name));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}
