//! The ears' verdict and the auto-fix loop.
//!
//! `judge_audio` measures a delivered file (or the current mix) against
//! delivery targets a producer would check by ear and meter: integrated
//! loudness, true peak, the band balance where mixes usually go wrong (mud
//! 250-500 Hz, vocal presence 1-4 kHz, harshness 2-5 kHz, air 5-11 kHz, each
//! as a share of the whole), clipped runs and dropouts inside the music.
//! Every failed check names the fix.
//!
//! `auto_fix_mix` closes the loop: deliver, judge, move the EQ that owns each
//! failed check (a mud bell on the melodic parts and the vocal, a presence
//! bell on the vocal, a harshness bell and an air shelf on the master), deliver
//! again, keep the best round. One undo step; the project keeps the winner.

use crate::analysis;
use crate::engine::Engine;
use crate::groove_feel::role_of;
use crate::tools::{f_opt, obj, s_opt, u_or, Tool};
use crate::tools_delivery::{deliver, DecodedOutput, DeliverOpts};
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// Targets for a modern trap/pop master (dB shares are band energy minus
/// the whole signal's energy).
#[derive(Debug, Clone, Copy)]
pub struct JudgeTargets {
    pub lufs: (f32, f32),
    pub tp_max: f32,
    pub mud_max: f32,
    pub pres_min: f32,
    pub harsh_max: f32,
    pub air_min: f32,
}

impl Default for JudgeTargets {
    fn default() -> Self {
        JudgeTargets { lufs: (-11.0, -8.5), tp_max: -1.0, mud_max: -8.0, pres_min: -15.0, harsh_max: -12.0, air_min: -22.0 }
    }
}

/// What the meters read.
#[derive(Debug, Clone, Default)]
pub struct Reading {
    pub lufs: f32,
    pub true_peak: f32,
    pub mud: f32,
    pub presence: f32,
    pub harsh: f32,
    pub air: f32,
    pub clipped_runs: usize,
    pub dropouts: Vec<(f32, f32)>,
    pub seconds: f32,
}

fn energy_db(x: &[f32]) -> f32 {
    let ms = x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len().max(1) as f64;
    (10.0 * (ms + 1e-20).log10()) as f32
}

/// Measure stereo audio at `sr`.
pub fn read(l: &[f32], r: &[f32], sr: f32) -> Reading {
    let n = l.len().min(r.len());
    let mono: Vec<f32> = (0..n).map(|i| 0.5 * (l[i] + r[i])).collect();
    // band shares at the engine rate the filters are tuned for
    let m = if (sr - crate::dsp::SR).abs() > 1.0 { crate::samples::resample(&mono, sr, crate::dsp::SR) } else { mono.clone() };
    let all = energy_db(&m);
    let share = |lo: f32, hi: f32| crate::mix_rules::band_energy_db(&m, lo, hi) - all;
    let loud = analysis::loudness_at(&l[..n], &r[..n], sr);
    let tp = analysis::true_peak(&l[..n]).max(analysis::true_peak(&r[..n]));
    // clipped runs: 3+ consecutive samples at full scale
    let mut clipped_runs = 0;
    for ch in [&l[..n], &r[..n]] {
        let mut run = 0;
        for v in ch {
            if v.abs() >= 0.998 {
                run += 1;
                if run == 3 {
                    clipped_runs += 1;
                }
            } else {
                run = 0;
            }
        }
    }
    // dropouts: 0.4 s+ under -50 dBFS between the first and last sound
    let win = (sr * 0.05) as usize;
    let levels: Vec<f32> = mono.chunks(win.max(1)).map(energy_db).collect();
    let first = levels.iter().position(|d| *d > -50.0).unwrap_or(0);
    let last = levels.iter().rposition(|d| *d > -50.0).unwrap_or(0);
    let mut dropouts = Vec::new();
    let mut k = first;
    while k < last {
        if levels[k] <= -50.0 {
            let s = k;
            while k < last && levels[k] <= -50.0 {
                k += 1;
            }
            if (k - s) as f32 * 0.05 >= 0.4 {
                dropouts.push((s as f32 * 0.05, k as f32 * 0.05));
            }
        }
        k += 1;
    }
    Reading {
        lufs: loud.integrated_lufs,
        true_peak: crate::dsp::gain_to_db(tp),
        mud: share(250.0, 500.0),
        presence: share(1000.0, 4000.0),
        harsh: share(2000.0, 5000.0),
        air: share(5000.0, 11000.0),
        clipped_runs,
        dropouts,
        seconds: n as f32 / sr,
    }
}

fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

/// The checks, each with pass/fail and the fix; and a score (dB outside
/// the targets, summed: 0 = every target met).
pub fn verdict(x: &Reading, t: &JudgeTargets) -> (Vec<Value>, f32) {
    let mut checks = Vec::new();
    let mut score = 0.0f32;
    let mut add = |name: &str, value: f32, over: f32, target: String, fix: &str| {
        let pass = over <= 0.05;
        score += over.max(0.0);
        checks.push(json!({"check": name, "value": r1(value), "target": target, "pass": pass, "off_by_db": r1(over.max(0.0)), "fix": if pass { Value::Null } else { json!(fix) }}));
    };
    let lo = (t.lufs.0 - x.lufs).max(x.lufs - t.lufs.1);
    add("loudness", x.lufs, lo, format!("{}..{} LUFS", t.lufs.0, t.lufs.1), "export with target_lufs inside the range");
    add("true_peak", x.true_peak, x.true_peak - t.tp_max, format!("<= {} dBTP", t.tp_max), "lower the ceiling (true_peak_ceiling) or the target");
    add("mud_250_500", x.mud, x.mud - t.mud_max, format!("<= {} dB share", t.mud_max), "bell cut ~350 Hz on pads, keys, leads and the vocal");
    add("presence_1_4k", x.presence, t.pres_min - x.presence, format!(">= {} dB share", t.pres_min), "bell boost ~2.5 kHz on the vocal (or the lead)");
    add("harsh_2_5k", x.harsh, x.harsh - t.harsh_max, format!("<= {} dB share", t.harsh_max), "bell cut ~3.5 kHz on the master or the brightest part");
    add("air_5_11k", x.air, t.air_min - x.air, format!(">= {} dB share", t.air_min), "high shelf ~8 kHz on the master");
    add("clipping", x.clipped_runs as f32, x.clipped_runs as f32, "0 clipped runs".into(), "turn the mix down before the limiter");
    add("dropouts", x.dropouts.len() as f32, x.dropouts.len() as f32, "no silence >= 0.4 s inside the music".into(), "fill or re-render the gap");
    (checks, score)
}

fn reading_json(x: &Reading) -> Value {
    json!({"lufs": r1(x.lufs), "true_peak_dbtp": r1(x.true_peak), "mud_250_500_db": r1(x.mud), "presence_1_4k_db": r1(x.presence),
        "harsh_2_5k_db": r1(x.harsh), "air_5_11k_db": r1(x.air), "clipped_runs": x.clipped_runs,
        "dropouts_s": x.dropouts.iter().map(|(a, b)| json!([r1(*a), r1(*b)])).collect::<Vec<_>>(), "seconds": r1(x.seconds)})
}

fn targets_from(a: &Value) -> JudgeTargets {
    let mut t = JudgeTargets::default();
    if let Some(v) = f_opt(a, "target_lufs") {
        t.lufs = (v - 1.0, v + 1.0);
    }
    if let Some(v) = f_opt(a, "mud_max_db") {
        t.mud_max = v;
    }
    if let Some(v) = f_opt(a, "presence_min_db") {
        t.pres_min = v;
    }
    if let Some(v) = f_opt(a, "harsh_max_db") {
        t.harsh_max = v;
    }
    if let Some(v) = f_opt(a, "air_min_db") {
        t.air_min = v;
    }
    t
}

