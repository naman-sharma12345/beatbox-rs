//! Model Context Protocol server over stdio (newline-delimited JSON-RPC 2.0).
//!
//! Two modes: `Local` owns an engine in-process; `Remote` forwards every tool
//! call to a running Beatbox Studio so you can watch the AI work live.

use crate::engine::Engine;
use crate::tools;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

pub enum Backend {
    Local(Box<Engine>),
    Remote(String),
}

impl Backend {
    fn call(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        match self {
            Backend::Local(e) => e.call_from(name, args, "mcp").map_err(|e| format!("{e:#}")),
            Backend::Remote(url) => {
                let resp = ureq::post(&format!("{}/call", url.trim_end_matches('/')))
                    .send_json(json!({"name": name, "arguments": args}));
                match resp {
                    Ok(r) => {
                        let v: Value = r.into_json().map_err(|e| e.to_string())?;
                        if v["ok"].as_bool().unwrap_or(false) {
                            Ok(v["result"].clone())
                        } else {
                            Err(v["error"].as_str().unwrap_or("studio error").to_string())
                        }
                    }
                    Err(e) => Err(format!(
                        "can't reach Beatbox Studio at {url}: {e}. Is `beatbox studio` running?"
                    )),
                }
            }
        }
    }
}

/// Which tool list `tools/list` advertises: 0 categories (default),
/// 1 legacy (every engine tool by name), 2 compact.
static TOOL_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// `beatbox mcp --tools categories|legacy|full|compact` (or --legacy-tools).
pub fn set_tool_mode(mode: &str) {
    let m = match mode.to_ascii_lowercase().as_str() {
        "legacy" | "full" | "all" => 1,
        "compact" => 2,
        _ => 0,
    };
    TOOL_MODE.store(m, std::sync::atomic::Ordering::Relaxed);
}

/// Kept for older callers: compact on/off.
pub fn set_compact_tools(on: bool) {
    set_tool_mode(if on { "compact" } else { "categories" });
}

/// The mode in force: the flag, else BEATBOX_LEGACY_TOOLS=1 or BEATBOX_MCP_TOOLS.
fn tool_mode() -> u8 {
    let m = TOOL_MODE.load(std::sync::atomic::Ordering::Relaxed);
    if m != 0 {
        return m;
    }
    if std::env::var("BEATBOX_LEGACY_TOOLS").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")) {
        return 1;
    }
    match std::env::var("BEATBOX_MCP_TOOLS").unwrap_or_default().to_ascii_lowercase().as_str() {
        "legacy" | "full" | "all" => 1,
        "compact" => 2,
        _ => 0,
    }
}

/// The `tools/list` result for a mode (0 categories, 1 legacy, 2 compact).
pub fn tools_list_mode(mode: u8) -> Value {
    json!({ "tools": match mode {
        1 => tools::tools_json(),
        2 => crate::tools_discovery::compact_tools_json(),
        _ => crate::tools_categories::tools_json(),
    } })
}

/// The `tools/list` result, compact or the default categories.
pub fn tools_list(compact: bool) -> Value {
    tools_list_mode(if compact { 2 } else { 0 })
}

pub fn handle(backend: &mut Backend, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?; // notifications get no response
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let result: Result<Value, (i64, String)> = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("2025-06-18"),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "beatbox", "title": "Beatbox — AI-native beat maker", "version": env!("CARGO_PKG_VERSION") },
            "instructions": match tool_mode() {
                1 => tools::GUIDE.to_string(),
                2 => format!("{} This server lists a compact tool set; list_tool_groups, search_tools and describe_tool find the other {} tools, and call_tool runs any of them.", tools::GUIDE, tools::registry().len().saturating_sub(crate::tools_discovery::COMPACT.len())),
                _ => format!("Beatbox: a beat-making engine. Each tool is a category; call it with {{\"action\": \"<name>\", \"params\": {{...}}}} (the actions and their arguments are listed in the action field). Fastest start: generate {{\"action\":\"make_beat\",\"params\":{{\"prompt\":\"dark trap 140 bpm\"}}}}, then transport render or export export_audio. {{\"action\":\"help\",\"params\":{{\"action\":\"<name>\"}}}} on any tool gives one action's full schema and an example; help search_tools finds an action from plain words. Engine tool names in the guide below are actions.\n\n{}", tools::GUIDE),
            },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list_mode(tool_mode())),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            Ok(match backend.call(name, &args) {
                Ok(v) => {
                    let (v, blocks) = crate::media::split_content(v);
                    let text = serde_json::to_string_pretty(&v).unwrap_or_default();
                    let mut content = vec![json!({ "type": "text", "text": text })];
                    content.extend(blocks);
                    let mut r = json!({ "content": content, "isError": false });
                    if v.is_object() {
                        r["structuredContent"] = v;
                    }
                    r
                }
                Err(e) => {
                    json!({ "content": [{ "type": "text", "text": format!("Error: {e}") }], "isError": true })
                }
            })
        }
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        other => Err((-32601, format!("method not found: {other}"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, m)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": m } })
        }
    })
}

pub fn serve(mut backend: Backend) -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&mut backend, &msg),
            Err(e) => Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}),
            ),
        };
        if let Some(r) = resp {
            writeln!(out, "{r}")?;
            out.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_tools_list_is_small_and_still_reaches_every_tool() {
        let full = tools_list_mode(1)["tools"].as_array().unwrap().len();
        let small = tools_list(true)["tools"].as_array().unwrap().len();
        assert_eq!(full, tools::registry().len());
        assert!(small <= 30 && small * 5 < full, "{small} of {full}");
        // a tool outside the compact list still runs by name through call_tool
        let mut b = Backend::Local(Box::new(Engine::new(std::env::temp_dir().join("bb_mcp_compact"))));
        let r = handle(&mut b, &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"call_tool","arguments":{"name":"theory_scale","arguments":{"root":"C","scale":"minor"}}}})).unwrap();
        assert_eq!(r["result"]["isError"], false, "{r}");
    }

    #[test]
    fn mcp_handshake_list_and_call() {
        let mut b = Backend::Local(Box::new(Engine::new(std::env::temp_dir())));
        let init = handle(&mut b, &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}})).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "beatbox");
        assert!(handle(
            &mut b,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .is_none());
        let list = handle(
            &mut b,
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .unwrap();
        // the default list is the category tools, never more than the ceiling
        let n = list["result"]["tools"].as_array().unwrap().len();
        assert!((10..=crate::tools_categories::MAX_TOOLS).contains(&n), "{n} tools listed");
        assert!(list["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "mixer"));
        // a category call
        let call = handle(&mut b, &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"project","arguments":{"action":"set_tempo","params":{"bpm":97}}}})).unwrap();
        assert_eq!(call["result"]["isError"], false, "{call}");
        assert_eq!(call["result"]["structuredContent"]["bpm"], 97.0);
        // the old fine-grained name still works
        let call = handle(&mut b, &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"set_tempo","arguments":{"bpm":98}}})).unwrap();
        assert_eq!(call["result"]["isError"], false);
        assert_eq!(call["result"]["structuredContent"]["bpm"], 98.0);
        // legacy mode lists every engine tool
        assert_eq!(tools_list_mode(1)["tools"].as_array().unwrap().len(), tools::registry().len());
        let bad = handle(
            &mut b,
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope"}}),
        )
        .unwrap();
        assert_eq!(bad["result"]["isError"], true);
    }
}
