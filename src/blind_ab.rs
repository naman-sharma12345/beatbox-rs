//! The creative critic: a blind A/B listening harness.
//!
//! The technical validator (producer::critique) vetoes failures; it does not
//! judge taste. Taste comes from listeners who cannot see seeds, configs or
//! which generator made what:
//!
//! 1. `create` loudness-matches the candidates (same integrated LUFS, true
//!    peak kept under -1 dBTP), writes them as plain WAVs named A, B, C...
//!    in shuffled order, plus a rubric. The label -> source key is written
//!    *outside* the session folder.
//! 2. Judges listen: a human through `rate` (blind_ab_rate), and optionally
//!    a listening-capable model through a pluggable backend (`judge`).
//!    Both return qualitative notes per criterion (identity, groove,
//!    development, memorability, emotional impact, production) and a
//!    preference, not a single score.
//! 3. `reveal` joins the notes to the sources and turns them into revision
//!    hints for the producer.
//!
//! Backends: `stub` (default; says how to plug one in) and `command`
//! (BEATBOX_AB_JUDGE_CMD or the `command` argument): an executable that gets
//! the session folder as its only argument, may read A.wav/B.wav and
//! RUBRIC.md, and prints JSON on stdout:
//! {"judge": "name", "preference": "A"|"B"|"tie",
//!  "notes": {"A": {"identity": "...", "groove": "...", ...}, "B": {...}},
//!  "comment": "..."}
//! No paid API is wired in; a local model (or a free one) plugs in here.

use crate::engine::Engine;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const CRITERIA: &[(&str, &str)] = &[
    ("identity", "Does it have its own character? Could you tell it apart from other beats in a playlist?"),
    ("groove", "Does the drum/808 pocket make you move? Is it stiff, busy, or just right?"),
    ("development", "Does the main idea grow across sections (variation, contrast, a payoff), or loop unchanged?"),
    ("memorability", "What, if anything, is still in your head afterwards (the hook, a drum pattern, a sound)?"),
    ("emotion", "What does it make you feel? Does that match the intent it seems to have?"),
    ("production", "Mix and sound quality: clarity, low end, harshness, loudness, anything distracting."),
];

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rubric(labels: &[String]) -> String {
    let mut s = String::from("# Blind A/B listening\n\nListen to ");
    s.push_str(
        &labels
            .iter()
            .map(|l| format!("{l}.wav"))
            .collect::<Vec<_>>()
            .join(", "),
    );
    s.push_str(" (loudness-matched; you are not told how they were made). For each, write a short note per criterion, then pick the one you prefer (or tie).\n\n");
    for (k, q) in CRITERIA {
        s.push_str(&format!("- **{k}**: {q}\n"));
    }
    s.push_str("\nRecord it with the MCP tool `blind_ab_rate` {\"session\": <this folder>, \"rater\": \"you\", \"preference\": \"A\", \"notes\": {\"A\": {\"groove\": \"...\"}, \"B\": {...}}, \"comment\": \"...\"}.\nThe key stays hidden until `blind_ab_reveal`.\n");
    s
}

fn key_path(session: &Path) -> PathBuf {
    let id = session
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("session");
    session.with_file_name(format!("{id}.key.json"))
}

pub fn session_dir(e: &Engine, a: &Value) -> Result<PathBuf> {
    let s = a["session"]
        .as_str()
        .ok_or_else(|| anyhow!("missing 'session' (the folder blind_ab_create returned)"))?;
    let p = e.resolve(s);
    if !p.join("session.json").exists() {
        bail!(
            "{} is not a blind A/B session (no session.json)",
            p.display()
        );
    }
    Ok(p)
}

