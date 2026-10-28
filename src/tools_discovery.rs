//! Staged tool discovery for models of every size. The full registry is
//! large (250+ tools); a small model does better with a short list and a
//! way to find the rest. Three steps, each cheap:
//!
//! 1. `list_tool_groups`: the categories, what each is for, how many tools,
//!    and a few starter tools per group (a few hundred tokens).
//! 2. `search_tools {query}`: ranked matches with one line each, the
//!    required arguments and whether the tool changes the project.
//! 3. `describe_tool {name}`: the full JSON schema plus a ready example,
//!    only for the tool about to be called.
//!
//! `beatbox mcp --tools compact` (or `BEATBOX_MCP_TOOLS=compact`) makes MCP
//! `tools/list` advertise only the compact set below, which includes these
//! discovery tools and `call_tool`, so every other tool stays one search
//! away without its schema sitting in the model's context.

use crate::tools::{self, obj, s_opt, s_req, Tool};
use anyhow::{anyhow, bail};
use serde_json::{json, Value};

/// The tools a compact `tools/list` advertises: discovery, the one-call
/// producers, the everyday project edits, render/export and undo.
pub const COMPACT: &[&str] = &[
    "get_guide",
    "list_tool_groups",
    "search_tools",
    "describe_tool",
    "call_tool",
    "get_project_summary",
    "new_project",
    "load_project",
    "save_project",
    "make_beat",
    "produce_song",
    "add_track",
    "add_notes",
    "get_pattern",
    "set_mixer",
    "add_effect",
    "tweak_effect",
    "set_tempo",
    "set_key",
    "list_ids",
    "lookup_id",
    "render",
    "export_audio",
    "undo",
    "redo",
    "history",
];

/// The compact tool list in MCP `tools/list` shape.
pub fn compact_tools_json() -> Value {
    Value::Array(
        COMPACT
            .iter()
            .filter_map(|n| tools::find(n))
            .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": (t.schema)()}))
            .collect(),
    )
}

fn required_of(t: &Tool) -> Vec<String> {
    (t.schema)()["required"]
        .as_array()
        .map(|r| {
            r.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Words that mean the same thing to a person but not to a substring match.
const SYNONYMS: &[(&str, &[&str])] = &[
    ("louder", &["volume", "gain", "mixer"]),
    ("quieter", &["volume", "gain", "mixer"]),
    ("level", &["volume", "mixer", "balance"]),
    ("echo", &["delay"]),
    ("space", &["reverb", "delay"]),
    ("room", &["reverb"]),
    ("speed", &["tempo", "bpm"]),
    ("bpm", &["tempo"]),
    ("cut", &["split", "trim", "delete"]),
    ("chop", &["split", "slice", "chop"]),
    ("crossfade", &["crossfade"]),
    ("fade", &["fade", "automation"]),
    ("save", &["save_project", "export"]),
    ("mp3", &["export"]),
    ("wav", &["export", "render"]),
    ("drum", &["drums", "kit", "steps"]),
    ("melody", &["melody", "notes"]),
    ("plugin", &["plugin", "clap"]),
    ("vocal", &["vocal", "voice"]),
    ("voice", &["vocal", "voice"]),
    ("noise", &["denoise", "noise"]),
    ("instrument", &["instrument", "preset", "sound"]),
    ("compare", &["compare", "ab"]),
    ("listen", &["listen", "ab", "analyze"]),
];

/// Rank tools for a plain-words query.
pub fn search(query: &str, category: Option<&str>, limit: usize) -> Vec<(i32, &'static Tool)> {
    let q = query.to_lowercase();
    let mut words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| {
            w.len() > 1
                && ![
                    "the", "a", "an", "to", "of", "and", "on", "in", "my", "it", "is", "for",
                    "with", "me",
                ]
                .contains(w)
        })
        .map(|w| w.to_string())
        .collect();
    let extra: Vec<String> = words
        .iter()
        .flat_map(|w| {
            SYNONYMS
                .iter()
                .filter(|(k, _)| w.starts_with(k))
                .flat_map(|(_, v)| v.iter().map(|s| s.to_string()))
        })
        .collect();
    let n_orig = words.len();
    words.extend(extra);
    let mut out: Vec<(i32, &'static Tool)> = tools::registry()
        .iter()
        .filter(|t| category.is_none_or(|c| crate::tools_meta::category(t.name) == c))
        .map(|t| {
            let name = t.name;
            let parts: Vec<&str> = name.split('_').collect();
            let d = t.description.to_lowercase();
            let cat = crate::tools_meta::category(name);
            let mut s = 0;
            if name == q.replace(' ', "_") {
                s += 50;
            }
            for (i, w) in words.iter().enumerate() {
                let wt = if i < n_orig { 2 } else { 1 };
                let stem = w.trim_end_matches('s');
                if parts.iter().any(|p| *p == w || *p == stem) {
                    s += 6 * wt;
                } else if name.contains(stem) {
                    s += 4 * wt;
                }
                if d.contains(stem) {
                    s += 2 * wt;
                }
                if cat == w {
                    s += 3 * wt;
                }
            }
            (s, t)
        })
        .filter(|(s, _)| *s > 0)
        .collect();
    out.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.name.len().cmp(&b.1.name.len()))
            .then(a.1.name.cmp(b.1.name))
    });
    out.truncate(limit);
    out
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_tool_groups",
            description: "The tool categories at a glance: what each group is for, how many tools it has and a few to start with. The cheapest way to see everything this engine can do; then search_tools or list_tools {category}.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |_, _| {
                let groups: Vec<Value> = crate::tools_meta::groups()
                    .into_iter()
                    .map(|(c, purpose, names)| json!({"group": c, "purpose": purpose, "tools": names.len(), "start_with": names.iter().take(4).collect::<Vec<_>>()}))
                    .collect();
                Ok(json!({"groups": groups, "total_tools": tools::registry().len(),
                    "next": "search_tools {query} to find a tool, describe_tool {name} for its full schema"}))
            },
        },
        Tool {
            name: "search_tools",
            description: "Find tools by plain words (\"make the vocal louder\", \"split clip\", \"reverb\"): ranked names with one line each, the required arguments and whether the tool changes the project. Optional category narrows it. Then describe_tool {name} for the full schema.",
            mutates: false,
            schema: || obj(json!({
                "query": {"type": "string"},
                "category": {"type": "string", "description": "only this group (list_tool_groups)"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 25, "default": 8}
            }), &["query"]),
            run: |_, a| {
                let q = s_req(a, "query")?;
                let cat = s_opt(a, "category").map(|c| c.to_lowercase());
                let lim = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(8).clamp(1, 25) as usize;
                let hits = search(&q, cat.as_deref(), lim);
                let list: Vec<Value> = hits
                    .iter()
                    .map(|(s, t)| json!({"name": t.name, "does": crate::tools_meta::short(t.description), "group": crate::tools_meta::category(t.name),
                        "call": json!({"tool": crate::tools_meta::category(t.name), "action": t.name}),
                        "required": required_of(t), "changes_project": t.mutates, "score": s}))
                    .collect();
                if list.is_empty() {
                    return Ok(json!({"query": q, "tools": [], "tip": "no match; try other words or list_tool_groups"}));
                }
                Ok(json!({"query": q, "tools": list, "next": format!("describe_tool {{\"name\": \"{}\"}}", hits[0].1.name)}))
            },
        },
        Tool {
            name: "call_tool",
            description: "Call any tool by name with its arguments, including tools not in a compact tool list (find them with search_tools, read their schema with describe_tool). Same validation, undo and results as calling the tool directly.",
            mutates: true,
            schema: || obj(json!({
                "name": {"type": "string"},
                "arguments": {"type": "object", "description": "the tool's arguments (describe_tool shows them)"}
            }), &["name"]),
            // The normalizer peels this wrapper without running its handler.
            // Internal validated dispatch of the registry entry uses it too.
            run: |e, a| e.call_nested("call_tool", a, "call_tool"),
        },
    ]
}

