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
        let bars = p.patterns.iter().find(|x| x.name == s.pattern).map(|x| x.bars).unwrap_or(4) as f32;
        beat += bars * 4.0 * s.repeats.max(1) as f32;
    }
    v
}

fn add_marker(e: &mut Engine, a: &Value) -> Result<Value> {
    let name = s_req(a, "name")?;
    let beat = match (f_opt(a, "beat"), f_opt(a, "bar")) {
        (Some(b), _) => b,
        (None, Some(bar)) => (bar - 1.0).max(0.0) * 4.0,
        (None, None) => bail!("add_marker: give beat (song beats from 0) or bar (1-based)"),
    };
    if beat < 0.0 {
        bail!("add_marker: beat must be >= 0");
    }
    let ts = s_opt(a, "time_signature");
    if let Some(t) = &ts {
        let ok = t.split_once('/').map(|(n, d)| n.trim().parse::<u32>().map(|n| (1..=32).contains(&n)).unwrap_or(false) && matches!(d.trim(), "2" | "4" | "8" | "16")).unwrap_or(false);
        if !ok {
            bail!("time_signature '{t}' (like 4/4, 3/4, 6/8, 7/8)");
        }
    }
    e.project.markers.retain(|m| m.name != name);
    e.project.markers.push(Marker { name: name.clone(), beat, time_signature: ts });
    e.project.markers.sort_by(|x, y| x.beat.partial_cmp(&y.beat).unwrap_or(std::cmp::Ordering::Equal));
    Ok(json!({"added": name, "beat": beat, "bar": beat / 4.0 + 1.0, "markers": e.project.markers.len()}))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "add_marker",
            description: "Add (or move) a named playlist marker at a song position: beat (quarter notes from 0) or bar (1-based). Optional time_signature label ('3/4', '6/8'; metadata, the renderer counts 4/4). Use marker beats with render start_beat/end_beat.",
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
                let spb = 60.0 / e.project.bpm;
                let m: Vec<Value> = e.project.markers.iter().map(|m| json!({"name": m.name, "beat": m.beat, "bar": m.beat / 4.0 + 1.0, "seconds": (m.beat * spb * 100.0).round() / 100.0, "time_signature": m.time_signature})).collect();
                let s: Vec<Value> = section_starts(e).into_iter().map(|(n, b)| json!({"section": n, "beat": b, "bar": b / 4.0 + 1.0, "seconds": (b * spb * 100.0).round() / 100.0})).collect();
                Ok(json!({"markers": m, "sections": s}))
            },
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
        e.call("add_marker", &json!({"name": "drop", "bar": 9})).unwrap();
        e.call("add_marker", &json!({"name": "intro", "beat": 0, "time_signature": "3/4"})).unwrap();
        let l = e.call("list_markers", &json!({})).unwrap();
        assert_eq!(l["markers"][0]["name"], "intro");
        assert_eq!(l["markers"][1]["beat"], 32.0);
        assert!(e.call("add_marker", &json!({"name": "x", "beat": 1, "time_signature": "3/5"})).is_err());
        e.call("remove_marker", &json!({"name": "drop"})).unwrap();
        assert!(e.call("remove_marker", &json!({"name": "drop"})).is_err());
    }
}
