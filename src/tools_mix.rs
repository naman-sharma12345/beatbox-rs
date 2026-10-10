//! Mix-balance and per-source listening tools: role-based level calibration
//! (level hints without a song render), balance_mix auto gain-staging with
//! genre targets, analyze_track (YIN fundamental / key fit / decay),
//! detect_transients and strip_silence.

use crate::analysis;
use crate::dsp::{gain_to_db, SR};
use crate::instruments::{self, DrumKind, Instrument};
use crate::project::Project;
use crate::render::INFO_BLOCK;
use crate::samples::SampleBank;
use crate::sc_dsp;
use crate::theory;
use crate::tools::{b_or, f_opt, obj, s_opt, u_or, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Musical role of a track, from its instrument and name.
pub fn role_of(name: &str, inst: &Instrument) -> &'static str {
    if let Instrument::Drum(d) = inst {
        return match d.kind {
            DrumKind::Kick => "kick",
            DrumKind::Snare | DrumKind::Clap | DrumKind::Rim => "snare",
            DrumKind::ClosedHat | DrumKind::Shaker => "hats",
            DrumKind::OpenHat | DrumKind::Crash => "cymbal",
            DrumKind::Tom | DrumKind::Cowbell | DrumKind::Tabla | DrumKind::Bayan => "perc",
        };
    }
    let n = name.to_lowercase();
    let has = |k: &[&str]| k.iter().any(|w| n.contains(w));
    if matches!(inst, Instrument::Bass808(_)) || has(&["808", "bass", "sub"]) {
        "bass"
    } else if has(&["kick"]) {
        "kick"
    } else if has(&["snare", "clap", "rim"]) {
        "snare"
    } else if has(&["hat", "shaker", "hh"]) {
        "hats"
    } else if has(&["perc", "tom", "conga", "bongo", "tabla"]) {
        "perc"
    } else if has(&["pad", "string", "choir", "atmos", "drone"])
        || matches!(inst, Instrument::Ensemble(_) | Instrument::Granular(_))
    {
        "pad"
    } else if has(&["fx", "riser", "sweep", "impact", "noise", "vinyl"]) {
        "fx"
    } else if has(&["chord", "keys", "piano", "rhodes", "organ"])
        || matches!(inst, Instrument::Piano(_))
    {
        "keys"
    } else {
        "lead"
    }
}

/// Target loudest-50 ms note level (dBFS at a 0 dB fader) per role, so a
/// fresh project starts roughly balanced: kick and bass on top, snare just
/// under, melodic parts in the middle, hats and pads behind.
pub fn role_target_db(role: &str) -> f32 {
    match role {
        "kick" => -9.0,
        "bass" => -11.0,
        "snare" => -12.0,
        "lead" => -15.0,
        "keys" => -17.0,
        "perc" => -19.0,
        "pad" => -20.0,
        "cymbal" => -21.0,
        "hats" => -21.0,
        "fx" => -24.0,
        _ => -16.0,
    }
}

/// Loudest 50 ms RMS (dBFS) of one representative note of an instrument.
pub fn note_level_db(inst: &Instrument, role: &str, bank: &SampleBank) -> f32 {
    let pitch = match role {
        "bass" | "kick" => 36.0,
        "pad" | "keys" => 60.0,
        _ => 64.0,
    };
    let gate = if role == "pad" { 1.0 } else { 0.4 };
    let x = instruments::render_note(inst, pitch, 0.8, gate, bank, 7);
    let w = (0.05 * SR) as usize;
    let mut best = 0.0f64;
    let mut i = 0;
    while i + w <= x.len().max(w) && i < x.len() {
        let end = (i + w).min(x.len());
        let e: f64 = x[i..end].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / w as f64;
        best = best.max(e);
        i += w / 2;
    }
    (10.0 * best.max(1e-14).log10()) as f32
}

fn cache() -> &'static Mutex<HashMap<String, f32>> {
    static C: OnceLock<Mutex<HashMap<String, f32>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Fader setting (dB) that puts a new track's instrument at its role target.
