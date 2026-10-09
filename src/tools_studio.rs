//! Studio-grade tools: automation, buses/sends, A/B variants + diff, and
//! validation / master QC. Registered after the core tools in `tools.rs`.

use crate::analysis;
use crate::automation::{
    self, canonical, parse_target, AutoPoint, AutomationLane, Curve, Shape, Target,
};
use crate::diff;
use crate::engine::{Engine, Snapshot};
use crate::fx::{Effect, FilterFx};
use crate::project::{Bus, Project, Send};
use crate::tools::{b_or, f_opt, obj, s_opt, s_req, Tool};
use crate::validate;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

// ---------- helpers ----------

/// Canonical owner name: a track, a bus or "master".
fn owner_of(p: &Project, key: &str) -> Result<String> {
    if key.eq_ignore_ascii_case("master") {
        return Ok("master".into());
    }
    if let Ok(i) = p.track_index(key) {
        return Ok(p.tracks[i].name.clone());
    }
    if let Ok(i) = p.bus_index(key) {
        return Ok(p.buses[i].name.clone());
    }
    bail!(
        "no track or bus '{key}'. Tracks: [{}], buses: [{}], or 'master'",
        p.tracks
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        p.buses
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn chain_of<'a>(p: &'a Project, owner: &str) -> &'a [Effect] {
    if owner.eq_ignore_ascii_case("master") {
        &p.master_effects
    } else if let Ok(i) = p.track_index(owner) {
        &p.tracks[i].effects
    } else if let Ok(i) = p.bus_index(owner) {
        &p.buses[i].effects
    } else {
        &[]
    }
}

