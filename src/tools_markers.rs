//! Playlist markers (FL): named song positions an AI can navigate by
//! ("render from 'drop'", "the hook starts at marker 'hook2'"), with an
//! optional time-signature label. Section starts are offered as markers too.

use crate::engine::Engine;
use crate::project::Marker;
use crate::tools::{f_opt, obj, s_opt, s_req, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// Arrangement section starts as (name, beat).
fn section_starts(e: &Engine) -> Vec<(String, f32)> {
    let p = &e.project;
    let mut beat = 0.0f32;
    let mut v = Vec::new();
    for s in &p.arrangement {
        v.push((s.pattern.clone(), beat));
        let beats = p
            .patterns
            .iter()
            .find(|x| x.name == s.pattern)
            .map(|x| x.steps() as f32 / 4.0)
            .unwrap_or(16.0);
        beat += beats * s.repeats.max(1) as f32;
    }
    v
}

fn add_marker(e: &mut Engine, a: &Value) -> Result<Value> {
    let name = s_req(a, "name")?;
    let beat = match (f_opt(a, "beat"), f_opt(a, "bar")) {
        (Some(b), _) => b,
        (None, Some(bar)) => e.project.beat_at_bar(bar.max(1.0)),
        (None, None) => bail!("add_marker: give beat (song beats from 0) or bar (1-based)"),
    };
    if !beat.is_finite() || beat < 0.0 {
        bail!("add_marker: beat must be finite and >= 0");
    }
    let ts = s_opt(a, "time_signature");
    if let Some(t) = &ts {
        let ok = t
            .split_once('/')
            .map(|(n, d)| {
                n.trim()
                    .parse::<u32>()
                    .map(|n| (1..=32).contains(&n))
                    .unwrap_or(false)
                    && matches!(d.trim(), "2" | "4" | "8" | "16")
            })
            .unwrap_or(false);
        if !ok {
            bail!("time_signature '{t}' (like 4/4, 3/4, 6/8, 7/8)");
        }
    }
    e.project.markers.retain(|m| m.name != name);
    e.project.markers.push(Marker {
        name: name.clone(),
        beat,
        time_signature: ts,
    });
    e.project.markers.sort_by(|x, y| {
        x.beat
            .partial_cmp(&y.beat)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(
        json!({"added": name, "beat": beat, "bar": e.project.bar_at_beat(beat), "markers": e.project.markers.len()}),
    )
}

fn rename_marker(e: &mut Engine, a: &Value) -> Result<Value> {
    let name = s_req(a, "name")?;
    let new_name = s_req(a, "new_name")?.trim().to_string();
    if new_name.is_empty() {
        bail!("New marker name cannot be blank");
    }
    let Some(i) = e.project.markers.iter().position(|m| m.name == name) else {
        bail!("no marker '{name}'");
    };
    if new_name != name && e.project.markers.iter().any(|m| m.name == new_name) {
        bail!("A marker named '{new_name}' already exists");
    }
    e.project.markers[i].name = new_name.clone();
    Ok(json!({"renamed":name,"name":new_name,"beat":e.project.markers[i].beat}))
}

fn move_marker(e: &mut Engine, a: &Value) -> Result<Value> {
    let name = s_req(a, "name")?;
    let beat = f_opt(a, "beat").ok_or_else(|| anyhow::anyhow!("beat is required"))?;
    if !beat.is_finite() || beat < 0. {
        bail!("Marker beat must be finite and nonnegative");
    }
    let Some(m) = e.project.markers.iter_mut().find(|m| m.name == name) else {
        bail!("no marker '{name}'");
    };
    m.beat = beat;
    e.project.markers.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    Ok(json!({"moved":name,"beat":beat,"bar":e.project.bar_at_beat(beat)}))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "add_marker",
            description: "Add (or move) a named playlist marker at a song position: beat (quarter notes from 0) or bar (1-based). Optional time_signature label ('3/4', '6/8'; a label: set_time_signature makes a pattern play in it). Bars count each pattern's own bar length. Use marker beats with render start_beat/end_beat.",
            mutates: true,
            schema: || obj(json!({
                "name": {"type": "string"},
                "beat": {"type": "number"},
                "bar": {"type": "number", "description": "1-based bar (instead of beat)"},
                "time_signature": {"type": "string"}
            }), &["name"]),
            run: add_marker,
        },
        Tool {
            name: "list_markers",
            description: "List the playlist markers (name, beat, bar, seconds, time signature) plus every arrangement section start as an implicit marker.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| {
                let p = &e.project;
                let curve = crate::tempo_curve::TempoCurve::new(p);
                let m: Vec<Value> = e.project.markers.iter().map(|m| json!({"name": m.name, "beat": m.beat, "bar": e.project.bar_at_beat(m.beat), "seconds": (curve.secs_at_beat(m.beat as f64) * 100.0).round() / 100.0, "time_signature": m.time_signature})).collect();
                let s: Vec<Value> = section_starts(e).into_iter().map(|(n, b)| json!({"section": n, "beat": b, "bar": p.bar_at_beat(b), "seconds": (curve.secs_at_beat(b as f64) * 100.0).round() / 100.0})).collect();
                Ok(json!({"markers": m, "sections": s}))
            },
        },
        Tool {
            name:"move_marker",
            description:"Move an existing saved marker to a song beat (quarter notes from zero), preserving its name and time-signature label. No new marker is created; missing name or invalid beat rejects atomically. Undo restores its position.",
            mutates:true,
            schema: || obj(json!({"name":{"type":"string"},"beat":{"type":"number","minimum":0}}), &["name","beat"]),
            run:move_marker,
        },
        Tool {
            name: "rename_marker",
            description: "Rename a saved song marker without moving it or changing its time-signature label. Rejects blank or conflicting names. Undo restores the old name.",
            mutates: true,
            schema: || obj(json!({"name":{"type":"string"},"new_name":{"type":"string"}}), &["name","new_name"]),
            run: rename_marker,
        },
        Tool {
            name: "remove_marker",
            description: "Remove a playlist marker by name.",
            mutates: true,
            schema: || obj(json!({"name": {"type": "string"}}), &["name"]),
            run: |e, a| {
                let name = s_req(a, "name")?;
                let n = e.project.markers.len();
                e.project.markers.retain(|m| m.name != name);
                if e.project.markers.len() == n {
                    bail!("no marker '{name}' ({})", e.project.markers.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", "));
                }
                Ok(json!({"removed": name, "markers": e.project.markers.len()}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_add_list_remove() {
        let mut e = Engine::new(std::env::temp_dir().join("bb_marker_test"));
        e.call("add_marker", &json!({"name": "drop", "bar": 9}))
            .unwrap();
        e.call(
            "add_marker",
            &json!({"name": "intro", "beat": 0, "time_signature": "3/4"}),
        )
        .unwrap();
        let l = e.call("list_markers", &json!({})).unwrap();
        assert_eq!(l["markers"][0]["name"], "intro");
        assert_eq!(l["markers"][1]["beat"], 32.0);
        assert!(e
            .call(
                "add_marker",
                &json!({"name": "x", "beat": 1, "time_signature": "3/5"})
            )
            .is_err());
        e.call("remove_marker", &json!({"name": "drop"})).unwrap();
        assert!(e.call("remove_marker", &json!({"name": "drop"})).is_err());
    }
}

#[cfg(test)]
mod timing_tests {
    use super::*;
    #[test]
    fn markers_and_sections_follow_mixed_meters_and_tempo() {
        let mut e = Engine::new(std::env::temp_dir().join("marker-timing-tests"));
        e.call("add_pattern", &json!({"name":"waltz","bars":2}))
            .unwrap();
        e.call("add_pattern", &json!({"name":"odd","bars":1}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"waltz","time_signature":"3/4"}),
        )
        .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":"waltz","repeats":1},{"pattern":"odd","repeats":2}]}),
        )
        .unwrap();
        e.project.bpm = 120.0;
        e.project.tempo_points = vec![crate::project::TempoPoint {
            beat: 4.0,
            bpm: 60.0,
            ramp: false,
        }];
        let b = serde_json::to_value(&e.project).unwrap();
        e.call("add_marker", &json!({"name":"drop","bar":3}))
            .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        let l = e.call("list_markers", &json!({})).unwrap();
        assert_eq!(l["markers"][0]["beat"], 6.0);
        assert_eq!(l["markers"][0]["bar"], 3.0);
        assert_eq!(l["markers"][0]["seconds"], 4.0);
        assert_eq!(l["sections"][1]["bar"], 3.0);
        assert_eq!(l["sections"][1]["seconds"], 4.0);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        e.call("add_marker", &json!({"name":"drop","beat":9.5}))
            .unwrap();
        assert_eq!(e.project.markers.len(), 1);
        e.call("remove_marker", &json!({"name":"drop"})).unwrap();
        assert!(e.project.markers.is_empty());
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project.markers[0].beat, 9.5);
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call("add_marker", &json!({"name":"bad","beat":-1}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
}

#[cfg(test)]
mod rename_tests {
    use super::*;
    #[test]
    fn renaming_retains_position_label_and_complete_history() {
        let mut e = Engine::new(std::env::temp_dir().join("marker-rename-test"));
        e.call(
            "add_marker",
            &json!({"name":"old","beat":4.5,"time_signature":"7/8"}),
        )
        .unwrap();
        e.call("add_marker", &json!({"name":"other","beat":8.0}))
            .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        e.call(
            "rename_marker",
            &json!({"name":"old","new_name":"  Drop  "}),
        )
        .unwrap();
        let after = serde_json::to_value(&e.project).unwrap();
        let mut expected = before.clone();
        expected["markers"][0]["name"] = json!("Drop");
        assert_eq!(after, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        for a in [
            json!({"name":"Drop","new_name":"other"}),
            json!({"name":"Drop","new_name":"   "}),
            json!({"name":"missing","new_name":"new"}),
        ] {
            assert!(e.call("rename_marker", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
        }
        let history = e.history().0.len();
        e.call("rename_marker", &json!({"name":"Drop","new_name":"Drop"}))
            .unwrap();
        assert_eq!(e.history().0.len(), history);
        let path = e.workdir.join("roundtrip.json");
        e.call("save_project", &json!({"path":path})).unwrap();
        e.call("load_project", &json!({"path":path})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), after);
    }
}

#[cfg(test)]
mod move_marker_tests {
    use super::*;
    #[test]
    fn move_preserves_metadata_order_and_exact_history() {
        let dir = std::env::temp_dir().join("move-marker-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call(
            "add_marker",
            &json!({"name":"Drop","beat":2,"time_signature":"7/8"}),
        )
        .unwrap();
        e.call("add_marker", &json!({"name":"End","beat":8}))
            .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        e.call(
            "arrange",
            &json!({"action":"move_marker","name":"Drop","beat":12}),
        )
        .unwrap();
        let a = serde_json::to_value(&e.project).unwrap();
        let mut expected = b.clone();
        expected["markers"][0]["beat"] = json!(12.);
        expected["markers"].as_array_mut().unwrap().swap(0, 1);
        assert_eq!(a, expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        for args in [
            json!({"name":"Drop","beat":-1}),
            json!({"name":"missing","beat":1}),
            json!({"name":"Drop"}),
        ] {
            assert!(e.call("move_marker", &args).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
        }
        let len = e.history().0.len();
        e.call("move_marker", &json!({"name":"Drop","beat":12}))
            .unwrap();
        assert_eq!(e.history().0.len(), len);
        e.call("save_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        e.call("load_project", &json!({"path":"roundtrip.json"}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), a);
    }
}
