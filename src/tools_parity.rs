//! MCP parity with the studio GUI: transport over the live link, the engine
//! history, and studio screenshots returned as MCP images.

use crate::engine::TransportCmd;
use crate::media;
use crate::tools::{f_opt, obj, s_opt, u_or, Tool};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn transport_state(e: &crate::engine::Engine) -> Value {
    let t = &e.transport;
    let bs = crate::ears::beat_secs(&e.project);
    json!({
        "studio_attached": t.attached, "playing": t.playing,
        "position_s": (t.position_s * 100.0).round() / 100.0,
        "position_beat": (t.position_s / bs * 100.0).round() / 100.0,
        "song_seconds": (e.project.song_seconds() * 100.0).round() / 100.0,
        "pending": t.pending.len(),
    })
}

/// Run `<exe> studio <project> --screenshot <png> --view <v>`, under
/// xvfb-run when there is no display.
fn studio_screenshot(project: &Path, png: &Path, view: &str) -> Result<()> {
    let exe = std::env::current_exe().context("locating the beatbox binary")?;
    let headless =
        std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none();
    let mut cmd = if headless {
        let mut c = std::process::Command::new("xvfb-run");
        c.args(["-a", "-s", "-screen 0 1480x920x24"]).arg(&exe);
        c
    } else {
        std::process::Command::new(&exe)
    };
    cmd.arg("studio")
        .arg(project)
        .arg("--screenshot")
        .arg(png)
        .arg("--view")
        .arg(view);
    cmd.env("BEATBOX_NO_LISTEN", "1");
    let out = cmd.output().with_context(|| {
        if headless {
            "running xvfb-run (install xvfb for headless screenshots)".to_string()
        } else {
            "starting the studio".to_string()
        }
    })?;
    if !png.exists() {
        bail!(
            "studio did not write a screenshot (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(400)
                .collect::<String>()
        );
    }
    Ok(())
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "transport",
            description: "Studio transport over the live link (beatbox mcp --connect to a running `beatbox studio`): action play | stop | toggle | seek (position_s or beat) | status. Returns playing state and the play-head position in seconds and beats. Without an attached studio the command is queued and studio_attached is false.",
            mutates: false,
            schema: || obj(json!({
                "action": {"type": "string", "enum": ["play", "stop", "toggle", "seek", "status"]},
                "position_s": {"type": "number"},
                "beat": {"type": "number", "description": "Seek to this song beat"},
                "section": {"description": "Seek to the start of this section (index or pattern name)", "type": ["string", "integer"]},
            }), &["action"]),
            run: |e, a| {
                let act = s_opt(a, "action").unwrap_or_default();
                let bs = crate::ears::beat_secs(&e.project);
                let seek_to = if let Some(s) = a.get("section").and_then(|v| v.as_str().map(String::from).or_else(|| v.as_u64().map(|n| n.to_string()))) {
                    Some(e.project.section_beats(&s)?.0 * bs)
                } else if let Some(b) = f_opt(a, "beat") {
                    Some(b * bs)
                } else {
                    f_opt(a, "position_s")
                };
                match act.as_str() {
                    "play" => {
                        if let Some(s) = seek_to { e.transport.pending.push(TransportCmd::Seek(s)); }
                        e.transport.pending.push(TransportCmd::Play);
                    }
                    "stop" => e.transport.pending.push(TransportCmd::Stop),
                    "toggle" => e.transport.pending.push(if e.transport.playing { TransportCmd::Stop } else { TransportCmd::Play }),
                    "seek" => e.transport.pending.push(TransportCmd::Seek(seek_to.ok_or_else(|| anyhow!("seek needs position_s, beat or section"))?)),
                    "status" => {}
                    other => bail!("unknown action '{other}' (play, stop, toggle, seek, status)"),
                }
                if e.transport.pending.len() > 32 { e.transport.pending.drain(..16); }
                let mut v = transport_state(e);
                if !e.transport.attached && act != "status" {
                    v["note"] = json!("no studio attached: run `beatbox studio` and connect with `beatbox mcp --connect`; the command is queued");
                }
                Ok(v)
            },
        },
        Tool {
            name: "get_history",
            description: "What happened so far: the engine activity log (tool, args summary, ok, source = mcp/ai/you/cli), revision, undo/redo depth and snapshot names. Use it to resume work, audit what another agent did, or decide how far to undo.",
            mutates: false,
            schema: || obj(json!({"limit": {"type": "integer", "description": "newest entries to return (default 30, max 500)"}, "errors_only": {"type": "boolean"}}), &[]),
            run: |e, a| {
                let lim = u_or(a, "limit", 30).clamp(1, 500) as usize;
                let only_err = a.get("errors_only").and_then(|v| v.as_bool()).unwrap_or(false);
                let entries: Vec<Value> = e.log.iter().enumerate().rev()
                    .filter(|(_, l)| !only_err || !l.ok)
                    .take(lim)
                    .map(|(i, l)| json!({"seq": i, "tool": l.tool, "args": l.summary, "ok": l.ok, "source": l.source}))
                    .collect();
                let (u, r) = e.undo_depth();
                Ok(json!({"revision": e.revision, "undo_depth": u, "redo_depth": r, "log_size": e.log.len(),
                    "snapshots": e.snapshots.iter().map(|(n, s)| json!({"name": n, "note": s.note, "revision": s.revision})).collect::<Vec<_>>(),
                    "entries": entries}))
            },
        },
        Tool {
            name: "screenshot",
            description: "Take a screenshot of Beatbox Studio showing the current project in a view (sequencer, mixer, automation): saves a PNG and returns it as an MCP image. Runs the studio offscreen (xvfb-run when headless); needs a build with the GUI feature.",
            mutates: false,
            schema: || obj(json!({
                "view": {"type": "string", "enum": ["sequencer", "mixer", "automation"], "description": "default sequencer"},
                "out": {"type": "string", "description": "PNG path (default renders/screenshot_<view>.png)"},
                "inline": {"type": "boolean", "description": "Return the image block (default true)"},
            }), &[]),
            run: |e, a| {
                let view = s_opt(a, "view").unwrap_or_else(|| "sequencer".into());
                let out: PathBuf = match s_opt(a, "out") { Some(p) => e.resolve(&p), None => e.renders_dir().join(format!("screenshot_{view}.png")) };
                if let Some(d) = out.parent() { std::fs::create_dir_all(d)?; }
                let _ = std::fs::remove_file(&out);
                let proj = std::env::temp_dir().join(format!("beatbox_shot_{}.json", std::process::id()));
                std::fs::write(&proj, serde_json::to_string(&e.project)?)?;
                let r = studio_screenshot(&proj, &out, &view);
                let _ = std::fs::remove_file(&proj);
                r?;
                let png = std::fs::read(&out)?;
                let (w, h) = media::png_dims(&png)?;
                let mut v = json!({"path": out, "view": view, "width": w, "height": h});
                let mut blocks = vec![media::link_block(&out, "image/png")];
                if a.get("inline").and_then(|x| x.as_bool()).unwrap_or(true) { blocks.insert(0, media::image_block(&png)); }
                media::attach(&mut v, blocks);
                Ok(v)
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use crate::engine::Engine;
    use serde_json::json;

    #[test]
    fn transport_queue_history_and_region_render() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_parity_tests"));
        e.call("generate_beat", &json!({"style": "house", "seed": 1}))
            .unwrap();
        let t = e
            .call("transport", &json!({"action": "play", "beat": 8}))
            .unwrap();
        assert_eq!(t["studio_attached"], false);
        assert_eq!(e.transport.pending.len(), 2);
        let h = e.call("get_history", &json!({"limit": 5})).unwrap();
        assert!(h["entries"].as_array().unwrap().len() >= 2);
        assert!(h["undo_depth"].as_u64().unwrap() >= 1);
        let r = e
            .call(
                "render",
                &json!({"path": "r.wav", "start_beat": 0, "end_beat": 4, "tail": 0.5}),
            )
            .unwrap();
        let want = 4.0 * 60.0 / e.project.bpm as f64 + 0.5;
        assert!((r["seconds"].as_f64().unwrap() - want).abs() < 0.11, "{r}");
    }
}
