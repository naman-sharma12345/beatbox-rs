//! Mixing-suite primitives as MCP actions: numeric gain staging and the
//! measurements an engineer mixing through MCP reaches for. Every action
//! acts only when called; nothing here is on the default render path.
//!
//! Meters: integrated loudness per ITU-R BS.1770-4 (`analysis::loudness_at`),
//! true peak per BS.1770-4 Annex 2 (`dsp::true_peak`), sample peak, and an
//! active RMS over 400 ms blocks gated 40 dB under the loudest block (a
//! relative gate, so a fader move shifts the reading by exactly the move).

use crate::analysis;
use crate::automation::{parse_target, Target};
use crate::dsp::true_peak::true_peak_dbtp;
use crate::dsp::SR;
use crate::engine::Engine;
use crate::project::Project;
use crate::tools::{b_or, f_opt, obj, s_opt, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// Fader range the mixer accepts (same as set_mixer).
const FADER_MIN_DB: f32 = -60.0;
const FADER_MAX_DB: f32 = 12.0;

fn r2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

/// The meters gain staging can aim at.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Meter {
    Peak,
    TruePeak,
    Rms,
    Lufs,
}

impl Meter {
    pub(crate) fn parse(s: &str) -> Result<Meter> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "peak" | "sample_peak" => Meter::Peak,
            "true_peak" | "tp" | "dbtp" => Meter::TruePeak,
            "rms" => Meter::Rms,
            "lufs" | "loudness" | "integrated" => Meter::Lufs,
            other => bail!("measure must be peak, true_peak, rms or lufs (got '{other}')"),
        })
    }
    fn unit(self) -> &'static str {
        match self {
            Meter::Peak => "dBFS sample peak",
            Meter::TruePeak => "dBTP",
            Meter::Rms => "dBFS active RMS",
            Meter::Lufs => "LUFS integrated",
        }
    }
}

/// Sample peak of both channels, dBFS (None when silent).
pub(crate) fn peak_db(l: &[f32], r: &[f32]) -> Option<f32> {
    let p = l.iter().chain(r.iter()).fold(0.0f32, |m, v| m.max(v.abs()));
    (p > 1e-9).then(|| 20.0 * p.log10())
}

/// RMS over the 400 ms blocks within 40 dB of the loudest block, dBFS
/// (both channels' mean square averaged, so a centred mono signal at
/// amplitude A reads like A on one channel).
pub(crate) fn active_rms_db(l: &[f32], r: &[f32]) -> Option<f32> {
    let n = l.len().min(r.len());
    let blk = (0.4 * SR) as usize;
    let mut ms = Vec::new();
    let mut i = 0;
    while i < n {
        let j = (i + blk).min(n);
        let s: f64 = (i..j).map(|k| (l[k] as f64).powi(2) + (r[k] as f64).powi(2)).sum();
        ms.push(s / (2.0 * (j - i) as f64));
        i = j;
    }
    let top = ms.iter().cloned().fold(0.0f64, f64::max);
    if top <= 1e-18 {
        return None;
    }
    let gate = top * 1e-4;
    let act: Vec<f64> = ms.into_iter().filter(|v| *v >= gate).collect();
    let mean = act.iter().sum::<f64>() / act.len() as f64;
    Some((10.0 * mean.log10()) as f32)
}

/// One meter's reading of a stereo signal (None when silent / below the gate).
pub(crate) fn read_meter(m: Meter, l: &[f32], r: &[f32]) -> Option<f32> {
    match m {
        Meter::Peak => peak_db(l, r),
        Meter::TruePeak => {
            let v = true_peak_dbtp(&[l, r]);
            (v.is_finite() && v > -180.0).then_some(v)
        }
        Meter::Rms => active_rms_db(l, r),
        Meter::Lufs => {
            let v = analysis::loudness_at(l, r, SR).integrated_lufs;
            (v.is_finite() && v > -69.0).then_some(v)
        }
    }
}

