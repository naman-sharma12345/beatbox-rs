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
use serde::{Deserialize, Serialize};
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
        Some("off") => MixProfile::off(),
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
    for (k, slot) in [("presence_hz", &mut p.presence_hz), ("presence_db", &mut p.presence_db), ("presence_q", &mut p.presence_q), ("air_hz", &mut p.air_hz), ("air_db", &mut p.air_db), ("extra_hz", &mut p.extra_hz), ("extra_db", &mut p.extra_db), ("extra_q", &mut p.extra_q)] {
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
    let on = p.is_on();
    Ok(json!({"profile": p, "on": on, "applied": apply, "saved_as": saved, "presets": lift_names(e), "built_in": ["default", "off"]}))
}

// ---------- the auto judge: every render judged against the gates ----------

/// Tools whose result names a rendered file (`audio`, `path` or `file`)
/// that the auto judge listens to when it is on.
pub const AUTO_JUDGE_TOOLS: &[&str] = &["render", "export_audio", "song_create", "song_render", "song_patch"];

/// A gate profile (Naman, 2026-10-13 8:22). `streaming` is the default;
/// `balanced` holds B34's gates (the ones D1-D4 were judged by); `custom`
/// is what the profile becomes once a gate is set by hand.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GateProfile {
    #[default]
    Streaming,
    ClubTrap,
    Dnb,
    Balanced,
    Custom,
}

/// The profiles a request can name.
pub const GATE_PROFILES: &[&str] = &["streaming", "club_trap", "dnb", "balanced"];

impl GateProfile {
    pub fn parse(s: &str) -> Option<GateProfile> {
        serde_json::from_value(json!(s)).ok().filter(|p| *p != GateProfile::Custom)
    }
    pub fn name(self) -> &'static str {
        match self {
            GateProfile::Streaming => "streaming",
            GateProfile::ClubTrap => "club_trap",
            GateProfile::Dnb => "dnb",
            GateProfile::Balanced => "balanced",
            GateProfile::Custom => "custom",
        }
    }
}

/// The auto judge's switch and gates (kept in `<workdir>/auto_judge.json`,
/// so it holds across CLI calls, MCP sessions and projects in that
/// workdir). Off by default, gates from the streaming profile.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AutoJudge {
    pub enabled: bool,
    pub profile: GateProfile,
    pub lufs_min: f32,
    pub lufs_max: f32,
    pub tp_max: f32,
    pub mud_max: f32,
    pub presence_min: f32,
    pub harsh_max: f32,
    pub air_min: f32,
}

impl Default for AutoJudge {
    fn default() -> Self {
        AutoJudge::profile(GateProfile::Streaming)
    }
}

impl AutoJudge {
    /// A profile's gates (off; switch it on with enabled). Band gates are
    /// judge_audio's (mud <= -8, presence >= -15, harshness <= -12,
    /// air >= -22 dB shares) unless the profile says otherwise:
    /// - streaming: -15..-13 LUFS (aim -14), true peak <= -1.0 dBTP
    /// - club_trap: -9..-8 LUFS, true peak <= -0.1 dBTP
    /// - dnb: -9..-7 LUFS, true peak <= -0.3 dBTP
    /// - balanced: B34's gates: -11..-8.5 LUFS, true peak <= -1.9 dBTP,
    ///   harshness <= -20
    pub fn profile(p: GateProfile) -> AutoJudge {
        let t = JudgeTargets::default();
        let base = AutoJudge { enabled: false, profile: p, lufs_min: t.lufs.0, lufs_max: t.lufs.1, tp_max: t.tp_max, mud_max: t.mud_max, presence_min: t.pres_min, harsh_max: t.harsh_max, air_min: t.air_min };
        match p {
            GateProfile::Streaming => AutoJudge { lufs_min: -15.0, lufs_max: -13.0, tp_max: -1.0, ..base },
            GateProfile::ClubTrap => AutoJudge { lufs_min: -9.0, lufs_max: -8.0, tp_max: -0.1, ..base },
            GateProfile::Dnb => AutoJudge { lufs_min: -9.0, lufs_max: -7.0, tp_max: -0.3, ..base },
            GateProfile::Balanced => AutoJudge { tp_max: -1.9, harsh_max: -20.0, ..base },
            GateProfile::Custom => AutoJudge { profile: GateProfile::Custom, ..base },
        }
    }
    pub fn targets(&self) -> JudgeTargets {
        JudgeTargets { lufs: (self.lufs_min, self.lufs_max), tp_max: self.tp_max, mud_max: self.mud_max, pres_min: self.presence_min, harsh_max: self.harsh_max, air_min: self.air_min }
    }
    /// What is wrong with the gates; empty = fine.
    pub fn problems(&self) -> Vec<String> {
        let mut v = Vec::new();
        for (k, x) in [("lufs_min", self.lufs_min), ("lufs_max", self.lufs_max), ("tp_max", self.tp_max), ("mud_max", self.mud_max), ("presence_min", self.presence_min), ("harsh_max", self.harsh_max), ("air_min", self.air_min)] {
            if !x.is_finite() || !(-60.0..=6.0).contains(&x) {
                v.push(format!("{k}: {x} is outside -60..6"));
            }
        }
        if self.lufs_min >= self.lufs_max {
            v.push(format!("lufs_min {} must be below lufs_max {}", self.lufs_min, self.lufs_max));
        }
        v
    }
}

