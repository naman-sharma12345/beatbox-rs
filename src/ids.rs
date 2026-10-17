//! Stable object identity. Every track, send, bus, pattern, note, playlist
//! clip, audio clip, automation lane and automation clip carries an `id`
//! (inserts already carry theirs, see `fx::ensure_ids`). Ids are short
//! ("trk3", "n41", "clip7"), unique across the whole project, never reused
//! after a delete, survive renames, undo/redo and save/load, and are the
//! durable way for a model or a script to point at one object: tools keep
//! accepting names and indices as aliases.
//!
//! The engine assigns ids after every mutating tool call and on load, so a
//! tool that creates objects never has to: it leaves `id` empty. When an
//! edit copies an object (a duplicated pattern, a pasted note) the copy is
//! told apart from its source by where the id lived before the call, and
//! the copy gets a fresh id.

use crate::project::Project;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Project file schema written by this build. 1 = before stable ids
/// (absent in old files), 2 = ids on every object.
pub const SCHEMA_VERSION: u32 = 2;

/// Where an object lives ("track:lead", "note:verse/bass"): used to decide
/// which of two objects sharing an id is the original.
pub type OwnerKeys = HashMap<String, String>;

macro_rules! ref_of {
    (iter_mut, $e:expr) => {
        &mut $e
    };
    (iter, $e:expr) => {
        &$e
    };
}

macro_rules! visit_body {
    ($p:expr, $f:expr, $iter:ident) => {{
        for t in $p.tracks.$iter() {
            let tn = t.name.to_lowercase();
            $f("trk", format!("track:{tn}"), ref_of!($iter, t.id));
            for s in t.sends.$iter() {
                $f(
                    "snd",
                    format!("send:{tn}>{}", s.bus.to_lowercase()),
                    ref_of!($iter, s.id),
                );
            }
        }
        for b in $p.buses.$iter() {
            $f(
                "bus",
                format!("bus:{}", b.name.to_lowercase()),
                ref_of!($iter, b.id),
            );
        }
        for pat in $p.patterns.$iter() {
            let pn = pat.name.to_lowercase();
            $f("pat", format!("pattern:{pn}"), ref_of!($iter, pat.id));
            for (tk, notes) in pat.clips.$iter() {
                for n in notes.$iter() {
                    $f("n", format!("note:{pn}/{tk}"), ref_of!($iter, n.id));
                }
            }
        }
        for c in $p.playlist.$iter() {
            $f(
                "pl",
                format!("pl:{}", c.pattern.to_lowercase()),
                ref_of!($iter, c.id),
            );
        }
        for c in $p.audio_clips.$iter() {
            $f(
                "clip",
                format!("clip:{}", c.track.to_lowercase()),
                ref_of!($iter, c.id),
            );
        }
        for l in $p.automation.$iter() {
            $f(
                "lane",
                format!("lane:{}.{}", l.target.to_lowercase(), l.param),
                ref_of!($iter, l.id),
            );
        }
        for c in $p.automation_clips.$iter() {
            $f(
                "ac",
                format!("ac:{}", c.name.to_lowercase()),
                ref_of!($iter, c.id),
            );
        }
    }};
}