/// The current static value of an automation target (validates it too).
fn static_value(p: &Project, owner: &str, param: &str) -> Result<f32> {
    let t = parse_target(param).ok_or_else(|| {
        anyhow!("unknown parameter '{param}'. Use volume, pan, instrument.<param> (e.g. instrument.cutoff) or fx.<index>.<param> (e.g. fx.0.cutoff). list_automation shows every option.")
    })?;
    let is_master = owner.eq_ignore_ascii_case("master");
    let track = p.track_index(owner).ok();
    let bus = p.bus_index(owner).ok();
    match t {
        Target::Volume => Ok(if is_master {
            p.master_volume_db
        } else if let Some(i) = track {
            p.tracks[i].volume_db
        } else {
            bus.map(|b| p.buses[b].volume_db).unwrap_or(0.0)
        }),
        Target::Pan => {
            if is_master {
                bail!("the master has no pan; automate a track or bus")
            }
            Ok(track
                .map(|i| p.tracks[i].pan)
                .or(bus.map(|b| p.buses[b].pan))
                .unwrap_or(0.0))
        }
        Target::Instrument(path) => {
            let i = track.ok_or_else(|| {
                anyhow!("only tracks have instruments ('{owner}' is not a track)")
            })?;
            let v = serde_json::to_value(&p.tracks[i].instrument)?;
            automation::get_path(&v, &path)
                .map(|x| x as f32)
                .ok_or_else(|| {
                    anyhow!(
                        "{} instrument on '{owner}' has no numeric parameter '{}'. Options: {}",
                        p.tracks[i].instrument.kind_name(),
                        path.join("."),
                        numeric_paths(&v, "instrument").join(", ")
                    )
                })
        }
        Target::Fx(idx, path) => {
            let chain = chain_of(p, owner);
            let fx = chain.get(idx).ok_or_else(|| {
                anyhow!(
                    "no effect at index {idx} on '{owner}' (chain: [{}]). add_effect first.",
                    chain
                        .iter()
                        .map(|e| e.type_name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            let v = serde_json::to_value(fx)?;
            automation::get_path(&v, &path)
                .map(|x| x as f32)
                .ok_or_else(|| {
                    anyhow!(
                        "{} has no numeric parameter '{}'. Options: {}",
                        fx.type_name(),
                        path.join("."),
                        numeric_paths(&v, &format!("fx.{idx}")).join(", ")
                    )
                })
        }
    }
}

/// Every numeric leaf under `v` as `prefix.path`.
fn numeric_paths(v: &Value, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Value::Object(o) = v {
        for (k, x) in o {
            let p = format!("{prefix}.{k}");
            if x.is_number() {
                out.push(p);
            } else if x.is_object() {
                out.extend(numeric_paths(x, &p));
            }
        }
    }
    out
}

pub fn automatable(p: &Project, owner: &str) -> Vec<String> {
    let mut v = vec!["volume".to_string()];
    if !owner.eq_ignore_ascii_case("master") {
        v.push("pan".into());
    }
    if let Ok(i) = p.track_index(owner) {
        if let Ok(iv) = serde_json::to_value(&p.tracks[i].instrument) {
            v.extend(numeric_paths(&iv, "instrument"));
        }
    }
    for (i, fx) in chain_of(p, owner).iter().enumerate() {
        if let Ok(fv) = serde_json::to_value(fx) {
            v.extend(numeric_paths(&fv, &format!("fx.{i}")));
        }
    }
    v
}

fn points_from(a: &Value, default_curve: Curve) -> Result<Vec<AutoPoint>> {
    let arr = match a.get("points") {
        None => return Ok(Vec::new()),
        Some(Value::Array(x)) => x,
        Some(_) => bail!("points must be an array of {{beat, value, curve?}}"),
    };
    arr.iter()
        .map(|pt| {
            // accept {beat, value} objects or [beat, value] pairs
            let (beat, value, curve) = if let Some(pair) = pt.as_array() {
                (
                    pair.first().and_then(|x| x.as_f64()),
                    pair.get(1).and_then(|x| x.as_f64()),
                    pair.get(2).and_then(|x| x.as_str()).and_then(Curve::parse),
                )
            } else {
                (
                    pt.get("beat")
                        .and_then(|x| x.as_f64())
                        .or_else(|| pt.get("bar").and_then(|x| x.as_f64()).map(|b| b * 4.0)),
                    pt.get("value").and_then(|x| x.as_f64()),
                    pt.get("curve")
                        .and_then(|x| x.as_str())
                        .and_then(Curve::parse),
                )
            };
            let beat = beat.ok_or_else(|| anyhow!("point {pt} is missing 'beat'"))? as f32;
            let value = value.ok_or_else(|| anyhow!("point {pt} is missing 'value'"))? as f32;
            if !beat.is_finite() || !value.is_finite() || beat < 0.0 {
                bail!("point {pt} has an invalid beat/value");
            }
            Ok(AutoPoint {
                beat,
                value,
                curve: curve.unwrap_or(default_curve),
            })
        })
        .collect()
}

fn clamp_value(param: &str, v: f32) -> f32 {
    match parse_target(param) {
        Some(Target::Volume) => v.clamp(-100.0, 12.0),
        Some(Target::Pan) => v.clamp(-1.0, 1.0),
        _ => {
            let leaf = param.rsplit('.').next().unwrap_or("");
            if automation::is_log_param(param) {
                v.clamp(10.0, 22_000.0)
            } else if matches!(leaf, "mix" | "amount" | "resonance" | "damping") {
                v.clamp(0.0, 1.0)
            } else {
                v
            }
        }
    }
}

fn lane_json(l: &AutomationLane) -> Value {
    let (lo, hi) = l.range().unwrap_or((0.0, 0.0));
    json!({
        "track": l.target,
        "param": l.param,
        "enabled": l.enabled,
        "points": l.points.len(),
        "range": [lo, hi],
        "span_beats": [l.points.first().map(|p| p.beat), l.points.last().map(|p| p.beat)],
        "preview": l.points.iter().take(12).map(|p| json!([p.beat, p.value, p.curve])).collect::<Vec<_>>(),
    })
}

fn lane_mut<'a>(p: &'a mut Project, owner: &str, param: &str) -> &'a mut AutomationLane {
    let pos = p.automation.iter().position(|l| l.is_target(owner, param));
    match pos {
        Some(i) => &mut p.automation[i],
        None => {
            p.automation.push(AutomationLane::new(owner, param));
            p.automation.last_mut().unwrap()
        }
    }
}

/// After removing effect `removed` from `owner`'s chain: drop its lanes and
/// shift the indices of later ones. Returns the number of lanes dropped.
pub fn reindex_fx_lanes(p: &mut Project, owner: &str, removed: usize) -> usize {
    let before = p.automation.len();
    p.automation.retain(|l| {
        !(l.target.eq_ignore_ascii_case(owner)
            && matches!(parse_target(&l.param), Some(Target::Fx(i, _)) if i == removed))
    });
    for l in p.automation.iter_mut() {
        if !l.target.eq_ignore_ascii_case(owner) {
            continue;
        }
        if let Some(Target::Fx(i, path)) = parse_target(&l.param) {
            if i > removed {
                l.param = format!("fx.{}.{}", i - 1, path.join("."));
            }
        }
    }
    before - p.automation.len()
}

/// Parse a tempo-synced rate: number of beats, or "1/4", "1/8", "1/16",
/// "1/2", "1 bar", "2 bars", "1/8t" (triplet), "1/4." (dotted).
fn parse_rate(v: Option<&Value>) -> Result<f32> {
    let Some(v) = v else { return Ok(1.0) };
    if let Some(n) = v.as_f64() {
        return Ok(n as f32);
    }
    let s = v.as_str().unwrap_or("").trim().to_lowercase();
    let (s, mult) = if let Some(x) = s.strip_suffix('t') {
        (x.to_string(), 2.0 / 3.0)
    } else if let Some(x) = s.strip_suffix('.') {
        (x.to_string(), 1.5)
    } else {
        (s, 1.0)
    };
    let beats = if let Some(b) = s.strip_suffix("bars").or_else(|| s.strip_suffix("bar")) {
        b.trim().parse::<f32>().unwrap_or(1.0) * 4.0
    } else if let Some((a, b)) = s.split_once('/') {
        let (a, b): (f32, f32) = (a.trim().parse()?, b.trim().parse()?);
        4.0 * a / b
    } else {
        s.parse::<f32>()?
    };
    if beats <= 0.0 {
        bail!("rate must be positive");
    }
    Ok(beats * mult)
}

fn bus_preset(kind: &str) -> Result<Vec<Effect>> {
    let fx = |t: &str, params: Value| -> Effect {
        let mut v = json!({ "type": t });
        crate::tools::merge(&mut v, &params);
        serde_json::from_value(v).expect("valid preset effect")
    };
    Ok(match kind.to_lowercase().as_str() {
        "" | "none" | "empty" => vec![],
        // 100% wet returns, low end cleaned so they don't muddy the mix
        "reverb" | "reverb_return" | "hall" => vec![
            Effect::Filter(FilterFx {
                mode: crate::dsp::FilterMode::Highpass,
                cutoff: 280.0,
                resonance: 0.1,
            }),
            fx(
                "reverb",
                json!({"size": 0.85, "damping": 0.45, "mix": 1.0, "predelay_ms": 25.0, "width": 1.0}),
            ),
        ],
        "room" => vec![
            Effect::Filter(FilterFx {
                mode: crate::dsp::FilterMode::Highpass,
                cutoff: 220.0,
                resonance: 0.1,
            }),
            fx(
                "reverb",
                json!({"size": 0.45, "damping": 0.6, "mix": 1.0, "predelay_ms": 8.0}),
            ),
        ],
        "delay" | "delay_return" => vec![
            Effect::Filter(FilterFx {
                mode: crate::dsp::FilterMode::Highpass,
                cutoff: 350.0,
                resonance: 0.1,
            }),
            fx(
                "delay",
                json!({"steps": 3.0, "feedback": 0.4, "mix": 1.0, "ping_pong": true, "tone": 3500.0}),
            ),
        ],
        "drum" | "drums" | "drum_bus" => vec![
            fx(
                "compressor",
                json!({"threshold_db": -16.0, "ratio": 3.0, "attack_ms": 15.0, "release_ms": 90.0, "makeup_db": 2.0}),
            ),
            fx("transient", json!({"attack": 0.25, "sustain": -0.1})),
            fx("distortion", json!({"drive": 0.12, "mix": 0.35})),
        ],
        "parallel" | "parallel_comp" | "crush" => vec![
            fx(
                "compressor",
                json!({"threshold_db": -30.0, "ratio": 10.0, "attack_ms": 2.0, "release_ms": 60.0, "makeup_db": 12.0}),
            ),
            fx("distortion", json!({"drive": 0.3, "mix": 0.5})),
        ],
        "music" | "glue" => vec![fx(
            "compressor",
            json!({"threshold_db": -18.0, "ratio": 2.0, "attack_ms": 30.0, "release_ms": 200.0, "makeup_db": 1.5}),
        )],
        other => bail!(
            "unknown bus preset '{other}'. Use reverb, room, delay, drum, parallel, glue or none."
        ),
    })
}

fn routing_json(p: &Project) -> Value {
    json!({
        "tracks": p.tracks.iter().map(|t| json!({
            "track": t.name,
            "output": t.output.clone().unwrap_or_else(|| "master".into()),
            "sends": t.sends.iter().map(|s| json!({"bus": s.bus, "db": s.db, "pre_fader": s.pre_fader})).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "buses": p.buses.iter().map(|b| json!({
            "bus": b.name,
            "volume_db": b.volume_db,
            "pan": b.pan,
            "mute": b.mute,
            "effects": b.effects.iter().map(|e| e.type_name()).collect::<Vec<_>>(),
            "inputs": p.tracks.iter().filter(|t| t.output.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(&b.name))).map(|t| t.name.clone()).collect::<Vec<_>>(),
            "sends_from": p.tracks.iter().filter_map(|t| t.sends.iter().find(|s| s.bus.eq_ignore_ascii_case(&b.name)).map(|s| format!("{} {:+.1} dB", t.name, s.db))).collect::<Vec<_>>(),
            "output": "master",
        })).collect::<Vec<_>>(),
        "master": {"volume_db": p.master_volume_db, "effects": p.master_effects.iter().map(|e| e.type_name()).collect::<Vec<_>>()},
        "signal_flow": "track: instrument -> pan -> effects -> fader -> output (bus or master); sends tap pre/post fader into buses; bus: effects -> fader -> master; master: volume -> DC block -> master effects",
    })
}

fn track_list(a: &Value) -> Vec<String> {
    match a.get("track").or_else(|| a.get("tracks")) {
        Some(Value::Array(v)) => v
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn variant_metrics(e: &mut Engine, name: &str) -> Result<Value> {
    let proj = e.version(name)?;
    let mix = e.render_version(&proj)?;
    let rep = analysis::analyze(&mix);
    Ok(json!({
        "variant": name,
        "score": rep.score,
        "integrated_lufs": rep.loudness.integrated_lufs,
        "true_peak_dbtp": rep.loudness.true_peak_dbtp,
        "rms_dbfs": rep.master.rms_dbfs,
        "crest_db": rep.master.crest_db,
        "stereo_correlation": rep.stereo_correlation,
        "spectral_centroid_hz": rep.master.spectral_centroid_hz,
        "bands": rep.master.bands.iter().map(|b| (b.band.to_string(), json!(b.percent))).collect::<serde_json::Map<String, Value>>(),
        "seconds": (rep.seconds * 10.0).round() / 10.0,
        "suggestions": rep.suggestions,
    }))
}

fn targets_from(a: &Value) -> validate::MasterTargets {
    let d = validate::MasterTargets::default();
    validate::MasterTargets {
        lufs: f_opt(a, "target_lufs").unwrap_or(d.lufs),
        lufs_tolerance: f_opt(a, "lufs_tolerance").unwrap_or(d.lufs_tolerance),
        true_peak_ceiling: f_opt(a, "true_peak_ceiling").unwrap_or(d.true_peak_ceiling),
    }
}

fn points_schema() -> Value {
    json!({"type": "array", "description": "Breakpoints: [{beat, value, curve?}] where beat is the song position in quarter notes (bar 2 = beat 4) and curve (linear|step|smooth) shapes the segment to the next point. [beat, value] pairs also work.", "items": {}})
}

// ---------- the tools ----------

pub fn tools() -> Vec<Tool> {
    vec![
        // ----- automation -----
        Tool {
            name: "add_automation",
            description: "Create (or replace) an automation lane on a track, bus or 'master'. param: volume (dB), pan (-1..1), instrument.<param> (any numeric synth param, e.g. instrument.cutoff, instrument.amp_env.release; applied per note) or fx.<index>.<param> (any numeric effect param, e.g. fx.0.cutoff for a filter sweep, fx.1.mix; applied per 64-sample block). Points are in song beats. For musical shapes use generate_automation instead.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string", "description": "Track, bus or 'master'"},
                "param": {"type": "string"},
                "points": points_schema(),
                "curve": {"type": "string", "enum": ["linear", "step", "smooth"], "description": "Default segment curve"}
            }), &["track", "param"]),
            run: |e, a| {
                let owner = owner_of(&e.project, &s_req(a, "track")?)?;
                let raw = s_req(a, "param")?;
                let param = canonical(&raw).ok_or_else(|| anyhow!("unknown parameter '{raw}'. Use volume, pan, instrument.<param> or fx.<index>.<param>."))?;
                let current = static_value(&e.project, &owner, &param)?;
                let curve = s_opt(a, "curve").and_then(|c| Curve::parse(&c)).unwrap_or_default();
                let mut pts = points_from(a, curve)?;
                for pt in pts.iter_mut() {
                    pt.value = clamp_value(&param, pt.value);
                }
                let lane = lane_mut(&mut e.project, &owner, &param);
                lane.points = pts;
                lane.enabled = true;
                lane.sort();
                let out = lane_json(lane);
                Ok(json!({"lane": out, "static_value": current, "suggested_range": automation::default_range(&param), "song_beats": e.project.song_beats()}))
            },
        },
        Tool {
            name: "set_automation_points",
            description: "Edit an existing lane's breakpoints. mode 'replace' (default) swaps all points; 'merge' inserts/overwrites points at the same beats; 'erase' removes points between from_beat and to_beat. Also toggles a lane with enabled:false (bypass without deleting).",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string"},
                "param": {"type": "string"},
                "points": points_schema(),
                "mode": {"type": "string", "enum": ["replace", "merge", "erase"]},
                "from_beat": {"type": "number"},
                "to_beat": {"type": "number"},
                "curve": {"type": "string", "enum": ["linear", "step", "smooth"]},
                "enabled": {"type": "boolean"}
            }), &["track", "param"]),
            run: |e, a| {
                let owner = owner_of(&e.project, &s_req(a, "track")?)?;
                let raw = s_req(a, "param")?;
                let param = canonical(&raw).ok_or_else(|| anyhow!("unknown parameter '{raw}'"))?;
                let idx = e.project.automation.iter().position(|l| l.is_target(&owner, &param)).ok_or_else(|| anyhow!("no automation lane {owner}:{param}. Create it with add_automation or generate_automation."))?;
                let curve = s_opt(a, "curve").and_then(|c| Curve::parse(&c)).unwrap_or_default();
                let mut pts = points_from(a, curve)?;
                for pt in pts.iter_mut() {
                    pt.value = clamp_value(&param, pt.value);
                }
                let lane = &mut e.project.automation[idx];
                match s_opt(a, "mode").as_deref().unwrap_or("replace") {
                    "replace" => {
                        if a.get("points").is_some() {
                            lane.points = pts;
                        }
                    }
                    "merge" => {
                        for pt in pts {
                            lane.points.retain(|q| (q.beat - pt.beat).abs() > 1e-4);
                            lane.points.push(pt);
                        }
                    }
                    "erase" => {
                        let from = f_opt(a, "from_beat").unwrap_or(0.0);
                        let to = f_opt(a, "to_beat").unwrap_or(f32::MAX);
                        lane.points.retain(|q| q.beat < from || q.beat > to);
                    }
                    m => bail!("unknown mode '{m}' (replace|merge|erase)"),
                }
                if let Some(en) = a.get("enabled").and_then(|v| v.as_bool()) {
                    lane.enabled = en;
                }
                lane.sort();
                Ok(lane_json(lane))
            },
        },
        Tool {
            name: "clear_automation",
            description: "Delete automation: one lane (track + param), every lane of a track/bus (track only), or all lanes (no args). The static fader/param value takes over again.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "param": {"type": "string"}}), &[]),
            run: |e, a| {
                let owner = s_opt(a, "track").map(|t| owner_of(&e.project, &t)).transpose()?;
                let param = s_opt(a, "param").map(|p| canonical(&p).unwrap_or(p));
                let before = e.project.automation.len();
                e.project.automation.retain(|l| {
                    let o = owner.as_ref().is_none_or(|o| l.target.eq_ignore_ascii_case(o));
                    let p = param.as_ref().is_none_or(|p| l.param.eq_ignore_ascii_case(p));
                    !(o && p)
                });
                let removed = before - e.project.automation.len();
                if removed == 0 && (owner.is_some() || param.is_some()) {
                    bail!("no matching automation lane");
                }
                Ok(json!({"removed": removed, "remaining": e.project.automation.len()}))
            },
        },
        Tool {
            name: "list_automation",
            description: "List automation lanes (optionally for one track/bus/master) with point counts, value ranges and a preview, plus every automatable parameter of that target. Give at_beat to read each lane's value at a song position.",
            mutates: false,
            schema: || obj(json!({"track": {"type": "string"}, "at_beat": {"type": "number"}}), &[]),
            run: |e, a| {
                let owner = s_opt(a, "track").map(|t| owner_of(&e.project, &t)).transpose()?;
                let at = f_opt(a, "at_beat");
                let lanes: Vec<Value> = e.project.automation.iter()
                    .filter(|l| owner.as_ref().is_none_or(|o| l.target.eq_ignore_ascii_case(o)))
                    .map(|l| {
                        let mut j = lane_json(l);
                        if let Some(b) = at {
                            j["value_at_beat"] = json!(l.value_at(b));
                        }
                        j
                    })
                    .collect();
                let mut out = json!({"lanes": lanes, "song_beats": e.project.song_beats(), "sections": section_map(&e.project)});
                if let Some(o) = owner {
                    out["automatable"] = json!(automatable(&e.project, &o));
                }
                Ok(out)
            },
        },
        Tool {
            name: "generate_automation",
            description: "Write a musical automation shape over a section: riser/sweep_up (accelerating rise, great on fx filter cutoff before a drop), sweep_down, fade_in, fade_out, pump (tempo-synced ducking, rate e.g. '1/4'), lfo (tempo-synced sine, rate '1/8', '1/4t', '2 bars'...). Range: min/max (defaults per param; frequency params sweep logarithmically). Choose where with section (arrangement index or pattern name), or start_beat/end_beat, or bars from start_beat. Points outside the range are kept.",
            mutates: true,
            schema: || obj(json!({
                "track": {"type": "string", "description": "Track, bus or 'master'"},
                "param": {"type": "string", "description": "volume, pan, instrument.<p> or fx.<i>.<p>"},
                "shape": {"type": "string", "enum": Shape::NAMES},
                "section": {"type": "string", "description": "Arrangement section index ('2') or pattern name ('drop'); default = whole song"},
                "start_beat": {"type": "number"},
                "end_beat": {"type": "number"},
                "bars": {"type": "number"},
                "min": {"type": "number"},
                "max": {"type": "number"},
                "rate": {"description": "pump/lfo period: beats (number) or '1/4', '1/8', '1/16', '1/8t', '1/4.', '1 bar'"}
            }), &["track", "param", "shape"]),
            run: |e, a| {
                let owner = owner_of(&e.project, &s_req(a, "track")?)?;
                let raw = s_req(a, "param")?;
                let param = canonical(&raw).ok_or_else(|| anyhow!("unknown parameter '{raw}'"))?;
                let current = static_value(&e.project, &owner, &param)?;
                let shape_s = s_req(a, "shape")?;
                let shape = Shape::parse(&shape_s).ok_or_else(|| anyhow!("unknown shape '{shape_s}'. Use one of {:?}", Shape::NAMES))?;
                let (mut start, mut end) = match s_opt(a, "section") {
                    Some(sec) => e.project.section_beats(&sec)?,
                    None => (0.0, e.project.song_beats()),
                };
                if let Some(s) = f_opt(a, "start_beat") {
                    start = s.max(0.0);
                    if s_opt(a, "section").is_none() && f_opt(a, "end_beat").is_none() && f_opt(a, "bars").is_none() {
                        end = e.project.song_beats().max(start + 4.0);
                    }
                }
                if let Some(b) = f_opt(a, "bars") {
                    end = start + b.max(0.25) * 4.0;
                }
                if let Some(x) = f_opt(a, "end_beat") {
                    end = x;
                }
                if end <= start {
                    bail!("empty range: start_beat {start} >= end_beat {end}");
                }
                let (dlo, dhi) = automation::default_range(&param);
                let (dlo, dhi) = match (parse_target(&param), shape) {
                    // fades go to silence and back to the fader level
                    (Some(Target::Volume), Shape::FadeIn | Shape::FadeOut) => (-60.0, current),
                    (Some(Target::Volume), Shape::Pump) => (current - 12.0, current),
                    (Some(Target::Volume), _) => (current - 18.0, current),
                    _ => (dlo, dhi),
                };
                let lo = clamp_value(&param, f_opt(a, "min").unwrap_or(dlo));
                let hi = clamp_value(&param, f_opt(a, "max").unwrap_or(dhi));
                let rate = parse_rate(a.get("rate"))?;
                let log = automation::is_log_param(&param) && lo > 0.0 && hi > 0.0;
                let pts = automation::generate(shape, start, end, lo, hi, rate, log);
                let n = pts.len();
                let lane = lane_mut(&mut e.project, &owner, &param);
                lane.points.retain(|q| q.beat < start - 1e-4 || q.beat > end + 1e-4);
                lane.points.extend(pts);
                lane.enabled = true;
                lane.sort();
                Ok(json!({"lane": lane_json(lane), "shape": shape_s, "beats": [start, end], "range": [lo, hi], "rate_beats": rate, "points_written": n}))
            },
        },
        // ----- buses / sends / routing -----
        Tool {
            name: "add_bus",
            description: "Create a mix bus that sums into the master. preset: reverb (100% wet hall return, low-cut), room, delay (dotted-8th ping-pong return), drum (glue comp + punch + grit), parallel (crushed parallel comp), glue, or none. Feed it with route_track (groups) or set_send (returns); add more effects with add_effect {track:<bus>}.",
            mutates: true,
            schema: || obj(json!({
                "name": {"type": "string"},
                "preset": {"type": "string", "enum": ["reverb", "room", "delay", "drum", "parallel", "glue", "none"]},
                "volume_db": {"type": "number"}
            }), &["name"]),
            run: |e, a| {
                let name = s_req(a, "name")?;
                if name.eq_ignore_ascii_case("master") || e.project.track_index(&name).is_ok() || e.project.bus_index(&name).is_ok() {
                    bail!("'{name}' is already a track, bus or the master");
                }
                let preset = s_opt(a, "preset").unwrap_or_else(|| name.to_lowercase());
                let effects = bus_preset(&preset).or_else(|err| if s_opt(a, "preset").is_some() { Err(err) } else { Ok(vec![]) })?;
                let mut b = Bus::new(&name);
                b.effects = effects;
                b.volume_db = f_opt(a, "volume_db").unwrap_or(0.0).clamp(-60.0, 12.0);
                let chain: Vec<String> = b.effects.iter().map(|x| x.type_name()).collect();
                e.project.buses.push(b);
                Ok(json!({"bus": name, "effects": chain, "next": "set_send {track, bus, db:-12} for returns, route_track {track, bus} for groups"}))
            },
        },
        Tool {
            name: "remove_bus",
            description: "Delete a bus. Tracks routed to it go back to the master; sends to it and its automation are removed.",
            mutates: true,
            schema: || obj(json!({"name": {"type": "string"}}), &["name"]),
            run: |e, a| {
                let bi = e.project.bus_index(&s_req(a, "name")?)?;
                let b = e.project.buses.remove(bi);
                let mut rerouted = Vec::new();
                for t in e.project.tracks.iter_mut() {
                    if t.output.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(&b.name)) {
                        t.output = None;
                        rerouted.push(t.name.clone());
                    }
                    t.sends.retain(|s| !s.bus.eq_ignore_ascii_case(&b.name));
                }
                e.project.automation.retain(|l| !l.target.eq_ignore_ascii_case(&b.name));
                Ok(json!({"removed": b.name, "rerouted_to_master": rerouted}))
            },
        },
        Tool {
            name: "set_send",
            description: "Set an aux send from track(s) to a bus in dB (-12 is a typical reverb send, 0 = unity). pre_fader:true taps before the track fader. remove:true deletes the send. track accepts a name, a comma list or an array.",
            mutates: true,
            schema: || obj(json!({
                "track": {"description": "Track name, 'a,b' or [names]"},
                "bus": {"type": "string"},
                "db": {"type": "number"},
                "pre_fader": {"type": "boolean"},
                "remove": {"type": "boolean"}
            }), &["track", "bus"]),
            run: |e, a| {
                let bi = e.project.bus_index(&s_req(a, "bus")?)?;
                let bus = e.project.buses[bi].name.clone();
                let names = track_list(a);
                if names.is_empty() {
                    bail!("missing track");
                }
                let db = f_opt(a, "db").unwrap_or(-12.0).clamp(-60.0, 12.0);
                let mut out = Vec::new();
                for n in names {
                    let ti = e.project.track_index(&n)?;
                    let t = &mut e.project.tracks[ti];
                    if t.output.as_deref().is_some_and(|o| o.eq_ignore_ascii_case(&bus)) {
                        bail!("'{}' already outputs to '{bus}'; a send would double it", t.name);
                    }
                    t.sends.retain(|s| !s.bus.eq_ignore_ascii_case(&bus));
                    if !b_or(a, "remove", false) {
                        t.sends.push(Send { bus: bus.clone(), db, pre_fader: b_or(a, "pre_fader", false) });
                    }
                    out.push(json!({"track": t.name, "sends": t.sends.iter().map(|s| format!("{} {:+.1} dB{}", s.bus, s.db, if s.pre_fader { " pre" } else { "" })).collect::<Vec<_>>()}));
                }
                Ok(json!({"updated": out}))
            },
        },
        Tool {
            name: "route_track",
            description: "Route track(s) output to a bus (group processing, e.g. all drums into a 'drums' bus) or back to 'master'. track accepts a name, a comma list or an array.",
            mutates: true,
            schema: || obj(json!({"track": {"description": "Track name, 'a,b' or [names]"}, "bus": {"type": "string", "description": "Bus name or 'master'"}}), &["track", "bus"]),
            run: |e, a| {
                let bus = s_req(a, "bus")?;
                let dest = if bus.eq_ignore_ascii_case("master") { None } else { Some(e.project.buses[e.project.bus_index(&bus)?].name.clone()) };
                let names = track_list(a);
                if names.is_empty() {
                    bail!("missing track");
                }
                let mut routed = Vec::new();
                for n in names {
                    let ti = e.project.track_index(&n)?;
                    let t = &mut e.project.tracks[ti];
                    if let Some(d) = &dest {
                        t.sends.retain(|s| !s.bus.eq_ignore_ascii_case(d));
                    }
                    t.output = dest.clone();
                    routed.push(t.name.clone());
                }
                Ok(json!({"routed": routed, "to": dest.unwrap_or_else(|| "master".into())}))
            },
        },
        Tool {
            name: "list_routing",
            description: "Show the signal flow: each track's output and sends, each bus's inputs, effects and level, and the master chain.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| Ok(routing_json(&e.project)),
        },
        // ----- A/B variants -----
        Tool {
            name: "snapshot",
            description: "Save the current project as a named version (A/B variant) before trying something bold. Re-using a name overwrites it. Compare with diff_project / compare_variants, go back with restore_snapshot.",
            mutates: false,
            schema: || obj(json!({"name": {"type": "string"}, "note": {"type": "string"}}), &["name"]),
            run: |e, a| {
                let name = s_req(a, "name")?;
                if name.eq_ignore_ascii_case("current") {
                    bail!("'current' is reserved");
                }
                let snap = Snapshot { project: e.project.clone(), note: s_opt(a, "note").unwrap_or_default(), revision: e.revision };
                let replaced = if let Some(slot) = e.snapshots.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(&name)) {
                    slot.1 = snap;
                    true
                } else {
                    e.snapshots.push((name.clone(), snap));
                    false
                };
                Ok(json!({"snapshot": name, "replaced": replaced, "count": e.snapshots.len()}))
            },
        },
        Tool {
            name: "list_snapshots",
            description: "List saved versions with their note, tempo, track count and how many changes separate each from the current project.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| {
                let cur = &e.project;
                Ok(json!({"snapshots": e.snapshots.iter().map(|(n, s)| json!({
                    "name": n,
                    "note": s.note,
                    "bpm": s.project.bpm,
                    "tracks": s.project.tracks.len(),
                    "changes_vs_current": diff::diff(&s.project, cur).len(),
                })).collect::<Vec<_>>()}))
            },
        },
        Tool {
            name: "restore_snapshot",
            description: "Make a saved version the current project (undoable). The snapshot itself is kept.",
            mutates: true,
            schema: || obj(json!({"name": {"type": "string"}}), &["name"]),
            run: |e, a| {
                let name = s_req(a, "name")?;
                let p = e.version(&name)?;
                let changes = diff::diff(&e.project, &p).len();
                e.project = p;
                e.bank.sync(&e.project.samples);
                Ok(json!({"restored": name, "changes": changes}))
            },
        },
        Tool {
            name: "diff_project",
            description: "Structured diff between two versions: from/to are snapshot names or 'current' (to defaults to current). Returns each change (path, kind added|removed|changed|notes|points, from, to), e.g. tracks.bass.volume_db -2 -> -5, patterns.A.clips.kick +4 notes.",
            mutates: false,
            schema: || obj(json!({"from": {"type": "string"}, "to": {"type": "string"}, "limit": {"type": "integer"}}), &["from"]),
            run: |e, a| {
                let from = s_req(a, "from")?;
                let to = s_opt(a, "to").unwrap_or_else(|| "current".into());
                let (pa, pb) = (e.version(&from)?, e.version(&to)?);
                let changes = diff::diff(&pa, &pb);
                let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
                Ok(json!({
                    "from": from, "to": to,
                    "total": changes.len(),
                    "summary": changes.iter().take(limit).map(diff::describe).collect::<Vec<_>>(),
                    "changes": changes.into_iter().take(limit).collect::<Vec<_>>(),
                }))
            },
        },
        Tool {
            name: "compare_variants",
            description: "Render and analyze several versions (snapshot names and/or 'current') side by side: mix score, integrated LUFS, true peak, crest, stereo correlation, spectral balance, suggestions. Returns a ranking by score and the winner. Default: every snapshot plus current.",
            mutates: false,
            schema: || obj(json!({"variants": {"type": "array", "items": {"type": "string"}}}), &[]),
            run: |e, a| {
                let mut names: Vec<String> = a.get("variants").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                if names.is_empty() {
                    names = e.snapshots.iter().map(|(n, _)| n.clone()).collect();
                    names.push("current".into());
                }
                if names.len() > 8 {
                    bail!("compare at most 8 variants at once");
                }
                let mut rows = Vec::new();
                for n in &names {
                    rows.push(variant_metrics(e, n)?);
                }
                let mut rank: Vec<(String, u64)> = rows.iter().map(|r| (r["variant"].as_str().unwrap_or("").to_string(), r["score"].as_u64().unwrap_or(0))).collect();
                rank.sort_by_key(|x| std::cmp::Reverse(x.1));
                Ok(json!({"variants": rows, "ranking": rank.iter().map(|(n, s)| format!("{n} ({s})")).collect::<Vec<_>>(), "winner": rank.first().map(|r| r.0.clone())}))
            },
        },
        // ----- validation -----
        Tool {
            name: "validate_project",
            description: "Pre-flight check of the whole project: missing samples, broken routing/sends, dead or out-of-range automation, illegal values (NaN, out-of-range volume/pan/params), notes outside patterns, empty or muted tracks, master chain without a limiter; plus (render:true, default) master QC like check_master. Returns pass/warn/fail items, each with the tool that fixes it.",
            mutates: false,
            schema: || obj(json!({"render": {"type": "boolean"}, "target_lufs": {"type": "number"}, "true_peak_ceiling": {"type": "number"}}), &[]),
            run: |e, a| {
                e.bank.sync(&e.project.samples);
                let mut items = validate::structural(&e.project, &e.bank);
                if b_or(a, "render", true) && !e.project.tracks.is_empty() {
                    let mix = e.mix()?;
                    let loud = analysis::loudness(&mix.left, &mix.right);
                    items.extend(validate::master(&mix, &loud, &targets_from(a)));
                }
                items.sort_by_key(|x| std::cmp::Reverse(x.status));
                Ok(validate::summarize(&items))
            },
        },
        Tool {
            name: "check_master",
            description: "Delivery QC of the rendered master: overs/clipping, true peak (4x oversampled) vs ceiling (default -1 dBTP), integrated K-weighted LUFS vs target (default -14 +/- 2), loudness range, DC offset, mono compatibility (correlation + fold-down loss) and silence. Returns pass/warn/fail items and the raw loudness numbers.",
            mutates: false,
            schema: || obj(json!({"target_lufs": {"type": "number"}, "lufs_tolerance": {"type": "number"}, "true_peak_ceiling": {"type": "number"}}), &[]),
            run: |e, a| {
                let mix = e.mix()?;
                let loud = analysis::loudness(&mix.left, &mix.right);
                let mut items = validate::master(&mix, &loud, &targets_from(a));
                items.sort_by_key(|x| std::cmp::Reverse(x.status));
                let mut out = validate::summarize(&items);
                out["loudness"] = serde_json::to_value(&loud)?;
                Ok(out)
            },
        },
    ]
}