/// A stable error payload: a caller branches on `error_code`; `message`
/// is for people. Codes: unknown_profile, invalid_gates, unreadable_file,
/// write_failed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct JudgeError {
    pub error_code: String,
    pub message: String,
}

fn jerr(code: &str, message: impl Into<String>) -> JudgeError {
    JudgeError { error_code: code.to_string(), message: message.into() }
}

fn auto_judge_path(e: &Engine) -> std::path::PathBuf {
    e.workdir.join("auto_judge.json")
}

/// The auto judge's settings in this workdir (default: off, streaming).
pub fn auto_judge_config(e: &Engine) -> AutoJudge {
    std::fs::read_to_string(auto_judge_path(e)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// The rendered file a tool result names, if any.
pub fn rendered_file(v: &Value) -> Option<String> {
    ["audio", "path", "file"].iter().find_map(|k| {
        let s = v.get(*k)?.as_str()?;
        let l = s.to_lowercase();
        (l.ends_with(".wav") || l.ends_with(".mp3") || l.ends_with(".flac")).then(|| s.to_string())
    })
}

/// One gate's verdict. `delta_db` is how far past its limit the value is
/// (positive = over the limit, i.e. failing; negative = headroom), in the
/// gate's unit (dB, LUFS, dBTP, or a count for clipped_runs / dropouts).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GateCheck {
    pub name: String,
    pub value: f32,
    pub limit: f32,
    pub pass: bool,
    pub delta_db: f32,
}

/// One band's share of the whole (band energy minus total energy, dB) and
/// the gain to apply to that band to pass its gate (+ boost, - cut; 0 when
/// it passes), sized so the share moves past the limit (a band is part of
/// the total, so a gain of g moves its share by about g x (1 - its
/// fraction of the energy)).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BandReading {
    pub share_db: f32,
    pub target_band_gain_db: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BandReadings {
    /// 250-500 Hz
    pub mud: BandReading,
    /// 1-4 kHz
    pub presence: BandReading,
    /// 2-5 kHz
    pub harshness: BandReading,
    /// 5-11 kHz
    pub air: BandReading,
}

/// The auto judge's strict, structured verdict on one file (the
/// `auto_judge` field of a render's result, and `judged` of the tool):
/// a caller acts on `pass`, `checks` and the corrective gains without
/// parsing any text.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct JudgeVerdict {
    pub file: String,
    pub profile: GateProfile,
    pub pass: bool,
    pub integrated_lufs: f32,
    pub true_peak_dbtp: f32,
    /// master gain to apply (+ = louder): brings loudness into the window
    /// without pushing true peak past its gate; negative when true peak
    /// fails; 0 when both pass
    pub corrective_gain_db: f32,
    pub crest_factor_db: f32,
    pub bands: BandReadings,
    /// one entry per gate, in a fixed order
    pub checks: Vec<GateCheck>,
    /// the names of the failed gates (empty when pass)
    pub failed: Vec<String>,
    /// sum of the failing deltas (dB off target, as judge_audio's score)
    pub score_db_off: f32,
    pub seconds: f32,
}

fn r2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}

/// Round a correction away from zero to 0.1 dB (so applying it clears the
/// gate), ignoring float dust.
fn up1(x: f32) -> f32 {
    let m = ((x.abs() - 1e-3).max(0.0) * 10.0).ceil() / 10.0;
    m.copysign(x)
}