fn visit_mut(p: &mut Project, f: &mut dyn FnMut(&'static str, String, &mut String)) {
    visit_body!(p, f, iter_mut)
}

fn visit(p: &Project, f: &mut dyn FnMut(&'static str, String, &String)) {
    visit_body!(p, f, iter)
}

/// Owner key of every id in the project (taken before a mutating call).
pub fn owner_keys(p: &Project) -> OwnerKeys {
    let mut m = HashMap::new();
    visit(p, &mut |_, key, id| {
        if !id.is_empty() {
            m.entry(id.clone()).or_insert(key);
        }
    });
    m
}

/// Give every object without an id (or with a duplicated one) a fresh id and
/// stamp the current schema. `prev` = owner keys from before the edit, so a
/// copy loses the id and the original keeps it. Returns whether anything
/// changed.
pub fn ensure_ids(p: &mut Project, prev: Option<&OwnerKeys>) -> bool {
    let mut slots: Vec<(&'static str, String, String)> = Vec::new();
    visit(p, &mut |pre, key, id| slots.push((pre, key, id.clone())));
    let mut by_id: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, s) in slots.iter().enumerate() {
        if !s.2.is_empty() {
            by_id.entry(s.2.as_str()).or_default().push(i);
        }
    }
    let mut fresh: Vec<bool> = slots.iter().map(|s| s.2.is_empty()).collect();
    for (id, idx) in &by_id {
        if idx.len() < 2 {
            continue;
        }
        let winner = prev
            .and_then(|m| m.get(*id))
            .and_then(|k| idx.iter().copied().find(|&i| &slots[i].1 == k))
            .unwrap_or(idx[0]);
        for &i in idx {
            if i != winner {
                fresh[i] = true;
            }
        }
    }
    let changed_schema = p.schema_version < SCHEMA_VERSION;
    if !fresh.iter().any(|&f| f) {
        if changed_schema {
            p.schema_version = SCHEMA_VERSION;
        }
        return changed_schema;
    }
    let mut taken: HashSet<String> = by_id.keys().map(|s| s.to_string()).collect();
    let mut seq = p.id_seq;
    let mut new_ids: Vec<Option<String>> = Vec::with_capacity(slots.len());
    for (i, s) in slots.iter().enumerate() {
        if fresh[i] {
            let id = loop {
                seq += 1;
                let cand = format!("{}{}", s.0, seq);
                if !taken.contains(&cand) {
                    break cand;
                }
            };
            taken.insert(id.clone());
            new_ids.push(Some(id));
        } else {
            new_ids.push(None);
        }
    }
    p.id_seq = seq;
    p.schema_version = SCHEMA_VERSION;
    let mut k = 0;
    visit_mut(p, &mut |_, _, id| {
        if let Some(n) = new_ids[k].take() {
            *id = n;
        }
        k += 1;
    });
    true
}

/// A fresh id that no object in the project carries yet (for a tool that
/// must hand back the id of an object it is creating, e.g. a split).
pub fn next_id(p: &mut Project, prefix: &str) -> String {
    let mut taken = HashSet::new();
    visit(p, &mut |_, _, id| {
        taken.insert(id.clone());
    });
    loop {
        p.id_seq += 1;
        let c = format!("{prefix}{}", p.id_seq);
        if !taken.contains(&c) {
            return c;
        }
    }
}

/// What an id points at: its kind, its owner and its current name/index.
pub fn lookup(p: &Project, id: &str) -> Option<Value> {
    let id = id.trim();
    for (i, t) in p.tracks.iter().enumerate() {
        if t.id == id {
            return Some(json!({"id": id, "kind": "track", "name": t.name, "index": i}));
        }
        for (j, s) in t.sends.iter().enumerate() {
            if s.id == id {
                return Some(
                    json!({"id": id, "kind": "send", "track": t.name, "track_id": t.id, "bus": s.bus, "index": j, "db": s.db}),
                );
            }
        }
        if let Some(j) = t.effects.iter().position(|e| e.id() == id) {
            return Some(
                json!({"id": id, "kind": "insert", "owner": t.name, "owner_id": t.id, "index": j, "type": t.effects[j].type_name()}),
            );
        }
    }
    for (i, b) in p.buses.iter().enumerate() {
        if b.id == id {
            return Some(json!({"id": id, "kind": "bus", "name": b.name, "index": i}));
        }
        if let Some(j) = b.effects.iter().position(|e| e.id() == id) {
            return Some(
                json!({"id": id, "kind": "insert", "owner": b.name, "owner_id": b.id, "index": j, "type": b.effects[j].type_name()}),
            );
        }
    }
    if let Some(j) = p.master_effects.iter().position(|e| e.id() == id) {
        return Some(
            json!({"id": id, "kind": "insert", "owner": "master", "index": j, "type": p.master_effects[j].type_name()}),
        );
    }
    for (i, pat) in p.patterns.iter().enumerate() {
        if pat.id == id {
            return Some(json!({"id": id, "kind": "pattern", "name": pat.name, "index": i}));
        }
        for (tk, notes) in &pat.clips {
            if let Some(j) = notes.iter().position(|n| n.id == id) {
                let n = &notes[j];
                let track_id = p
                    .tracks
                    .iter()
                    .find(|t| t.name.to_lowercase() == *tk)
                    .map(|t| t.id.clone())
                    .unwrap_or_default();
                return Some(
                    json!({"id": id, "kind": "note", "pattern": pat.name, "pattern_id": pat.id, "track": tk, "track_id": track_id, "index": j,
                    "start": n.start, "len": n.len, "pitch": n.pitch, "vel": n.vel}),
                );
            }
        }
    }
    if let Some(i) = p.playlist.iter().position(|c| c.id == id) {
        let c = &p.playlist[i];
        return Some(
            json!({"id": id, "kind": "playlist_clip", "pattern": c.pattern, "start_bar": c.start_bar, "bars": c.bars, "index": i}),
        );
    }
    if let Some(i) = p.audio_clips.iter().position(|c| c.id == id) {
        return Some(json!({"id": id, "kind": "audio_clip", "index": i, "clip": p.audio_clips[i]}));
    }
    if let Some(i) = p.automation.iter().position(|l| l.id == id) {
        let l = &p.automation[i];
        return Some(
            json!({"id": id, "kind": "automation_lane", "target": l.target, "param": l.param, "index": i, "points": l.points.len()}),
        );
    }
    if let Some(i) = p.automation_clips.iter().position(|c| c.id == id) {
        return Some(
            json!({"id": id, "kind": "automation_clip", "name": p.automation_clips[i].name, "index": i}),
        );
    }
    None
}

/// Argument keys that name a track, bus or pattern. A value equal to an
/// object's id (and to no object's name) is swapped for that object's
/// current name before the tool runs, so every tool takes ids.
const NAME_KEYS: &[&str] = &[
    "track",
    "tracks",
    "pattern",
    "patterns",
    "to_patterns",
    "from_pattern",
    "to_pattern",
    "copy_from",
    "source",
    "sidechain",
    "bus",
    "buses",
    "to",
    "output",
    "target",
    "owner",
    "dest",
    "destination",
    "from_track",
    "to_track",
    "only_tracks",
    "exclude",
    "solo",
    "mute",
];

fn name_for_id(p: &Project, s: &str) -> Option<String> {
    let k = s.trim();
    if k.is_empty() || !k.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let low = k.to_lowercase();
    let is_name = p.tracks.iter().any(|t| t.name.to_lowercase() == low)
        || p.buses.iter().any(|b| b.name.to_lowercase() == low)
        || p.patterns.iter().any(|x| x.name.to_lowercase() == low);
    if is_name {
        return None;
    }
    p.tracks
        .iter()
        .find(|t| t.id == k)
        .map(|t| t.name.clone())
        .or_else(|| p.buses.iter().find(|b| b.id == k).map(|b| b.name.clone()))
        .or_else(|| {
            p.patterns
                .iter()
                .find(|x| x.id == k)
                .map(|x| x.name.clone())
        })
}

fn swap(p: &Project, v: &mut Value) {
    match v {
        Value::String(s) => {
            if s.contains(',') {
                let parts: Vec<String> = s
                    .split(',')
                    .map(|x| name_for_id(p, x).unwrap_or_else(|| x.trim().to_string()))
                    .collect();
                *s = parts.join(",");
            } else if let Some(n) = name_for_id(p, s) {
                *s = n;
            }
        }
        Value::Array(a) => {
            for x in a.iter_mut() {
                swap(p, x);
            }
        }
        _ => {}
    }
}

/// Resolve id references in a tool's arguments to current names.
pub fn resolve_id_refs(p: &Project, args: &Value) -> Value {
    let mut out = args.clone();
    if let Value::Object(m) = &mut out {
        for (k, v) in m.iter_mut() {
            if NAME_KEYS.contains(&k.as_str())
                || k.ends_with("_track")
                || k.ends_with("_pattern")
                || k.ends_with("_bus")
            {
                swap(p, v);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    fn eng() -> Engine {
        let dir = std::env::temp_dir().join(format!("beatbox_ids_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("new_project", &json!({"name": "ids", "bpm": 120}))
            .unwrap();
        e
    }

    #[test]
    fn every_object_gets_a_unique_id_that_survives_edits_and_undo() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "keys", "preset": "pad"}))
            .unwrap();
        e.call("add_notes", &json!({"track": "keys", "notes": [{"start": 0, "len": 4, "pitch": 60}, {"start": 4, "len": 4, "pitch": 64}]})).unwrap();
        e.call("add_bus", &json!({"name": "verb"})).unwrap();
        e.call(
            "set_send",
            &json!({"track": "keys", "bus": "verb", "db": -12}),
        )
        .unwrap();
        let p = &e.project;
        assert_eq!(p.schema_version, SCHEMA_VERSION);
        let tid = p.tracks[0].id.clone();
        assert!(tid.starts_with("trk"), "{tid}");
        let notes = p.patterns[0].notes("keys");
        assert!(notes.iter().all(|n| n.id.starts_with('n')));
        assert_ne!(notes[0].id, notes[1].id);
        assert!(p.tracks[0].sends[0].id.starts_with("snd"));
        assert!(p.buses[0].id.starts_with("bus"));
        assert!(p.patterns[0].id.starts_with("pat"));
        let mut all = Vec::new();
        visit(p, &mut |_, _, id| all.push(id.clone()));
        let uniq: HashSet<_> = all.iter().collect();
        assert_eq!(uniq.len(), all.len(), "ids are unique: {all:?}");
        let n0 = notes[0].id.clone();
        // a tool addressed by id
        e.call("set_mixer", &json!({"track": tid, "volume_db": -3}))
            .unwrap();
        assert_eq!(e.project.tracks[0].volume_db, -3.0);
        // notes selected by id
        e.call(
            "edit_notes",
            &json!({"track": "keys", "ids": [n0.clone()], "transpose": 2}),
        )
        .unwrap();
        let ns = e.project.patterns[0].notes("keys");
        assert_eq!(ns.iter().find(|n| n.id == n0).unwrap().pitch, 62);
        assert_eq!(
            ns.iter().filter(|n| n.pitch == 64).count(),
            1,
            "the other note did not move"
        );
        // remove a note, undo: the same id comes back
        e.call("delete_notes", &json!({"track": tid, "ids": [n0.clone()]}))
            .unwrap();
        assert!(e.project.patterns[0]
            .notes("keys")
            .iter()
            .all(|n| n.id != n0));
        e.call("undo", &json!({})).unwrap();
        assert!(e.project.patterns[0]
            .notes("keys")
            .iter()
            .any(|n| n.id == n0));
        // lookup
        let v = e.call("lookup_id", &json!({"id": n0})).unwrap();
        assert_eq!(v["kind"], "note");
        assert_eq!(v["track_id"], tid.as_str());
        // a new object never reuses a deleted id
        let seq = e.project.id_seq;
        e.call("add_track", &json!({"name": "bass", "preset": "pad"}))
            .unwrap();
        assert!(e.project.id_seq > seq);
    }

    #[test]
    fn a_copy_gets_a_fresh_id_and_the_original_keeps_its_own() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "keys", "preset": "pad"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track": "keys", "notes": [{"start": 0, "len": 4, "pitch": 60}]}),
        )
        .unwrap();
        let src_pat = e.project.patterns[0].id.clone();
        let src_note = e.project.patterns[0].notes("keys")[0].id.clone();
        // insert a copy BEFORE the original: the original still keeps its ids
        let mut copy = e.project.patterns[0].clone();
        copy.name = "B".into();
        let before = owner_keys(&e.project);
        e.project.patterns.insert(0, copy);
        assert!(ensure_ids(&mut e.project, Some(&before)));
        let a = e.project.patterns.iter().find(|p| p.name == "A").unwrap();
        let b = e.project.patterns.iter().find(|p| p.name == "B").unwrap();
        assert_eq!(a.id, src_pat);
        assert_eq!(a.notes("keys")[0].id, src_note);
        assert_ne!(b.id, src_pat);
        assert_ne!(b.notes("keys")[0].id, src_note);
    }

    #[test]
    fn legacy_project_files_migrate_on_load() {
        let mut e = eng();
        e.call("add_track", &json!({"name": "keys", "preset": "pad"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track": "keys", "notes": [{"start": 0, "len": 4, "pitch": 60}]}),
        )
        .unwrap();
        // strip ids and the schema, as a file written before ids
        let mut v = serde_json::to_value(&e.project).unwrap();
        fn strip(v: &mut Value) {
            match v {
                Value::Object(m) => {
                    if m.contains_key("start")
                        || m.contains_key("instrument")
                        || m.contains_key("clips")
                        || m.contains_key("bus")
                    {
                        m.remove("id");
                    }
                    m.remove("schema_version");
                    m.remove("id_seq");
                    for x in m.values_mut() {
                        strip(x);
                    }
                }
                Value::Array(a) => a.iter_mut().for_each(strip),
                _ => {}
            }
        }
        strip(&mut v);
        let s = v.to_string();
        assert!(!s.contains("\"trk"), "stripped: {s}");
        let path = e.workdir.join("legacy.json");
        std::fs::write(&path, s).unwrap();
        e.call("load_project", &json!({"path": path.to_string_lossy()}))
            .unwrap();
        assert_eq!(e.project.schema_version, SCHEMA_VERSION);
        assert!(!e.project.tracks[0].id.is_empty());
        assert!(!e.project.patterns[0].notes("keys")[0].id.is_empty());
        // ids round-trip through save/load unchanged
        let tid = e.project.tracks[0].id.clone();
        let out = e.workdir.join("saved.json");
        e.call("save_project", &json!({"path": out.to_string_lossy()}))
            .unwrap();
        e.call("load_project", &json!({"path": out.to_string_lossy()}))
            .unwrap();
        assert_eq!(e.project.tracks[0].id, tid);
    }
}
