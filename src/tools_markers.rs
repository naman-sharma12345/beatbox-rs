//! Playlist markers (FL): named song positions an AI can navigate by
//! ("render from 'drop'", "the hook starts at marker 'hook2'"), with an
//! optional time-signature label. Section starts are offered as markers too.

use crate::engine::Engine;
use crate::project::Marker;
use crate::tools::{f_opt, obj, s_opt, s_req, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};

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

fn move_arrangement_section(e: &mut Engine, a: &Value) -> Result<Value> {
    if !e.project.playlist.is_empty() {
        bail!("Section order is controlled by playlist clips. Move those clips instead");
    }
    if e.project.arrangement.is_empty() {
        bail!("Create an explicit section arrangement before moving sections");
    }
    let from = a
        .get("from")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("from must be a nonnegative integer"))?;
    let to = a
        .get("to")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("to must be a nonnegative integer"))?;
    let len = e.project.arrangement.len() as u64;
    if from >= len || to >= len {
        bail!("Section index is outside the arrangement");
    }
    // Validate all references before changing the order.
    for section in &e.project.arrangement {
        e.project.pattern_index(&section.pattern)?;
    }
    let section = e.project.arrangement.remove(from as usize);
    e.project.arrangement.insert(to as usize, section);
    Ok(
        json!({"from":from,"to":to,"sections":e.project.arrangement,"fixed_song_positions":["markers","tempo_points","audio_clips","automation"]}),
    )
}

fn duplicate_arrangement_section(e: &mut Engine, a: &Value) -> Result<Value> {
    if !e.project.playlist.is_empty() {
        bail!("Playlist clips control this song; duplicate those clips instead");
    }
    if e.project.arrangement.is_empty() {
        bail!("Create an explicit section arrangement before duplicating a section");
    }
    let index = a
        .get("section")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("section must be a nonnegative integer"))?;
    if index >= e.project.arrangement.len() as u64 {
        bail!("Section index is outside the arrangement");
    }
    for part in &e.project.arrangement {
        e.project.pattern_index(&part.pattern)?;
    }
    let section = e.project.arrangement[index as usize].clone();
    e.project
        .arrangement
        .insert(index as usize + 1, section.clone());
    Ok(
        json!({"source_section":index,"section":index+1,"pattern":section.pattern,"repeats":section.repeats,
        "shared_pattern":true,"fixed_song_positions":["markers","tempo_points","audio_clips","automation"]}),
    )
}

fn set_section_repeats(e: &mut Engine, a: &Value) -> Result<Value> {
    if !e.project.playlist.is_empty() {
        bail!("Playlist clips control this song; resize or duplicate those clips instead");
    }
    if e.project.arrangement.is_empty() {
        bail!("Create an explicit section arrangement before changing repeats");
    }
    let section = a
        .get("section")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("section must be a nonnegative integer"))?;
    let repeats = a
        .get("repeats")
        .and_then(Value::as_u64)
        .filter(|n| (1..=64).contains(n))
        .ok_or_else(|| anyhow::anyhow!("repeats must be an integer from 1 to 64"))?;
    if section >= e.project.arrangement.len() as u64 {
        bail!("Section index is outside the arrangement");
    }
    for part in &e.project.arrangement {
        e.project.pattern_index(&part.pattern)?;
    }
    let part = &mut e.project.arrangement[section as usize];
    let before = part.repeats;
    part.repeats = repeats as u32;
    Ok(
        json!({"section":section,"pattern":part.pattern,"previous_repeats":before,"repeats":repeats,
        "fixed_song_positions":["markers","tempo_points","audio_clips","automation"]}),
    )
}

fn append_arrangement_section(e: &mut Engine, a: &Value) -> Result<Value> {
    if !e.project.playlist.is_empty() {
        bail!("Playlist clips control this song; place a pattern clip instead");
    }
    let i = e.project.pattern_index(&s_req(a, "pattern")?)?;
    let pattern = e.project.patterns[i].name.clone();
    if pattern.to_lowercase().starts_with(crate::tools_fl::PL) {
        bail!("Choose a user pattern, not a generated playlist pattern");
    }
    let repeats = match a.get("repeats") {
        None => 1,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=64).contains(n))
            .ok_or_else(|| anyhow::anyhow!("repeats must be an integer from 1 to 64"))?,
    };
    for part in &e.project.arrangement {
        e.project.pattern_index(&part.pattern)?;
    }
    let started = e.project.arrangement.is_empty();
    let index = e.project.arrangement.len();
    e.project.arrangement.push(crate::project::Section {
        pattern: pattern.clone(),
        repeats: repeats as u32,
    });
    Ok(
        json!({"section":index,"pattern":pattern,"repeats":repeats,"started_explicit_arrangement":started,
        "fixed_song_positions":["markers","tempo_points","audio_clips","automation"]}),
    )
}