/// Peak-to-RMS ratio of a stereo signal, dB (both channels together).
pub fn crest_factor_db(l: &[f32], r: &[f32]) -> f32 {
    let n = l.len().min(r.len());
    if n == 0 {
        return 0.0;
    }
    let (mut peak, mut sum) = (0.0f32, 0.0f64);
    for i in 0..n {
        for x in [l[i], r[i]] {
            peak = peak.max(x.abs());
            sum += (x as f64) * (x as f64);
        }
    }
    let rms = (sum / (2 * n) as f64).sqrt() as f32;
    if rms <= 1e-9 || peak <= 1e-9 {
        return 0.0;
    }
    20.0 * (peak / rms).log10()
}

/// The master gain that fixes loudness / true peak (0 when both pass).
pub fn corrective_gain_db(x: &Reading, g: &AutoJudge) -> f32 {
    let tp_room = g.tp_max - x.true_peak; // + = headroom
    if tp_room < -0.05 {
        return up1(tp_room);
    }
    if x.lufs < g.lufs_min - 0.05 || x.lufs > g.lufs_max + 0.05 {
        let want = 0.5 * (g.lufs_min + g.lufs_max) - x.lufs;
        let gain = if want > 0.0 { want.min(tp_room.max(0.0)) } else { want };
        return (gain * 10.0).round() / 10.0;
    }
    0.0
}

/// A band's reading and its corrective gain against a min or max gate.
pub fn band_reading(share: f32, limit: f32, is_max: bool) -> BandReading {
    let over = if is_max { share - limit } else { limit - share };
    let gain = if over > 0.05 {
        let frac = 10f32.powf(share / 10.0).clamp(0.0, 0.9);
        let trim = over / (1.0 - frac);
        up1(if is_max { -trim } else { trim })
    } else {
        0.0
    };
    BandReading { share_db: r2(share), target_band_gain_db: gain }
}

/// The verdict on a reading against the gates.
pub fn build_verdict(file: &str, x: &Reading, crest_db: f32, g: &AutoJudge) -> JudgeVerdict {
    // (name, value, limit, is a maximum)
    let gates = [
        ("loudness_min", x.lufs, g.lufs_min, false),
        ("loudness_max", x.lufs, g.lufs_max, true),
        ("true_peak_max", x.true_peak, g.tp_max, true),
        ("mud_max", x.mud, g.mud_max, true),
        ("presence_min", x.presence, g.presence_min, false),
        ("harshness_max", x.harsh, g.harsh_max, true),
        ("air_min", x.air, g.air_min, false),
        ("clipped_runs_max", x.clipped_runs as f32, 0.0, true),
        ("dropouts_max", x.dropouts.len() as f32, 0.0, true),
    ];
    let checks: Vec<GateCheck> = gates
        .iter()
        .map(|(name, value, limit, is_max)| {
            let delta = if *is_max { value - limit } else { limit - value };
            // the same tolerance judge_audio uses (meters read to 0.1 dB)
            GateCheck { name: name.to_string(), value: r2(*value), limit: *limit, pass: delta <= 0.05, delta_db: r2(delta) }
        })
        .collect();
    let failed: Vec<String> = checks.iter().filter(|c| !c.pass).map(|c| c.name.clone()).collect();
    let score: f32 = checks.iter().filter(|c| !c.pass).map(|c| c.delta_db.max(0.0)).sum();
    JudgeVerdict {
        file: file.to_string(),
        profile: g.profile,
        pass: failed.is_empty(),
        integrated_lufs: r2(x.lufs),
        true_peak_dbtp: r2(x.true_peak),
        corrective_gain_db: corrective_gain_db(x, g),
        crest_factor_db: r2(crest_db),
        bands: BandReadings {
            mud: band_reading(x.mud, g.mud_max, true),
            presence: band_reading(x.presence, g.presence_min, false),
            harshness: band_reading(x.harsh, g.harsh_max, true),
            air: band_reading(x.air, g.air_min, false),
        },
        checks,
        failed,
        score_db_off: r2(score),
        seconds: r2(x.seconds),
    }
}

/// Judge one file against the gates.
pub fn judge_file(e: &Engine, path: &str, g: &AutoJudge) -> std::result::Result<JudgeVerdict, JudgeError> {
    let d = DecodedOutput::read(&e.resolve(path)).map_err(|err| jerr("unreadable_file", format!("{path}: {err}")))?;
    let x = read(&d.left, &d.right, d.sample_rate as f32);
    Ok(build_verdict(path, &x, crest_factor_db(&d.left, &d.right), g))
}

