//! Local control server: lets `beatbox mcp --connect` (and any script) drive a
//! running studio over HTTP on 127.0.0.1, so AI edits show up live.

use crate::engine::Engine;
use crate::tools;
use serde_json::{json, Value};
use std::io::Read;
use std::sync::{Arc, Mutex};

pub const DEFAULT_ADDR: &str = "127.0.0.1:7878";

/// Spawn the control server on a background thread.
pub fn spawn(engine: Arc<Mutex<Engine>>, addr: &str) -> anyhow::Result<()> {
    let server = tiny_http::Server::http(addr).map_err(|e| anyhow::anyhow!("bind {addr}: {e}"))?;
    std::thread::Builder::new()
        .name("beatbox-control".into())
        .spawn(move || {
            for mut req in server.incoming_requests() {
                let url = req.url().to_string();
                let (status, body) = match (req.method(), url.as_str()) {
                    (tiny_http::Method::Get, "/tools") => (200, tools::tools_json()),
                    (tiny_http::Method::Get, "/health") => {
                        (200, json!({"ok": true, "app": "beatbox-studio"}))
                    }
                    (tiny_http::Method::Post, "/call") => {
                        let mut s = String::new();
                        let _ = req.as_reader().take(8 * 1024 * 1024).read_to_string(&mut s);
                        match serde_json::from_str::<Value>(&s) {
                            Ok(v) => {
                                let name = v["name"].as_str().unwrap_or("").to_string();
                                let args = v.get("arguments").cloned().unwrap_or(json!({}));
                                let mut e = engine.lock().unwrap();
                                match e.call_from(&name, &args, "ai") {
                                    Ok(r) => (200, json!({"ok": true, "result": r})),
                                    Err(err) => {
                                        (200, json!({"ok": false, "error": format!("{err:#}")}))
                                    }
                                }
                            }
                            Err(e) => {
                                (400, json!({"ok": false, "error": format!("bad json: {e}")}))
                            }
                        }
                    }
                    _ => (404, json!({"ok": false, "error": "not found"})),
                };
                let resp = tiny_http::Response::from_string(body.to_string())
                    .with_status_code(status)
                    .with_header(
                        "Content-Type: application/json"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    );
                let _ = req.respond(resp);
            }
        })?;
    Ok(())
}