pub fn calibrated_volume(
    name: &str,
    inst: &Instrument,
    bank: &SampleBank,
) -> (f32, &'static str, f32) {
    let role = role_of(name, inst);
    let key = format!("{role}|{}", serde_json::to_string(inst).unwrap_or_default());
    let lvl = {
        let hit = cache().lock().ok().and_then(|c| c.get(&key).copied());
        match hit {
            Some(v) => v,
            None => {
                let v = note_level_db(inst, role, bank);
                if let Ok(mut c) = cache().lock() {
                    if c.len() > 512 {
                        c.clear();
                    }
                    c.insert(key, v);
                }
                v
            }
        }
    };
    if lvl < -90.0 {
        return (0.0, role, lvl);
    }
    let vol = ((role_target_db(role) - lvl) * 2.0).round() / 2.0;
    (vol.clamp(-24.0, 12.0), role, lvl)
}

/// Genre balance targets: each role's active RMS relative to the kick (dB).
fn genre_targets(genre: &str) -> Result<Vec<(&'static str, f32)>> {
    Ok(match genre {
        "trap" | "drill" => vec![("bass", 0.0), ("snare", -2.0), ("hats", -11.0), ("perc", -10.0), ("cymbal", -12.0), ("lead", -6.0), ("keys", -8.0), ("pad", -12.0), ("fx", -15.0)],
        // boom bap / desi hip-hop: the low end carries it (critic, Oct 2026: only
        // ~30% of power under 250 Hz with mids and air heavy; hip-hop sits at
        // 60-70%), so the 808 sits level with the kick and the top is quieter
        "boom_bap" | "hiphop" | "hip_hop" => vec![("bass", 0.0), ("snare", -1.5), ("hats", -12.0), ("perc", -11.0), ("cymbal", -14.0), ("lead", -7.0), ("keys", -7.0), ("pad", -12.0), ("fx", -17.0)],
        "lofi" => vec![("bass", -2.0), ("snare", -1.0), ("hats", -9.0), ("perc", -9.0), ("cymbal", -12.0), ("lead", -5.0), ("keys", -5.0), ("pad", -10.0), ("fx", -14.0)],
        "house" | "techno" | "edm" => vec![("bass", -2.0), ("snare", -4.0), ("hats", -8.0), ("perc", -9.0), ("cymbal", -10.0), ("lead", -5.0), ("keys", -7.0), ("pad", -10.0), ("fx", -13.0)],
        "pop" | "afrobeats" => vec![("bass", -2.0), ("snare", -2.0), ("hats", -9.0), ("perc", -8.0), ("cymbal", -11.0), ("lead", -3.0), ("keys", -5.0), ("pad", -9.0), ("fx", -14.0)],
        // R&B: soft, dark top (critic: air 14 dB over a reference, hissy hats)
        "rnb" => vec![("bass", 0.0), ("snare", -2.0), ("hats", -13.0), ("perc", -11.0), ("cymbal", -14.0), ("lead", -4.0), ("keys", -8.0), ("pad", -12.0), ("fx", -16.0)],
        "cinematic" | "ambient" => vec![("bass", -3.0), ("snare", -3.0), ("hats", -10.0), ("perc", -6.0), ("cymbal", -8.0), ("lead", -1.0), ("keys", -2.0), ("pad", -2.0), ("fx", -8.0)],
        g => bail!("unknown genre '{g}' (trap, drill, boom_bap, lofi, house, techno, edm, pop, rnb, afrobeats, cinematic, ambient)"),
    })
}

/// Active-RMS level (dBFS, pre-master) the anchor (kick) is placed at.
const ANCHOR_DB: f32 = -12.0;

fn active_db(blocks: &[f32]) -> Option<f32> {
    let act: Vec<f32> = blocks.iter().copied().filter(|b| *b > 1e-6).collect();
    if act.is_empty() {
        return None;
    }
    let ms = act.iter().sum::<f32>() / act.len() as f32;
    Some(10.0 * ms.max(1e-12).log10())
}