/// Called by the engine after a top-level tool call succeeds: when the
/// auto judge is on and `tool` rendered a file, judge it and put the
/// verdict on the result as `auto_judge` (a `JudgeVerdict`; a judge error
/// goes to `auto_judge_error` as a `JudgeError` instead, and the render
/// itself still stands).
pub fn auto_judge_after(e: &Engine, tool: &str, v: &mut Value) {
    if !AUTO_JUDGE_TOOLS.contains(&tool) {
        return;
    }
    let cfg = auto_judge_config(e);
    if !cfg.enabled {
        return;
    }
    let Some(f) = rendered_file(v) else {
        return;
    };
    let (key, out) = match judge_file(e, &f, &cfg) {
        Ok(j) => ("auto_judge", serde_json::to_value(j)),
        Err(err) => ("auto_judge_error", serde_json::to_value(err)),
    };
    if let (Value::Object(m), Ok(out)) = (v, out) {
        m.insert(key.into(), out);
    }
}

/// The tool's answer: {config, profiles, judges_after, judged?} or, on any
/// failure, {error: {error_code, message}} (nothing is changed then).
fn auto_judge_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    Ok(match auto_judge_apply(e, a) {
        Ok(v) => v,
        Err(err) => json!({"error": err}),
    })
}

fn auto_judge_apply(e: &mut Engine, a: &Value) -> std::result::Result<Value, JudgeError> {
    let mut c = if crate::tools::b_or(a, "reset", false) { AutoJudge { enabled: auto_judge_config(e).enabled, ..AutoJudge::default() } } else { auto_judge_config(e) };
    if let Some(name) = s_opt(a, "profile") {
        let p = GateProfile::parse(&name).ok_or_else(|| jerr("unknown_profile", format!("unknown profile '{name}' (profiles: {})", GATE_PROFILES.join(", "))))?;
        c = AutoJudge { enabled: c.enabled, ..AutoJudge::profile(p) };
    }
    if let Some(b) = a.get("enabled").and_then(|v| v.as_bool()) {
        c.enabled = b;
    }
    let mut custom = false;
    for (k, slot) in [("lufs_min", &mut c.lufs_min), ("lufs_max", &mut c.lufs_max), ("tp_max", &mut c.tp_max), ("mud_max", &mut c.mud_max), ("presence_min", &mut c.presence_min), ("harsh_max", &mut c.harsh_max), ("air_min", &mut c.air_min)] {
        if let Some(x) = f_opt(a, k) {
            *slot = x;
            custom = true;
        }
    }
    if custom {
        c.profile = GateProfile::Custom;
    }
    let bad = c.problems();
    if !bad.is_empty() {
        return Err(jerr("invalid_gates", bad.join("; ")));
    }
    // judge first (read-only), then save: a bad path changes nothing
    let judged = match s_opt(a, "path") {
        Some(p) => Some(judge_file(e, &p, &c)?),
        None => None,
    };
    let text = serde_json::to_string_pretty(&c).map_err(|err| jerr("write_failed", err.to_string()))?;
    std::fs::create_dir_all(&e.workdir).and_then(|_| std::fs::write(auto_judge_path(e), text)).map_err(|err| jerr("write_failed", err.to_string()))?;
    let mut out = json!({"config": c, "profiles": GATE_PROFILES, "judges_after": AUTO_JUDGE_TOOLS});
    if let Some(j) = judged {
        out["judged"] = serde_json::to_value(j).unwrap_or(Value::Null);
    }
    Ok(out)
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
            name: "auto_judge",
            description: "Autonomous judging: switch it on and every top-level render (render, export_audio, song_create, song_render, song_patch) comes back with an auto_judge field, strict JSON: {file, profile, pass, integrated_lufs, true_peak_dbtp, corrective_gain_db (master gain to apply, + louder), crest_factor_db, bands {mud, presence, harshness, air: {share_db, target_band_gain_db (+ boost / - cut to pass, 0 when it passes)}}, checks [{name, value, limit, pass, delta_db (>0 = past the limit)}], failed [gate names], score_db_off, seconds} (auto_judge_error {error_code, message} instead if the file can't be read). Pick a gate profile: streaming (default), club_trap, dnb, balanced (B34's gates), or set gates by hand (profile becomes custom). Settings persist in the workdir. Give path to judge one file now. Errors come back as {error: {error_code, message}}.",
            mutates: false,
            schema: || obj(json!({
                "enabled": {"type": "boolean", "description": "judge every render automatically"},
                "profile": {"type": "string", "enum": ["streaming", "club_trap", "dnb", "balanced"], "description": "gate profile, overriding every limit: streaming (-15..-13 LUFS, TP <= -1.0; the default), club_trap (-9..-8 LUFS, TP <= -0.1), dnb (-9..-7 LUFS, TP <= -0.3), balanced (-11..-8.5 LUFS, TP <= -1.9, harshness <= -20: B34's gates). Gates given in the same call override it (profile becomes custom)"},
                "lufs_min": {"type": "number"}, "lufs_max": {"type": "number"},
                "tp_max": {"type": "number", "description": "true-peak ceiling, dBTP"},
                "mud_max": {"type": "number", "description": "250-500 Hz share max, dB (default -8)"},
                "presence_min": {"type": "number", "description": "1-4 kHz share min, dB (default -15)"},
                "harsh_max": {"type": "number", "description": "2-5 kHz share max, dB (default -12; balanced -20)"},
                "air_min": {"type": "number", "description": "5-11 kHz share min, dB (default -22)"},
                "reset": {"type": "boolean", "description": "gates back to the streaming default (the switch is kept)"},
                "path": {"type": "string", "description": "also judge this file now against the gates"}
            }), &[]),
            run: auto_judge_tool,
        },
        Tool {
            name: "master_lift",
            description: "The master's tone lift (a MixProfile: presence bell presence_hz/presence_db/presence_q + air shelf air_hz/air_db + an optional extra bell nudge extra_hz/extra_db/extra_q, last on the master chain; every song render puts the default on). Read it, change any field, apply a preset (built in: default, off; or one saved with save_as), save the current one as a named preset. A song keeps it on re-mix (/mix/master_lift). One undo step.",
            mutates: true,
            schema: || obj(json!({
                "preset": {"type": "string", "description": "start from: default | off | a saved name (omit = the lift on the project now)"},
                "presence_hz": {"type": "number", "description": "500..5000 (default 2500)"},
                "presence_db": {"type": "number", "description": "-6..6 (default 5.5)"},
                "presence_q": {"type": "number", "description": "0.3..3 (default 0.6)"},
                "air_hz": {"type": "number", "description": "high shelf corner, 4000..16000 (default 8000)"},
                "air_db": {"type": "number", "description": "-6..6 (default 3.5)"},
                "extra_hz": {"type": "number", "description": "extra bell nudge centre, 500..8000 (default 3500)"},
                "extra_db": {"type": "number", "description": "extra bell nudge gain, -3..3 (default 0 = off; e.g. +1 dB at 3.5 kHz for more presence)"},
                "extra_q": {"type": "number", "description": "extra bell width, 0.3..4 (default 1)"},
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

    fn d4_reading() -> Reading {
        Reading { lufs: -10.2, true_peak: -1.9, mud: -8.3, presence: -14.7, harsh: -20.2, air: -22.1, clipped_runs: 0, dropouts: vec![], seconds: 92.0 }
    }

    #[test]
    fn the_verdict_schema_is_strict_and_round_trips() {
        let x = d4_reading();
        let g = AutoJudge { enabled: true, ..AutoJudge::profile(GateProfile::Balanced) };
        assert_eq!((g.tp_max, g.harsh_max, g.presence_min, g.air_min, g.mud_max, g.lufs_min, g.lufs_max), (-1.9, -20.0, -15.0, -22.0, -8.0, -11.0, -8.5));
        let v = build_verdict("d4.mp3", &x, 9.25, &g);
        assert_eq!(v.profile, GateProfile::Balanced);
        // D4's real numbers under B34's gates: only air fails (0.1 under -22)
        assert!(!v.pass);
        assert_eq!(v.failed, vec!["air_min".to_string()]);
        let air = v.checks.iter().find(|c| c.name == "air_min").unwrap();
        assert_eq!((air.value, air.limit, air.pass, air.delta_db), (-22.1, -22.0, false, 0.1));
        let tp = v.checks.iter().find(|c| c.name == "true_peak_max").unwrap();
        assert!(tp.pass && tp.delta_db == 0.0);
        let harsh = v.checks.iter().find(|c| c.name == "harshness_max").unwrap();
        assert!(harsh.pass && harsh.delta_db < 0.0, "headroom is negative: {harsh:?}");
        // corrections sit next to their readings
        assert_eq!(v.corrective_gain_db, 0.0);
        assert_eq!(v.bands.air, BandReading { share_db: -22.1, target_band_gain_db: 0.1 });
        assert_eq!(v.bands.mud.target_band_gain_db, 0.0);
        assert_eq!(v.bands.presence.target_band_gain_db, 0.0);
        // the exact JSON shape a caller reads
        let j = serde_json::to_value(&v).unwrap();
        let keys = |o: &Value| {
            let mut k: Vec<String> = o.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(keys(&j), ["bands", "checks", "corrective_gain_db", "crest_factor_db", "failed", "file", "integrated_lufs", "pass", "profile", "score_db_off", "seconds", "true_peak_dbtp"]);
        assert_eq!(keys(&j["bands"]), ["air", "harshness", "mud", "presence"]);
        for b in ["air", "harshness", "mud", "presence"] {
            assert_eq!(keys(&j["bands"][b]), ["share_db", "target_band_gain_db"]);
        }
        for c in j["checks"].as_array().unwrap() {
            assert_eq!(keys(c), ["delta_db", "limit", "name", "pass", "value"]);
            assert!(c["pass"].is_boolean() && c["delta_db"].is_number());
        }
        assert_eq!(j["profile"], json!("balanced"));
        assert!(j["pass"].is_boolean());
        // round trip: JSON -> struct -> JSON is lossless; unknown or missing fields are refused
        let back: JudgeVerdict = serde_json::from_value(j.clone()).unwrap();
        assert_eq!(back, v);
        assert_eq!(serde_json::to_value(&back).unwrap(), j);
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<JudgeVerdict>(&s).unwrap(), v);
        let mut extra = j.clone();
        extra["verdict_text"] = json!("nope");
        assert!(serde_json::from_value::<JudgeVerdict>(extra).is_err());
        let mut band_extra = j.clone();
        band_extra["bands"]["air"]["note"] = json!("x");
        assert!(serde_json::from_value::<JudgeVerdict>(band_extra).is_err());
        let mut missing = j.clone();
        missing.as_object_mut().unwrap().remove("checks");
        assert!(serde_json::from_value::<JudgeVerdict>(missing).is_err());
        let mut bad_profile = j;
        bad_profile["profile"] = json!("loudest");
        assert!(serde_json::from_value::<JudgeVerdict>(bad_profile).is_err());
        // the error payload: exactly {error_code, message}
        let e = serde_json::to_value(jerr("unknown_profile", "x")).unwrap();
        assert_eq!(keys(&e), ["error_code", "message"]);
        assert_eq!(serde_json::from_value::<JudgeError>(e).unwrap(), jerr("unknown_profile", "x"));
        // crest factor: a full-scale square wave is 0 dB, a sine ~3 dB
        let sq: Vec<f32> = (0..1000).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        assert!(crest_factor_db(&sq, &sq).abs() < 1e-3);
        let sine: Vec<f32> = (0..48000).map(|i| (i as f32 * 0.05).sin()).collect();
        assert!((crest_factor_db(&sine, &sine) - 3.01).abs() < 0.05);
    }

    #[test]
    fn gate_profiles_override_limits_and_corrections_are_signed() {
        let x = d4_reading();
        // the default (no profile named) is streaming
        assert_eq!(AutoJudge::default().profile, GateProfile::Streaming);
        assert_eq!(GateProfile::parse("club_trap"), Some(GateProfile::ClubTrap));
        assert_eq!(GateProfile::parse("custom"), None);
        assert_eq!(GateProfile::parse("loudest"), None);
        for n in GATE_PROFILES {
            let p = GateProfile::parse(n).unwrap();
            assert_eq!(p.name(), *n);
            assert!(AutoJudge::profile(p).problems().is_empty(), "{n}");
        }
        // streaming: too loud for -15..-13 -> turn down to the middle (-14)
        let v = build_verdict("f", &x, 9.0, &AutoJudge::profile(GateProfile::Streaming));
        assert!(v.failed.contains(&"loudness_max".to_string()), "{:?}", v.failed);
        assert_eq!(v.corrective_gain_db, -3.8);
        // club_trap: too quiet for -9..-8 -> up toward -8.5, inside the true-peak room (1.8 dB)
        let ct = AutoJudge::profile(GateProfile::ClubTrap);
        let v = build_verdict("f", &x, 9.0, &ct);
        assert!(v.failed.contains(&"loudness_min".to_string()));
        assert_eq!(v.corrective_gain_db, 1.7);
        assert_eq!(corrective_gain_db(&Reading { true_peak: -0.6, ..x.clone() }, &ct), 0.5, "capped by the true-peak room");
        // dnb: -9..-7, aim -8
        assert_eq!(build_verdict("f", &x, 9.0, &AutoJudge::profile(GateProfile::Dnb)).corrective_gain_db, 1.6);
        // a true-peak failure turns it down, whatever the loudness
        let bal = AutoJudge::profile(GateProfile::Balanced);
        assert_eq!(corrective_gain_db(&Reading { true_peak: -0.4, ..x.clone() }, &bal), -1.5);
        // band gains: signed to apply (cut mud/harshness, boost presence/air), sized past the limit
        let bad = Reading { mud: -6.0, presence: -16.0, harsh: -18.0, air: -23.0, ..x.clone() };
        let v = build_verdict("f", &bad, 9.0, &bal);
        let b = &v.bands;
        assert!(b.mud.target_band_gain_db < -2.0 && b.mud.target_band_gain_db > -3.0, "{b:?}");
        assert!(b.presence.target_band_gain_db >= 1.0 && b.presence.target_band_gain_db < 1.2, "{b:?}");
        assert!(b.harshness.target_band_gain_db <= -2.0 && b.harshness.target_band_gain_db > -2.2, "{b:?}");
        assert!(b.air.target_band_gain_db >= 1.0 && b.air.target_band_gain_db < 1.2, "{b:?}");
        // a pass carries no corrections
        let v = build_verdict("f", &Reading { air: -21.5, ..x.clone() }, 9.0, &bal);
        assert!(v.pass, "{:?}", v.failed);
        assert_eq!(v.corrective_gain_db, 0.0);
        assert!([&v.bands.mud, &v.bands.presence, &v.bands.harshness, &v.bands.air].iter().all(|r| r.target_band_gain_db == 0.0));
        // through the tool: a profile overrides the limits; a hand gate makes it custom
        let d = std::env::temp_dir().join(format!("auto_judge_prof_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        let r = e.call("auto_judge", &json!({})).unwrap();
        assert_eq!(r["config"]["profile"], json!("streaming"));
        let r = e.call("auto_judge", &json!({"profile": "dnb"})).unwrap();
        assert_eq!((r["config"]["profile"].clone(), r["config"]["lufs_min"].clone(), r["config"]["lufs_max"].clone(), r["config"]["tp_max"].clone()), (json!("dnb"), json!(-9.0), json!(-7.0), json!(-0.3)));
        let r = e.call("auto_judge", &json!({"air_min": -23.0})).unwrap();
        assert_eq!(r["config"]["profile"], json!("custom"));
        assert_eq!(r["config"]["lufs_min"], json!(-9.0));
        let r = e.call("auto_judge", &json!({"profile": "balanced"})).unwrap();
        assert_eq!((r["config"]["air_min"].clone(), r["config"]["harsh_max"].clone()), (json!(-22.0), json!(-20.0)));
        // errors: a stable {error_code, message}, and nothing changes
        let r = e.call("auto_judge", &json!({"profile": "loudest"})).unwrap();
        assert_eq!(r["error"]["error_code"], json!("unknown_profile"));
        assert!(r["error"]["message"].as_str().unwrap().contains("club_trap"));
        let r = e.call("auto_judge", &json!({"lufs_min": -5.0, "lufs_max": -9.0})).unwrap();
        assert_eq!(r["error"]["error_code"], json!("invalid_gates"));
        let r = e.call("auto_judge", &json!({"path": "nope/missing.wav", "profile": "streaming"})).unwrap();
        assert_eq!(r["error"]["error_code"], json!("unreadable_file"));
        assert_eq!(auto_judge_config(&e).profile, GateProfile::Balanced, "a failed call changes nothing");
        let r = e.call("auto_judge", &json!({"reset": true})).unwrap();
        assert_eq!(r["config"]["profile"], json!("streaming"));
    }

    #[test]
    fn the_auto_judge_judges_every_render_against_the_gates() {
        let d = std::env::temp_dir().join(format!("auto_judge_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d.clone());
        e.call("generate_beat", &json!({"style": "lofi", "seed": 11})).unwrap();
        let ex = json!({"format": "wav", "bits": 16, "target_lufs": -10, "path": "out/a.wav", "section": 0});
        // off by default: no verdict on the result
        let w = e.call("export_audio", &ex).unwrap();
        assert!(w.get("auto_judge").is_none(), "{w}");
        assert_eq!(rendered_file(&w).as_deref(), w["path"].as_str());
        // on, with gates nothing can fail on loudness/peak/bands
        let r = e.call("auto_judge", &json!({"enabled": true, "lufs_min": -20.0, "lufs_max": -2.0, "tp_max": 0.0, "mud_max": 0.0, "presence_min": -60.0, "harsh_max": 0.0, "air_min": -60.0})).unwrap();
        assert_eq!(r["config"]["enabled"], json!(true), "{r}");
        let w = e.call("export_audio", &ex).unwrap();
        let v = &w["auto_judge"];
        let typed: JudgeVerdict = serde_json::from_value(v.clone()).expect("auto_judge is a strict JudgeVerdict");
        assert_eq!(typed.checks.len(), 9);
        assert_eq!(typed.profile, GateProfile::Custom);
        assert!(typed.crest_factor_db > 0.0, "{w}");
        assert_eq!(typed.pass, typed.failed.is_empty());
        for k in ["loudness_min", "loudness_max", "true_peak_max", "mud_max", "presence_min", "harshness_max", "air_min"] {
            assert!(!typed.failed.contains(&k.to_string()), "{k} failed under open gates: {w}");
        }
        assert_eq!(typed.corrective_gain_db, 0.0);
        // a gate it can't meet: fail, named, with a boost on that band
        e.call("auto_judge", &json!({"presence_min": 0.0})).unwrap();
        let w = e.call("export_audio", &ex).unwrap();
        let typed: JudgeVerdict = serde_json::from_value(w["auto_judge"].clone()).unwrap();
        assert!(!typed.pass && typed.failed.contains(&"presence_min".to_string()), "{w}");
        let pc = typed.checks.iter().find(|c| c.name == "presence_min").unwrap();
        assert!(!pc.pass && pc.limit == 0.0 && pc.delta_db > 0.0, "{pc:?}");
        assert!(typed.bands.presence.target_band_gain_db > 0.0);
        // the setting lives in the workdir: a fresh engine there sees it
        let e2 = Engine::new(d.clone());
        assert!(auto_judge_config(&e2).enabled);
        assert_eq!(auto_judge_config(&e2).presence_min, 0.0);
        // judge one file now
        let r = e.call("auto_judge", &json!({"path": "out/a.wav"})).unwrap();
        assert_eq!(r["judged"]["pass"], json!(false));
        // a render whose file can't be read: a structured error field, the render result kept
        let mut fake = json!({"path": "out/missing.wav"});
        auto_judge_after(&e, "export_audio", &mut fake);
        assert_eq!(fake["auto_judge_error"]["error_code"], json!("unreadable_file"), "{fake}");
        assert!(fake.get("auto_judge").is_none());
        // tools that render nothing get no verdict
        let mut plain = json!({"ok": true});
        auto_judge_after(&e, "set_mixer", &mut plain);
        auto_judge_after(&e, "export_audio", &mut plain);
        assert!(plain.get("auto_judge").is_none() && plain.get("auto_judge_error").is_none());
        let r = e.call("auto_judge", &json!({"enabled": false})).unwrap();
        assert_eq!(r["config"]["enabled"], json!(false));
        let w = e.call("export_audio", &ex).unwrap();
        assert!(w.get("auto_judge").is_none());
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
        // the extra bell nudge (+1 dB at 3.5 kHz) set through the tool, saved
        // as a preset, survives off -> preset, and is range-checked
        let r = e.call("master_lift", &json!({"preset": "default", "presence_db": 3.5, "extra_hz": 3500.0, "extra_db": 1.0, "save_as": "nudge35"})).unwrap();
        assert_eq!(r["profile"]["extra_db"], json!(1.0));
        let want = MixProfile { presence_db: 3.5, extra_hz: 3500.0, extra_db: 1.0, ..MixProfile::default() };
        assert_eq!(current_master_lift(&e.project).unwrap(), want);
        e.call("master_lift", &json!({"preset": "off"})).unwrap();
        assert!(current_master_lift(&e.project).is_none());
        e.call("master_lift", &json!({"preset": "nudge35"})).unwrap();
        assert_eq!(current_master_lift(&e.project).unwrap(), want);
        assert!(e.call("master_lift", &json!({"extra_db": 5.0})).is_err());
        let r = e.call("master_lift", &json!({"extra_db": 0.0})).unwrap();
        assert_eq!(r["on"], json!(true));
        assert_eq!(current_master_lift(&e.project).unwrap().extra_db, 0.0);
    }
}