/// Explicit or implicit section boundaries in song beats. No rounded display
/// value is used for seeking or calculating the next boundary.
pub fn arrangement_positions(p: &crate::project::Project) -> Result<Vec<Value>> {
    if !p.playlist.is_empty() {
        bail!("Playlist clips control this song; inspect the playlist instead");
    }
    let curve = crate::tempo_curve::TempoCurve::new(p);
    let mut beat = 0.0f64;
    let mut rows = Vec::new();
    for (index, section) in p.song_sections().iter().enumerate() {
        let pattern = &p.patterns[p.pattern_index(&section.pattern)?];
        let start = beat;
        beat += f64::from(pattern.steps()) / 4.0 * f64::from(section.repeats.max(1));
        rows.push(
            json!({"section":index,"pattern":pattern.name,"pattern_id":pattern.id,
            "repeats":section.repeats.max(1),"start_beat":start,"end_beat":beat,
            "start_bar":p.bar_at_beat(start as f32),"end_bar":p.bar_at_beat(beat as f32),
            "start_seconds":curve.secs_at_beat(start),"end_seconds":curve.secs_at_beat(beat),
            "meter":pattern.meter()}),
        );
    }
    Ok(rows)
}

fn get_arrangement(e: &mut Engine, _: &Value) -> Result<Value> {
    Ok(
        json!({"implicit":e.project.arrangement.is_empty(),"sections":arrangement_positions(&e.project)?,"end_positions_exclusive":true}),
    )
}

fn mark_arrangement_section(e: &mut Engine, a: &Value) -> Result<Value> {
    let index = a
        .get("section")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("section must be a nonnegative integer"))?;
    let rows = arrangement_positions(&e.project)?;
    let row = rows
        .get(usize::try_from(index).unwrap_or(usize::MAX))
        .ok_or_else(|| anyhow::anyhow!("Section index is outside the arrangement"))?;
    let beat = row["start_beat"].as_f64().unwrap_or(0.0) as f32;
    let name = if let Some(v) = a.get("name") {
        let name = v
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("name must be a string"))?
            .trim()
            .to_string();
        if name.is_empty() {
            bail!("Marker name cannot be blank");
        }
        if e.project.markers.iter().any(|m| m.name == name) {
            bail!("Marker name already exists; choose a new name");
        }
        name
    } else {
        let base = format!(
            "Section {} - {}",
            index + 1,
            row["pattern"].as_str().unwrap_or("pattern")
        );
        let mut name = base.clone();
        let mut suffix = 2u64;
        while e.project.markers.iter().any(|m| m.name == name) {
            name = format!("{base} ({suffix})");
            suffix += 1;
        }
        name
    };
    e.project.markers.push(Marker {
        name: name.clone(),
        beat,
        time_signature: None,
    });
    e.project.markers.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    Ok(json!({"name":name,"beat":beat,"section":index,"fixed_position":true}))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name:"duplicate_arrangement_section",
            description:"Duplicate one explicit arrangement section immediately after itself, preserving all repeats and sharing the SAME existing pattern (no independent note copy). Zero-based section index. Following sections move later. Saved markers, tempo, automation and audio clips remain fixed at absolute song positions. Rejects playlist-controlled/implicit songs, invalid indices or broken references atomically. One Undo restores.",
            mutates:true,
            schema:||obj(json!({"section":{"type":"integer","minimum":0}}), &["section"]),
            run:duplicate_arrangement_section,
        },
        Tool {
            name:"mark_arrangement_section",
            description:"Create a saved marker at one section's current start, using its zero-based index. Optional name is trimmed and must be new/nonblank; otherwise chooses a collision-safe 'Section N - pattern' name. Marker is a fixed song position, NOT attached to the section: later repeats/reorders do not move it. Includes the implicit first-pattern song. Existing marker names/positions/signature labels and pattern material stay unchanged. Rejects playlist-controlled songs, bad indices/names or broken references atomically. Undo removes the added marker.",
            mutates:true,
            schema:||obj(json!({"section":{"type":"integer","minimum":0},"name":{"type":"string"}}), &["section"]),
            run:mark_arrangement_section,
        },
        Tool {
            name:"get_arrangement",
            description:"Inspect section boundaries without changing the project or transport: canonical pattern/ID, repeats, start/end song beats, bars and seconds, and meter. Includes the implicit first-pattern song when no explicit sections exist. Seconds honor tempo points/ramps, bars honor pattern meters. End positions are exclusive. MCP numeric output uses the engine's four-decimal response precision; GUI seek uses unrounded internal seconds. Rejects playlist-controlled songs or broken references; use list_playlist for clip songs.",
            mutates:false,
            schema:||obj(json!({}), &[]),
            run:get_arrangement,
        },
        Tool {
            name:"append_arrangement_section",
            description:"Append an existing user pattern as a song section (repeats 1-64, default 1). Accepts name or stable ID and stores its canonical name. If no explicit arrangement exists, STARTS one with the selected pattern, replacing the implicit first-pattern song. No pattern/note material or IDs are copied or edited. Saved markers, tempo, automation and audio clips stay at absolute song positions. Rejects playlist-controlled songs, generated patterns, invalid repeats or broken existing references atomically. One Undo restores.",
            mutates:true,
            schema:||obj(json!({"pattern":{"type":"string"},"repeats":{"type":"integer","minimum":1,"maximum":64}}), &["pattern"]),
            run:append_arrangement_section,
        },
        Tool {
            name:"set_section_repeats",
            description:"Set one explicit arrangement section's repeat count (1-64), by zero-based section index. Changes song length and moves following sections; does not copy/edit pattern material or stable IDs. Saved markers, tempo, automation and audio clips stay at absolute song positions, not attached to sections. Rejects playlist-mode, implicit arrangements, invalid counts/indices or missing pattern references atomically. One Undo restores.",
            mutates:true,
            schema:||obj(json!({"section":{"type":"integer","minimum":0},"repeats":{"type":"integer","minimum":1,"maximum":64}}), &["section","repeats"]),
            run:set_section_repeats,
        },
        Tool {
            name:"move_arrangement_section",
            description:"Move one explicit song section from a zero-based index to a final zero-based index. Keeps each section's pattern/repeats, notes and IDs. Saved markers, tempo points, automation and audio clips stay at absolute song positions, not attached to sections. Rejects playlist-mode or implicit arrangements. One Undo restores order.",
            mutates:true,
            schema:||obj(json!({"from":{"type":"integer","minimum":0},"to":{"type":"integer","minimum":0}}), &["from","to"]),
            run:move_arrangement_section,
        },
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
                let s: Vec<Value> = if p.playlist.is_empty() {
                    arrangement_positions(p)?.into_iter().map(|row| json!({"section":row["pattern"],"beat":row["start_beat"],"bar":row["start_bar"],"seconds":row["start_seconds"]})).collect()
                } else { Vec::new() };
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