/// Who carries a band: each track stem's share of the mix's energy in
/// `lo..hi` (dB; 0 = all of it), loudest first.
pub fn stem_shares(e: &mut Engine, lo: f32, hi: f32) -> Result<Vec<(String, &'static str, f32)>> {
    let p = e.project.clone();
    let m = e.render_stems_all(&p)?;
    let mono = |l: &[f32], r: &[f32]| -> Vec<f32> { l.iter().zip(r).map(|(a, b)| 0.5 * (a + b)).collect() };
    let total = crate::mix_rules::band_energy_db(&mono(&m.left, &m.right), lo, hi);
    let mut v: Vec<(String, &'static str, f32)> = m
        .stems
        .iter()
        .filter(|s| !s.left.is_empty())
        .map(|s| (s.name.clone(), role_of(&s.name), crate::mix_rules::band_energy_db(&mono(&s.left, &s.right), lo, hi) - total))
        .collect();
    v.sort_by(|a, b| b.2.total_cmp(&a.2));
    Ok(v)
}

/// The tracks a mud cut belongs on: the melodic, bass and vocal stems that
/// together carry ~80% of the 250-500 Hz energy (drums keep their body).
pub fn mud_owners(shares: &[(String, &'static str, f32)]) -> Vec<String> {
    let mut acc = 0.0f32;
    let mut out = Vec::new();
    for (n, role, db) in shares {
        if matches!(*role, "kick" | "snare" | "hat" | "perc") {
            continue;
        }
        if acc >= 0.8 {
            break;
        }
        acc += 10f32.powf(db / 10.0);
        out.push(n.clone());
    }
    out
}

/// The EQ the loop owns, as cumulative gains.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FixEq {
    pub mud_db: f32,
    pub pres_db: f32,
    pub harsh_db: f32,
    pub air_db: f32,
}

impl FixEq {
    /// Move each knob toward its target by what the reading missed it by
    /// (a step of 1..3 dB per round, inside safe ranges).
    pub fn step(&self, x: &Reading, t: &JudgeTargets) -> FixEq {
        let mut f = *self;
        let mv = |over: f32, k: f32, lo: f32, hi: f32| if over > 0.05 { (over * k).clamp(lo, hi) } else { 0.0 };
        f.mud_db = (f.mud_db - mv(x.mud - t.mud_max, 0.8, 1.0, 3.0)).max(-9.0);
        f.pres_db = (f.pres_db + mv(t.pres_min - x.presence, 0.6, 1.0, 2.5)).min(6.0);
        f.harsh_db = (f.harsh_db - mv(x.harsh - t.harsh_max, 0.7, 1.0, 2.5)).max(-6.0);
        f.air_db = (f.air_db + mv(t.air_min - x.air, 0.6, 1.0, 2.0)).min(5.0);
        f
    }
}

/// Apply `f` to a fresh copy of the project: a mud bell on every melodic
/// part and the vocal, a presence bell on the vocal (the lead when there is
/// none), and a harshness bell + air shelf on the master.
pub fn apply_fix(e: &mut Engine, f: &FixEq, mud_tracks: Option<&[String]>) -> Result<Vec<String>> {
    let mut touched = Vec::new();
    let names: Vec<(String, &'static str)> = e.project.tracks.iter().map(|t| (t.name.clone(), role_of(&t.name))).collect();
    let has_vocal = names.iter().any(|(_, r)| *r == "vocal");
    for (n, role) in &names {
        let mut bands = Vec::new();
        // the mud cut goes where the stems say the mud is (the bass at half
        // depth: its 250-500 Hz is the 808's harmonics, the note's identity)
        let owns = match mud_tracks {
            Some(m) => m.iter().any(|x| x.eq_ignore_ascii_case(n)),
            None => matches!(*role, "keys" | "lead" | "vocal"),
        };
        if f.mud_db < -0.05 && owns {
            let g = if *role == "bass" { f.mud_db * 0.5 } else { f.mud_db };
            bands.push(json!({"kind": "bell", "freq": 350.0, "gain_db": g, "q": 0.9}));
        }
        let pres_here = if has_vocal { *role == "vocal" } else { *role == "lead" };
        if f.pres_db > 0.05 && pres_here {
            bands.push(json!({"kind": "bell", "freq": 2500.0, "gain_db": f.pres_db, "q": 0.8}));
        }
        if !bands.is_empty() {
            e.call_nested("add_effect", &json!({"track": n, "type": "parametric_eq", "params": {"bands": bands}}), "auto_fix_mix")?;
            touched.push(n.clone());
        }
    }
    let mut mb = Vec::new();
    if f.harsh_db < -0.05 {
        mb.push(json!({"kind": "bell", "freq": 3500.0, "gain_db": f.harsh_db, "q": 1.2}));
    }
    if f.air_db > 0.05 {
        mb.push(json!({"kind": "high_shelf", "freq": 8000.0, "gain_db": f.air_db, "q": 0.7}));
    }
    if !mb.is_empty() {
        e.call_nested("add_effect", &json!({"track": "master", "type": "parametric_eq", "params": {"bands": mb}}), "auto_fix_mix")?;
        touched.push("master".into());
    }
    Ok(touched)
}

fn judge_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let t = targets_from(a);
    let (x, what) = match s_opt(a, "path") {
        Some(p) => {
            let d = DecodedOutput::read(&e.resolve(&p))?;
            (read(&d.left, &d.right, d.sample_rate as f32), p)
        }
        None => {
            let m = e.mix()?;
            (read(&m.left, &m.right, crate::dsp::SR), "the current mix (pre-master: loudness and peak are the raw mix's)".into())
        }
    };
    let (checks, score) = verdict(&x, &t);
    let pass = checks.iter().all(|c| c["pass"] == true);
    let mut out = json!({"judged": what, "reading": reading_json(&x), "checks": checks, "score_db_off": r1(score), "pass": pass});
    // stem probe: which tracks carry each band, so a fix lands on its source
    if crate::tools::b_or(a, "stems", false) {
        let mut by = serde_json::Map::new();
        for (k, lo, hi) in [("mud_250_500", 250.0, 500.0), ("presence_1_4k", 1000.0, 4000.0), ("harsh_2_5k", 2000.0, 5000.0)] {
            let sh = stem_shares(e, lo, hi)?;
            by.insert(k.into(), json!(sh.iter().take(6).map(|(n, role, d)| json!({"track": n, "role": role, "share_db": r1(*d)})).collect::<Vec<_>>()));
        }
        out["by_stem"] = Value::Object(by);
        out["mud_owners"] = json!(mud_owners(&stem_shares(e, 250.0, 500.0)?));
    }
    Ok(out)
}

fn auto_fix_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let t = targets_from(a);
    let rounds = u_or(a, "rounds", 3).clamp(1, 6) as usize;
    let target = f_opt(a, "target_lufs").unwrap_or(-9.75);
    let out = match s_opt(a, "path") {
        Some(p) => e.resolve(&p),
        None => e.renders_dir().join(format!("{}_autofix.wav", crate::samples::sample_name(&e.project.name))),
    };
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d)?;
    }
    let ext = out.extension().and_then(|x| x.to_str()).unwrap_or("wav").to_ascii_lowercase();
    if !["wav", "mp3", "flac"].contains(&ext.as_str()) {
        bail!("path must end in .wav, .mp3 or .flac");
    }
    let base = e.project.clone();
    // stems first: the mud cut goes on the tracks that carry the mud
    let shares = stem_shares(e, 250.0, 500.0)?;
    let owners = mud_owners(&shares);
    let mut fix = FixEq::default();
    let mut log = Vec::new();
    let mut best: Option<(f32, FixEq, usize)> = None;
    for round in 0..=rounds {
        e.project = base.clone();
        let touched = apply_fix(e, &fix, Some(&owners))?;
        let m = e.mix()?;
        let tmp = out.with_file_name(format!(".autofix_round{round}.{ext}"));
        let o = DeliverOpts { format: ext.clone(), target_lufs: Some(target), ceiling_dbtp: t.tp_max - 0.2, ..Default::default() };
        deliver(&m.left, &m.right, &tmp, &o)?;
        let d = DecodedOutput::read(&tmp)?;
        let x = read(&d.left, &d.right, d.sample_rate as f32);
        let (checks, score) = verdict(&x, &t);
        let failed: Vec<Value> = checks.iter().filter(|c| c["pass"] == false).map(|c| c["check"].clone()).collect();
        log.push(json!({"round": round, "eq": {"mud_db": r1(fix.mud_db), "presence_db": r1(fix.pres_db), "harsh_db": r1(fix.harsh_db), "air_db": r1(fix.air_db)}, "touched": touched, "reading": reading_json(&x), "score_db_off": r1(score), "failed": failed}));
        if best.map(|b| score < b.0 - 0.05).unwrap_or(true) {
            best = Some((score, fix, round));
            std::fs::rename(&tmp, &out)?;
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
        if failed.is_empty() {
            break;
        }
        let next = fix.step(&x, &t);
        if next == fix {
            break;
        }
        fix = next;
    }
    let (score, f, round) = best.expect("at least one round");
    e.project = base;
    apply_fix(e, &f, Some(&owners))?;
    Ok(json!({"path": out, "mud_by_stem_db": shares.iter().take(8).map(|(n, _, d)| json!([n, r1(*d)])).collect::<Vec<_>>(), "mud_owners": owners, "best_round": round, "score_db_off": r1(score), "kept_eq": {"mud_db": r1(f.mud_db), "presence_db": r1(f.pres_db), "harsh_db": r1(f.harsh_db), "air_db": r1(f.air_db)}, "rounds": log,
        "note": "the project keeps the best round's EQ (one undo step); score 0 = every target met. Metrics only: listen before trusting it."}))
}

