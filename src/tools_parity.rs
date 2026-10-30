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
    let curve = crate::tempo_curve::TempoCurve::new(&e.project);
    json!({
        "studio_attached": t.attached, "playing": t.playing,
        "position_s": (t.position_s * 100.0).round() / 100.0,
        "position_beat": (curve.beat_at_secs(t.position_s as f64) * 100.0).round() / 100.0,
        "song_seconds": (curve.secs_at_beat(e.project.song_beats() as f64) * 100.0).round() / 100.0,
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
    run_screenshot_command(&mut cmd, png, headless)
}

// In headless mode the collected status is xvfb-run's, not a separate GUI status.
fn run_screenshot_command(
    cmd: &mut std::process::Command,
    png: &Path,
    headless: bool,
) -> Result<()> {
    let out = cmd.output().with_context(|| {
        if headless {
            "running xvfb-run (install xvfb for headless screenshots)".to_string()
        } else {
            "starting the studio".to_string()
        }
    })?;
    let failed = !out.status.success();
    if failed || !png.exists() {
        // This bounds the diagnostic, not Command::output's captured buffers.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let count = stderr.chars().count();
        let diagnostic = if count == 0 {
            "<empty>".to_string()
        } else if count <= 1600 {
            stderr.into_owned()
        } else {
            let mut diagnostic: String = stderr.chars().take(400).collect();
            diagnostic.push_str(&format!("\n[omitted {} stderr characters]\n", count - 1600));
            diagnostic.extend(stderr.chars().skip(count - 1200));
            diagnostic
        };
        if failed {
            bail!(
                "screenshot command failed ({}); stderr: {}",
                out.status,
                diagnostic
            );
        }
        bail!(
            "studio did not write a screenshot ({}); stderr: {}",
            out.status,
            diagnostic
        );
    }
    Ok(())
}

fn perf_state(e: &crate::engine::Engine) -> Value {
    let t = &e.transport;
    let waiting = t
        .pending_launch
        .as_ref()
        .map(|l| l.pattern.clone().unwrap_or_else(|| "song".into()));
    json!({
        "studio_attached": t.attached, "playing": t.playing,
        "performing": t.perf_playing, "queued": t.perf_queued.clone().or(waiting),
        "patterns": e.project.patterns.iter().map(|p| json!({"id": p.id, "name": p.name, "bars": p.bars})).collect::<Vec<_>>(),
    })
}

/// A pattern by name, id, or 0-based index.
fn perf_pattern(p: &crate::project::Project, key: &str) -> Result<usize> {
    p.pattern_index(key)
        .or_else(|err| match key.trim().parse::<usize>() {
            Ok(i) if i < p.patterns.len() => Ok(i),
            _ => Err(err),
        })
}

/// Seconds between launch points: one bar of the pattern, one beat, or 0.
fn perf_quantum(p: &crate::project::Project, pat: Option<usize>, q: &str) -> Result<f32> {
    let spb = pat.map_or(crate::project::STEPS_PER_BAR, |i| {
        p.patterns[i].steps_per_bar()
    });
    Ok(match q {
        "bar" => spb as f32 * p.step_secs(),
        "beat" => 4.0 * p.step_secs(),
        "now" => 0.0,
        other => bail!("unknown quantize '{other}' (bar, beat, now)"),
    })
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "perform",
            description: "Performance mode over the live link: loop patterns live and switch between them in time. action launch (pattern = name, id or index; starts looping it, or switches on the next bar while something plays) | exit (back to the song arrangement on the next bar) | prepare (render pattern loops ahead so launches are instant; all patterns unless pattern is given) | status. quantize bar (default) | beat | now. The project is not changed. Without an attached studio the launch is queued and studio_attached is false.",
            mutates: false,
            schema: || obj(json!({
                "action": {"type": "string", "enum": ["launch", "exit", "prepare", "status"]},
                "pattern": {"type": "string", "description": "Pattern name, id or 0-based index"},
                "quantize": {"type": "string", "enum": ["bar", "beat", "now"], "description": "When the switch happens (default bar)"},
            }), &["action"]),
            run: |e, a| {
                let act = s_opt(a, "action").unwrap_or_default();
                let q = s_opt(a, "quantize").unwrap_or_else(|| "bar".into());
                let mut extra = json!({});
                match act.as_str() {
                    "launch" => {
                        let key = s_opt(a, "pattern").ok_or_else(|| anyhow!("launch needs pattern (name or id)"))?;
                        let i = perf_pattern(&e.project, &key)?;
                        let quantum = perf_quantum(&e.project, Some(i), &q)?;
                        let name = e.project.patterns[i].name.clone();
                        let (mix, loop_secs) = e.render_pattern_loop(&name)?;
                        extra = json!({"launched": name, "loop_secs": (loop_secs * 1000.0).round() / 1000.0,
                            "loop_bars": e.project.patterns[i].bars, "quantize": q,
                            "switch_in": if e.transport.playing && quantum > 0.0 { format!("next {q}") } else { "now".into() }});
                        e.transport.pending_launch = Some(crate::engine::PerfLaunch { pattern: Some(name), mix: Some(mix), loop_secs, quantum_secs: quantum });
                    }
                    "exit" => {
                        let quantum = perf_quantum(&e.project, None, &q)?;
                        e.transport.pending_launch = Some(crate::engine::PerfLaunch { pattern: None, mix: None, loop_secs: e.project.song_seconds(), quantum_secs: quantum });
                        extra = json!({"exit": true, "quantize": q});
                    }
                    "prepare" => {
                        let keys: Vec<String> = match s_opt(a, "pattern") {
                            Some(k) => vec![e.project.patterns[perf_pattern(&e.project, &k)?].name.clone()],
                            None => e.project.patterns.iter().map(|p| p.name.clone()).collect(),
                        };
                        let t0 = std::time::Instant::now();
                        for k in &keys { e.render_pattern_loop(k)?; }
                        extra = json!({"prepared": keys, "seconds": (t0.elapsed().as_secs_f32() * 100.0).round() / 100.0});
                    }
                    "status" => {}
                    other => bail!("unknown action '{other}' (launch, exit, prepare, status)"),
                }
                let mut v = perf_state(e);
                if let (Some(o), Some(x)) = (v.as_object_mut(), extra.as_object()) {
                    for (k, val) in x { o.insert(k.clone(), val.clone()); }
                }
                if !e.transport.attached && matches!(act.as_str(), "launch" | "exit") {
                    v["note"] = json!("no studio attached: run `beatbox studio` and connect with `beatbox mcp --connect`; the launch is queued");
                }
                Ok(v)
            },
        },
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
                let curve = crate::tempo_curve::TempoCurve::new(&e.project);
                let seek_to = if let Some(s) = a.get("section").and_then(|v| v.as_str().map(String::from).or_else(|| v.as_u64().map(|n| n.to_string()))) {
                    Some(curve.secs_at_beat(e.project.section_beats(&s)?.0 as f64) as f32)
                } else if let Some(b) = f_opt(a, "beat") {
                    Some(curve.secs_at_beat(b as f64) as f32)
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
            description: "Take a screenshot of Beatbox Studio showing the current project in a view (sequencer, mixer, automation, playlist, vocal, browser): saves a PNG and returns it as an MCP image. Runs the studio offscreen (xvfb-run when headless); needs a build with the GUI feature.",
            mutates: false,
            schema: || obj(json!({
                "view": {"type": "string", "enum": ["sequencer", "mixer", "automation", "playlist", "vocal", "browser"], "description": "default sequencer"},
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
    fn perform_launches_a_pattern_loop_on_the_bar_grid() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_perform_tests"));
        e.call("generate_beat", &json!({"style": "house", "seed": 2}))
            .unwrap();
        let before = serde_json::to_string(&e.project).unwrap();
        let rev = e.revision;
        let pat = e.project.patterns[0].clone();
        let r = e
            .call("perform", &json!({"action": "launch", "pattern": pat.name}))
            .unwrap();
        assert_eq!(r["launched"], pat.name.as_str());
        assert_eq!(r["studio_attached"], false);
        let l = e.transport.pending_launch.clone().expect("launch queued");
        let bar = pat.steps_per_bar() as f32 * e.project.step_secs();
        assert!((l.quantum_secs - bar).abs() < 1e-4);
        assert!(
            (l.loop_secs - pat.bars as f32 * bar).abs() < 1e-3,
            "{} vs {}",
            l.loop_secs,
            pat.bars as f32 * bar
        );
        let m = l.mix.unwrap();
        assert!(m.left.len() as f32 >= l.loop_secs * crate::dsp::SR * 0.99);
        assert!(m.left.iter().any(|x| x.abs() > 1e-3), "loop is silent");
        // by id, beat grid, cached second time
        let r = e
            .call(
                "perform",
                &json!({"action": "launch", "pattern": pat.id, "quantize": "beat"}),
            )
            .unwrap();
        assert_eq!(r["queued"], pat.name.as_str());
        assert!(
            (e.transport.pending_launch.as_ref().unwrap().quantum_secs
                - 4.0 * e.project.step_secs())
            .abs()
                < 1e-4
        );
        assert!(e
            .call("perform", &json!({"action": "launch", "pattern": "nope"}))
            .is_err());
        assert_eq!(
            e.call(
                "perform",
                &json!({"action": "launch", "pattern": "0", "quantize": "now"})
            )
            .unwrap()["launched"],
            pat.name.as_str()
        );
        assert!(e
            .call(
                "perform",
                &json!({"action": "launch", "pattern": pat.name, "quantize": "2bars"})
            )
            .is_err());
        let x = e.call("perform", &json!({"action": "exit"})).unwrap();
        assert_eq!(x["queued"], "song");
        assert!(e
            .transport
            .pending_launch
            .as_ref()
            .unwrap()
            .pattern
            .is_none());
        let pr = e.call("perform", &json!({"action": "prepare"})).unwrap();
        assert_eq!(
            pr["prepared"].as_array().unwrap().len(),
            e.project.patterns.len()
        );
        assert_eq!(
            serde_json::to_string(&e.project).unwrap(),
            before,
            "perform must not edit the project"
        );
        assert_eq!(e.revision, rev);
    }

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

    #[test]
    fn stable_effect_ids_survive_reorder_and_migrate_old_projects() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_id_tests"));
        e.call("add_track", &json!({"name": "pad", "preset": "warm_pad"}))
            .unwrap();
        e.call("add_effect", &json!({"track": "pad", "type": "filter"}))
            .unwrap();
        e.call("add_effect", &json!({"track": "pad", "type": "reverb"}))
            .unwrap();
        e.call("add_effect", &json!({"track": "pad", "type": "filter"}))
            .unwrap();
        let ids: Vec<String> = e.project.tracks[0]
            .effects
            .iter()
            .map(|f| f.id().to_string())
            .collect();
        assert_eq!(ids, vec!["filter1", "reverb1", "filter2"]);
        // address by id, then reorder: the id follows the effect
        e.call(
            "tweak_effect",
            &json!({"track": "pad", "index": "reverb1", "params": {"mix": 0.5}}),
        )
        .unwrap();
        e.call(
            "reorder_effects",
            &json!({"track": "pad", "from": "reverb1", "to": 0}),
        )
        .unwrap();
        assert_eq!(e.project.tracks[0].effects[0].id(), "reverb1");
        e.call(
            "tweak_effect",
            &json!({"track": "pad", "index": "filter2", "params": {"cutoff": 900}}),
        )
        .unwrap();
        let f2 = e.project.tracks[0]
            .effects
            .iter()
            .find(|f| f.id() == "filter2")
            .unwrap();
        assert!(
            serde_json::to_value(f2).unwrap()["cutoff"]
                .as_f64()
                .unwrap()
                == 900.0
        );
        // automation by id
        e.call(
            "add_automation",
            &json!({"track": "pad", "param": "fx.filter1.cutoff", "points": [[0, 200], [8, 4000]]}),
        )
        .unwrap();
        assert!(
            e.project
                .automation
                .iter()
                .any(|l| l.param.starts_with("fx.1.")),
            "{:?}",
            e.project.automation
        );
        assert!(e
            .call("bypass_effect", &json!({"track": "pad", "index": "nope9"}))
            .is_err());
        // a legacy project (no ids) is migrated on load
        let mut legacy = serde_json::to_value(&e.project).unwrap();
        for f in legacy["tracks"][0]["effects"].as_array_mut().unwrap() {
            f.as_object_mut().unwrap().remove("id");
        }
        let path = std::env::temp_dir().join("beatbox_id_tests/legacy.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, legacy.to_string()).unwrap();
        e.call("load_project", &json!({"path": path})).unwrap();
        assert!(e.project.tracks[0]
            .effects
            .iter()
            .all(|f| !f.id().is_empty()));
    }
}

#[cfg(test)]
mod marker_transport_timing_tests {
    use super::*;
    #[test]
    fn transport_status_and_seeks_follow_tempo_curve() {
        let mut e = crate::engine::Engine::new(std::env::temp_dir().join("marker-transport"));
        e.call("add_pattern", &json!({"name":"next","bars":1}))
            .unwrap();
        let first = e.project.patterns[0].name.clone();
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":first,"repeats":1},{"pattern":"next","repeats":1}]}),
        )
        .unwrap();
        e.project.bpm = 120.0;
        e.project.tempo_points = vec![crate::project::TempoPoint {
            beat: 4.0,
            bpm: 60.0,
            ramp: false,
        }];
        e.transport.position_s = 12.0;
        let v = e.call("transport", &json!({"action":"status"})).unwrap();
        assert_eq!(v["position_beat"], 14.0);
        let curve = crate::tempo_curve::TempoCurve::new(&e.project);
        assert_eq!(
            v["song_seconds"],
            json!((curve.secs_at_beat(e.project.song_beats() as f64) * 100.0).round() / 100.0)
        );
        let before = serde_json::to_value(&e.project).unwrap();
        for (args, beat) in [
            (json!({"action":"seek","beat":14}), 14.0),
            (json!({"action":"seek","section":"next"}), 16.0),
        ] {
            e.call("transport", &args).unwrap();
            match e.transport.pending.last().unwrap() {
                TransportCmd::Seek(sec) => {
                    assert!((*sec - curve.secs_at_beat(beat) as f32).abs() < 0.0001)
                }
                _ => panic!("expected seek"),
            }
        }
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }
}

#[cfg(test)]
mod screenshot_process_tests {
    use super::run_screenshot_command;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    // A valid 1x1 PNG, not just the PNG signature.
    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4,
        0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5,
        1, 1, 39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            for _ in 0..64 {
                let path = std::env::temp_dir().join(format!(
                    "beatbox-shot-status-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("scratch directory: {e}"),
                }
            }
            panic!("could not allocate a unique scratch directory");
        }
        fn png(&self) -> PathBuf {
            self.0.join("shot.png")
        }
        fn child(&self, mode: &str) -> Command {
            let mut cmd = Command::new(std::env::current_exe().unwrap());
            cmd.args([
                "--exact",
                "tools_parity::screenshot_process_tests::screenshot_child_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("BEATBOX_SCREENSHOT_TEST_CHILD", mode)
            .env("BEATBOX_SCREENSHOT_TEST_DIR", &self.0);
            cmd
        }
        fn run(&self, mode: &str) -> anyhow::Result<()> {
            run_screenshot_command(&mut self.child(mode), &self.png(), false)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // Added 2026-10-29: adds one ignored fixture per feature campaign, not a
    // missing GUI acceptance test. Only the exact child invocation executes it.
    #[test]
    #[ignore = "child-only fixture for screenshot command tests"]
    fn screenshot_child_fixture() {
        let Ok(mode) = std::env::var("BEATBOX_SCREENSHOT_TEST_CHILD") else {
            return;
        };
        let dir = PathBuf::from(std::env::var_os("BEATBOX_SCREENSHOT_TEST_DIR").unwrap());
        assert!(dir.is_absolute() && dir.is_dir());
        if matches!(
            mode.as_str(),
            "success_png" | "nonzero_png" | "nonzero_long_png"
        ) {
            std::fs::write(dir.join("shot.png"), PNG).unwrap();
        }
        let (stdout, stderr) = match mode.as_str() {
            "success_png" | "nonzero_png" | "success_missing" | "nonzero_missing" => (
                b"stdout-must-stay-private".to_vec(),
                b"fixture diagnostic".to_vec(),
            ),
            "empty" => (Vec::new(), Vec::new()),
            "lossy" => {
                let mut bytes = vec![0xff];
                bytes.extend("界".repeat(405).as_bytes());
                (b"stdout-must-stay-private".to_vec(), bytes)
            }
            mode if mode.starts_with("boundary_") => {
                let count: usize = mode.strip_prefix("boundary_").unwrap().parse().unwrap();
                (
                    b"stdout-must-stay-private".to_vec(),
                    "界".repeat(count).into_bytes(),
                )
            }
            "long" | "success_long_missing" | "nonzero_long_png" | "middle_only" => {
                let (head, middle, tail) = long_stderr_regions(mode == "middle_only");
                let mut bytes = head.into_bytes();
                bytes.extend(middle.as_bytes());
                // One malformed byte decodes to one replacement scalar in the omitted middle.
                bytes.push(0xff);
                bytes.extend(tail.as_bytes());
                (b"stdout-must-stay-private".to_vec(), bytes)
            }
            "streams" => (vec![b'o'; 128 * 1024], vec![b'e'; 128 * 1024]),
            other => panic!("unknown child fixture mode: {other}"),
        };
        std::io::stdout().write_all(&stdout).unwrap();
        std::io::stdout().flush().unwrap();
        std::io::stderr().write_all(&stderr).unwrap();
        std::io::stderr().flush().unwrap();
        std::process::exit(if mode.starts_with("success_") { 0 } else { 23 });
    }

    fn long_stderr_regions(middle_only: bool) -> (String, String, String) {
        let reason = "screenshot FAILED: synthetic terminal reason";
        let head = "界🙂".repeat(200);
        let middle = if middle_only {
            "screenshot FAILED: middle-only-synthetic"
        } else {
            "omitted-middle-private"
        };
        let tail = if middle_only {
            "終🙂".repeat(600)
        } else {
            format!("{}{}", "終".repeat(1200 - reason.chars().count()), reason)
        };
        (head, middle.to_string(), tail)
    }

    fn assert_failure(error: &anyhow::Error) {
        let text = error.to_string();
        assert!(text.starts_with("screenshot command failed ("), "{text}");
        assert!(text.contains("23"), "{text}");
        assert!(!text.contains("stdout-must-stay-private"), "{text}");
        assert!(!text.contains("did not write"), "{text}");
    }
    #[test]
    fn screenshot_child_success_with_png() {
        let scratch = Scratch::new();
        scratch.run("success_png").unwrap();
        assert_eq!(std::fs::read(scratch.png()).unwrap(), PNG);
    }
    #[test]
    fn screenshot_child_nonzero_with_png_is_error() {
        let scratch = Scratch::new();
        let status = scratch.child("nonzero_png").output().unwrap().status;
        std::fs::remove_file(scratch.png()).unwrap();
        assert!(!scratch.png().exists());
        let error = scratch.run("nonzero_png").unwrap_err();
        assert_failure(&error);
        assert!(error
            .to_string()
            .starts_with(&format!("screenshot command failed ({status}); stderr: ")));
        assert!(error.to_string().contains("; stderr: fixture diagnostic"));
        assert_eq!(std::fs::read(scratch.png()).unwrap(), PNG);
    }
    #[test]
    fn screenshot_child_nonzero_with_preexisting_png_is_error() {
        let scratch = Scratch::new();
        std::fs::write(scratch.png(), PNG).unwrap();
        assert_failure(&scratch.run("nonzero_missing").unwrap_err());
        assert_eq!(std::fs::read(scratch.png()).unwrap(), PNG);
    }
    #[test]
    fn screenshot_child_success_without_png_is_error() {
        let scratch = Scratch::new();
        let text = scratch.run("success_missing").unwrap_err().to_string();
        assert!(
            text.starts_with("studio did not write a screenshot ("),
            "{text}"
        );
        assert!(text.contains("; stderr: fixture diagnostic"));
        assert!(!scratch.png().exists());
    }
    #[test]
    fn screenshot_child_nonzero_without_png_is_error() {
        let scratch = Scratch::new();
        assert_failure(&scratch.run("nonzero_missing").unwrap_err());
        assert!(!scratch.png().exists());
    }
    #[test]
    fn screenshot_child_stderr_is_lossy_bounded_and_labelled() {
        let scratch = Scratch::new();
        let error = scratch.run("lossy").unwrap_err();
        assert_failure(&error);
        let text = error.to_string();
        let diagnostic = text.split_once("; stderr: ").unwrap().1;
        assert_eq!(diagnostic, format!("\u{fffd}{}", "界".repeat(405)));
    }
    #[test]
    fn screenshot_child_empty_stderr_is_labelled() {
        let scratch = Scratch::new();
        let error = scratch.run("empty").unwrap_err();
        assert_failure(&error);
        assert!(error.to_string().ends_with("; stderr: <empty>"));
    }
    #[test]
    fn screenshot_child_spawn_error_keeps_context() {
        let scratch = Scratch::new();
        for (headless, context) in [
            (false, "starting the studio"),
            (
                true,
                "running xvfb-run (install xvfb for headless screenshots)",
            ),
        ] {
            let mut cmd = Command::new(scratch.0.join("absent-executable"));
            let error = run_screenshot_command(&mut cmd, &scratch.png(), headless).unwrap_err();
            assert_eq!(error.to_string(), context);
            assert!(error.chain().count() >= 2);
        }
    }
    #[test]
    fn screenshot_child_collects_both_modest_streams() {
        let scratch = Scratch::new();
        let error = scratch.run("streams").unwrap_err();
        assert_failure(&error);
        assert!(!error.to_string().contains(&"o".repeat(400)));
        assert_eq!(
            error.to_string().split_once("; stderr: ").unwrap().1,
            format!(
                "{}\n[omitted 129472 stderr characters]\n{}",
                "e".repeat(400),
                "e".repeat(1200)
            )
        );
    }
    #[test]
    fn screenshot_child_stderr_scalar_boundaries() {
        for count in [0, 399, 400, 401, 1599, 1600, 1601] {
            let scratch = Scratch::new();
            let error = scratch.run(&format!("boundary_{count}")).unwrap_err();
            assert_failure(&error);
            let text = error.to_string();
            let diagnostic = text.split_once("; stderr: ").unwrap().1;
            let expected = match count {
                0 => "<empty>".to_string(),
                1601 => format!(
                    "{}\n[omitted 1 stderr characters]\n{}",
                    "界".repeat(400),
                    "界".repeat(1200)
                ),
                _ => "界".repeat(count),
            };
            assert_eq!(diagnostic, expected, "scalar count {count}");
            assert!(!diagnostic.contains("screenshot FAILED"));
        }
    }

    #[test]
    fn screenshot_child_long_stderr_keeps_head_tail_not_middle_or_stdout() {
        for mode in [
            "long",
            "success_long_missing",
            "nonzero_long_png",
            "middle_only",
        ] {
            let scratch = Scratch::new();
            let error = scratch.run(mode).unwrap_err();
            let text = error.to_string();
            if mode == "success_long_missing" {
                assert!(text.starts_with("studio did not write a screenshot ("));
            } else {
                assert_failure(&error);
            }
            let (head, middle, tail) = long_stderr_regions(mode == "middle_only");
            let diagnostic = text.split_once("; stderr: ").unwrap().1;
            assert_eq!(
                diagnostic,
                format!(
                    "{head}\n[omitted {} stderr characters]\n{tail}",
                    middle.chars().count() + 1
                )
            );
            assert!(!diagnostic.contains(&middle));
            assert!(!text.contains("stdout-must-stay-private"));
            assert!(!diagnostic.contains('\u{fffd}'));
            assert_eq!(
                diagnostic.contains("screenshot FAILED:"),
                mode != "middle_only"
            );
            if mode == "nonzero_long_png" {
                assert_eq!(std::fs::read(scratch.png()).unwrap(), PNG);
            } else {
                assert!(!scratch.png().exists());
            }
        }
    }
}

#[cfg(test)]
mod screenshot_contract_tests {
    #[test]
    fn screenshot_schema_is_unchanged() {
        let tool = super::tools()
            .into_iter()
            .find(|t| t.name == "screenshot")
            .unwrap();
        assert!(!tool.mutates);
        assert_eq!(
            (tool.schema)(),
            super::obj(
                serde_json::json!({
                    "view": {"type": "string", "enum": ["sequencer", "mixer", "automation", "playlist", "vocal", "browser"], "description": "default sequencer"},
                    "out": {"type": "string", "description": "PNG path (default renders/screenshot_<view>.png)"},
                    "inline": {"type": "boolean", "description": "Return the image block (default true)"},
                }),
                &[]
            )
        );
    }
    #[test]
    fn screenshot_source_preserves_routing_cleanup_and_success_contract() {
        let source = include_str!("tools_parity.rs");
        let wrapper = source
            .split("fn studio_screenshot(")
            .nth(1)
            .unwrap()
            .split("fn run_screenshot_command(")
            .next()
            .unwrap();
        for part in [
            "cmd.arg(\"studio\")",
            ".arg(project)",
            ".arg(\"--screenshot\")",
            ".arg(png)",
            ".arg(\"--view\")",
            ".arg(view)",
            "cmd.env(\"BEATBOX_NO_LISTEN\", \"1\")",
            "run_screenshot_command(&mut cmd, png, headless)",
        ] {
            assert!(wrapper.contains(part), "{part}");
        }
        let helper = source
            .split("fn run_screenshot_command(")
            .nth(1)
            .unwrap()
            .split("fn perf_state(")
            .next()
            .unwrap();
        assert!(
            helper.find("!out.status.success()").unwrap() < helper.find("!png.exists()").unwrap()
        );
        let handler = source
            .split("name: \"screenshot\"")
            .nth(1)
            .unwrap()
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(handler.find("remove_file(&proj)").unwrap() < handler.find("r?;").unwrap());
        for part in [
            "media::png_dims(&png)?",
            "json!({\"path\": out, \"view\": view, \"width\": w, \"height\": h})",
            "media::link_block(&out, \"image/png\")",
            "media::image_block(&png)",
            "media::attach(&mut v, blocks)",
        ] {
            assert!(handler.contains(part), "{part}");
        }
    }
}