#[cfg(test)]
mod section_order_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/section-order-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let name = e.project.patterns[0].name.clone();
        e.call(
            "add_pattern",
            &json!({"name":"Break","copy_from":name,"bars":1}),
        )
        .unwrap();
        for v in e.project.patterns[1].clips.values_mut() {
            v.retain(|n| n.start < 16.);
            for n in v {
                n.vel *= 0.5;
            }
        }
        e.call("add_pattern", &json!({"name":"Outro","bars":1}))
            .unwrap();
        e.call("set_arrangement",&json!({"sections":[{"pattern":name,"repeats":1},{"pattern":"Break","repeats":2},{"pattern":"Outro","repeats":1}]})).unwrap();
        e
    }
    #[test]
    fn section_order_scope_history_reopen() {
        let mut e = fixture();
        e.call("add_marker", &json!({"name":"Fixed","beat":8}))
            .unwrap();
        e.call("add_tempo_point", &json!({"beat":12,"bpm":110}))
            .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        e.call(
            "arrange",
            &json!({"action":"move_arrangement_section","params":{"from":0,"to":2}}),
        )
        .unwrap();
        let mut x = b.clone();
        let a = x["arrangement"].as_array_mut().unwrap();
        let v = a.remove(0);
        a.insert(2, v);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        let h = e.history().0.len();
        e.call("move_arrangement_section", &json!({"from":1,"to":1}))
            .unwrap();
        assert_eq!(h, e.history().0.len());
        e.call(
            "save_project",
            &json!({"path":"/tmp/section-order-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/section-order-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        e.call("move_arrangement_section", &json!({"from":2,"to":0}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
    #[test]
    fn section_order_render_cache_stream() {
        let mut e = fixture();
        e.project.master_effects.clear();
        for t in &mut e.project.tracks {
            t.effects.clear();
            t.mute = t.name != "drums";
        }
        let opts = beatbox::render::RenderOptions {
            tail: 0.,
            keep_stems: true,
            ..Default::default()
        };
        let before = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        let mut cache = beatbox::render::TrackCache::default();
        beatbox::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        let mut expected = e.project.clone();
        expected.arrangement.swap(0, 1);
        e.call("move_arrangement_section", &json!({"from":1,"to":0}))
            .unwrap();
        let c =
            beatbox::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        let f = beatbox::render::render(&expected, &e.bank, &opts).unwrap();
        assert_eq!(c.left, f.left);
        assert_eq!(c.right, f.right);
        assert_ne!(c.left, before.left);
        let mut stream = beatbox::rt::VoiceStreamer::new(&e.project, &e.bank, &opts, 127, 0);
        let (l, r) = stream.render_all();
        let mut reference = beatbox::rt::VoiceStreamer::new(&expected, &e.bank, &opts, 127, 0);
        let (el, er) = reference.render_all();
        assert!(
            l == el,
            "stream order differs from manually reordered reference"
        );
        assert!(r == er);
    }
    #[test]
    fn section_order_rejections_atomic() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        for a in [
            json!({"from":0,"to":3}),
            json!({"from":3,"to":0}),
            json!({"from":-1,"to":0}),
            json!({"from":0.5,"to":0}),
            json!({"from":"1","to":0}),
            json!({"from":0}),
            json!({"from":0,"to":null}),
        ] {
            assert!(e.call("move_arrangement_section", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
            assert_eq!(h, e.history().0.len());
        }
        e.project.arrangement.clear();
        assert!(e
            .call("move_arrangement_section", &json!({"from":0,"to":0}))
            .is_err());
        e.call("place_pattern", &json!({"pattern":"Break","start_bar":0}))
            .unwrap();
        let p = e.project.clone();
        assert!(e
            .call("move_arrangement_section", &json!({"from":0,"to":0}))
            .is_err());
        assert_eq!(p, e.project);
    }

    #[test]
    fn section_order_matched_stage_parity() {
        let mut e = fixture();
        e.project.master_effects.clear();
        for t in &mut e.project.tracks {
            t.effects.clear();
            t.mute = t.name != "drums";
        }
        let opts = beatbox::render::RenderOptions {
            tail: 0.,
            keep_stems: true,
            ..Default::default()
        };
        for reordered in [false, true] {
            if reordered {
                e.call("move_arrangement_section", &json!({"from":1,"to":0}))
                    .unwrap();
            }
            let offline = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
            for block in [127, 512, 1024] {
                let mut voices =
                    beatbox::rt::VoiceStreamer::new(&e.project, &e.bank, &opts, block, 0);
                let (l, r) = voices.render_all();
                let gain = 10f32.powf(e.project.tracks[0].volume_db / 20.);
                assert_eq!(l.len(), offline.stems[0].left.len());
                assert!(
                    l.iter()
                        .zip(&offline.stems[0].left)
                        .all(|(a, b)| *a * gain == *b),
                    "pre-fader voices must match post-fader stem after gain"
                );
                assert!(r
                    .iter()
                    .zip(&offline.stems[0].right)
                    .all(|(a, b)| *a * gain == *b));
                let mut mix = beatbox::rt::MixNode::new(&e.project, &e.bank, &opts, block);
                let (ml, mr) = mix.render();
                assert!(
                    ml == offline.left && mr == offline.right,
                    "matched final mixes differ"
                );
            }
        }
    }
    #[test]
    fn section_order_final_mix_with_original_fx() {
        let mut e = fixture();
        let opts = beatbox::render::RenderOptions {
            tail: 0.,
            keep_stems: true,
            ..Default::default()
        };
        for reordered in [false, true] {
            if reordered {
                e.call("move_arrangement_section", &json!({"from":1,"to":0}))
                    .unwrap();
            }
            let offline = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
            for block in [127, 512, 1024] {
                let mut mix = beatbox::rt::MixNode::new(&e.project, &e.bank, &opts, block);
                let (l, r) = mix.render();
                assert!(
                    l == offline.left && r == offline.right,
                    "final mixed stereo differs"
                );
            }
        }
    }
}

#[cfg(test)]
mod section_repeat_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/section-repeat-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        let n = e.project.patterns[0].name.clone();
        e.call(
            "duplicate_pattern",
            &json!({"pattern":n,"new_name":"Break"}),
        )
        .unwrap();
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":n,"repeats":1},{"pattern":"Break","repeats":1}]}),
        )
        .unwrap();
        e.call("add_marker", &json!({"name":"Fixed","beat":8}))
            .unwrap();
        e.call("add_tempo_point", &json!({"beat":12,"bpm":110}))
            .unwrap();
        e
    }
    #[test]
    fn repeats_scope_undo_reopen_noop() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        e.call(
            "arrange",
            &json!({"action":"set_section_repeats","params":{"section":0,"repeats":3}}),
        )
        .unwrap();
        let mut expected = b.clone();
        expected["arrangement"][0]["repeats"] = json!(3);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        let h = e.history().0.len();
        e.call("set_section_repeats", &json!({"section":0,"repeats":3}))
            .unwrap();
        assert_eq!(e.history().0.len(), h);
        e.call(
            "save_project",
            &json!({"path":"/tmp/section-repeat-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/section-repeat-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), expected);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), expected);
        e.call("set_section_repeats", &json!({"section":0,"repeats":64}))
            .unwrap();
        assert_eq!(e.project.arrangement[0].repeats, 64);
        e.call("set_section_repeats", &json!({"section":0,"repeats":1}))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
    }
    #[test]
    fn repeats_rejects_atomically() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        for a in [
            json!({"section":0,"repeats":0}),
            json!({"section":0,"repeats":65}),
            json!({"section":0,"repeats":1.5}),
            json!({"section":0,"repeats":"2"}),
            json!({"section":0,"repeats":-1}),
            json!({"section":2,"repeats":2}),
            json!({"section":-1,"repeats":2}),
            json!({"section":0.5,"repeats":2}),
            json!({"section":0}),
            json!({"repeats":2}),
        ] {
            assert!(e.call("set_section_repeats", &a).is_err(), "{a}");
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
            assert_eq!(e.history().0.len(), h);
        }
        e.project.arrangement[1].pattern = "missing".into();
        let b = e.project.clone();
        assert!(e
            .call("set_section_repeats", &json!({"section":0,"repeats":2}))
            .is_err());
        assert_eq!(b, e.project);
        e.project.arrangement.clear();
        assert!(e
            .call("set_section_repeats", &json!({"section":0,"repeats":2}))
            .is_err());
        e.call("place_pattern", &json!({"pattern":"Break","start_bar":0}))
            .unwrap();
        let b = e.project.clone();
        assert!(e
            .call("set_section_repeats", &json!({"section":0,"repeats":2}))
            .is_err());
        assert_eq!(b, e.project);
    }
    #[test]
    fn repeats_render_cached_and_full_mix() {
        let mut e = fixture();
        let opts = beatbox::render::RenderOptions {
            tail: 0.,
            ..Default::default()
        };
        let before = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        let mut cache = beatbox::render::TrackCache::default();
        beatbox::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        e.call("set_section_repeats", &json!({"section":1,"repeats":2}))
            .unwrap();
        let mut expected = e.project.clone();
        expected.arrangement[1].repeats = 2;
        let rendered = beatbox::render::render(&expected, &e.bank, &opts).unwrap();
        let cached =
            beatbox::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        assert_eq!(cached.left, rendered.left);
        assert_eq!(cached.right, rendered.right);
        assert!(cached.left.len() > before.left.len());
        for block in [127, 512, 1024] {
            let mut mix = beatbox::rt::MixNode::new(&e.project, &e.bank, &opts, block);
            let (l, r) = mix.render();
            assert!(l == rendered.left && r == rendered.right);
        }
    }
}

#[cfg(test)]
mod section_append_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/section-append-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.call(
            "duplicate_pattern",
            &json!({"pattern":"groove","new_name":"Break"}),
        )
        .unwrap();
        e.call("add_marker", &json!({"name":"Fixed","beat":8}))
            .unwrap();
        e.call("add_tempo_point", &json!({"beat":12,"bpm":110}))
            .unwrap();
        e
    }
    #[test]
    fn append_start_scope_undo_reopen() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        let id = e.project.patterns[1].id.clone();
        let r = e
            .call(
                "arrange",
                &json!({"action":"append_arrangement_section","params":{"pattern":id}}),
            )
            .unwrap();
        assert_eq!(r["started_explicit_arrangement"], true);
        assert_eq!(r["pattern"], "Break");
        let mut x = b.clone();
        x["arrangement"] = json!([{"pattern":"Break","repeats":1}]);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        e.call(
            "append_arrangement_section",
            &json!({"pattern":"GROOVE","repeats":2}),
        )
        .unwrap();
        x["arrangement"]
            .as_array_mut()
            .unwrap()
            .push(json!({"pattern":"groove","repeats":2}));
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        e.call(
            "save_project",
            &json!({"path":"/tmp/section-append-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/section-append-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        e.call(
            "append_arrangement_section",
            &json!({"pattern":id,"repeats":64}),
        )
        .unwrap();
        assert_eq!(e.project.arrangement.last().unwrap().repeats, 64);
    }
    #[test]
    fn append_rejects_atomically() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        for a in [
            json!({"pattern":"missing"}),
            json!({"pattern":"groove","repeats":0}),
            json!({"pattern":"groove","repeats":65}),
            json!({"pattern":"groove","repeats":1.5}),
            json!({"pattern":"groove","repeats":"2"}),
            json!({"pattern":"groove","repeats":-1}),
            json!({"pattern":"groove","repeats":null}),
            json!({}),
        ] {
            assert!(e.call("append_arrangement_section", &a).is_err(), "{a}");
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
            assert_eq!(e.history().0.len(), h);
        }
        e.project
            .patterns
            .push(beatbox::project::Pattern::new("pl:hidden", 1));
        assert!(e
            .call(
                "append_arrangement_section",
                &json!({"pattern":"pl:hidden"})
            )
            .is_err());
        e.project.arrangement.push(beatbox::project::Section {
            pattern: "missing".into(),
            repeats: 1,
        });
        let b = e.project.clone();
        assert!(e
            .call("append_arrangement_section", &json!({"pattern":"groove"}))
            .is_err());
        assert_eq!(b, e.project);
        e.project.arrangement.clear();
        e.call("place_pattern", &json!({"pattern":"Break","start_bar":0}))
            .unwrap();
        let b = e.project.clone();
        assert!(e
            .call("append_arrangement_section", &json!({"pattern":"groove"}))
            .is_err());
        assert_eq!(b, e.project);
    }
    #[test]
    fn append_render_cache_mix() {
        let mut e = fixture();
        let opts = beatbox::render::RenderOptions {
            tail: 0.,
            ..Default::default()
        };
        let mut cache = beatbox::render::TrackCache::default();
        beatbox::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        let mut expected = e.project.clone();
        expected.arrangement = vec![
            beatbox::project::Section {
                pattern: "Break".into(),
                repeats: 1,
            },
            beatbox::project::Section {
                pattern: "groove".into(),
                repeats: 2,
            },
        ];
        e.call("append_arrangement_section", &json!({"pattern":"Break"}))
            .unwrap();
        e.call(
            "append_arrangement_section",
            &json!({"pattern":"groove","repeats":2}),
        )
        .unwrap();
        let off = beatbox::render::render(&expected, &e.bank, &opts).unwrap();
        let c =
            beatbox::render::render_cached(&e.project, &e.bank, &opts, Some(&mut cache)).unwrap();
        assert_eq!(c.left, off.left);
        assert_eq!(c.right, off.right);
        for block in [127, 512, 1024] {
            let mut m = beatbox::rt::MixNode::new(&e.project, &e.bank, &opts, block);
            let (l, r) = m.render();
            assert!(l == off.left && r == off.right);
        }
    }
}