fn judge_props() -> Value {
    json!({
        "target_lufs": {"type": "number", "description": "loudness target (the pass range is +-1 LU around it; default -11..-8.5)"},
        "mud_max_db": {"type": "number", "description": "250-500 Hz share ceiling (default -8 dB)"},
        "presence_min_db": {"type": "number", "description": "1-4 kHz share floor (default -15 dB)"},
        "harsh_max_db": {"type": "number", "description": "2-5 kHz share ceiling (default -12 dB)"},
        "air_min_db": {"type": "number", "description": "5-11 kHz share floor (default -22 dB)"},
    })
}

/// Saved master-lift presets live here, one JSON profile per name.
fn lift_dir(e: &Engine) -> std::path::PathBuf {
    e.workdir.join("mix_profiles")
}

fn lift_names(e: &Engine) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(lift_dir(e))
        .map(|d| d.filter_map(|x| x.ok()).filter_map(|x| x.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(String::from)).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn lift_name_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= 40 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn master_lift_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    use crate::mix_rules::{current_master_lift, set_master_lift, MixProfile};
    let mut p = match s_opt(a, "preset").as_deref() {
        Some("default") => MixProfile::default(),
        Some("off") => MixProfile { presence_db: 0.0, air_db: 0.0, ..MixProfile::default() },
        Some(n) => {
            if !lift_name_ok(n) {
                bail!("preset names are letters, digits, _ and - (got '{n}')");
            }
            let f = lift_dir(e).join(format!("{n}.json"));
            let txt = std::fs::read_to_string(&f).map_err(|_| anyhow::anyhow!("no preset '{n}' (saved: {:?}; built in: default, off)", lift_names(e)))?;
            serde_json::from_str(&txt)?
        }
        None => current_master_lift(&e.project).unwrap_or_default(),
    };
    for (k, slot) in [("presence_hz", &mut p.presence_hz), ("presence_db", &mut p.presence_db), ("presence_q", &mut p.presence_q), ("air_hz", &mut p.air_hz), ("air_db", &mut p.air_db)] {
        if let Some(x) = f_opt(a, k) {
            *slot = x;
        }
    }
    let bad = p.problems();
    if !bad.is_empty() {
        bail!("{}", bad.iter().map(|(k, w)| format!("{k}: {w}")).collect::<Vec<_>>().join("; "));
    }
    let mut saved = Value::Null;
    if let Some(n) = s_opt(a, "save_as") {
        if !lift_name_ok(&n) || n == "default" || n == "off" {
            bail!("save_as: letters, digits, _ and -, and not 'default' or 'off' (got '{n}')");
        }
        std::fs::create_dir_all(lift_dir(e))?;
        std::fs::write(lift_dir(e).join(format!("{n}.json")), serde_json::to_string_pretty(&p)?)?;
        saved = json!(n);
    }
    let apply = a.get("apply").and_then(|v| v.as_bool()).unwrap_or(true);
    if apply {
        set_master_lift(e, &p)?;
        e.project.ensure_fx_ids();
        // a song keeps it on every re-mix (song_patch reads /mix/master_lift)
        if let Some(s) = e.project.song.as_mut() {
            if s.spec.is_object() {
                if !s.spec["mix"].is_object() {
                    s.spec["mix"] = json!({});
                }
                s.spec["mix"]["master_lift"] = serde_json::to_value(&p)?;
            }
        }
    }
    let on = p.presence_db.abs() >= 0.05 || p.air_db.abs() >= 0.05;
    Ok(json!({"profile": p, "on": on, "applied": apply, "saved_as": saved, "presets": lift_names(e), "built_in": ["default", "off"]}))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "judge_audio",
            description: "The ears' verdict on a file (path) or the current mix: integrated LUFS, true peak, band shares (mud 250-500 Hz, vocal presence 1-4 kHz, harshness 2-5 kHz, air 5-11 kHz; band energy minus the whole), clipped runs and dropouts inside the music. Each check has pass/fail, how far off in dB and the fix; score_db_off sums the misses (0 = all targets met). Targets default to a modern trap/pop master and can be set.",
            mutates: false,
            schema: || {
                let mut p = judge_props();
                p["path"] = json!({"type": "string", "description": "an audio file (wav/mp3/flac); omit to judge the current mix"});
                p["stems"] = json!({"type": "boolean", "description": "also probe the project's stems: which tracks carry the mud, presence and harsh bands (fixes belong on the source)"});
                obj(p, &[])
            },
            run: judge_tool,
        },
        Tool {
            name: "auto_fix_mix",
            description: "Closed loop: deliver the song, judge_audio it, move the EQ that owns each failed check (mud bell ~350 Hz on keys/pads/leads/vocal, presence bell ~2.5 kHz on the vocal, harshness bell ~3.5 kHz and air shelf ~8 kHz on the master), deliver again, up to `rounds` (default 3). Keeps the best round's file at path and its EQ in the project (one undo step). Returns every round's reading.",
            mutates: true,
            schema: || {
                let mut p = judge_props();
                p["path"] = json!({"type": "string", "description": "output file (.wav/.mp3/.flac; default renders/<project>_autofix.wav)"});
                p["rounds"] = json!({"type": "integer", "minimum": 1, "maximum": 6});
                obj(p, &[])
            },
            run: auto_fix_tool,
        },
        Tool {
            name: "master_lift",
            description: "The master's tone lift (a MixProfile: presence bell presence_hz/presence_db/presence_q + air shelf air_hz/air_db, last on the master chain; every song render puts the default on). Read it, change any field, apply a preset (built in: default, off; or one saved with save_as), save the current one as a named preset. A song keeps it on re-mix (/mix/master_lift). One undo step.",
            mutates: true,
            schema: || obj(json!({
                "preset": {"type": "string", "description": "start from: default | off | a saved name (omit = the lift on the project now)"},
                "presence_hz": {"type": "number", "description": "500..5000 (default 2500)"},
                "presence_db": {"type": "number", "description": "-6..6 (default 3.5)"},
                "presence_q": {"type": "number", "description": "0.3..3 (default 0.6)"},
                "air_hz": {"type": "number", "description": "high shelf corner, 4000..16000 (default 8000)"},
                "air_db": {"type": "number", "description": "-6..6 (default 3)"},
                "save_as": {"type": "string", "description": "also save the result as a preset (letters, digits, _ -)"},
                "apply": {"type": "boolean", "description": "put it on the master (default true; false = only save/read)"}
            }), &[]),
            run: master_lift_tool,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f32, secs: f32, amp: f32) -> Vec<f32> {
        let sr = crate::dsp::SR;
        (0..(secs * sr) as usize).map(|i| amp * (i as f32 * f * std::f32::consts::TAU / sr).sin()).collect()
    }

    #[test]
    fn a_muddy_signal_fails_mud_and_presence_and_names_fixes() {
        let x = tone(350.0, 3.0, 0.3);
        let r = read(&x, &x, crate::dsp::SR);
        // a band-pass share of a pure in-band tone reads a few dB under 0 (filter skirts); what matters is it dwarfs the other bands
        assert!(r.mud > -6.0 && r.mud > r.presence + 20.0, "{r:?}");
        let (checks, score) = verdict(&r, &JudgeTargets::default());
        let get = |n: &str| checks.iter().find(|c| c["check"] == n).unwrap().clone();
        assert_eq!(get("mud_250_500")["pass"], false);
        assert_eq!(get("presence_1_4k")["pass"], false);
        assert!(get("mud_250_500")["fix"].as_str().unwrap().contains("350"));
        assert!(score > 5.0);
    }

    #[test]
    fn dropouts_and_clipping_are_found() {
        let mut x = tone(1000.0, 3.0, 1.2);
        let sr = crate::dsp::SR as usize;
        for v in &mut x[sr..sr * 2] {
            *v = 0.0;
        }
        for v in &mut x {
            *v = v.clamp(-1.0, 1.0);
        }
        let r = read(&x, &x, crate::dsp::SR);
        assert_eq!(r.dropouts.len(), 1, "{:?}", r.dropouts);
        assert!(r.clipped_runs > 10);
    }

    #[test]
    fn mud_owners_skip_drums_and_stop_at_most_of_the_energy() {
        let sh = vec![("kick".to_string(), "kick", -1.0), ("vocal".to_string(), "vocal", -2.0), ("chords".to_string(), "keys", -5.0), ("808".to_string(), "bass", -9.0), ("lead".to_string(), "lead", -15.0)];
        let o = mud_owners(&sh);
        assert_eq!(o, vec!["vocal".to_string(), "chords".to_string()], "{o:?}");
    }

    #[test]
    fn the_fix_moves_toward_the_targets_and_stays_in_range() {
        let t = JudgeTargets::default();
        let x = Reading { mud: -3.0, presence: -19.0, harsh: -25.0, air: -25.0, lufs: -10.0, true_peak: -1.5, ..Default::default() };
        let mut f = FixEq::default();
        for _ in 0..10 {
            f = f.step(&x, &t);
        }
        assert!(f.mud_db <= -3.0 && f.mud_db >= -9.0, "{f:?}");
        assert!(f.pres_db >= 2.0 && f.pres_db <= 6.0, "{f:?}");
        assert_eq!(f.harsh_db, 0.0, "not harsh: no cut");
        assert!(f.air_db > 0.0 && f.air_db <= 5.0);
    }

    #[test]
    fn master_lift_reads_sets_saves_and_applies_presets() {
        use crate::mix_rules::{current_master_lift, MixProfile};
        let d = std::env::temp_dir().join(format!("lift_tool_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        assert!(current_master_lift(&e.project).is_none());
        let r = e.call("master_lift", &json!({"presence_db": 2.0, "save_as": "soft"})).unwrap();
        assert_eq!(r["saved_as"], json!("soft"));
        let cur = current_master_lift(&e.project).unwrap();
        assert_eq!(cur, MixProfile { presence_db: 2.0, ..MixProfile::default() });
        e.call("master_lift", &json!({"preset": "off"})).unwrap();
        assert!(current_master_lift(&e.project).is_none());
        e.call("master_lift", &json!({"preset": "soft", "air_db": 1.0})).unwrap();
        assert_eq!(current_master_lift(&e.project).unwrap(), MixProfile { presence_db: 2.0, air_db: 1.0, ..MixProfile::default() });
        assert!(e.call("master_lift", &json!({"presence_db": 12.0})).is_err());
        assert!(e.call("master_lift", &json!({"preset": "nope"})).is_err());
        assert!(e.call("master_lift", &json!({"save_as": "../x"})).is_err());
        let r = e.call("master_lift", &json!({"apply": false})).unwrap();
        assert_eq!(r["presets"], json!(["soft"]));
        assert_eq!(e.project.master_effects.iter().filter(|f| f.id() == crate::mix_rules::MASTER_LIFT_ID).count(), 1);
    }
}