/// Build a blind session from audio files. `items` are (path, display name).
pub fn create(out_dir: &Path, items: &[(PathBuf, String)], target_lufs: f32) -> Result<Value> {
    if items.len() < 2 {
        bail!("give at least two renders to compare");
    }
    if items.len() > 8 {
        bail!("at most 8 candidates per session");
    }
    let mut decoded = Vec::new();
    for (p, _) in items {
        let (l, r) = crate::samples::decode_stereo(p)?;
        let lo = crate::analysis::loudness(&l, &r);
        decoded.push((l, r, lo.integrated_lufs, lo.true_peak_dbtp));
    }
    // one common loudness that keeps every file under -1 dBTP
    let mut target = target_lufs;
    for (_, _, lufs, tp) in &decoded {
        target = target.min(lufs + (-1.0 - tp));
    }
    let id = format!("ab-{:x}", crate::creative::fresh_seed() & 0xFFFF_FFFF);
    let dir = out_dir.join(&id);
    std::fs::create_dir_all(&dir)?;
    // shuffle the order with fresh entropy
    let mut rng = crate::dsp::Rng::new(crate::creative::fresh_seed());
    let mut order: Vec<usize> = (0..items.len()).collect();
    for i in (1..order.len()).rev() {
        let j = rng.below(i + 1);
        order.swap(i, j);
    }
    let labels: Vec<String> = (0..items.len())
        .map(|i| ((b'A' + i as u8) as char).to_string())
        .collect();
    let mut mapping = serde_json::Map::new();
    for (li, &src) in order.iter().enumerate() {
        let (l, r, lufs, tp) = &decoded[src];
        let g = 10f32.powf((target - lufs) / 20.0);
        let l2: Vec<f32> = l.iter().map(|x| x * g).collect();
        let r2: Vec<f32> = r.iter().map(|x| x * g).collect();
        let bytes = crate::export::wav(&l2, &r2, crate::dsp::SR as u32, 16, true, false)?;
        std::fs::write(dir.join(format!("{}.wav", labels[li])), bytes)?;
        mapping.insert(
            labels[li].clone(),
            json!({"source": items[src].0.to_string_lossy(), "name": items[src].1, "original_lufs": lufs, "original_true_peak": tp, "gain_db": target - lufs}),
        );
    }
    std::fs::write(dir.join("RUBRIC.md"), rubric(&labels))?;
    let session = json!({
        "id": id, "labels": labels, "created_unix": now(), "loudness_matched_lufs": (target * 10.0).round() / 10.0,
        "files": labels.iter().map(|l| format!("{l}.wav")).collect::<Vec<_>>(),
        "criteria": CRITERIA.iter().map(|(k, q)| json!({"criterion": k, "question": q})).collect::<Vec<_>>(),
        "note": "No seeds, configs or generator names are in this folder. The key is stored next to it and is read only by blind_ab_reveal.",
    });
    std::fs::write(
        dir.join("session.json"),
        serde_json::to_string_pretty(&session)?,
    )?;
    std::fs::write(
        key_path(&dir),
        serde_json::to_string_pretty(&json!({"id": id, "mapping": mapping}))?,
    )?;
    Ok(
        json!({"session": dir.to_string_lossy(), "id": id, "labels": labels, "loudness_matched_lufs": (target * 10.0).round() / 10.0,
        "next": "listen to the WAVs, then blind_ab_rate; optionally blind_ab_judge with a listening backend; blind_ab_reveal joins notes to sources"}),
    )
}