fn level_rows(p: &Project, bank: &SampleBank) -> Vec<Value> {
    p.tracks
        .iter()
        .map(|t| {
            let role = role_of(&t.name, &t.instrument);
            let lvl = note_level_db(&t.instrument, role, bank);
            let at_fader = lvl + t.volume_db;
            let target = role_target_db(role);
            let hint = if lvl < -90.0 {
                "silent instrument (missing sample?)".to_string()
            } else if at_fader > target + 4.0 {
                format!("about {:.0} dB hot for a {role}: try volume_db {:.1}", at_fader - target, t.volume_db - (at_fader - target))
            } else if at_fader < target - 4.0 {
                format!("about {:.0} dB quiet for a {role}: try volume_db {:.1}", target - at_fader, t.volume_db + (target - at_fader))
            } else {
                "in range".to_string()
            };
            json!({"track": t.name, "role": role, "note_level_db": (lvl * 10.0).round() / 10.0, "volume_db": t.volume_db, "at_fader_db": (at_fader * 10.0).round() / 10.0, "role_target_db": target, "hint": hint})
        })
        .collect()
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "carve_mix",
            description: "Arrangement-level EQ carving so the low end belongs to the kick and the bass/808: a role-sized low cut on every other track (snare ~110 Hz, hats ~320, percs ~160, keys/pads/leads 170-220 when a bass plays) and, when a bass or 808 plays, a -2.5..-4.5 dB dip at ~320 Hz on keys, pads and leads where chord voicings and bass harmonics build mud. One parametric EQ with id 'carve' appended to each chain; re-running replaces it. tracks: only these (default all).",
            mutates: true,
            schema: || obj(json!({"tracks": {"type": "array", "items": {"type": "string"}}}), &[]),
            run: |e, a| {
                let only: Vec<String> = a.get("tracks").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                Ok(crate::carve::carve(e, &only))
            },
        },
        Tool {
            name: "level_hints",
            description: "Fast level check WITHOUT rendering the song: renders one representative note per track's instrument, measures its loudest 50 ms at the current fader and compares it with a role target (kick, bass, snare, hats, perc, lead, keys, pad, fx). Flags a kick 15 dB over the hats before you write a single bar. apply:true sets every track's volume_db to its role target (calibration).",
            mutates: true,
            schema: || obj(json!({"apply": {"type": "boolean", "description": "Set faders to the role targets (default false)"}}), &[]),
            run: |e, a| {
                e.bank.sync(&e.project.samples);
                let mut changed = Vec::new();
                if b_or(a, "apply", false) {
                    for i in 0..e.project.tracks.len() {
                        let (v, _, lvl) = calibrated_volume(&e.project.tracks[i].name.clone(), &e.project.tracks[i].instrument.clone(), &e.bank);
                        if lvl > -90.0 {
                            e.project.tracks[i].volume_db = v;
                            changed.push(json!({"track": e.project.tracks[i].name, "volume_db": v}));
                        }
                    }
                }
                Ok(json!({"tracks": level_rows(&e.project, &e.bank), "applied": changed, "note": "note levels are per hit; dense parts (hats, pads) also add up over time, so confirm with balance_mix after writing patterns"}))
            },
        },
        Tool {
            name: "balance_mix",
            description: "Auto gain-staging: renders the song, measures each track's ACTIVE loudness (only where it plays) and moves faders so every role sits at its genre target relative to the kick (e.g. trap: 808 level with the kick, snare -2 dB, hats -11 dB, lead -6 dB, pads -12 dB). Iterates (re-rendering) up to `iterations` times, never moves a fader more than max_step_db per pass, then optionally sets the master gain so the mix hits target_lufs before the limiter. dry_run:true returns the moves only. Per-track overrides: offsets {track: dB} relative to its genre target.",
            mutates: true,
            schema: || obj(json!({
                "genre": {"type": "string", "description": "trap, drill, boom_bap, lofi, house, techno, edm, pop, rnb, afrobeats, cinematic, ambient (default trap)"},
                "iterations": {"type": "integer", "description": "default 2"},
                "max_step_db": {"type": "number", "description": "default 9"},
                "offsets": {"type": "object", "description": "{track: dB} nudges on top of the genre target"},
                "target_lufs": {"type": "number", "description": "Also trim master_volume_db toward this integrated loudness"},
                "dry_run": {"type": "boolean"},
            }), &[]),
            run: |e, a| {
                let genre = s_opt(a, "genre").unwrap_or_else(|| "trap".into()).to_lowercase();
                let targets = genre_targets(&genre)?;
                let max_step = f_opt(a, "max_step_db").unwrap_or(9.0).clamp(1.0, 24.0);
                let iters = u_or(a, "iterations", 2).clamp(1, 5);
                let offsets = a.get("offsets").and_then(|v| v.as_object()).cloned().unwrap_or_default();
                let mut p = e.project.clone();
                let before = e.analyze()?;
                let mut moves: HashMap<String, f32> = HashMap::new();
                let mut passes = Vec::new();
                for pass in 0..iters {
                    let m = e.render_version(&p)?;
                    let lv: Vec<(usize, &'static str, Option<f32>)> = p.tracks.iter().enumerate().map(|(i, t)| {
                        let info = m.track_info.iter().find(|x| x.name == t.name);
                        (i, role_of(&t.name, &t.instrument), info.and_then(|x| active_db(&x.blocks)))
                    }).collect();
                    // the anchor: the loudest kick (or bass when there is no kick)
                    let anchor = lv.iter().filter(|x| x.1 == "kick").filter_map(|x| x.2).fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v))))
                        .or_else(|| lv.iter().filter(|x| x.1 == "bass").filter_map(|x| x.2).fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v)))));
                    let Some(mut anchor) = anchor else { bail!("balance_mix needs a kick or bass track to anchor the balance") };
                    let anchor_role = if p.tracks.iter().any(|t| role_of(&t.name, &t.instrument) == "kick") { "kick" } else { "bass" };
                    // keep the anchor itself near a sane pre-master level
                    let mut moved = 0.0f32;
                    if (anchor - ANCHOR_DB).abs() > 1.0 {
                        let d = (ANCHOR_DB - anchor).clamp(-max_step, max_step);
                        for t in p.tracks.iter_mut() {
                            if role_of(&t.name, &t.instrument) == anchor_role {
                                t.volume_db = (((t.volume_db + d) * 2.0).round() / 2.0).clamp(-40.0, 12.0);
                                *moves.entry(t.name.clone()).or_insert(0.0) += d;
                            }
                        }
                        anchor += d;
                        moved = d.abs();
                    }
                    for (i, role, act) in lv {
                        let Some(act) = act else { continue };
                        if role == anchor_role { continue; }
                        let rel = targets.iter().find(|t| t.0 == role).map(|t| t.1).unwrap_or(-8.0);
                        let off = offsets.get(&p.tracks[i].name).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                        let want = anchor + rel + off;
                        let d = (want - act).clamp(-max_step, max_step);
                        if d.abs() >= 0.5 {
                            let t = &mut p.tracks[i];
                            t.volume_db = ((t.volume_db + d) * 2.0).round() / 2.0;
                            t.volume_db = t.volume_db.clamp(-40.0, 12.0);
                            *moves.entry(t.name.clone()).or_insert(0.0) += d;
                            moved = moved.max(d.abs());
                        }
                    }
                    passes.push(json!({"pass": pass + 1, "anchor_active_db": (anchor * 10.0).round() / 10.0, "largest_move_db": (moved * 10.0).round() / 10.0}));
                    if moved < 1.0 { break; }
                }
                // master gain toward a loudness target
                let mut lufs_after = None;
                let mut m = e.render_version(&p)?;
                if let Some(t) = f_opt(a, "target_lufs") {
                    for _ in 0..3 {
                        let l = analysis::loudness(&m.left, &m.right).integrated_lufs;
                        if (t - l).abs() <= 0.5 || l <= -69.0 { break; }
                        p.master_volume_db = (p.master_volume_db + (t - l)).clamp(-24.0, 18.0);
                        m = e.render_version(&p)?;
                    }
                }
                let after = analysis::analyze(&m);
                lufs_after.get_or_insert(after.loudness.integrated_lufs);
                let shares: Vec<Value> = after.tracks.iter().map(|t| json!({"track": t.track, "energy_share_percent": t.energy_share_percent, "active_rms_db": t.active_rms_db})).collect();
                let dry = b_or(a, "dry_run", false);
                let mut mv: Vec<Value> = moves.iter().map(|(k, v)| json!({"track": k, "change_db": (v * 10.0).round() / 10.0, "volume_db": p.tracks.iter().find(|t| &t.name == k).map(|t| t.volume_db)})).collect();
                mv.sort_by(|x, y| x["track"].as_str().cmp(&y["track"].as_str()));
                if !dry {
                    e.project = p.clone();
                }
                Ok(json!({
                    "applied": !dry, "genre": genre, "moves": mv, "passes": passes,
                    "master_volume_db": p.master_volume_db,
                    "before": {"score": before.score, "integrated_lufs": before.loudness.integrated_lufs},
                    "after": {"score": after.score, "integrated_lufs": lufs_after, "tracks": shares},
                    "next": "listen (render_preview), then analyze_sections and master_assistant",
                }))
            },
        },
        Tool {
            name: "analyze_track",
            description: "Listen to ONE source (a track rendered solo, a sample or a file): YIN fundamental in Hz over the sustain, nearest note and cents off, whether that note is in the project key (and the closest in-key note), decay time to -20/-40 dB, active RMS, and the first detected notes (audio-to-MIDI). Use it to tune a kick/808 to the key or check a sample's pitch.",
            mutates: false,
            schema: || obj(json!({
                "track": {"type": "string"}, "sample": {"type": "string"}, "path": {"type": "string"},
                "min_hz": {"type": "number", "description": "default 25"}, "max_hz": {"type": "number", "description": "default 2000"},
                "max_notes": {"type": "integer", "description": "default 16"},
            }), &[]),
            run: |e, a| {
                let (name, l, r, _) = crate::tools_ears::source(e, a)?;
                let mono: Vec<f32> = l.iter().zip(r.iter()).map(|(x, y)| 0.5 * (x + y)).collect();
                if mono.iter().all(|v| v.abs() < 1e-5) { bail!("{name} is silent"); }
                // the loudest hit and its sustain window
                let pk_i = mono.iter().enumerate().fold((0, 0.0f32), |m, (i, v)| if v.abs() > m.1 { (i, v.abs()) } else { m }).0;
                let start = pk_i + (0.02 * SR) as usize;
                let win = 4096usize;
                let (lo, hi) = (f_opt(a, "min_hz").unwrap_or(25.0), f_opt(a, "max_hz").unwrap_or(2000.0));
                let mut f0s = Vec::new();
                let mut at = start;
                while at + win <= mono.len() && at < start + (0.5 * SR) as usize {
                    if let Some(f) = sc_dsp::yin(&mono[at..at + win], SR, lo, hi, 0.15) { f0s.push(f); }
                    at += win / 4;
                }
                f0s.sort_by(|x, y| x.total_cmp(y));
                let f0 = f0s.get(f0s.len() / 2).copied();
                // decay from the peak
                let hop = (0.005 * SR) as usize;
                let env: Vec<f32> = mono[pk_i..].chunks(hop).map(|c| gain_to_db(c.iter().fold(0.0f32, |m, v| m.max(v.abs())))).collect();
                let top = env.first().copied().unwrap_or(-120.0);
                let decay = |db: f32| env.iter().position(|v| *v < top - db).map(|k| (k * hop) as f32 / SR * 1000.0);
                let p = &e.project;
                let key_pc = theory::pitch_class(&p.key_root).unwrap_or(0);
                let scale = theory::scale_intervals(&p.scale).map(|s| s.to_vec()).unwrap_or_default();
                let in_key = |pc: u8| scale.iter().any(|iv| (key_pc + iv) % 12 == pc);
                let pitch = f0.map(|f| {
                    let midi = 69.0 + 12.0 * (f / 440.0).log2();
                    let near = midi.round().clamp(0.0, 127.0) as u8;
                    let cents = ((midi - near as f32) * 100.0).round();
                    let ok = in_key(near % 12);
                    let closest = (0..7).flat_map(|d| [near as i32 - d, near as i32 + d]).find(|m| (0..128).contains(m) && in_key((*m % 12) as u8)).unwrap_or(near as i32);
                    json!({"fundamental_hz": (f * 100.0).round() / 100.0, "nearest_note": theory::note_name(near), "midi": near, "cents_off": cents,
                        "in_key": ok, "key": format!("{} {}", p.key_root, p.scale),
                        "closest_in_key": theory::note_name(closest as u8), "retune_semitones": closest - near as i32,
                        "voiced_frames": f0s.len()})
                });
                let notes = sc_dsp::audio_to_notes(&mono, SR);
                let blocks: Vec<f32> = mono.chunks(INFO_BLOCK).map(|c| c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).collect();
                Ok(json!({
                    "source": name,
                    "pitch": pitch.unwrap_or(json!({"fundamental_hz": null, "note": "unpitched or too noisy for YIN (drums, noise)"})),
                    "decay_ms": {"to_minus20_db": decay(20.0), "to_minus40_db": decay(40.0)},
                    "peak_dbfs": (gain_to_db(mono[pk_i].abs()) * 10.0).round() / 10.0,
                    "active_rms_db": active_db(&blocks).map(|v| (v * 10.0).round() / 10.0),
                    "notes": notes.iter().take(u_or(a, "max_notes", 16) as usize).map(|n| json!({"start_s": (n.start as f32 / SR * 1000.0).round() / 1000.0, "end_s": (n.end as f32 / SR * 1000.0).round() / 1000.0, "note": theory::note_name(n.pitch), "velocity": n.velocity})).collect::<Vec<_>>(),
                    "notes_detected": notes.len(),
                }))
            },
        },
        Tool {
            name: "detect_transients",
            description: "Spectral-flux transient (onset) detection with an adaptive threshold on a track (solo render), sample or file: returns hit times in seconds and beats. sensitivity 0..1 (default 0.5). Better than energy onsets for slicing breaks and vocal chops.",
            mutates: false,
            schema: || obj(json!({"track": {"type": "string"}, "sample": {"type": "string"}, "path": {"type": "string"}, "sensitivity": {"type": "number"}, "max": {"type": "integer"}}), &[]),
            run: |e, a| {
                let (name, l, r, _) = crate::tools_ears::source(e, a)?;
                let mono: Vec<f32> = l.iter().zip(r.iter()).map(|(x, y)| 0.5 * (x + y)).collect();
                let t = sc_dsp::detect_transients(&mono, f_opt(a, "sensitivity").unwrap_or(0.5));
                let bs = crate::ears::beat_secs(&e.project);
                let max = u_or(a, "max", 128) as usize;
                Ok(json!({"source": name, "count": t.len(), "times_s": t.iter().take(max).map(|s| (*s as f32 / SR * 1000.0).round() / 1000.0).collect::<Vec<_>>(), "beats": t.iter().take(max).map(|s| (*s as f32 / SR / bs * 100.0).round() / 100.0).collect::<Vec<_>>()}))
            },
        },
        Tool {
            name: "strip_silence",
            description: "Find the non-silent regions of a sample (or file) above threshold_db (default -48), ignoring gaps shorter than min_gap_ms (default 80) and padding each region (pad_ms, default 10). apply:true on a sample registers a new sample '<name>_stripped' with leading/trailing silence removed and inner gaps longer than min_gap_ms closed up.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "path": {"type": "string"}, "threshold_db": {"type": "number"}, "min_gap_ms": {"type": "number"}, "pad_ms": {"type": "number"}, "apply": {"type": "boolean"}}), &[]),
            run: |e, a| {
                let (name, l, r) = crate::tools_sound::audio_for(e, a)?;
                if s_opt(a, "sample").is_none() && s_opt(a, "path").is_none() { bail!("give a sample or a path"); }
                let mono: Vec<f32> = l.iter().zip(r.iter()).map(|(x, y)| 0.5 * (x + y)).collect();
                let pad = (f_opt(a, "pad_ms").unwrap_or(10.0).max(0.0) * 0.001 * SR) as usize;
                let rg = sc_dsp::non_silent_ranges(&mono, f_opt(a, "threshold_db").unwrap_or(-48.0), (f_opt(a, "min_gap_ms").unwrap_or(80.0).max(1.0) * 0.001 * SR) as usize, pad, pad);
                let regions: Vec<Value> = rg.iter().map(|(s, t)| json!([(*s as f32 / SR * 1000.0).round() / 1000.0, (*t as f32 / SR * 1000.0).round() / 1000.0])).collect();
                let mut out = json!({"source": name, "regions_s": regions, "count": rg.len(), "original_s": (mono.len() as f32 / SR * 100.0).round() / 100.0});
                if b_or(a, "apply", false) {
                    let Some(sname) = s_opt(a, "sample") else { bail!("apply needs a registered sample") };
                    let mut data: Vec<f32> = Vec::new();
                    for (s, t) in &rg { data.extend_from_slice(&mono[*s..*t]); }
                    if data.is_empty() { bail!("everything is below the threshold"); }
                    let path = e.samples_dir().join(format!("{}_stripped.wav", crate::samples::sample_name(&sname)));
                    crate::render::write_wav(&path, &data, &data)?;
                    let info = crate::samples::SampleInfo { name: format!("{sname}_stripped"), path: path.to_string_lossy().to_string(), source: format!("strip_silence of {sname}"), license: String::new(), author: String::new(), duration: 0.0 };
                    let reg = crate::tools::register_sample(e, info)?;
                    out["new_sample"] = reg["sample"].clone();
                    out["new_s"] = json!((data.len() as f32 / SR * 100.0).round() / 100.0);
                }
                Ok(out)
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use crate::engine::Engine;
    use serde_json::json;

    #[test]
    fn calibrated_presets_start_balanced_and_balance_mix_fixes_a_bad_draft() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_mix_tests"));
        for (n, p) in [
            ("kick", "kick"),
            ("hat", "hat"),
            ("bass", "808"),
            ("keys", "epiano"),
        ] {
            e.call("add_track", &json!({"name": n, "preset": p}))
                .unwrap();
        }
        let h = e.call("level_hints", &json!({})).unwrap();
        for t in h["tracks"].as_array().unwrap() {
            assert_eq!(t["hint"], "in range", "{t}");
        }
        e.call("generate_drums", &json!({"style": "trap", "seed": 2}))
            .unwrap();
        e.call("add_notes", &json!({"track": "bass", "notes": [{"start": 0, "pitch": "C2", "len": 8}, {"start": 8, "pitch": "C2", "len": 8}]})).unwrap();
        e.call("add_notes", &json!({"track": "keys", "notes": [{"start": 0, "pitch": "C4", "len": 8}, {"start": 8, "pitch": "Eb4", "len": 8}]})).unwrap();
        // ruin it: kick +12, hats -15
        e.call("set_mixer", &json!({"track": "kick", "volume_db": 12}))
            .unwrap();
        e.call("set_mixer", &json!({"track": "hat", "volume_db": -20}))
            .unwrap();
        let r = e
            .call("balance_mix", &json!({"genre": "trap", "iterations": 3}))
            .unwrap();
        assert_eq!(r["applied"], true);
        let kick_share = r["after"]["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["track"] == "kick")
            .unwrap()["energy_share_percent"]
            .as_f64()
            .unwrap();
        assert!(kick_share < 60.0, "{r}");
        let hat = r["moves"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["track"] == "hat")
            .unwrap();
        assert!(hat["change_db"].as_f64().unwrap() > 5.0, "{r}");
    }

    #[test]
    fn analyze_track_finds_pitch_and_key_fit() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_mix_tests2"));
        e.call("set_key", &json!({"root": "D", "scale": "minor"}))
            .unwrap();
        e.call("add_track", &json!({"name": "sub", "preset": "sub_bass"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track": "sub", "notes": [{"start": 0, "pitch": "C#2", "len": 8}]}),
        )
        .unwrap();
        let a = e.call("analyze_track", &json!({"track": "sub"})).unwrap();
        assert_eq!(a["pitch"]["nearest_note"], "C#2", "{a}");
        assert!(
            (a["pitch"]["fundamental_hz"].as_f64().unwrap() - 69.3).abs() < 1.5,
            "{a}"
        );
        assert_eq!(a["pitch"]["in_key"], false);
        let c = a["pitch"]["closest_in_key"].as_str().unwrap();
        assert!(c == "D2" || c == "C2", "{a}");
        let t = e
            .call("detect_transients", &json!({"track": "sub"}))
            .unwrap();
        assert!(t["count"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn unknown_nested_params_are_rejected() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_mix_tests3"));
        e.call("add_track", &json!({"name": "pad", "preset": "warm_pad"}))
            .unwrap();
        let err = e.call("add_effect", &json!({"track": "pad", "type": "parametric_eq", "params": {"bands": [{"kind": "low_cut", "freq": 100, "slope": 24}]}})).unwrap_err();
        assert!(format!("{err:#}").contains("bands[0].slope"), "{err:#}");
        assert!(e.project.tracks[0].effects.is_empty());
        let ok = e.call("add_effect", &json!({"track": "pad", "type": "parametric_eq", "params": {"bands": [{"kind": "low_cut", "freq": 100, "stages": 2}]}})).unwrap();
        assert_eq!(ok["effective"]["bands"][0]["stages"], 2);
        assert!(e
            .call(
                "tweak_effect",
                &json!({"track": "pad", "index": 0, "params": {"output_gain": 3}})
            )
            .is_err());
        assert!(e
            .call("set_tempo", &json!({"bpm": 90, "tempo": 3}))
            .is_err());
        assert!(e
            .call(
                "tweak_instrument",
                &json!({"track": "pad", "params": {"cutof": 300}})
            )
            .is_err());
    }
}
