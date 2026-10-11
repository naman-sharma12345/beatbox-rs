//! Seamless vocal edits: cut points snap to the quiet between syllables and
//! then to a zero crossing (never mid-syllable), every join gets an
//! equal-power crossfade (5-30 ms), timeline clips get their edges moved to
//! safe points with fades, and check_vocal_edits reports any clipped
//! syllable or click at an edit.

use crate::dsp::{Biquad, BiquadKind, SR};
use crate::engine::Engine;
use crate::tools::{b_or, f_or, obj, s_opt, s_req, Tool};
use crate::vocal_flex::{band_db, voice_threshold, HOP};
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// The best place to cut near `t` (seconds): the quietest 5 ms hop of the
/// voice band within ±`search_s` (ties go to the nearest), then the nearest
/// zero crossing within 2 ms. Returns (seconds, level dB there, voiced?).
pub fn snap_cut(x: &[f32], db: &[f32], thr: f32, t: f32, search_s: f32) -> (f32, f32, bool) {
    if db.is_empty() || x.is_empty() {
        return (t, -120.0, false);
    }
    let h = ((t / HOP).round() as i64).clamp(0, db.len() as i64 - 1) as usize;
    let w = (search_s / HOP).round() as usize;
    let (a, b) = (h.saturating_sub(w), (h + w).min(db.len() - 1));
    let mut best = h;
    for i in a..=b {
        let pen = |j: usize| db[j] + 0.02 * (j as f32 - h as f32).abs();
        if pen(i) < pen(best) {
            best = i;
        }
    }
    let mut s = ((best as f32 * HOP) * SR) as usize;
    s = s.min(x.len() - 1);
    let zw = (0.002 * SR) as usize;
    let mut z = s;
    for d in 0..zw {
        let up = s + d;
        if up + 1 < x.len() && (x[up] <= 0.0) != (x[up + 1] <= 0.0) {
            z = up;
            break;
        }
        if d <= s && s - d >= 1 && (x[s - d - 1] <= 0.0) != (x[s - d] <= 0.0) {
            z = s - d;
            break;
        }
    }
    (z as f32 / SR, db[best], db[best] >= thr)
}

/// Join pieces with equal-power crossfades of `xf` samples.
pub fn join(pieces: &[Vec<f32>], xf: usize) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::new();
    for p in pieces {
        if out.is_empty() {
            out.extend_from_slice(p);
            continue;
        }
        let n = xf.min(out.len()).min(p.len());
        let start = out.len() - n;
        for i in 0..n {
            let a = std::f32::consts::FRAC_PI_2 * (i as f32 + 0.5) / n as f32;
            out[start + i] = out[start + i] * a.cos() + p[i] * a.sin();
        }
        out.extend_from_slice(&p[n..]);
    }
    out
}

/// Click score at sample `s`: the high-passed jump over its surroundings (dB).
pub fn click_db(x: &[f32], s: usize) -> f32 {
    if x.len() < 64 {
        return 0.0;
    }
    let s = s.clamp(16, x.len() - 17);
    let mut hp = Biquad::new(BiquadKind::LowCut, 4000.0, 0.707, 0.0);
    let a = s.saturating_sub((0.02 * SR) as usize);
    let b = (s + (0.02 * SR) as usize).min(x.len());
    let y: Vec<f32> = x[a..b].iter().map(|v| hp.process(*v).abs()).collect();
    let c = s - a;
    let near = y[c.saturating_sub(16)..(c + 16).min(y.len())].iter().cloned().fold(0.0f32, f32::max);
    let around: f32 = y.iter().sum::<f32>() / y.len() as f32;
    20.0 * (near.max(1e-7) / around.max(1e-7)).log10()
}

fn r3(x: f32) -> f32 {
    (x * 1000.0).round() / 1000.0
}

/// One edit point's health.
pub fn check_point(x: &[f32], db: &[f32], thr: f32, t: f32) -> Value {
    let h = ((t / HOP) as usize).min(db.len().saturating_sub(1));
    let lvl = db.get(h).copied().unwrap_or(-120.0);
    // the loudest the voice gets within 150 ms
    let w = (0.15 / HOP) as usize;
    let peak = db[h.saturating_sub(w)..(h + w).min(db.len())].iter().cloned().fold(-120.0f32, f32::max);
    let cdb = click_db(x, (t * SR) as usize);
    let mid_syllable = lvl >= thr && lvl > peak - 10.0;
    let mut issues = Vec::new();
    if mid_syllable {
        issues.push("cuts into a syllable");
    }
    if cdb > 14.0 {
        issues.push("click");
    }
    json!({"at_s": r3(t), "level_db": (lvl * 10.0).round() / 10.0, "below_nearby_peak_db": ((peak - lvl) * 10.0).round() / 10.0, "click_db": (cdb * 10.0).round() / 10.0, "ok": issues.is_empty(), "issues": issues})
}

