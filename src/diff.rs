//! Structured project diff: what changed between two versions of a song,
//! phrased in producer terms (track 'bass' volume_db -3 -> -6, pattern 'A'
//! kick: +4 notes -1 note) rather than raw JSON.

use crate::project::Project;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Change {
    /// Dotted location, e.g. `tracks.bass.volume_db`, `patterns.A.clips.kick`.
    pub path: String,
    /// added | removed | changed | notes | points
    pub kind: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub from: Value,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub to: Value,
}

fn ch(path: &str, kind: &str, from: Value, to: Value) -> Change {
    Change {
        path: path.to_string(),
        kind: kind.to_string(),
        from,
        to,
    }
}

fn join(base: &str, k: &str) -> String {
    if base.is_empty() {
        k.to_string()
    } else {
        format!("{base}.{k}")
    }
}

/// Collections whose items are matched by a key instead of by position.
fn key_of(path: &str, item: &Value) -> Option<String> {
    let last = path.rsplit('.').next().unwrap_or(path);
    match last {
        "tracks" | "patterns" | "buses" | "samples" => {
            item.get("name").and_then(|v| v.as_str()).map(String::from)
        }
        "automation" => Some(format!(
            "{}:{}",
            item.get("target")?.as_str()?,
            item.get("param")?.as_str()?
        )),
        "sends" => item.get("bus").and_then(|v| v.as_str()).map(String::from),
        _ => None,
    }
}

fn note_key(n: &Value) -> String {
    let f = |k: &str| n.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let prob = n.get("prob").and_then(|v| v.as_f64()).unwrap_or(1.0);
    format!(
        "{:.3}/{:.3}/{}/{:.2}/{:.2}/{:.3}",
        f("start"),
        f("len"),
        f("pitch") as i64,
        f("vel"),
        prob,
        f("offset")
    )
}

fn multiset(v: &Value) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for n in v.as_array().into_iter().flatten() {
        *m.entry(note_key(n)).or_insert(0) += 1;
    }
    m
}

fn diff_notes(path: &str, a: &Value, b: &Value, out: &mut Vec<Change>) {
    let (ma, mb) = (multiset(a), multiset(b));
    let mut added = 0;
    let mut removed = 0;
    for (k, &cb) in &mb {
        added += (cb - ma.get(k).copied().unwrap_or(0)).max(0);
    }
    for (k, &ca) in &ma {
        removed += (ca - mb.get(k).copied().unwrap_or(0)).max(0);
    }
    if added + removed > 0 {
        out.push(ch(
            path,
            "notes",
            json!({"count": a.as_array().map(|x| x.len()).unwrap_or(0), "removed": removed}),
            json!({"count": b.as_array().map(|x| x.len()).unwrap_or(0), "added": added}),
        ));
    }
}

fn walk(path: &str, a: &Value, b: &Value, out: &mut Vec<Change>) {
    if a == b {
        return;
    }
    let in_clips = path.contains(".clips.") || path.ends_with(".clips");
    match (a, b) {
        (Value::Array(_), Value::Array(_)) if in_clips => diff_notes(path, a, b, out),
        (Value::Array(_), Value::Array(_)) if path.ends_with(".points") => {
            out.push(ch(
                path,
                "points",
                json!(a.as_array().map(|x| x.len())),
                json!(b.as_array().map(|x| x.len())),
            ));
        }
        (Value::Object(x), Value::Object(y)) => {
            // an effect that changed type is a replacement, not a param edit
            if x.get("type").is_some() && x.get("type") != y.get("type") {
                out.push(ch(
                    path,
                    "changed",
                    x.get("type").cloned().unwrap_or_default(),
                    y.get("type").cloned().unwrap_or_default(),
                ));
                return;
            }
            let clips = path.ends_with(".clips");
            let empty = Value::Array(Vec::new());
            for (k, va) in x {
                match y.get(k) {
                    Some(vb) => walk(&join(path, k), va, vb, out),
                    None if clips => walk(&join(path, k), va, &empty, out),
                    None => out.push(ch(&join(path, k), "removed", summarize(va), Value::Null)),
                }
            }
            for (k, vb) in y {
                if !x.contains_key(k) {
                    if clips {
                        walk(&join(path, k), &empty, vb, out);
                    } else {
                        out.push(ch(&join(path, k), "added", Value::Null, summarize(vb)));
                    }
                }
            }
        }
        (Value::Array(xa), Value::Array(ya)) => {
            let keyed = xa
                .iter()
                .chain(ya.iter())
                .all(|i| key_of(path, i).is_some())
                && !(xa.is_empty() && ya.is_empty());
            if keyed {
                let index = |v: &Vec<Value>| -> Map<String, Value> {
                    v.iter()
                        .filter_map(|i| key_of(path, i).map(|k| (k, i.clone())))
                        .collect()
                };
                let (ia, ib) = (index(xa), index(ya));
                for (k, va) in &ia {
                    match ib.get(k) {
                        Some(vb) => walk(&join(path, k), va, vb, out),
                        None => out.push(ch(&join(path, k), "removed", summarize(va), Value::Null)),
                    }
                }
                for (k, vb) in &ib {
                    if !ia.contains_key(k) {
                        out.push(ch(&join(path, k), "added", Value::Null, summarize(vb)));
                    }
                }
                // pure reorder (e.g. arrangement of tracks)
                let oa: Vec<_> = xa.iter().filter_map(|i| key_of(path, i)).collect();
                let ob: Vec<_> = ya.iter().filter_map(|i| key_of(path, i)).collect();
                if oa != ob && ia.len() == ib.len() && ia.keys().all(|k| ib.contains_key(k)) {
                    out.push(ch(path, "reordered", json!(oa), json!(ob)));
                }
            } else if xa.iter().chain(ya.iter()).all(|v| v.is_object()) {
                // positional (effect chains)
                for i in 0..xa.len().max(ya.len()) {
                    let p = join(path, &i.to_string());
                    match (xa.get(i), ya.get(i)) {
                        (Some(va), Some(vb)) => walk(&p, va, vb, out),
                        (Some(va), None) => out.push(ch(&p, "removed", summarize(va), Value::Null)),
                        (None, Some(vb)) => out.push(ch(&p, "added", Value::Null, summarize(vb))),
                        (None, None) => {}
                    }
                }
            } else {
                out.push(ch(path, "changed", a.clone(), b.clone()));
            }
        }
        _ => out.push(ch(path, "changed", a.clone(), b.clone())),
    }
}

