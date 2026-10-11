//! Sound matching: which of the engine's own instruments best plays a part the
//! way a reference part sounds, and how to finish the match with EQ, reverb
//! and width. The reference is only measured (tonal balance, attack, sustain,
//! width, tail); every candidate is the engine's instrument rendering the
//! transcribed notes, so nothing of the reference's audio is used.

use crate::dsp::SR;
use crate::engine::Engine;
use crate::record_map::{frames, OCT_EDGES};
use crate::tools::{f_opt, obj, s_opt, s_req, u_or, Tool};
use crate::transcribe as tr;
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::{json, Value};

fn pct(v: &[f32], p: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    s[((s.len() - 1) as f32 * p.clamp(0.0, 1.0)).round() as usize]
}

fn db(x: f32) -> f32 {
    10.0 * x.max(1e-12).log10()
}

fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

#[derive(Clone, Debug, Serialize)]
pub struct Timbre {
    /// octave levels (OCT_EDGES) relative to the total, over the sounding frames
    pub oct: [f32; 9],
    pub attack_ms: f32,
    /// level change 150 ms after each note's peak, dB (0 = sustained, -20 = plucked)
    pub sustain_db: f32,
    /// side/mid energy, dB
    pub width_db: f32,
    /// level 300 ms after the notes stop relative to while they sound, dB (room / reverb)
    pub tail_db: f32,
}

pub fn measure(l: &[f32], r: &[f32]) -> Timbre {
    let mono: Vec<f32> = l.iter().zip(r.iter()).map(|(a, b)| 0.5 * (a + b)).collect();
    let f = frames(&mono);
    let fr = SR / tr::HOP as f32;
    let top = pct(&f.e_db, 0.95);
    let act: Vec<usize> = (0..f.e_db.len()).filter(|&i| f.e_db[i] > top - 30.0).collect();
    let mut oct = [0.0f32; 9];
    let tot = db(act.iter().map(|&i| 10f32.powf(f.e_db[i] / 10.0)).sum::<f32>() / act.len().max(1) as f32);
    for k in 0..9 {
        oct[k] = db(act.iter().map(|&i| 10f32.powf(f.oct[i][k] / 10.0)).sum::<f32>() / act.len().max(1) as f32) - tot;
    }
    let peaks = tr::pick_peaks(&f.flux, 0.6, 4);
    let mut att = Vec::new();
    let mut sus = Vec::new();
    for (o, s) in peaks {
        if s < 0.2 {
            continue;
        }
        let z = (o + (0.1 * fr) as usize).min(f.e_db.len().saturating_sub(1));
        let (p, pv) = (o..=z).map(|i| (i, f.e_db[i])).fold((o, f32::MIN), |m, c| if c.1 > m.1 { c } else { m });
        att.push((p - o) as f32 / fr * 1000.0 + 5.0);
        let q = p + (0.15 * fr) as usize;
        if q < f.e_db.len() {
            sus.push(f.e_db[q] - pv);
        }
    }
    let (mut m2, mut s2) = (0.0f32, 0.0f32);
    for (a, b) in l.iter().zip(r.iter()) {
        let (m, s) = (0.5 * (a + b), 0.5 * (a - b));
        m2 += m * m;
        s2 += s * s;
    }
    // tail: frames 250-400 ms after the level falls from the active range
    let mut tails = Vec::new();
    let n = f.e_db.len();
    for i in 1..n {
        if f.e_db[i - 1] > top - 12.0 && f.e_db[i] <= top - 12.0 {
            let a = i + (0.25 * fr) as usize;
            let z = (i + (0.4 * fr) as usize).min(n);
            if a < z {
                tails.push(pct(&f.e_db[a..z], 0.5) - top);
            }
        }
    }
    Timbre { oct, attack_ms: pct(&att, 0.5), sustain_db: if sus.is_empty() { 0.0 } else { pct(&sus, 0.5) }, width_db: db(s2) - db(m2), tail_db: if tails.is_empty() { -60.0 } else { pct(&tails, 0.5).max(-60.0) } }
}

/// Distance between two timbres (lower = closer) and its parts.
pub fn distance(t: &Timbre, c: &Timbre) -> (f32, f32) {
    // tonal balance over the octaves where the target has energy (within 40 dB of its strongest band)
    let mx = t.oct.iter().cloned().fold(f32::MIN, f32::max);
    let ks: Vec<usize> = (0..9).filter(|&k| t.oct[k] > mx - 40.0).collect();
    let tone = (ks.iter().map(|&k| (t.oct[k] - c.oct[k]).clamp(-30.0, 30.0).powi(2)).sum::<f32>() / ks.len().max(1) as f32).sqrt();
    let d = tone + 0.03 * (t.attack_ms - c.attack_ms).abs() + 0.25 * (t.sustain_db - c.sustain_db).abs().min(30.0);
    (d, tone)
}