fn regions(a: &Value, key: &str) -> Result<Vec<(f32, f32)>> {
    let Some(v) = a.get(key).and_then(|v| v.as_array()) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for r in v {
        let (f, t) = if let Some(arr) = r.as_array() {
            (arr.first().and_then(|x| x.as_f64()), arr.get(1).and_then(|x| x.as_f64()))
        } else {
            (r.get("from_s").and_then(|x| x.as_f64()), r.get("to_s").and_then(|x| x.as_f64()))
        };
        match (f, t) {
            (Some(f), Some(t)) if t > f => out.push((f as f32, t as f32)),
            _ => bail!("{key}: each region is {{from_s, to_s}} or [from, to] with to > from"),
        }
    }
    Ok(out)
}

fn edit_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
    let len = x.len() as f32 / SR;
    let cuts = regions(a, "cut")?;
    let keep = regions(a, "keep")?;
    if cuts.is_empty() == keep.is_empty() {
        bail!("give either cut (regions to remove) or keep (regions to keep, in the order to play them)");
    }
    // keep-list = the pieces; a cut-list becomes its complement
    let pieces_t: Vec<(f32, f32)> = if !keep.is_empty() {
        keep
    } else {
        let mut c = cuts.clone();
        c.sort_by(|p, q| p.0.partial_cmp(&q.0).unwrap());
        let mut v = Vec::new();
        let mut at = 0.0;
        for (f, t) in c {
            if f > at {
                v.push((at, f));
            }
            at = at.max(t);
        }
        if at < len {
            v.push((at, len));
        }
        v
    };
    let db = band_db(&x);
    let thr = voice_threshold(&db, None);
    let search = f_or(a, "snap_ms", 40.0).clamp(0.0, 200.0) / 1000.0;
    let xf_ms = f_or(a, "crossfade_ms", 12.0).clamp(5.0, 30.0);
    let xf = (xf_ms / 1000.0 * SR) as usize;
    let mut pieces = Vec::new();
    let mut points = Vec::new();
    for (f, t) in pieces_t {
        let (sf, lf, vf) = if f <= 0.001 { (0.0, -120.0, false) } else { snap_cut(&x, &db, thr, f, search) };
        let (st, lt, vt) = if t >= len - 0.001 { (len, -120.0, false) } else { snap_cut(&x, &db, thr, t, search) };
        if st <= sf + 0.02 {
            continue;
        }
        let (s0, s1) = ((sf * SR) as usize, ((st * SR) as usize).min(x.len()));
        pieces.push(x[s0..s1].to_vec());
        points.push(json!({"asked": [r3(f), r3(t)], "snapped": [r3(sf), r3(st)], "moved_ms": [r3((sf - f) * 1000.0), r3((st - t) * 1000.0)], "level_db": [(lf * 10.0).round() / 10.0, (lt * 10.0).round() / 10.0], "voiced_at_cut": vf || vt}));
    }
    if pieces.is_empty() {
        bail!("the edit leaves nothing");
    }
    let mut y = join(&pieces, xf);
    crate::audio_edit::fade(&mut y, 3.0, 8.0);
    let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_edit", info.name));
    let name = crate::samples::sample_name(&nn);
    std::fs::create_dir_all(e.samples_dir())?;
    let path = e.samples_dir().join(format!("{name}.wav"));
    crate::render::write_wav(&path, &y, &y)?;
    let si = crate::samples::SampleInfo { name, path: path.to_string_lossy().into(), source: format!("{} (seamless edit, {} pieces)", info.source, pieces.len()), license: info.license.clone(), author: info.author.clone(), duration: 0.0 };
    let reg = crate::tools::register_sample(e, si)?;
    e.revision += 1;
    // the joins, checked
    let ydb = band_db(&y);
    let ythr = voice_threshold(&ydb, None);
    let mut joins = Vec::new();
    let mut at = 0.0f32;
    for p in pieces.iter().take(pieces.len().saturating_sub(1)) {
        at += p.len() as f32 / SR - xf as f32 / SR;
        joins.push(check_point(&y, &ydb, ythr, at + 0.5 * xf as f32 / SR));
    }
    let bad = joins.iter().filter(|j| !j["ok"].as_bool().unwrap_or(true)).count();
    Ok(json!({"sample": reg["sample"], "pieces": points, "crossfade_ms": xf_ms, "seconds": r3(y.len() as f32 / SR), "joins": joins, "joins_with_issues": bad}))
}