#[cfg(test)]
mod arrangement_position_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/arrangement-position-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e
    }
    #[test]
    fn positions_meter_tempo_exact_readonly() {
        let mut e = fixture();
        e.project.patterns[0].bars = 2;
        e.project.patterns[0].meter = Some((7, 8));
        e.call(
            "duplicate_pattern",
            &json!({"pattern":"groove","new_name":"Break"}),
        )
        .unwrap();
        e.project.patterns[1].meter = Some((3, 4));
        e.project.patterns[1].bars = 1;
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":"groove","repeats":2},{"pattern":"Break","repeats":3}]}),
        )
        .unwrap();
        e.call("add_tempo_point", &json!({"beat":7,"bpm":120,"ramp":true}))
            .unwrap();
        e.call("add_tempo_point", &json!({"beat":16,"bpm":75}))
            .unwrap();
        let b = serde_json::to_value(&e.project).unwrap();
        let rev = e.revision;
        let hist = e.history().0.len();
        let r = e
            .call("arrange", &json!({"action":"get_arrangement","params":{}}))
            .unwrap();
        let rows = r["sections"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["start_beat"], 0.0);
        assert_eq!(rows[0]["end_beat"], 14.0);
        assert_eq!(rows[1]["start_beat"], 14.0);
        assert_eq!(rows[1]["end_beat"], 23.0);
        assert_eq!(rows[0]["start_bar"], 1.0);
        assert_eq!(rows[1]["start_bar"], 5.0);
        assert_eq!(rows[1]["end_bar"], 8.0);
        assert_eq!(rows[0]["meter"], json!([7, 8]));
        let c = beatbox::tempo_curve::TempoCurve::new(&e.project);
        assert!(
            (rows[1]["start_seconds"].as_f64().unwrap() - c.secs_at_beat(14.)).abs() <= 0.00005
        );
        assert!((rows[1]["end_seconds"].as_f64().unwrap() - c.secs_at_beat(23.)).abs() <= 0.00005);
        let raw = beatbox::tools_markers::arrangement_positions(&e.project).unwrap();
        assert_eq!(
            raw[1]["start_seconds"].as_f64().unwrap(),
            c.secs_at_beat(14.)
        );
        assert_eq!(raw[1]["end_seconds"].as_f64().unwrap(), c.secs_at_beat(23.));
        assert_eq!(rows[0]["end_seconds"], rows[1]["start_seconds"]);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        assert_eq!(e.revision, rev);
        assert_eq!(e.history().0.len(), hist);
        assert!(!e.transport.playing);
        assert_eq!(e.transport.position_s, 0.);
    }
    #[test]
    fn positions_implicit_canonical_and_boundaries_after_edits() {
        let mut e = fixture();
        let r = e.call("get_arrangement", &json!({})).unwrap();
        assert_eq!(r["implicit"], true);
        assert_eq!(r["sections"].as_array().unwrap().len(), 1);
        assert_eq!(r["sections"][0]["end_beat"], 16.0);
        let id = e.project.patterns[0].id.clone();
        assert_eq!(r["sections"][0]["pattern_id"], id);
        e.call(
            "duplicate_pattern",
            &json!({"pattern":id,"new_name":"Break"}),
        )
        .unwrap();
        e.project.patterns[1].bars = 1;
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":id,"repeats":1},{"pattern":"Break","repeats":1}]}),
        )
        .unwrap();
        let a = e.call("get_arrangement", &json!({})).unwrap();
        assert_eq!(a["sections"][0]["pattern"], "groove");
        assert_eq!(a["sections"][1]["start_beat"], 16.0);
        e.call("set_section_repeats", &json!({"section":0,"repeats":2}))
            .unwrap();
        assert_eq!(
            e.call("get_arrangement", &json!({})).unwrap()["sections"][1]["start_beat"],
            32.0
        );
        e.call("move_arrangement_section", &json!({"from":1,"to":0}))
            .unwrap();
        let r = e.call("get_arrangement", &json!({})).unwrap();
        assert_eq!(r["sections"][0]["pattern"], "Break");
        assert_eq!(r["sections"][1]["start_beat"], 4.0);
    }
    #[test]
    fn positions_rejects_playlist_and_brokenrefs_without_mutation() {
        let mut e = fixture();
        e.project.arrangement.push(beatbox::project::Section {
            pattern: "missing".into(),
            repeats: 1,
        });
        let b = e.project.clone();
        assert!(e.call("get_arrangement", &json!({})).is_err());
        assert_eq!(b, e.project);
        e.project.arrangement.clear();
        e.call("place_pattern", &json!({"pattern":"groove","start_bar":0}))
            .unwrap();
        let b = e.project.clone();
        assert!(e.call("get_arrangement", &json!({})).is_err());
        assert_eq!(b, e.project);
    }
}