fn section_map(p: &Project) -> Vec<Value> {
    let mut start = 0.0f32;
    p.song_sections()
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let len = p
                .pattern_index(&s.pattern)
                .map(|pi| (p.patterns[pi].steps() * s.repeats.max(1)) as f32 / 4.0)
                .unwrap_or(0.0);
            let v = json!({"index": i, "pattern": s.pattern, "start_beat": start, "end_beat": start + len});
            start += len;
            v
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::engine::Engine;
    use crate::render::{self, RenderOptions};
    use serde_json::json;

    fn eng() -> Engine {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_studio_tests"));
        e.call(
            "generate_beat",
            &json!({"style": "house", "bars": 2, "seed": 3}),
        )
        .unwrap();
        e
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn volume_automation_fades_the_render() {
        let mut e = eng();
        let first = e.project.tracks[0].name.clone();
        e.call(
            "generate_automation",
            &json!({"track": "master", "param": "volume", "shape": "fade_out"}),
        )
        .unwrap();
        let _ = first;
        let mix = e.mix().unwrap();
        let n = mix.left.len();
        let body = (e.project.song_seconds() * crate::dsp::SR) as usize;
        let head = rms(&mix.left[..body / 4]);
        let tail = rms(&mix.left[body * 3 / 4..body.min(n)]);
        assert!(tail < head * 0.5, "fade out: head {head} tail {tail}");
    }

    #[test]
    fn fx_and_instrument_automation_change_the_sound() {
        let mut e = eng();
        let t = "sweeplead".to_string();
        e.call("add_track", &json!({"name": t, "preset": "supersaw"}))
            .unwrap();
        let notes: Vec<_> = (0..8)
            .map(|i| json!({"start": i * 4, "len": 4, "pitch": "C5"}))
            .collect();
        e.call("add_notes", &json!({"track": t, "notes": notes}))
            .unwrap();
        e.call(
            "add_effect",
            &json!({"track": t, "type": "filter", "params": {"cutoff": 18000}}),
        )
        .unwrap();
        let idx = e.project.tracks[e.project.track_index(&t).unwrap()]
            .effects
            .len()
            - 1;
        let base = render::render(
            &e.project,
            &e.bank,
            &RenderOptions {
                keep_stems: true,
                ..Default::default()
            },
        )
        .unwrap();
        let param = format!("fx.{idx}.cutoff");
        e.call(
            "generate_automation",
            &json!({"track": t, "param": param, "shape": "sweep_up", "min": 150, "max": 18000}),
        )
        .unwrap();
        let swept = render::render(
            &e.project,
            &e.bank,
            &RenderOptions {
                keep_stems: true,
                ..Default::default()
            },
        )
        .unwrap();
        let si = base.stems.iter().position(|s| s.name == t).unwrap();
        let (a, b) = (&base.stems[si].left, &swept.stems[si].left);
        let q = a.len() / 4;
        assert!(
            rms(&b[..q]) < rms(&a[..q]) * 0.8,
            "low cutoff at the start darkens/quiets the track"
        );
        // instrument param automation
        e.call(
            "add_automation",
            &json!({"track": t, "param": "instrument.gain", "points": [[0, 0.0], [1000, 0.0]]}),
        )
        .unwrap();
        let silent = render::render(
            &e.project,
            &e.bank,
            &RenderOptions {
                keep_stems: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            rms(&silent.stems[si].left) < 1e-4,
            "instrument.gain=0 silences notes"
        );
        // invalid params are rejected with guidance
        let err = e
            .call(
                "add_automation",
                &json!({"track": t, "param": "fx.9.cutoff"}),
            )
            .unwrap_err();
        assert!(err.to_string().contains("no effect at index 9"));
        let err = e
            .call(
                "add_automation",
                &json!({"track": t, "param": "instrument.nope"}),
            )
            .unwrap_err();
        assert!(err.to_string().contains("Options"));
        let v = e
            .call("list_automation", &json!({"track": t, "at_beat": 0}))
            .unwrap();
        assert_eq!(v["lanes"].as_array().unwrap().len(), 2);
        assert!(v["automatable"].as_array().unwrap().len() > 5);
        e.call("clear_automation", &json!({"track": t})).unwrap();
        assert!(e.project.automation.is_empty());
    }

    #[test]
    fn removing_effects_reindexes_lanes() {
        let mut e = eng();
        let t = e.project.tracks[0].name.clone();
        let n0 = e.project.tracks[0].effects.len();
        e.call("add_effect", &json!({"track": t, "type": "gain"}))
            .unwrap();
        e.call("add_effect", &json!({"track": t, "type": "filter"}))
            .unwrap();
        e.call(
            "add_automation",
            &json!({"track": t, "param": format!("fx.{}.cutoff", n0 + 1), "points": [[0, 500]]}),
        )
        .unwrap();
        e.call("remove_effect", &json!({"track": t, "index": n0}))
            .unwrap();
        assert_eq!(e.project.automation[0].param, format!("fx.{n0}.cutoff"));
    }

    #[test]
    fn buses_sends_and_routing() {
        let mut e = eng();
        let dry = render::render(
            &e.project,
            &e.bank,
            &RenderOptions {
                keep_stems: true,
                ..Default::default()
            },
        )
        .unwrap();
        e.call("add_bus", &json!({"name": "verb", "preset": "reverb"}))
            .unwrap();
        e.call("add_bus", &json!({"name": "drums", "preset": "drum"}))
            .unwrap();
        let names: Vec<String> = e.project.tracks.iter().map(|t| t.name.clone()).collect();
        let drums: Vec<&String> = names
            .iter()
            .filter(|n| ["kick", "snare", "clap", "hat", "open_hat"].contains(&n.as_str()))
            .collect();
        assert!(!drums.is_empty());
        e.call("route_track", &json!({"track": drums, "bus": "drums"}))
            .unwrap();
        let lead = names.iter().find(|n| !drums.contains(n)).unwrap();
        e.call("set_send", &json!({"track": lead, "bus": "verb", "db": -6}))
            .unwrap();
        // a routed track can't also send to its own bus
        assert!(e
            .call("set_send", &json!({"track": drums[0], "bus": "drums"}))
            .is_err());
        let wet = render::render(
            &e.project,
            &e.bank,
            &RenderOptions {
                keep_stems: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(wet.bus_stems.len(), 2);
        let verb = &wet
            .bus_stems
            .iter()
            .find(|s| s.name == "verb")
            .unwrap()
            .left;
        assert!(rms(verb) > 1e-4, "reverb return receives the send");
        let drum_bus = &wet
            .bus_stems
            .iter()
            .find(|s| s.name == "drums")
            .unwrap()
            .left;
        assert!(rms(drum_bus) > 1e-3, "drum bus carries the drums");
        assert!(rms(&wet.left) > rms(&dry.left) * 0.5);
        // muting the drum bus removes the drums from the master
        e.call("set_mixer", &json!({"track": "drums", "mute": true}))
            .unwrap();
        let m = render::render(&e.project, &e.bank, &RenderOptions::default()).unwrap();
        assert!(rms(&m.left) < rms(&wet.left));
        let r = e.call("list_routing", &json!({})).unwrap();
        assert_eq!(
            r["buses"][1]["inputs"].as_array().unwrap().len(),
            drums.len()
        );
        e.call("remove_bus", &json!({"name": "drums"})).unwrap();
        assert!(e.project.tracks.iter().all(|t| t.output.is_none()));
        // old project files (no buses/sends/automation) still load
        let mut v = serde_json::to_value(&e.project).unwrap();
        v.as_object_mut().unwrap().remove("buses");
        for t in v["tracks"].as_array_mut().unwrap() {
            t.as_object_mut().unwrap().remove("sends");
        }
        let p: crate::project::Project = serde_json::from_value(v).unwrap();
        assert!(p.buses.is_empty());
    }

    #[test]
    fn snapshots_diff_and_compare() {
        let mut e = eng();
        e.call("snapshot", &json!({"name": "A", "note": "original"}))
            .unwrap();
        let t = e.project.tracks[1].name.clone();
        e.call("set_mixer", &json!({"track": t, "volume_db": -9}))
            .unwrap();
        e.call("set_tempo", &json!({"bpm": 128})).unwrap();
        e.call("snapshot", &json!({"name": "B"})).unwrap();
        let d = e
            .call("diff_project", &json!({"from": "A", "to": "B"}))
            .unwrap();
        let paths: Vec<String> = d["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["path"].as_str().unwrap().to_string())
            .collect();
        assert!(
            paths.contains(&format!("tracks.{t}.volume_db")),
            "{paths:?}"
        );
        assert!(paths.contains(&"bpm".to_string()));
        let c = e
            .call("compare_variants", &json!({"variants": ["A", "B"]}))
            .unwrap();
        assert_eq!(c["variants"].as_array().unwrap().len(), 2);
        assert!(c["variants"][0]["integrated_lufs"].as_f64().unwrap() > -40.0);
        assert!(c["winner"].is_string());
        e.call("restore_snapshot", &json!({"name": "A"})).unwrap();
        assert_eq!(e.project, e.version("A").unwrap());
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project.bpm, 128.0);
        let l = e.call("list_snapshots", &json!({})).unwrap();
        assert_eq!(l["snapshots"][1]["changes_vs_current"], 0);
        assert!(e.call("diff_project", &json!({"from": "nope"})).is_err());
    }

    #[test]
    fn validation_and_master_qc() {
        let mut e = eng();
        let v = e.call("validate_project", &json!({})).unwrap();
        assert!(v["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["check"] == "true_peak"));
        let m = e.call("check_master", &json!({})).unwrap();
        let tp = m["loudness"]["true_peak_dbtp"].as_f64().unwrap();
        assert!(tp < 0.5, "limiter keeps true peak near the ceiling: {tp}");
        assert!(m["loudness"]["integrated_lufs"].as_f64().unwrap() > -30.0);
        // break something
        e.project.tracks[0].output = Some("ghost".into());
        let v = e
            .call("validate_project", &json!({"render": false}))
            .unwrap();
        assert_eq!(v["passed"], false);
    }
}