fn check_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    if let Some(track) = s_opt(a, "track") {
        // every clip edge on a track, inside its own sample
        let ti = e.project.track_index(&track)?;
        let tname = e.project.tracks[ti].name.clone();
        let clips: Vec<(usize, crate::project::AudioClip)> = e.project.audio_clips.iter().cloned().enumerate().filter(|(_, c)| c.track.eq_ignore_ascii_case(&tname)).collect();
        if clips.is_empty() {
            bail!("no audio clips on '{tname}'");
        }
        let mut out = Vec::new();
        let mut bad = 0;
        for (i, c) in clips {
            let (_, x) = crate::tools_sound::sample_data(e, &c.sample)?;
            let db = band_db(&x);
            let thr = voice_threshold(&db, None);
            let len = x.len() as f32 / SR;
            let s = c.offset_s.max(0.0);
            let end = c.length_s.map(|l| s + l).unwrap_or(len).min(len);
            let mut pts = Vec::new();
            if s > 0.005 {
                pts.push(check_point(&x, &db, thr, s));
            }
            if end < len - 0.005 {
                pts.push(check_point(&x, &db, thr, end));
            }
            let issues: usize = pts.iter().filter(|p| p["issues"].as_array().map(|v| v.iter().any(|s| s == "cuts into a syllable")).unwrap_or(false)).count();
            bad += issues;
            out.push(json!({"clip": i, "sample": c.sample, "start_beat": c.start_beat, "edges": pts, "fades_ms": [c.fade_in_ms, c.fade_out_ms]}));
        }
        return Ok(json!({"track": tname, "clips": out, "edges_cutting_syllables": bad, "fix": if bad > 0 { "snap_clip_edges moves these edges to the quiet between syllables" } else { "all clip edges sit between syllables" }}));
    }
    let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
    let db = band_db(&x);
    let thr = voice_threshold(&db, None);
    let times: Vec<f32> = match a.get("at_s").and_then(|v| v.as_array()) {
        Some(v) => v.iter().filter_map(|t| t.as_f64()).map(|t| t as f32).collect(),
        None => {
            // no list: scan for clicks (sharp jumps) as likely edit points
            let mut v = Vec::new();
            let step = (0.005 * SR) as usize;
            let mut s = step * 4;
            while s + step * 4 < x.len() {
                if click_db(&x, s) > 18.0 {
                    v.push(s as f32 / SR);
                    s += (0.05 * SR) as usize;
                } else {
                    s += step;
                }
            }
            v
        }
    };
    let pts: Vec<Value> = times.iter().map(|&t| check_point(&x, &db, thr, t)).collect();
    let bad = pts.iter().filter(|p| !p["ok"].as_bool().unwrap_or(true)).count();
    Ok(json!({"sample": info.name, "points": pts, "with_issues": bad}))
}

