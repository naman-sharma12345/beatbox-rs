//! Tools for stable object ids (see `ids`): find what an id points at, and
//! list the ids of everything in the project.

use crate::tools::{obj, s_opt, s_req, Tool};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

fn list_ids(
    p: &crate::project::Project,
    kind: Option<&str>,
    pattern: Option<&str>,
) -> Result<Value> {
    let want = |k: &str| kind.is_none_or(|w| w == k || w.trim_end_matches('s') == k);
    let mut out = serde_json::Map::new();
    if want("track") {
        out.insert("tracks".into(), json!(p.tracks.iter().map(|t| json!({"id": t.id, "name": t.name,
            "inserts": t.effects.iter().map(|e| e.id()).collect::<Vec<_>>(),
            "sends": t.sends.iter().map(|s| json!({"id": s.id, "bus": s.bus})).collect::<Vec<_>>()})).collect::<Vec<_>>()));
    }
    if want("bus") {
        out.insert("buses".into(), json!(p.buses.iter().map(|b| json!({"id": b.id, "name": b.name, "inserts": b.effects.iter().map(|e| e.id()).collect::<Vec<_>>()})).collect::<Vec<_>>()));
        out.insert(
            "master_inserts".into(),
            json!(p.master_effects.iter().map(|e| e.id()).collect::<Vec<_>>()),
        );
    }
    if want("pattern") {
        out.insert("patterns".into(), json!(p.patterns.iter().map(|x| json!({"id": x.id, "name": x.name,
            "notes": x.clips.iter().map(|(k, v)| (k.clone(), json!(v.len()))).collect::<serde_json::Map<_, _>>()})).collect::<Vec<_>>()));
    }
    if want("note") {
        let pats: Vec<&crate::project::Pattern> = match pattern {
            Some(n) => vec![&p.patterns[p.pattern_index(n)?]],
            None => p.patterns.iter().collect(),
        };
        let mut notes = Vec::new();
        for x in pats {
            for (tk, v) in &x.clips {
                for n in v {
                    notes.push(json!({"id": n.id, "pattern": x.name, "track": tk, "start": n.start, "len": n.len, "pitch": n.pitch}));
                }
            }
        }
        if notes.len() > 2000 {
            return Err(anyhow!(
                "{} notes; pass pattern to list one pattern's note ids",
                notes.len()
            ));
        }
        out.insert("notes".into(), json!(notes));
    }
    if want("playlist_clip") || want("playlist") {
        out.insert("playlist".into(), json!(p.playlist.iter().map(|c| json!({"id": c.id, "pattern": c.pattern, "start_bar": c.start_bar, "bars": c.bars})).collect::<Vec<_>>()));
    }
    if want("audio_clip") || want("clip") {
        out.insert("audio_clips".into(), json!(p.audio_clips.iter().map(|c| json!({"id": c.id, "track": c.track, "sample": c.sample, "start_beat": c.start_beat})).collect::<Vec<_>>()));
    }
    if want("automation_lane") || want("lane") {
        out.insert(
            "automation_lanes".into(),
            json!(p
                .automation
                .iter()
                .map(|l| json!({"id": l.id, "target": l.target, "param": l.param}))
                .collect::<Vec<_>>()),
        );
        out.insert(
            "automation_clips".into(),
            json!(p
                .automation_clips
                .iter()
                .map(|c| json!({"id": c.id, "name": c.name}))
                .collect::<Vec<_>>()),
        );
    }
    out.insert("schema_version".into(), json!(p.schema_version));
    Ok(Value::Object(out))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "lookup_id",
            description: "What a stable id points at (track, send, insert, bus, pattern, note, playlist clip, audio clip, automation lane or clip): its kind, owner, current name and position. Ids survive renames, undo and save/load; every tool that takes a track/bus/pattern name also takes its id.",
            mutates: false,
            schema: || obj(json!({"id": {"type": "string", "description": "e.g. \"trk3\", \"n41\", \"clip7\", \"reverb1\""}}), &["id"]),
            run: |e, a| {
                let id = s_req(a, "id")?;
                crate::ids::lookup(&e.project, &id).ok_or_else(|| anyhow!("no object with id '{id}' (list_ids shows them)"))
            },
        },
        Tool {
            name: "list_ids",
            description: "Stable ids of the project's objects: tracks (with insert and send ids), buses, patterns, playlist clips, audio clips, automation lanes and clips; kind=note (optionally one pattern) lists note ids.",
            mutates: false,
            schema: || obj(json!({
                "kind": {"type": "string", "enum": ["track", "bus", "pattern", "note", "playlist_clip", "audio_clip", "automation_lane"]},
                "pattern": {"type": "string", "description": "with kind=note: only this pattern"}
            }), &[]),
            run: |e, a| {
                let kind = s_opt(a, "kind");
                let pat = s_opt(a, "pattern");
                let v = list_ids(&e.project, kind.as_deref(), pat.as_deref())?;
                if kind.is_none() {
                    // the note list can be huge; only on request
                    let mut v = v;
                    if let Value::Object(m) = &mut v {
                        m.remove("notes");
                    }
                    return Ok(v);
                }
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
    fn list_and_lookup_cover_clips_lanes_and_playlist() {
        let dir = std::env::temp_dir().join(format!("beatbox_tools_ids_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("new_project", &json!({"name": "x", "bpm": 120}))
            .unwrap();
        e.call("add_track", &json!({"name": "keys", "preset": "pad"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track": "keys", "notes": [{"start": 0, "len": 4, "pitch": 60}]}),
        )
        .unwrap();
        e.call(
            "place_pattern",
            &json!({"pattern": "A", "start_bar": 0, "bars": 4}),
        )
        .unwrap();
        e.call(
            "add_automation",
            &json!({"track": "keys", "param": "volume", "points": [[0, -6.0], [4, 0.0]]}),
        )
        .ok();
        let v = e.call("list_ids", &json!({})).unwrap();
        let pl = v["playlist"][0]["id"].as_str().unwrap().to_string();
        assert!(pl.starts_with("pl"), "{v}");
        assert_eq!(
            e.call("lookup_id", &json!({"id": pl})).unwrap()["kind"],
            "playlist_clip"
        );
        let n = e
            .call("list_ids", &json!({"kind": "note", "pattern": "A"}))
            .unwrap();
        assert_eq!(n["notes"].as_array().unwrap().len(), 1);
        // remove a playlist clip by its id
        e.call("remove_clip", &json!({"id": pl})).unwrap();
        assert!(e.project.playlist.is_empty());
        if let Some(l) = e.project.automation.first() {
            assert!(l.id.starts_with("lane"));
        }
        assert!(e.call("lookup_id", &json!({"id": "nope9"})).is_err());
    }
}