/// A compact stand-in for a big added/removed item.
fn summarize(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            if let Some(t) = o.get("type") {
                return t.clone();
            }
            if let Some(n) = o.get("name") {
                return n.clone();
            }
            if let (Some(t), Some(p)) = (o.get("target"), o.get("param")) {
                return json!(format!(
                    "{}:{}",
                    t.as_str().unwrap_or(""),
                    p.as_str().unwrap_or("")
                ));
            }
            let s = v.to_string();
            if s.len() > 120 {
                json!(format!("{{{} fields}}", o.len()))
            } else {
                v.clone()
            }
        }
        Value::Array(a) if a.len() > 6 => json!(format!("[{} items]", a.len())),
        _ => v.clone(),
    }
}

/// Every change needed to go from `a` to `b`.
pub fn diff(a: &Project, b: &Project) -> Vec<Change> {
    let va = serde_json::to_value(a).unwrap_or_default();
    let vb = serde_json::to_value(b).unwrap_or_default();
    let mut out = Vec::new();
    walk("", &va, &vb, &mut out);
    out
}

/// Short human sentence per change, for logs and the GUI.
pub fn describe(c: &Change) -> String {
    match c.kind.as_str() {
        "notes" => format!(
            "{}: {} -> {} notes (+{} / -{})",
            c.path, c.from["count"], c.to["count"], c.to["added"], c.from["removed"]
        ),
        "added" => format!("+ {} ({})", c.path, c.to),
        "removed" => format!("- {} ({})", c.path, c.from),
        _ => format!("{}: {} -> {}", c.path, c.from, c.to),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruments;
    use crate::project::{Note, Track};

    #[test]
    fn diff_reports_semantic_changes() {
        let mut a = Project::new("x", 120.0);
        a.tracks
            .push(Track::new("kick", instruments::preset("kick").unwrap()));
        a.tracks.push(Track::new(
            "bass",
            instruments::preset("acid_bass").unwrap(),
        ));
        let mut b = a.clone();
        assert!(diff(&a, &b).is_empty());
        b.bpm = 140.0;
        b.tracks[1].volume_db = -4.0;
        b.tracks
            .push(Track::new("hat", instruments::preset("hat").unwrap()));
        b.patterns[0].notes_mut("kick").push(Note {
            start: 0.0,
            len: 1.0,
            pitch: 60,
            vel: 1.0,
            ..Default::default()
        });
        let d = diff(&a, &b);
        let paths: Vec<&str> = d.iter().map(|c| c.path.as_str()).collect();
        assert!(paths.contains(&"bpm"), "{paths:?}");
        assert!(paths.contains(&"tracks.bass.volume_db"), "{paths:?}");
        assert!(paths.contains(&"tracks.hat"), "{paths:?}");
        let n = d.iter().find(|c| c.kind == "notes").expect("notes change");
        assert_eq!(n.to["added"], 1);
        assert!(d.iter().all(|c| !describe(c).is_empty()));
    }
}