fn labels_of(dir: &Path) -> Result<Vec<String>> {
    let s: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("session.json"))?)?;
    Ok(s["labels"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

fn append(path: &Path, v: &Value) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{}", serde_json::to_string(v)?)?;
    Ok(())
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .map(|s| {
            s.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Validate and normalise notes: {label: {criterion: text}}.
fn clean_notes(notes: &Value, labels: &[String]) -> Result<Value> {
    let mut out = serde_json::Map::new();
    if let Some(m) = notes.as_object() {
        for (l, crit) in m {
            if !labels.contains(l) {
                bail!(
                    "unknown label '{l}' (session labels: {})",
                    labels.join(", ")
                );
            }
            let mut c = serde_json::Map::new();
            if let Some(cm) = crit.as_object() {
                for (k, v) in cm {
                    let k2 = if k == "emotional_impact" {
                        "emotion"
                    } else {
                        k.as_str()
                    };
                    if !CRITERIA.iter().any(|(x, _)| *x == k2) && k2 != "other" {
                        bail!(
                            "unknown criterion '{k}' (use {} or other)",
                            CRITERIA.iter().map(|x| x.0).collect::<Vec<_>>().join(", ")
                        );
                    }
                    c.insert(k2.into(), v.clone());
                }
            } else if let Some(t) = crit.as_str() {
                c.insert("other".into(), json!(t));
            }
            out.insert(l.clone(), Value::Object(c));
        }
    }
    Ok(Value::Object(out))
}

/// Record a human's blind preference and notes.
pub fn rate(dir: &Path, a: &Value) -> Result<Value> {
    let labels = labels_of(dir)?;
    let pref = a["preference"].as_str().unwrap_or("").to_string();
    if !labels.contains(&pref) && pref != "tie" {
        bail!("preference must be one of {} or 'tie'", labels.join(", "));
    }
    let notes = clean_notes(&a["notes"], &labels)?;
    let entry = json!({
        "kind": "human", "rater": a["rater"].as_str().unwrap_or("human"), "preference": pref,
        "strength": a["strength"].as_u64(), "notes": notes, "comment": a["comment"].as_str().unwrap_or(""), "at_unix": now(),
    });
    append(&dir.join("ratings.jsonl"), &entry)?;
    let n = read_jsonl(&dir.join("ratings.jsonl")).len();
    Ok(
        json!({"recorded": true, "ratings_in_session": n, "revealed": false, "next": "blind_ab_reveal when every listener has rated"}),
    )
}

/// A listening-capable judge.
pub trait ListeningJudge {
    fn name(&self) -> String;
    /// Judge the blind session; returns {"preference", "notes", "comment"}.
    fn judge(&self, session: &Path, labels: &[String], rubric: &str) -> Result<Value>;
}

/// No backend configured: explains how to plug one in.
pub struct StubJudge;

impl ListeningJudge for StubJudge {
    fn name(&self) -> String {
        "stub".into()
    }
    fn judge(&self, _: &Path, _: &[String], _: &str) -> Result<Value> {
        Ok(json!({"status": "no_backend",
            "message": "No listening model is configured (no paid API is used). Set BEATBOX_AB_JUDGE_CMD (or pass command) to an executable that takes the session folder, listens to the WAVs and prints {\"preference\", \"notes\": {label: {criterion: text}}, \"comment\"}. Human ratings via blind_ab_rate are the quality test meanwhile."}))
    }
}

/// An external program (e.g. a local audio-language model wrapper).
pub struct CommandJudge {
    pub command: String,
}

impl ListeningJudge for CommandJudge {
    fn name(&self) -> String {
        format!(
            "command:{}",
            self.command.split_whitespace().next().unwrap_or("")
        )
    }
    fn judge(&self, session: &Path, labels: &[String], _: &str) -> Result<Value> {
        let mut parts = self.command.split_whitespace();
        let prog = parts.next().ok_or_else(|| anyhow!("empty judge command"))?;
        let out = std::process::Command::new(prog)
            .args(parts)
            .arg(session)
            .output()?;
        if !out.status.success() {
            bail!(
                "judge command failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let v: Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| anyhow!("judge printed invalid JSON: {e}"))?;
        let pref = v["preference"].as_str().unwrap_or("tie");
        if !labels.iter().any(|l| l == pref) && pref != "tie" {
            bail!("judge preference '{pref}' is not a session label");
        }
        Ok(
            json!({"preference": pref, "notes": clean_notes(&v["notes"], labels)?, "comment": v["comment"].clone(), "judge": v["judge"].clone()}),
        )
    }
}

pub fn judge(dir: &Path, backend: Option<&str>, command: Option<String>) -> Result<Value> {
    let labels = labels_of(dir)?;
    let cmd = command.or_else(|| std::env::var("BEATBOX_AB_JUDGE_CMD").ok());
    let j: Box<dyn ListeningJudge> = match (backend, cmd) {
        (Some("stub"), _) | (None, None) => Box::new(StubJudge),
        (_, Some(c)) => Box::new(CommandJudge { command: c }),
        (Some(b), None) => bail!("backend '{b}' needs a command (or BEATBOX_AB_JUDGE_CMD)"),
    };
    let v = j.judge(dir, &labels, &rubric(&labels))?;
    if v["status"] == "no_backend" {
        return Ok(v);
    }
    let entry = json!({"kind": "model", "judge": j.name(), "result": v, "at_unix": now()});
    append(&dir.join("judgments.jsonl"), &entry)?;
    Ok(entry)
}

/// Join every rating and judgment to its source and turn the notes into
/// revision hints. This is the only step that reads the key.
pub fn reveal(dir: &Path, preferences: Option<&Path>) -> Result<Value> {
    let key: Value = serde_json::from_str(&std::fs::read_to_string(key_path(dir))?)?;
    let map = key["mapping"].as_object().cloned().unwrap_or_default();
    let ratings = read_jsonl(&dir.join("ratings.jsonl"));
    let judgments = read_jsonl(&dir.join("judgments.jsonl"));
    let mut per = serde_json::Map::new();
    let mut hints = Vec::new();
    for (label, src) in &map {
        let name = src["name"].as_str().unwrap_or(label).to_string();
        let mut prefs = 0;
        let mut notes: serde_json::Map<String, Value> = serde_json::Map::new();
        let mut collect = |pref: &str, n: &Value, who: String| {
            if pref == label {
                prefs += 1;
            }
            if let Some(c) = n[label.as_str()].as_object() {
                for (k, v) in c {
                    let arr = notes.entry(k.clone()).or_insert(json!([]));
                    if let Some(a) = arr.as_array_mut() {
                        a.push(json!(format!(
                            "{who}: {}",
                            v.as_str().unwrap_or(&v.to_string())
                        )));
                    }
                }
            }
        };
        for r in &ratings {
            collect(
                r["preference"].as_str().unwrap_or(""),
                &r["notes"],
                r["rater"].as_str().unwrap_or("human").to_string(),
            );
        }
        for j in &judgments {
            collect(
                j["result"]["preference"].as_str().unwrap_or(""),
                &j["result"]["notes"],
                j["judge"].as_str().unwrap_or("model").to_string(),
            );
        }
        for (k, v) in &notes {
            for t in v.as_array().cloned().unwrap_or_default() {
                hints.push(format!("{name} / {k}: {}", t.as_str().unwrap_or("")));
            }
        }
        per.insert(
            label.clone(),
            json!({"source": src["source"], "name": name, "preferred_by": prefs, "notes": notes}),
        );
    }
    let out = json!({
        "session": dir.to_string_lossy(), "ratings": ratings.len(), "model_judgments": judgments.len(),
        "by_label": per, "revision_hints": hints,
        "note": "Preferences and notes are the quality signal; novelty scores only show the beats are not repeats.",
    });
    let id = dir
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("session");
    std::fs::write(
        dir.with_file_name(format!("{id}.feedback.json")),
        serde_json::to_string_pretty(&out)?,
    )?;
    // blind preference is tracked on its own, apart from any diagnostic score
    if let Some(p) = preferences {
        let done = read_jsonl(p).iter().any(|x| x["session"] == id);
        if !done {
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            for (label, v) in &per {
                append(
                    p,
                    &json!({"session": id, "label": label, "name": v["name"], "source": v["source"], "preferred_by": v["preferred_by"], "listeners": ratings.len() + judgments.len(), "at_unix": now()}),
                )?;
            }
        }
    }
    Ok(out)
}

/// Running tally of blind preferences per beat name (the quality signal).
pub fn preferences(path: &Path) -> Value {
    let rows = read_jsonl(path);
    let mut by: std::collections::BTreeMap<String, (u64, u64, u64)> = Default::default();
    for r in &rows {
        let e = by
            .entry(r["name"].as_str().unwrap_or("?").to_string())
            .or_default();
        e.0 += r["preferred_by"].as_u64().unwrap_or(0);
        e.1 += r["listeners"].as_u64().unwrap_or(0);
        e.2 += 1;
    }
    json!({
        "path": path.to_string_lossy(),
        "beats": by.iter().map(|(k, (w, n, s))| json!({"name": k, "preferred": w, "listener_votes": n, "sessions": s})).collect::<Vec<_>>(),
        "note": "Blind listener preference is the quality signal; diagnostic scores and novelty are tracked separately.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blind_session_rate_and_reveal() {
        let d = std::env::temp_dir().join(format!("bb_ab_{}", crate::creative::fresh_seed()));
        std::fs::create_dir_all(&d).unwrap();
        let sr = crate::dsp::SR as usize;
        let mk = |f: f32, amp: f32, name: &str| -> PathBuf {
            let l: Vec<f32> = (0..sr * 2)
                .map(|i| amp * (i as f32 * f * std::f32::consts::TAU / sr as f32).sin())
                .collect();
            let p = d.join(name);
            std::fs::write(
                &p,
                crate::export::wav(&l, &l, sr as u32, 16, false, false).unwrap(),
            )
            .unwrap();
            p
        };
        let a = mk(220.0, 0.5, "x_seed123.wav");
        let b = mk(330.0, 0.1, "y_seed456.wav");
        let s = create(
            &d.join("ab"),
            &[(a, "beat x".into()), (b, "beat y".into())],
            -14.0,
        )
        .unwrap();
        let dir = PathBuf::from(s["session"].as_str().unwrap());
        // nothing in the session folder names a source or a seed
        for f in std::fs::read_dir(&dir).unwrap() {
            let n = f.unwrap().file_name().to_string_lossy().to_string();
            assert!(
                !n.contains("seed") && !n.contains("x_") && !n.contains("y_"),
                "{n}"
            );
        }
        let sj = std::fs::read_to_string(dir.join("session.json")).unwrap();
        assert!(!sj.contains("seed123") && !sj.contains("beat x"));
        // loudness matched
        let la = crate::samples::decode_stereo(&dir.join("A.wav")).unwrap();
        let lb = crate::samples::decode_stereo(&dir.join("B.wav")).unwrap();
        let (x, y) = (
            crate::analysis::loudness(&la.0, &la.1).integrated_lufs,
            crate::analysis::loudness(&lb.0, &lb.1).integrated_lufs,
        );
        assert!((x - y).abs() < 0.6, "{x} vs {y}");
        assert!(rate(&dir, &json!({"preference": "C"})).is_err());
        rate(&dir, &json!({"rater": "naman", "preference": "A", "notes": {"A": {"groove": "bounces"}, "B": {"memorability": "forgot it"}}, "comment": "A"})).unwrap();
        assert_eq!(
            judge(&dir, Some("stub"), None).unwrap()["status"],
            "no_backend"
        );
        let prefs = d.join("prefs.jsonl");
        let r = reveal(&dir, Some(&prefs)).unwrap();
        reveal(&dir, Some(&prefs)).unwrap();
        let t = preferences(&prefs);
        assert_eq!(t["beats"].as_array().unwrap().len(), 2, "{t}");
        assert_eq!(r["by_label"]["A"]["preferred_by"], 1);
        assert!(!r["revision_hints"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(d);
    }
}