#[cfg(test)]
mod section_marker_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/section-marker-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("recorded-groove.json")}),
        )
        .unwrap();
        e.project.patterns[0].bars = 2;
        e.project.patterns[0].meter = Some((7, 8));
        e.call(
            "duplicate_pattern",
            &json!({"pattern":"groove","new_name":"Break"}),
        )
        .unwrap();
        let id = e.project.patterns[0].id.clone();
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":id,"repeats":2},{"pattern":"bReAk","repeats":1}]}),
        )
        .unwrap();
        e.call("add_tempo_point", &json!({"beat":7,"bpm":120,"ramp":true}))
            .unwrap();
        e
    }
    #[test]
    fn section_marker_alias_positions_scope_reopen_undo() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        let r = e.call("list_markers", &json!({})).unwrap();
        assert_eq!(r["sections"][1]["beat"], 14.0);
        assert_eq!(r["sections"][1]["section"], "Break");
        let c = beatbox::tempo_curve::TempoCurve::new(&e.project);
        assert!(
            (r["sections"][1]["seconds"].as_f64().unwrap() - c.secs_at_beat(14.)).abs() <= 0.00005
        );
        e.call(
            "arrange",
            &json!({"action":"mark_arrangement_section","params":{"section":1,"name":"  Hook  "}}),
        )
        .unwrap();
        let mut x = b.clone();
        x["markers"] = json!([{"name":"Hook","beat":14.0}]);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        e.call(
            "save_project",
            &json!({"path":"/tmp/section-marker-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/section-marker-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        e.call("set_section_repeats", &json!({"section":0,"repeats":3}))
            .unwrap();
        assert_eq!(e.project.markers[0].beat, 14.0);
        assert_eq!(
            e.call("get_arrangement", &json!({})).unwrap()["sections"][1]["start_beat"],
            21.0
        );
    }
    #[test]
    fn section_marker_names_and_atomic_rejections() {
        let mut e = fixture();
        e.call(
            "add_marker",
            &json!({"name":"Keep","beat":3,"time_signature":"3/4"}),
        )
        .unwrap();
        let keep = e.project.markers[0].clone();
        for n in ["Section 2 - Break", "Section 2 - Break (2)"] {
            let r = e
                .call("mark_arrangement_section", &json!({"section":1}))
                .unwrap();
            assert_eq!(r["name"], n);
        }
        assert_eq!(e.project.markers[0], keep);
        let b = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        for a in [
            json!({"section":-1}),
            json!({"section":2}),
            json!({"section":0.5}),
            json!({"section":"1"}),
            json!({}),
            json!({"section":1,"name":" "}),
            json!({"section":1,"name":7}),
            json!({"section":1,"name":"Keep"}),
        ] {
            assert!(e.call("mark_arrangement_section", &a).is_err(), "{a}");
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
            assert_eq!(e.history().0.len(), h);
        }
    }
    #[test]
    fn section_marker_implicit_and_modes() {
        let mut e = fixture();
        e.project.arrangement.clear();
        assert_eq!(
            e.call("list_markers", &json!({})).unwrap()["sections"][0]["beat"],
            0.0
        );
        e.call("mark_arrangement_section", &json!({"section":0}))
            .unwrap();
        assert_eq!(e.project.markers[0].beat, 0.);
        e.project.arrangement.push(beatbox::project::Section {
            pattern: "missing".into(),
            repeats: 1,
        });
        let b = e.project.clone();
        assert!(e
            .call("mark_arrangement_section", &json!({"section":0}))
            .is_err());
        assert!(e.call("list_markers", &json!({})).is_err());
        assert_eq!(e.project, b);
        e.project.arrangement.clear();
        e.call("place_pattern", &json!({"pattern":"groove","start_bar":0}))
            .unwrap();
        let b = e.project.clone();
        assert!(e
            .call("mark_arrangement_section", &json!({"section":0}))
            .is_err());
        assert_eq!(e.project, b);
        assert!(e.call("list_markers", &json!({})).unwrap()["sections"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}

#[cfg(test)]
mod duplicate_section_tests {
    use crate as beatbox;
    use serde_json::json;
    fn fixture() -> beatbox::Engine {
        let mut e = beatbox::Engine::new("/tmp/duplicate-section-test".into());
        e.call(
            "load_project",
            &json!({"path":crate::test_fixtures::path("v202-gui-after.json")}),
        )
        .unwrap();
        e.call("set_section_repeats", &json!({"section":0,"repeats":3}))
            .unwrap();
        e.call("add_marker", &json!({"name":"fixed","beat":2}))
            .unwrap();
        e
    }
    #[test]
    fn duplicate_scope_undo_reopen_shared() {
        let mut e = fixture();
        let b = serde_json::to_value(&e.project).unwrap();
        let h = e.history().0.len();
        let r = e
            .call(
                "arrange",
                &json!({"action":"duplicate_arrangement_section","params":{"section":0}}),
            )
            .unwrap();
        assert_eq!(r["shared_pattern"], true);
        assert_eq!(r["repeats"], 3);
        let mut x = b.clone();
        x["arrangement"]
            .as_array_mut()
            .unwrap()
            .insert(1, b["arrangement"][0].clone());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        assert_eq!(e.history().0.len(), h + 1);
        e.call(
            "save_project",
            &json!({"path":"/tmp/duplicate-section-reopen.json"}),
        )
        .unwrap();
        let mut f = fixture();
        f.call(
            "load_project",
            &json!({"path":"/tmp/duplicate-section-reopen.json"}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&f.project).unwrap(), x);
        e.call("undo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.call("redo", &json!({})).unwrap();
        assert_eq!(serde_json::to_value(&e.project).unwrap(), x);
        let name = e.project.arrangement[0].pattern.clone();
        e.call(
            "rename_pattern",
            &json!({"pattern":name,"new_name":"Shared hook"}),
        )
        .unwrap();
        assert_eq!(e.project.arrangement[0].pattern, "Shared hook");
        assert_eq!(e.project.arrangement[1].pattern, "Shared hook");
    }
    #[test]
    fn duplicate_reject_atomic() {
        let mut e = fixture();
        for a in [
            json!({}),
            json!({"section":-1}),
            json!({"section":1.5}),
            json!({"section":"0"}),
            json!({"section":999}),
            json!({"section":u64::MAX}),
        ] {
            let b = serde_json::to_value(&e.project).unwrap();
            let h = e.history().0.len();
            let rev = e.revision;
            assert!(e.call("duplicate_arrangement_section", &a).is_err());
            assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
            assert_eq!(e.history().0.len(), h);
            assert_eq!(e.revision, rev);
        }
        e.project.arrangement[1].pattern = "missing".into();
        let b = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call("duplicate_arrangement_section", &json!({"section":0}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), b);
        e.project.arrangement.clear();
        assert!(e
            .call("duplicate_arrangement_section", &json!({"section":0}))
            .is_err());
    }
    #[test]
    fn duplicate_tail_render_and_playlist_rejection() {
        let mut e = fixture();
        let last = e.project.arrangement.len() - 1;
        let mut expected = e.project.clone();
        expected
            .arrangement
            .push(expected.arrangement[last].clone());
        e.call("duplicate_arrangement_section", &json!({"section":last}))
            .unwrap();
        assert_eq!(
            serde_json::to_value(&e.project).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        let opts = beatbox::render::RenderOptions {
            tail: 0.,
            ..Default::default()
        };
        let a = beatbox::render::render(&expected, &e.bank, &opts).unwrap();
        let b = beatbox::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(a.left, b.left);
        assert_eq!(a.right, b.right);
        e.project.arrangement.clear();
        e.call("place_pattern", &json!({"pattern":"groove","start_bar":1}))
            .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        assert!(e
            .call("duplicate_arrangement_section", &json!({"section":0}))
            .is_err());
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }
}
