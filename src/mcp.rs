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

pub fn handle(backend: &mut Backend, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?; // notifications get no response
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let result: Result<Value, (i64, String)> = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("2025-06-18"),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "beatbox", "title": "Beatbox — AI-native beat maker", "version": env!("CARGO_PKG_VERSION") },
            "instructions": tools::GUIDE,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::tools_json() })),
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
        assert!(list["result"]["tools"].as_array().unwrap().len() >= 40);
        let call = handle(&mut b, &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"set_tempo","arguments":{"bpm":97}}})).unwrap();
        assert_eq!(call["result"]["isError"], false);
        assert_eq!(call["result"]["structuredContent"]["bpm"], 97.0);
        let bad = handle(
            &mut b,
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope"}}),
        )
        .unwrap();
        assert_eq!(bad["result"]["isError"], true);
    }
}