fn snap_clips_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let track = s_req(a, "track")?;
    let ti = e.project.track_index(&track)?;
    let tname = e.project.tracks[ti].name.clone();
    let search = f_or(a, "snap_ms", 60.0).clamp(0.0, 250.0) / 1000.0;
    let fade = f_or(a, "fade_ms", 10.0).clamp(3.0, 30.0);
    let bpm = e.project.bpm;
    let keep_timing = b_or(a, "keep_word_timing", true);
    let mut moved = Vec::new();
    let n = e.project.audio_clips.len();
    for i in 0..n {
        let c = e.project.audio_clips[i].clone();
        if !c.track.eq_ignore_ascii_case(&tname) {
            continue;
        }
        let (_, x) = crate::tools_sound::sample_data(e, &c.sample)?;
        let db = band_db(&x);
        let thr = voice_threshold(&db, None);
        let len = x.len() as f32 / SR;
        let s = c.offset_s.max(0.0);
        let end = c.length_s.map(|l| s + l).unwrap_or(len).min(len);
        let (ns, _, _) = if s > 0.005 { snap_cut(&x, &db, thr, s, search) } else { (s, 0.0, false) };
        let (ne, _, _) = if end < len - 0.005 { snap_cut(&x, &db, thr, end, search) } else { (end, 0.0, false) };
        if ne <= ns + 0.05 {
            continue;
        }
        let cl = &mut e.project.audio_clips[i];
        // moving the start keeps the words where they were in the song
        if keep_timing {
            cl.start_beat += (ns - s) * bpm / 60.0;
        }
        cl.offset_s = ns;
        cl.length_s = if ne < len - 0.001 { Some(ne - ns) } else { None };
        cl.fade_in_ms = Some(fade);
        cl.fade_out_ms = Some(fade);
        moved.push(json!({"clip": i, "start_moved_ms": r3((ns - s) * 1000.0), "end_moved_ms": r3((ne - end) * 1000.0)}));
    }
    if moved.is_empty() {
        bail!("no audio clips on '{tname}'");
    }
    e.revision += 1;
    Ok(json!({"track": tname, "clips": moved, "fade_ms": fade, "next": "check_vocal_edits {track} to confirm"}))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "edit_vocal",
            description: "Seamless vocal edit into a new sample: cut = regions to remove, or keep = regions to keep in the order to play them (a chop or rearrangement); regions are {from_s, to_s} or [from, to]. Every boundary snaps (within snap_ms, default 40) to the quietest point between syllables and then to a zero crossing, and every join gets an equal-power crossfade (crossfade_ms 5-30, default 12). Reports where each cut moved and checks every join for clipped syllables and clicks.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "cut": {"type": "array", "items": {"type": ["object", "array"]}}, "keep": {"type": "array", "items": {"type": ["object", "array"]}}, "snap_ms": {"type": "number"}, "crossfade_ms": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: edit_tool,
        },
        Tool {
            name: "check_vocal_edits",
            description: "Check edit points for clipped syllables and clicks. sample + at_s (list of seconds) checks those points (without at_s it scans the sample for click-like jumps); track checks every audio clip edge on that track inside its sample. Each point reports its level, how far under the nearby peak it is, a click score and its issues.",
            mutates: false,
            schema: || obj(json!({"sample": {"type": "string"}, "at_s": {"type": "array", "items": {"type": "number"}}, "track": {"type": "string"}}), &[]),
            run: check_tool,
        },
        Tool {
            name: "snap_clip_edges",
            description: "Make a track's audio clip edges seamless: each clip's start and end move (within snap_ms, default 60) to the quiet between syllables and a zero crossing, and get equal-power fades (fade_ms 3-30, default 10). keep_word_timing (default true) shifts the clip so its words stay where they were in the song.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "snap_ms": {"type": "number"}, "fade_ms": {"type": "number"}, "keep_word_timing": {"type": "boolean"}}), &["track"]),
            run: snap_clips_tool,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(times: &[f32], len: f32) -> Vec<f32> {
        let mut x = vec![0.0f32; (len * SR) as usize];
        for &t in times {
            let s = (t * SR) as usize;
            for i in 0..(0.2 * SR) as usize {
                if s + i < x.len() {
                    let env = (i as f32 / 200.0).min(1.0) * (1.0 - i as f32 / (0.2 * SR));
                    x[s + i] = 0.5 * env * (i as f32 / SR * 250.0 * std::f32::consts::TAU).sin();
                }
            }
        }
        x
    }

    #[test]
    fn cuts_snap_to_the_gap_and_joins_are_clean() {
        // words at 0.1, 0.4, 0.7 (200 ms each, 100 ms gaps); ask to cut mid-word
        let x = words(&[0.1, 0.4, 0.7], 1.2);
        let db = band_db(&x);
        let thr = voice_threshold(&db, None);
        let (t, _, voiced) = snap_cut(&x, &db, thr, 0.28, 0.06);
        assert!(!voiced && t > 0.30 && t < 0.40, "snapped to {t}");
        let pieces = vec![x[..(0.33 * SR) as usize].to_vec(), x[(0.63 * SR) as usize..].to_vec()];
        let y = join(&pieces, (0.012 * SR) as usize);
        let ydb = band_db(&y);
        let p = check_point(&y, &ydb, voice_threshold(&ydb, None), 0.33);
        assert!(p["ok"].as_bool().unwrap(), "{p}");
        // a raw mid-word cut is flagged
        let raw: Vec<f32> = [&x[..(0.2 * SR) as usize], &x[(0.5 * SR) as usize..]].concat();
        let rdb = band_db(&raw);
        let q = check_point(&raw, &rdb, voice_threshold(&rdb, None), 0.2);
        assert!(!q["ok"].as_bool().unwrap(), "{q}");
    }
}