/// Finishing moves for a candidate: EQ per octave (clamped +-6 dB), a reverb
/// send from the tail difference, a width change.
pub fn finishing(t: &Timbre, c: &Timbre) -> Value {
    let mx = t.oct.iter().cloned().fold(f32::MIN, f32::max);
    let eq: Vec<Value> = (0..9)
        .filter(|&k| t.oct[k] > mx - 40.0)
        .map(|k| (k, (t.oct[k] - c.oct[k]).clamp(-6.0, 6.0)))
        .filter(|(_, g)| g.abs() >= 1.5)
        .map(|(k, g)| json!({"freq_hz": ((OCT_EDGES[k] * OCT_EDGES[k + 1]).sqrt()).round(), "gain_db": r1(g), "q": 1.0}))
        .collect();
    let dt = t.tail_db - c.tail_db;
    let send = if dt > 3.0 { ((dt - 3.0) / 30.0).clamp(0.05, 0.5) } else { 0.0 };
    json!({"eq": eq, "reverb_send": r1(send * 10.0) / 10.0, "width_change_db": r1(t.width_db - c.width_db),
        "attack_diff_ms": r1(t.attack_ms - c.attack_ms), "sustain_diff_db": r1(t.sustain_db - c.sustain_db)})
}

struct Cand {
    label: String,
    args: Value,
    preset: Option<String>,
}

fn parse_cand(s: &str) -> Result<Cand> {
    let (k, v) = s.split_once(':').unwrap_or(("catalog", s));
    let v = v.trim().to_string();
    Ok(match k.trim() {
        "catalog" => Cand { label: s.to_string(), args: json!({"catalog_id": v}), preset: None },
        "font" | "instrument" => Cand { label: s.to_string(), args: json!({"instrument": v}), preset: None },
        "preset" => Cand { label: s.to_string(), args: Value::Null, preset: Some(v) },
        other => bail!("candidate '{s}': use catalog:<id>, font:<name> or preset:<name> (got '{other}')"),
    })
}

/// Render one candidate playing `notes` (start step, len steps, pitch, vel 0-1) in a scratch engine.
fn render_cand(dir: &std::path::Path, c: &Cand, notes: &[Value], bpm: f32, bars: usize) -> Result<(Vec<f32>, Vec<f32>)> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let mut s = Engine::new(dir.to_path_buf());
    s.call("new_project", &json!({"patterns": [{"name": "A", "bars": bars.clamp(1, 16)}]}))?;
    s.call("set_tempo", &json!({"bpm": bpm}))?;
    match &c.preset {
        Some(p) => {
            s.call("add_track", &json!({"name": "part", "preset": p}))?;
        }
        None => {
            let mut a = c.args.clone();
            a["track"] = json!("part");
            s.call("load_instrument", &a)?;
        }
    }
    s.call("add_notes", &json!({"track": "part", "notes": notes, "replace": true}))?;
    let out = dir.join("cand.wav");
    s.call("render", &json!({"path": out.to_string_lossy(), "tail": 0.8}))?;
    let st = crate::samples::decode_stereo(&out)?;
    let _ = std::fs::remove_file(&out);
    Ok(st)
}

