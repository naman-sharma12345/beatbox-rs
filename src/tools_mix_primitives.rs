//! Mixing-suite primitives as MCP actions: numeric gain staging and the
//! measurements an engineer mixing through MCP reaches for. Every action
//! acts only when called; nothing here is on the default render path.
//!
//! Meters: integrated loudness per ITU-R BS.1770-4 (`analysis::loudness_at`),
//! its short-term maximum (3 s window, 100 ms hop, EBU Tech 3341),
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

/// Short-term loudness maximum per EBU Tech 3341: an ungated 3 s
/// rectangular window moved in 100 ms steps over BS.1770 K-weighted samples
/// (shelf + RLB high-pass per channel, L and R weighted 1). One pass, a
/// running sum over the last 30 hop energies in a fixed ring, nothing
/// allocated. A signal shorter than 3 s reads its ungated loudness over the
/// whole duration. None when silent (below -70 LUFS).
pub(crate) fn short_term_max_lufs(l: &[f32], r: &[f32]) -> Option<f32> {
    const W: usize = 30;
    let n = l.len().min(r.len());
    if n == 0 {
        return None;
    }
    let hop = (SR * 0.1) as usize;
    let [mut l1, mut l2] = analysis::k_weighting(SR as f64);
    let [mut r1, mut r2_] = analysis::k_weighting(SR as f64);
    let mut ring = [0.0f64; W];
    let (mut head, mut hops) = (0usize, 0usize);
    let (mut win, mut acc, mut total, mut best) = (0.0f64, 0.0f64, 0.0f64, f64::NEG_INFINITY);
    for i in 0..n {
        let a = l2.run(l1.run(l[i] as f64));
        let b = r2_.run(r1.run(r[i] as f64));
        let e = a * a + b * b;
        acc += e;
        total += e;
        if (i + 1) % hop == 0 {
            win += acc - ring[head];
            ring[head] = acc;
            head = (head + 1) % W;
            if head == 0 {
                // re-sum once per lap so the running sum cannot drift
                win = ring.iter().sum();
            }
            acc = 0.0;
            hops += 1;
            if hops >= W {
                best = best.max(win);
            }
        }
    }
    let ms = if hops >= W { best / (W * hop) as f64 } else { total / n as f64 };
    let v = -0.691 + 10.0 * ms.log10();
    (v.is_finite() && v > -70.0).then_some(v as f32)
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

/// Stereo facts of one signal: side/mid energy ratio (0 = mono), L/R
/// correlation and the L-R balance in dB (positive = right louder).
pub(crate) fn stereo_facts(l: &[f32], r: &[f32]) -> (f32, f32, f32) {
    let (mut mm, mut ss, mut lr, mut ll, mut rr) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for (&a, &b) in l.iter().zip(r.iter()) {
        let (a, b) = (a as f64, b as f64);
        let (m, s) = (0.5 * (a + b), 0.5 * (a - b));
        mm += m * m;
        ss += s * s;
        lr += a * b;
        ll += a * a;
        rr += b * b;
    }
    let width = if mm > 1e-18 { (ss / mm).sqrt() as f32 } else if ss > 1e-18 { f32::INFINITY } else { 0.0 };
    let corr = if ll > 1e-18 && rr > 1e-18 { (lr / (ll * rr).sqrt()) as f32 } else { 1.0 };
    let bal = if ll > 1e-18 && rr > 1e-18 { (10.0 * (rr / ll).log10()) as f32 } else if rr > 1e-18 { 99.0 } else if ll > 1e-18 { -99.0 } else { 0.0 };
    (width, corr, bal)
}

/// Spectral centroid (Hz) of a mono signal's long-term power spectrum.
fn centroid_hz(mono: &[f32]) -> f32 {
    let ps = analysis::power_spectrum(mono);
    let hz = SR / (2.0 * ps.len() as f32);
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for (i, v) in ps.iter().enumerate() {
        let f = i as f32 * hz;
        if (20.0..=20000.0).contains(&f) {
            num += (f * v) as f64;
            den += *v as f64;
        }
    }
    if den > 0.0 { (num / den) as f32 } else { 0.0 }
}

const BAND_NAMES: [&str; 6] = ["sub_20_60", "low_60_250", "lowmid_250_800", "mid_800_2k", "presence_2k_5k", "air_5k_16k"];

fn meter_row(l: &[f32], r: &[f32]) -> Value {
    let opt = |v: Option<f32>| v.map(|x| json!(r2(x))).unwrap_or(Value::Null);
    let (pk, rms) = (peak_db(l, r), active_rms_db(l, r));
    if pk.is_none() {
        return json!({"silent": true});
    }
    let mono: Vec<f32> = l.iter().zip(r.iter()).map(|(a, b)| 0.5 * (a + b)).collect();
    let bands = crate::tools_critic::band_shares(&mono);
    let (width, corr, bal) = stereo_facts(l, r);
    json!({
        "lufs": opt(read_meter(Meter::Lufs, l, r)),
        "short_term_max_lufs": opt(short_term_max_lufs(l, r)),
        "active_rms_dbfs": opt(rms),
        "peak_dbfs": opt(pk),
        "true_peak_dbtp": opt(read_meter(Meter::TruePeak, l, r)),
        "crest_db": opt(pk.zip(rms).map(|(p, q)| p - q)),
        "centroid_hz": centroid_hz(&mono).round(),
        "band_share_db": BAND_NAMES.iter().zip(bands.iter()).map(|(n, v)| (n.to_string(), json!(r2(*v)))).collect::<serde_json::Map<String, Value>>(),
        "width_side_over_mid": if width.is_finite() { json!(r2(width)) } else { json!("side only (mid cancels)") },
        "correlation": r2(corr),
        "balance_db_r_minus_l": r2(bal.clamp(-99.0, 99.0)),
    })
}

fn mix_report(e: &mut Engine, a: &Value) -> Result<Value> {
    let p = e.project.clone();
    let want = name_list(a, "tracks");
    for n in &want {
        p.track_index(n)?;
    }
    let m = e.render_stems_all(&p)?;
    let energy = |l: &[f32], r: &[f32]| -> f64 { l.iter().chain(r.iter()).map(|v| (*v as f64).powi(2)).sum() };
    let total: f64 = p.tracks.iter().filter_map(|t| m.stems.iter().find(|s| s.name == t.name)).map(|s| energy(&s.left, &s.right)).sum();
    let mut rows = Vec::new();
    for t in &p.tracks {
        if !want.is_empty() && !want.iter().any(|w| w.eq_ignore_ascii_case(&t.name)) {
            continue;
        }
        let mut row = match m.stems.iter().find(|s| s.name == t.name && !s.left.is_empty()) {
            Some(s) => {
                let mut v = meter_row(&s.left, &s.right);
                if total > 0.0 && v.get("silent").is_none() {
                    v["energy_share_percent"] = json!(r2((100.0 * energy(&s.left, &s.right) / total) as f32));
                }
                v
            }
            None => json!({"silent": true}),
        };
        row["track"] = json!(t.name);
        row["volume_db"] = json!(t.volume_db);
        row["pan"] = json!(t.pan);
        row["mute"] = json!(t.mute);
        row["output"] = json!(t.output.clone().unwrap_or_else(|| "master".into()));
        row["volume_automated"] = json!(!volume_lanes(&p, &t.name).is_empty());
        rows.push(row);
    }
    let mut buses = Vec::new();
    if b_or(a, "buses", true) {
        for b in &p.buses {
            let mut row = match m.bus_stems.iter().find(|s| s.name == b.name && !s.left.is_empty()) {
                Some(s) => meter_row(&s.left, &s.right),
                None => json!({"silent": true}),
            };
            row["bus"] = json!(b.name);
            row["volume_db"] = json!(b.volume_db);
            buses.push(row);
        }
    }
    let mut master = meter_row(&m.left, &m.right);
    master["volume_db"] = json!(p.master_volume_db);
    Ok(json!({
        "measured_at": "tracks: post-fader output before buses; buses: post-fader output; master: the final mix",
        "meters": "LUFS BS.1770-4 integrated; short_term_max_lufs = loudest 3 s window, 100 ms hop (EBU Tech 3341); true peak BS.1770-4 Annex 2 (4x); crest = sample peak minus active RMS; active RMS over 400 ms blocks gated 40 dB under the loudest; band shares in dB of the 20 Hz-16 kHz total; width = side/mid RMS ratio (0 = mono)",
        "tracks": rows,
        "buses": buses,
        "master": master,
        "seconds": r2(m.seconds),
    }))
}

pub fn tools() -> Vec<Tool> {
    vec![
    Tool {
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
    },
    Tool {
        name: "mix_report",
        description: "One-call mix meter for every track (or tracks: name, comma list or array), every bus and the master, from one render: integrated LUFS (BS.1770-4) and short-term max LUFS (loudest ungated 3 s window, 100 ms hop, EBU Tech 3341; a clip under 3 s reads its whole length), active RMS, sample peak, true peak (4x), crest (sample peak minus active RMS), spectral centroid, six band shares (sub, low, low-mid, mid, presence, air, in dB of the 20 Hz-16 kHz total), stereo width (side/mid, 0 = mono), L/R correlation and balance, each track's share of the summed track energy, plus its fader, pan, mute, output and whether volume automation drives it. Tracks are measured post-fader before buses. buses:false leaves the buses out. Read-only; pair with gain_stage to set levels.",
        mutates: false,
        schema: || {
            obj(
                json!({
                    "tracks": {"type": ["string", "array"], "items": {"type": "string"}, "description": "Only these tracks (default: all)"},
                    "buses": {"type": "boolean", "description": "Include buses (default true)"}
                }),
                &[],
            )
        },
        run: mix_report,
    },
    ]
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

    #[test]
    fn mix_report_numbers_match_the_meters_and_the_stereo_facts() {
        let mut e = song("bb_mix_report");
        e.call("set_mixer", &json!({"track": "hat", "pan": -1.0})).unwrap();
        e.call("add_effect", &json!({"track": "keys", "type": "width", "params": {"amount": 0.0}})).unwrap();
        e.call("add_bus", &json!({"name": "grp", "preset": "none"})).unwrap();
        e.call("route_track", &json!({"track": "kick,hat", "bus": "grp"})).unwrap();
        let v = e.call("mix_report", &json!({})).unwrap();
        let rows = v["tracks"].as_array().unwrap();
        assert_eq!(rows.len(), e.project.tracks.len(), "{v}");
        let share: f64 = rows.iter().map(|r| r["energy_share_percent"].as_f64().unwrap_or(0.0)).sum();
        assert!((share - 100.0).abs() < 0.1, "{share}");
        // every reading equals the shared meters on the same stems
        let p = e.project.clone();
        let m = e.render_stems_all(&p).unwrap();
        for row in rows {
            let s = m.stems.iter().find(|s| s.name == row["track"].as_str().unwrap()).unwrap();
            let lufs = read_meter(Meter::Lufs, &s.left, &s.right).unwrap();
            assert!((row["lufs"].as_f64().unwrap() as f32 - lufs).abs() <= 0.006, "{row}");
            let pk = read_meter(Meter::Peak, &s.left, &s.right).unwrap();
            assert!((row["peak_dbfs"].as_f64().unwrap() as f32 - pk).abs() <= 0.006, "{row}");
        }
        let get = |n: &str| rows.iter().find(|r| r["track"] == n).unwrap().clone();
        // width amount 0 is exact mono: no side at all, correlation 1
        assert_eq!(get("keys")["width_side_over_mid"].as_f64().unwrap(), 0.0, "{}", get("keys"));
        assert_eq!(get("keys")["correlation"].as_f64().unwrap(), 1.0);
        // hard left: the right channel is (near) silent
        assert!(get("hat")["balance_db_r_minus_l"].as_f64().unwrap() < -40.0, "{}", get("hat"));
        assert!(v["master"]["lufs"].as_f64().is_some(), "{v}");
        // short-term max (3 s window, 100 ms hop) on every track, the bus and the master
        let st = |l: &[f32], r: &[f32]| r2(short_term_max_lufs(l, r).unwrap()) as f64;
        for row in rows {
            let s = m.stems.iter().find(|s| s.name == row["track"].as_str().unwrap()).unwrap();
            assert!((row["short_term_max_lufs"].as_f64().unwrap() - st(&s.left, &s.right)).abs() <= 0.006, "{row}");
        }
        let bus = v["buses"].as_array().unwrap().iter().find(|b| b["bus"] == "grp").unwrap().clone();
        let bs = m.bus_stems.iter().find(|s| s.name == "grp").unwrap();
        assert!((bus["short_term_max_lufs"].as_f64().unwrap() - st(&bs.left, &bs.right)).abs() <= 0.006, "{bus}");
        assert!((v["master"]["short_term_max_lufs"].as_f64().unwrap() - st(&m.left, &m.right)).abs() <= 0.006, "{v}");
        // a filter, and a bad name
        assert_eq!(e.call("mix_report", &json!({"tracks": "kick,bass"})).unwrap()["tracks"].as_array().unwrap().len(), 2);
        assert!(e.call("mix_report", &json!({"tracks": "nobody"})).is_err());
    }

    fn sine(f: f32, amp: f32, secs: f32, phase: f32) -> Vec<f32> {
        (0..(secs * SR) as usize).map(|i| amp * (2.0 * std::f32::consts::PI * f * i as f32 / SR + phase).sin()).collect()
    }

    #[test]
    fn short_term_max_of_a_steady_sine_matches_its_integrated_loudness() {
        // a steady -20 dBFS 1 kHz sine on both channels, 10 s: every 3 s window
        // reads the same, so the short-term max equals the integrated value
        let x = sine(1000.0, 10f32.powf(-20.0 / 20.0), 10.0, 0.0);
        let row = meter_row(&x, &x);
        let (st, int) = (row["short_term_max_lufs"].as_f64().unwrap(), row["lufs"].as_f64().unwrap());
        eprintln!("steady -20 dBFS 1 kHz: short-term max {st}, integrated {int}");
        assert!((st - int).abs() <= 0.01, "{row}");
        assert_eq!(meter_row(&[0.0; 4800], &[0.0; 4800])["silent"], true);
    }

    #[test]
    fn short_term_window_is_3_s_ungated_and_short_clips_read_their_whole_length() {
        // BS.1770 calibration: a 997 Hz sine at A peak in both channels reads 20 log10(A) LUFS,
        // so -23 dBFS peak is -23.0 LUFS (EBU Tech 3341 test case 1)
        let a = 10f32.powf(-23.0 / 20.0);
        let x = sine(997.0, a, 20.0, 0.0);
        let st = short_term_max_lufs(&x, &x).unwrap();
        let int = read_meter(Meter::Lufs, &x, &x).unwrap();
        eprintln!("-23 cal: short-term max {st:.4}, integrated {int:.4}");
        assert!((st + 23.0).abs() <= 0.01, "{st}");
        assert!((int + 23.0).abs() <= 0.01, "{int}");
        // 2 s of tone then 8 s of silence: the loudest window holds 2 s of tone in 3 s,
        // so it reads 10 log10(2/3) = -1.761 LU under the tone (rectangular, ungated)
        let mut y = sine(997.0, a, 2.0, 0.0);
        y.resize((10.0 * SR) as usize, 0.0);
        let sty = short_term_max_lufs(&y, &y).unwrap();
        eprintln!("2 s burst: {sty:.4} (expect {:.4})", -23.0 + 10.0 * (2.0f32 / 3.0).log10());
        assert!((sty - (-23.0 + 10.0 * (2.0f32 / 3.0).log10())).abs() <= 0.01, "{sty}");
        // under 3 s: ungated loudness over the whole clip; 1 s of silence then 1 s of tone
        // reads 3.01 LU under the tone (the integrated meter gates the silence out)
        let mut z = vec![0.0f32; SR as usize];
        z.extend(sine(997.0, a, 1.0, 0.0));
        let stz = short_term_max_lufs(&z, &z).unwrap();
        eprintln!("short clip: {stz:.4} (expect {:.4})", -23.0 - 10.0 * 2f32.log10());
        assert!((stz - (-23.0 - 10.0 * 2f32.log10())).abs() <= 0.01, "{stz}");
        let full = short_term_max_lufs(&z[SR as usize..], &z[SR as usize..]).unwrap();
        assert!((full + 23.0).abs() <= 0.01, "{full}");
        assert!(short_term_max_lufs(&[0.0; 100], &[0.0; 100]).is_none());
        assert!(short_term_max_lufs(&[], &[]).is_none());
    }

    #[test]
    fn peak_and_true_peak_read_0_db_cases() {
        // full-scale 997 Hz sine: sample peak and true peak both 0 dB
        let x = sine(997.0, 1.0, 2.0, 0.0);
        let (pk, tp) = (read_meter(Meter::Peak, &x, &x).unwrap(), read_meter(Meter::TruePeak, &x, &x).unwrap());
        eprintln!("0 dBFS 997 Hz: peak {pk:.4} dBFS, true peak {tp:.4} dBTP");
        assert!(pk.abs() <= 0.01 && tp.abs() <= 0.01, "{pk} {tp}");
        // fs/4 sine at 45 degrees: every sample is +-0.7071 (-3.01 dBFS) but the
        // waveform between them reaches 1.0, so true peak is 0 dBTP
        // (built from the exact sample pattern; a computed sin() drifts in phase at f32)
        let y: Vec<f32> = (0..2 * SR as usize).map(|i| if i % 4 < 2 { std::f32::consts::FRAC_1_SQRT_2 } else { -std::f32::consts::FRAC_1_SQRT_2 }).collect();
        let (pk, tp) = (read_meter(Meter::Peak, &y, &y).unwrap(), read_meter(Meter::TruePeak, &y, &y).unwrap());
        eprintln!("fs/4 at 45 deg: peak {pk:.4} dBFS, true peak {tp:.4} dBTP");
        assert!((pk + 3.0103).abs() <= 0.01, "{pk}");
        // Measured error, not a loosened bound: the BS.1770-4 Annex 2 4x interpolator
        // (48 taps, the standard's own coefficients, shared with the TP limiter and
        // locked) has passband gain a little above unity near fs/4, so this exact
        // 0 dBTP waveform reads +0.083 dBTP (a numpy model of the same filter gives
        // 0.0826). It over-reads, never under-reads, which is the safe side for a limiter.
        assert!(tp >= 0.0 && (tp - 0.0826).abs() <= 0.003, "{tp}");
        let row = meter_row(&y, &y);
        assert!((row["crest_db"].as_f64().unwrap() - (row["peak_dbfs"].as_f64().unwrap() - row["active_rms_dbfs"].as_f64().unwrap())).abs() <= 0.011, "{row}");
    }

    #[test]
    fn band_shares_put_known_tones_in_their_band() {
        for (k, f) in [35.0f32, 120.0, 450.0, 1250.0, 3200.0, 9000.0].into_iter().enumerate() {
            let x = sine(f, 0.5, 4.0, 0.0);
            let row = meter_row(&x, &x);
            let shares: Vec<f64> = BAND_NAMES.iter().map(|n| row["band_share_db"][*n].as_f64().unwrap()).collect();
            eprintln!("{f} Hz: {shares:?}");
            assert!(shares[k] >= -0.01, "{f} Hz -> {shares:?}");
            for (j, s) in shares.iter().enumerate() {
                if j != k {
                    assert!(*s < -30.0, "{f} Hz leaks into {}: {shares:?}", BAND_NAMES[j]);
                }
            }
        }
    }
}