/// Track names from a name, a comma list or an array (empty = none given).
fn name_list(a: &Value, k: &str) -> Vec<String> {
    match a.get(k) {
        Some(Value::String(s)) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        Some(Value::Array(v)) => v.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).collect(),
        _ => Vec::new(),
    }
}

/// Indices of the enabled volume lanes that drive a track (they replace the
/// fader while present, so staging shifts them instead).
fn volume_lanes(p: &Project, track: &str) -> Vec<usize> {
    p.automation
        .iter()
        .enumerate()
        .filter(|(_, l)| l.enabled && !l.points.is_empty() && l.target.eq_ignore_ascii_case(track) && parse_target(&l.param) == Some(Target::Volume))
        .map(|(i, _)| i)
        .collect()
}

/// Every track's post-fader stem (what it sends to its output), by track index.
fn track_stems(e: &mut Engine, p: &Project) -> Result<Vec<Option<(Vec<f32>, Vec<f32>)>>> {
    let m = e.render_stems_all(p)?;
    Ok(p
        .tracks
        .iter()
        .map(|t| m.stems.iter().find(|s| s.name == t.name && !s.left.is_empty()).map(|s| (s.left.clone(), s.right.clone())))
        .collect())
}

fn gain_stage(e: &mut Engine, a: &Value) -> Result<Value> {
    let meter = Meter::parse(&s_opt(a, "measure").unwrap_or_else(|| "lufs".into()))?;
    let target = f_opt(a, "target_db");
    let per = a.get("targets").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    if target.is_none() && per.is_empty() {
        bail!("give target_db (one level for every chosen track) and/or targets ({{track: dB}})");
    }
    for (k, v) in per.iter() {
        if !v.is_number() {
            bail!("targets.{k} must be a number in dB");
        }
    }
    let range = match meter {
        Meter::Lufs => -60.0..=0.0,
        _ => -80.0..=0.0,
    };
    let check = |v: f32, what: &str| -> Result<()> {
        if !range.contains(&v) {
            bail!("{what} {v} is outside {:?} for {}", range, meter.unit());
        }
        Ok(())
    };
    if let Some(t) = target {
        check(t, "target_db")?;
    }
    let mut p = e.project.clone();
    // which tracks: the explicit list, else the targets' keys, else every
    // track when target_db applies to all
    let mut chosen = name_list(a, "tracks");
    if chosen.is_empty() {
        chosen = if target.is_some() { p.tracks.iter().map(|t| t.name.clone()).collect() } else { per.keys().cloned().collect() };
    }
    let mut idx = Vec::new();
    for n in &chosen {
        let i = p.track_index(n)?;
        let want = per
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&p.tracks[i].name))
            .and_then(|(_, v)| v.as_f64())
            .map(|v| v as f32)
            .or(target);
        let Some(want) = want else { bail!("no target for track '{}' (add it to targets or give target_db)", p.tracks[i].name) };
        check(want, &format!("target for '{}'", p.tracks[i].name))?;
        if !idx.iter().any(|(j, _)| *j == i) {
            idx.push((i, want));
        }
    }
    for k in per.keys() {
        let i = p.track_index(k)?;
        if !idx.iter().any(|(j, _)| *j == i) {
            bail!("targets names '{k}', which is not in tracks");
        }
    }
    let stems = track_stems(e, &p)?;
    let mut rows = Vec::new();
    let mut moved = Vec::new();
    for &(i, want) in &idx {
        let name = p.tracks[i].name.clone();
        let before = stems[i].as_ref().and_then(|(l, r)| read_meter(meter, l, r));
        let Some(before) = before else {
            rows.push(json!({"track": name, "skipped": "silent in this render (muted, no notes, or soloed out)"}));
            continue;
        };
        let d = want - before;
        let lanes = volume_lanes(&p, &name);
        let mut row = json!({"track": name, "target_db": want, "before_db": r2(before), "change_db": r2(d)});
        if lanes.is_empty() {
            let t = &mut p.tracks[i];
            let v = t.volume_db + d;
            let landed = v.clamp(FADER_MIN_DB, FADER_MAX_DB);
            if landed != v {
                row["clamped"] = json!(format!("the fader stops at {landed} dB (range {FADER_MIN_DB}..{FADER_MAX_DB})"));
            }
            row["volume_db_before"] = json!(t.volume_db);
            t.volume_db = landed;
            row["volume_db"] = json!(r2(landed));
        } else {
            // a volume lane replaces the fader: shift the whole lane
            for li in lanes {
                for pt in p.automation[li].points.iter_mut() {
                    pt.value = (pt.value + d).clamp(-120.0, 24.0);
                }
            }
            row["applied_to"] = json!("volume automation lane (it overrides the fader), shifted by change_db");
        }
        moved.push(i);
        rows.push(row);
    }
    if b_or(a, "verify", true) && !moved.is_empty() {
        let after = track_stems(e, &p)?;
        for row in rows.iter_mut() {
            let Some(name) = row["track"].as_str().map(|s| s.to_string()) else { continue };
            if row.get("skipped").is_some() {
                continue;
            }
            let i = p.track_index(&name)?;
            if let Some(v) = after[i].as_ref().and_then(|(l, r)| read_meter(meter, l, r)) {
                row["after_db"] = json!((v * 1000.0).round() / 1000.0);
                row["error_db"] = json!(((v - row["target_db"].as_f64().unwrap_or(0.0) as f32) * 1000.0).round() / 1000.0);
            }
        }
    }
    let dry = b_or(a, "dry_run", false);
    if !dry {
        e.project = p;
    }
    Ok(json!({
        "applied": !dry,
        "measure": meter.unit(),
        "measured_at": "each track's post-fader output (after its effects, before buses and the master)",
        "tracks": rows,
    }))
}

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "gain_stage",
        description: "Numeric gain staging: sets track faders so each track's own output (post-fader, before buses and the master) hits a level. measure: lufs (default, BS.1770-4 integrated), rms (active RMS, 400 ms blocks gated 40 dB under the loudest), peak (sample peak dBFS) or true_peak (dBTP, 4x). target_db applies to every chosen track; targets {track: dB} sets them one by one. tracks = name, comma list or array (default: every track, or the keys of targets). One render, then the exact fader move; a track whose volume automation overrides the fader gets that lane shifted instead. verify (default true) re-renders and reports after_db and error_db per track. dry_run reports without changing the project. Silent tracks are skipped; a move past the fader range (-60..12 dB) is clamped and said.",
        mutates: true,
        schema: || {
            obj(
                json!({
                    "measure": {"type": "string", "enum": ["lufs", "rms", "peak", "true_peak"], "description": "Meter to stage against (default lufs)"},
                    "target_db": {"type": "number", "description": "Level for every chosen track, in the meter's unit (LUFS, dBFS or dBTP)"},
                    "targets": {"type": "object", "description": "Per-track levels, e.g. {\"kick\": -12, \"pad\": -24}", "additionalProperties": {"type": "number"}},
                    "tracks": {"type": ["string", "array"], "items": {"type": "string"}, "description": "Track name, comma list or array (default: every track, or the keys of targets)"},
                    "verify": {"type": "boolean", "description": "Re-render and report the landed level (default true)"},
                    "dry_run": {"type": "boolean", "description": "Report the moves without applying them"}
                }),
                &[],
            )
        },
        run: gain_stage,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn song(dir: &str) -> Engine {
        let mut e = Engine::new(std::env::temp_dir().join(dir));
        for (n, p) in [("kick", "kick"), ("hat", "hat"), ("bass", "808"), ("keys", "epiano")] {
            e.call("add_track", &json!({"name": n, "preset": p})).unwrap();
        }
        e.call("generate_drums", &json!({"style": "trap", "seed": 2})).unwrap();
        e.call("add_notes", &json!({"track": "bass", "notes": [{"start": 0, "pitch": "C2", "len": 8}, {"start": 8, "pitch": "C2", "len": 8}]})).unwrap();
        e.call("add_notes", &json!({"track": "keys", "notes": [{"start": 0, "pitch": "C4", "len": 8}, {"start": 8, "pitch": "Eb4", "len": 8}]})).unwrap();
        e
    }

    #[test]
    fn meters_read_known_signals() {
        // a 0.5-amplitude 1 kHz sine on both channels: peak -6.02 dBFS, RMS -9.03 dBFS
        let x: Vec<f32> = (0..48000).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / SR).sin()).collect();
        assert!((read_meter(Meter::Peak, &x, &x).unwrap() + 6.0206).abs() < 0.01);
        assert!((read_meter(Meter::Rms, &x, &x).unwrap() + 9.0309).abs() < 0.01);
        // silence then signal: the gate ignores the silent blocks
        let mut y = vec![0.0f32; 3 * (0.4 * SR) as usize]; // three whole 400 ms blocks
        y.extend_from_slice(&x);
        assert!((read_meter(Meter::Rms, &y, &y).unwrap() + 9.0309).abs() < 0.02);
        assert!(read_meter(Meter::Lufs, &[0.0; 4800], &[0.0; 4800]).is_none());
        assert!(Meter::parse("loudest").is_err());
    }

    #[test]
    fn gain_stage_lands_within_a_tenth_of_a_db_on_every_meter() {
        let mut e = song("bb_gain_stage");
        for (measure, tgt) in [("lufs", -20.0), ("rms", -22.0), ("peak", -8.0), ("true_peak", -6.0)] {
            let v = e.call("gain_stage", &json!({"measure": measure, "target_db": tgt})).unwrap();
            assert_eq!(v["applied"], true);
            let rows = v["tracks"].as_array().unwrap();
            assert_eq!(rows.len(), e.project.tracks.len(), "{v}");
            assert!(rows.len() >= 4);
            for row in rows {
                let err = row["error_db"].as_f64().unwrap_or(9.0);
                assert!(err.abs() <= 0.1, "{measure}: {row}");
            }
        }
        // per-track targets, and the project's faders really moved
        let before = e.project.tracks.iter().find(|t| t.name == "kick").unwrap().volume_db;
        let v = e.call("gain_stage", &json!({"measure": "rms", "targets": {"kick": -14.0, "keys": -26.0}})).unwrap();
        let rows = v["tracks"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{v}");
        for row in rows {
            assert!(row["error_db"].as_f64().unwrap().abs() <= 0.1, "{row}");
        }
        let after = e.project.tracks.iter().find(|t| t.name == "kick").unwrap().volume_db;
        let kick = rows.iter().find(|r| r["track"] == "kick").unwrap();
        assert!((after - before - kick["change_db"].as_f64().unwrap() as f32).abs() < 0.02, "{v}");
        // dry run leaves the project alone
        let snap = e.project.tracks.iter().map(|t| t.volume_db).collect::<Vec<_>>();
        let v = e.call("gain_stage", &json!({"measure": "peak", "target_db": -30.0, "dry_run": true})).unwrap();
        assert_eq!(v["applied"], false);
        assert_eq!(snap, e.project.tracks.iter().map(|t| t.volume_db).collect::<Vec<_>>());
        // bad input
        assert!(e.call("gain_stage", &json!({"measure": "rms"})).is_err());
        assert!(e.call("gain_stage", &json!({"target_db": 3.0})).is_err());
        assert!(e.call("gain_stage", &json!({"target_db": -20.0, "tracks": "nobody"})).is_err());
    }

    #[test]
    fn gain_stage_shifts_a_volume_lane_that_overrides_the_fader() {
        let mut e = song("bb_gain_stage_lane");
        e.call("add_automation", &json!({"track": "keys", "param": "volume", "points": [[0, -6.0], [4, -12.0]]})).unwrap();
        let v = e.call("gain_stage", &json!({"measure": "rms", "targets": {"keys": -24.0}})).unwrap();
        let row = &v["tracks"][0];
        assert!(row["applied_to"].as_str().unwrap().contains("lane"), "{v}");
        assert!(row["error_db"].as_f64().unwrap().abs() <= 0.1, "{v}");
    }
}