fn match_timbre(e: &mut Engine, a: &Value) -> Result<Value> {
    let path = e.resolve(&s_req(a, "target")?);
    if !path.exists() {
        bail!("no file at {}", path.display());
    }
    let (l, r) = crate::samples::decode_stereo(&path)?;
    let dur = l.len() as f32 / SR;
    let t0 = f_opt(a, "t0").unwrap_or(0.0).clamp(0.0, dur);
    let t1 = f_opt(a, "t1").unwrap_or(dur).clamp(t0, dur).min(t0 + 16.0);
    if t1 - t0 < 1.0 {
        bail!("the target span is under a second");
    }
    let (ia, iz) = ((t0 * SR) as usize, ((t1 * SR) as usize).min(l.len()));
    let target = measure(&l[ia..iz], &r[ia..iz]);
    // the notes the candidates play: a transcription file (transcribe_bass / transcribe_melody JSON) or a fresh one
    let role = s_opt(a, "role").unwrap_or_else(|| "bass".into());
    let (bpm, notes_abs): (f32, Vec<(f32, f32, u8, u8)>) = match s_opt(a, "notes_file") {
        Some(nf) => {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(e.resolve(&nf))?)?;
            let bpm = v["grid"]["bpm"].as_f64().ok_or_else(|| anyhow!("notes_file has no grid.bpm (use a transcribe_* JSON)"))? as f32;
            let ns: Vec<(f32, f32, u8, u8)> = v["notes"].as_array().cloned().unwrap_or_default().iter().filter_map(|n| Some((n["t0"].as_f64()? as f32, n["t1"].as_f64()? as f32, n["midi"].as_u64()? as u8, n["vel"].as_u64().unwrap_or(90) as u8))).collect();
            (bpm, ns)
        }
        None => {
            let mono: Vec<f32> = l.iter().zip(r.iter()).map(|(x, y)| 0.5 * (x + y)).collect();
            let (g, _) = crate::tools_transcribe::grid_for(e, a, &path, &mono)?;
            let ns = if role.contains("bass") { tr::transcribe_bass(&mono, g, 16) } else { tr::transcribe_top_line(&mono, g, 16, 80.0, 2000.0) };
            (g.bpm, ns.iter().map(|n| (n.t0, n.t1, n.pitch, n.vel)).collect())
        }
    };
    let step_s = 60.0 / bpm / 4.0;
    let notes: Vec<Value> = notes_abs
        .iter()
        .filter(|n| n.0 >= t0 && n.0 < t1)
        .map(|n| json!({"start": ((n.0 - t0) / step_s * 4.0).round() / 4.0, "len": ((n.1 - n.0) / step_s).max(0.25), "pitch": n.2, "vel": n.3 as f32 / 127.0}))
        .collect();
    if notes.is_empty() {
        bail!("no notes in the target span to play (check t0/t1 or notes_file)");
    }
    let bars = (((t1 - t0) / (step_s * 16.0)).ceil() as usize).max(1);
    // candidates: given, or the catalog's shortlist for the role (loadable formats, size-capped)
    let limit = u_or(a, "limit", 4).clamp(1, 8) as usize;
    let mut cands: Vec<Cand> = Vec::new();
    if let Some(arr) = a.get("candidates").and_then(|v| v.as_array()) {
        for c in arr {
            cands.push(parse_cand(c.as_str().ok_or_else(|| anyhow!("candidates are strings"))?)?);
        }
    } else {
        let max_mb = f_opt(a, "max_mb").unwrap_or(40.0) as f64;
        for (_, ent, _) in crate::sound_catalog::pick_for_role(&role, None, Some(max_mb)) {
            if ent.format == "sfz_git" || ent.format == "vsco_pack" {
                cands.push(Cand { label: format!("catalog:{}", ent.id), args: json!({"catalog_id": ent.id}), preset: None });
            }
            if cands.len() >= limit {
                break;
            }
        }
    }
    if cands.is_empty() {
        bail!("no candidates: pass candidates [\"catalog:<id>\", \"font:<name>\", \"preset:<name>\"]");
    }
    let scratch = e.workdir.join(".beatbox_match");
    let mut rows: Vec<(f32, Value, Option<Timbre>, usize)> = Vec::new();
    for (i, c) in cands.iter().enumerate().take(8) {
        match render_cand(&scratch.join(format!("c{i}")), c, &notes, bpm, bars) {
            Ok((cl, cr)) => {
                let tm = measure(&cl, &cr);
                let (d, tone) = distance(&target, &tm);
                rows.push((d, json!({"candidate": c.label, "distance": r1(d), "tone_diff_db": r1(tone), "attack_ms": r1(tm.attack_ms), "sustain_db": r1(tm.sustain_db)}), Some(tm), i));
            }
            Err(err) => rows.push((f32::MAX, json!({"candidate": c.label, "error": format!("{err:#}")}), None, i)),
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let best = rows.first().filter(|r| r.2.is_some()).map(|r| (r.3, r.2.clone().unwrap()));
    let mut applied = Value::Null;
    let finish = best.as_ref().map(|(_, tm)| finishing(&target, tm));
    if let (Some(track), Some((i, _))) = (s_opt(a, "apply_to_track"), best.as_ref()) {
        let c = &cands[*i];
        let res = match &c.preset {
            Some(p) => {
                if e.project.track_index(&track).is_ok() {
                    e.call_from("set_instrument", &json!({"track": track, "preset": p}), "match_timbre")
                } else {
                    e.call_from("add_track", &json!({"name": track, "preset": p}), "match_timbre")
                }
            }
            None => {
                let mut la = c.args.clone();
                la["track"] = json!(track);
                e.call_from("load_instrument", &la, "match_timbre")
            }
        };
        applied = match res {
            Ok(_) => json!({"track": track, "instrument": c.label}),
            Err(err) => json!({"error": format!("{err:#}")}),
        };
    }
    Ok(json!({"target": {"span_s": [r1(t0), r1(t1)], "attack_ms": r1(target.attack_ms), "sustain_db": r1(target.sustain_db), "width_db": r1(target.width_db), "tail_db": r1(target.tail_db),
            "octaves_db": target.oct.iter().map(|v| r1(*v)).collect::<Vec<_>>()},
        "notes_played": notes.len(), "ranked": rows.iter().map(|r| r.1.clone()).collect::<Vec<_>>(),
        "best": rows.first().map(|r| r.1["candidate"].clone()), "finish_with": finish, "applied": applied,
        "legend": "distance = octave-balance RMS dB + attack and sustain differences; finish_with = EQ moves (dB per octave centre), reverb send 0-0.5, width change, to apply with eq_7band / add_effect / set_send",
        "next": ["load_instrument", "add_effect", "set_send", "score_sections"]}))
}

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "match_timbre",
        description: "Find which of the engine's instruments sounds most like a part in a recording, and how to finish the match. Measures the target span of a stem (octave balance, attack, sustain, width, room tail), renders each candidate playing the part's notes (from notes_file = a transcribe_bass / transcribe_melody JSON, or transcribed on the spot), ranks them, and returns EQ moves, a reverb send and a width change for the best. Candidates: [\"catalog:<id>\", \"font:<name>\", \"preset:<name>\"] or, by default, the catalog's shortlist for role. apply_to_track loads the winner on that track.",
        mutates: true,
        schema: || obj(json!({
            "target": {"type": "string", "description": "the reference part's audio (a stem)"},
            "t0": {"type": "number"}, "t1": {"type": "number", "description": "span to match (default the first 16 s)"},
            "notes_file": {"type": "string"},
            "role": {"type": "string", "description": "bass, guitar, piano, strings, lead, ... (catalog shortlist; default bass)"},
            "candidates": {"type": "array", "items": {"type": "string"}},
            "limit": {"type": "integer"}, "max_mb": {"type": "number"},
            "bpm": {"type": "number"}, "grid_from": {"type": "string"},
            "apply_to_track": {"type": "string"}
        }), &["target"]),
        run: match_timbre,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn note(hz: f32, secs: f32, bright: bool, decay: f32) -> Vec<f32> {
        (0..(secs * SR) as usize)
            .map(|i| {
                let t = i as f32 / SR;
                let env = (t * 300.0).min(1.0) * (-t * decay).exp();
                let mut v = (2.0 * PI * hz * t).sin();
                if bright {
                    for h in 2..10 {
                        v += (2.0 * PI * hz * h as f32 * t).sin() / h as f32;
                    }
                }
                0.3 * env * v
            })
            .collect()
    }

    #[test]
    fn measures_tell_dull_from_bright_and_plucked_from_held() {
        let mut dull = Vec::new();
        let mut bright = Vec::new();
        for _ in 0..4 {
            dull.extend(note(110.0, 0.5, false, 0.5));
            bright.extend(note(110.0, 0.5, true, 12.0));
        }
        let td = measure(&dull, &dull);
        let tb = measure(&bright, &bright);
        // the bright one has more energy above 640 Hz, and decays faster
        let hi = |t: &Timbre| t.oct[4..].iter().map(|v| 10f32.powf(v / 10.0)).sum::<f32>();
        assert!(hi(&tb) > 10.0 * hi(&td), "{:?} {:?}", tb.oct, td.oct);
        assert!(tb.sustain_db < td.sustain_db - 3.0, "{} {}", tb.sustain_db, td.sustain_db);
        assert!(distance(&tb, &tb).0 < 0.01);
        assert!(distance(&tb, &td).0 > distance(&tb, &tb).0 + 3.0);
        let f = finishing(&tb, &td);
        assert!(f["eq"].as_array().unwrap().iter().any(|m| m["gain_db"].as_f64().unwrap() > 1.5), "{f}");
        assert!(td.width_db < -30.0);
    }

    #[test]
    fn candidates_parse() {
        assert!(parse_cand("catalog:abc").unwrap().args["catalog_id"] == "abc");
        assert!(parse_cand("preset:bass").unwrap().preset.as_deref() == Some("bass"));
        assert!(parse_cand("bogus:x").is_err());
    }
}