/// Shared wrapper parsing for pure invocation normalization and the fallback
/// handler. Keep the existing target/default/validation behavior in one place.
pub(crate) fn call_target(a: &Value) -> anyhow::Result<(String, Value)> {
    let name = s_req(a, "name")?;
    if name == "call_tool" {
        bail!("call_tool cannot call itself");
    }
    if tools::find(&name).is_none() && crate::tools_categories::find(&name).is_none() {
        return Err(anyhow!(
            "unknown tool '{name}'. Did you mean: {}? (search_tools finds tools)",
            crate::tools_meta::suggest(&name, 3).join(", ")
        ));
    }
    let args = a.get("arguments").cloned().unwrap_or(json!({}));
    Ok((name, args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    #[test]
    fn compact_list_is_small_and_complete() {
        let v = compact_tools_json();
        let n = v.as_array().unwrap().len();
        assert_eq!(n, COMPACT.len(), "every compact tool is registered");
        assert!(n <= 30);
        let full = crate::tools::tools_json().to_string().len();
        let small = v.to_string().len();
        assert!(
            small * 5 < full,
            "compact list {small} bytes vs full {full}"
        );
        for must in [
            "search_tools",
            "describe_tool",
            "call_tool",
            "list_tool_groups",
        ] {
            assert!(COMPACT.contains(&must));
        }
    }

    #[test]
    fn search_finds_the_obvious_tool() {
        let top = |q: &str| {
            search(q, None, 5)
                .iter()
                .map(|(_, t)| t.name)
                .collect::<Vec<_>>()
        };
        assert!(
            top("split clip").contains(&"split_clip"),
            "{:?}",
            top("split clip")
        );
        assert!(
            top("reverb")
                .iter()
                .any(|n| n.contains("effect") || n.contains("reverb")),
            "{:?}",
            top("reverb")
        );
        assert!(
            top("export mp3").contains(&"export_audio"),
            "{:?}",
            top("export mp3")
        );
        assert!(top("make the vocal louder")
            .iter()
            .any(|n| n.contains("vocal") || n.contains("mixer") || n.contains("gain")));
        assert!(top("undo")[0] == "undo");
    }

    #[test]
    fn groups_cover_every_tool_and_call_tool_is_one_undo_step() {
        let mut e =
            Engine::new(std::env::temp_dir().join(format!("bb_disc_{}", std::process::id())));
        let g = e.call("list_tool_groups", &json!({})).unwrap();
        let sum: u64 = g["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["tools"].as_u64().unwrap())
            .sum();
        assert_eq!(
            sum,
            crate::tools::registry().len() as u64,
            "every tool sits in a group"
        );
        let d = e
            .call("describe_tool", &json!({"name": "split_clip"}))
            .unwrap();
        assert!(
            d["input_schema"]["properties"]["at_beat"].is_object(),
            "{d}"
        );
        e.call(
            "call_tool",
            &json!({"name": "add_track", "arguments": {"name": "keys", "preset": "pad"}}),
        )
        .unwrap();
        assert_eq!(e.project.tracks.len(), 1);
        assert!(e
            .call(
                "call_tool",
                &json!({"name": "add_track", "arguments": {"nope": 1}})
            )
            .is_err());
        assert!(e
            .call("call_tool", &json!({"name": "splitclip"}))
            .unwrap_err()
            .to_string()
            .contains("split_clip"));
        e.call("undo", &json!({})).unwrap();
        assert!(e.project.tracks.is_empty());
    }
}
