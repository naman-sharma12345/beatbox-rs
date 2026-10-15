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

/// Key level detector of `sidechain_compress`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Detector {
    /// max |key| (stereo-linked) held over the last PEAK_HOLD_MS
    Peak,
    /// RMS of the key (L and R averaged) over the last RMS_WINDOW_MS
    Rms,
}

/// Peak-hold window of the peak detector: a steady tone reads its peak, not
/// its instantaneous value, so the gain sits on the static curve.
pub(crate) const PEAK_HOLD_MS: f32 = 10.0;
/// Rectangular RMS window: one full cycle of 50 Hz, so a kick's fundamental
/// does not ripple the level.
pub(crate) const RMS_WINDOW_MS: f32 = 20.0;

/// Feed-forward compressor parameters for `sidechain_compress`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct KeyedComp {
    pub threshold_db: f32,
    /// 1..20, or infinity (a limiter)
    pub ratio: f32,
    pub knee_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
    pub key_hpf_hz: f32,
    pub detector: Detector,
}

/// Static curve (dB in -> gain reduction dB, <= 0) with a quadratic soft
/// knee, after Giannoulis, Massberg & Reiss, "Digital Dynamic Range
/// Compressor Design - A Tutorial and Analysis", JAES 60(6), 2012, eq. 4.
/// An infinite ratio gives 1/r = 0: the output holds at the threshold.
pub(crate) fn gain_computer(x_db: f32, c: &KeyedComp) -> f32 {
    let (t, w) = (c.threshold_db, c.knee_db.max(0.0));
    let inv = 1.0 / c.ratio;
    let y = if 2.0 * (x_db - t) < -w {
        x_db
    } else if w > 0.0 && 2.0 * (x_db - t).abs() <= w {
        x_db + (inv - 1.0) * (x_db - t + w / 2.0).powi(2) / (2.0 * w)
    } else {
        t + (x_db - t) * inv
    };
    y - x_db
}

/// The key's level in dB per sample: optional one-pole high-pass, then the
/// peak-hold or RMS detector, stereo-linked. Causal; O(n) with a monotonic
/// queue (peak) or a running sum over a fixed ring (RMS).
pub(crate) fn key_level_db(key_l: &[f32], key_r: &[f32], c: &KeyedComp) -> Vec<f32> {
    let n = key_l.len().min(key_r.len());
    let hp = if c.key_hpf_hz > 0.0 { (-2.0 * std::f32::consts::PI * c.key_hpf_hz / SR).exp() } else { 0.0 };
    let (mut xl, mut yl, mut xr, mut yr) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let w = match c.detector {
        Detector::Peak => (PEAK_HOLD_MS * 0.001 * SR).round() as usize,
        Detector::Rms => (RMS_WINDOW_MS * 0.001 * SR).round() as usize,
    }
    .max(1);
    let mut q: std::collections::VecDeque<(usize, f32)> = std::collections::VecDeque::with_capacity(w + 1);
    let mut ring = vec![0.0f64; w];
    let mut sum = 0.0f64;
    let mut out = Vec::with_capacity(n);
    let db = |v: f32| if v > 1e-9 { 20.0 * v.log10() } else { -180.0 };
    for i in 0..n {
        let (mut a, mut b) = (key_l[i], key_r[i]);
        if hp > 0.0 {
            yl = hp * (yl + a - xl);
            xl = a;
            yr = hp * (yr + b - xr);
            xr = b;
            a = yl;
            b = yr;
        }
        match c.detector {
            Detector::Peak => {
                let v = a.abs().max(b.abs());
                while q.back().is_some_and(|&(_, x)| x <= v) {
                    q.pop_back();
                }
                q.push_back((i, v));
                while q.front().is_some_and(|&(j, _)| j + w <= i) {
                    q.pop_front();
                }
                out.push(db(q.front().map(|&(_, x)| x).unwrap_or(0.0)));
            }
            Detector::Rms => {
                let e = 0.5 * ((a as f64).powi(2) + (b as f64).powi(2));
                let k = i % w;
                sum += e - ring[k];
                ring[k] = e;
                if k == w - 1 {
                    sum = ring.iter().sum(); // re-sum once per lap: no drift
                }
                out.push(db((sum.max(0.0) / w as f64).sqrt() as f32));
            }
        }
    }
    out
}

/// Gain (linear, per sample) a compressor keyed by `key` applies: the key
/// level, the static curve in dB, then a branching attack/release smoother on
/// the gain reduction (same paper, sec. 3.2), plus makeup.
pub(crate) fn keyed_gain(key_l: &[f32], key_r: &[f32], c: &KeyedComp) -> Vec<f32> {
    let coef = |ms: f32| if ms <= 0.0 { 0.0 } else { (-1.0 / (ms * 0.001 * SR)).exp() };
    let (aa, ar) = (coef(c.attack_ms), coef(c.release_ms));
    let mut gr = 0.0f32;
    key_level_db(key_l, key_r, c)
        .into_iter()
        .map(|x_db| {
            let target = gain_computer(x_db, c);
            // more reduction = attack, less = release
            let k = if target < gr { aa } else { ar };
            gr = k * gr + (1.0 - k) * target;
            10f32.powf((gr + c.makeup_db) / 20.0)
        })
        .collect()
}

/// A stereo source by track name (its post-fader stem) or file path.
fn source_audio(e: &mut Engine, a: &Value, track_key: &str, path_key: &str) -> Result<(String, Vec<f32>, Vec<f32>)> {
    if let Some(pth) = s_opt(a, path_key) {
        let path = e.resolve(&pth);
        let (l, r) = crate::samples::decode_stereo(&path)?;
        return Ok((path.display().to_string(), l, r));
    }
    let Some(name) = s_opt(a, track_key) else { bail!("give {track_key} (a track) or {path_key} (an audio file)") };
    let p = e.project.clone();
    let i = p.track_index(&name)?;
    let m = e.render_stems_all(&p)?;
    let s = m.stems.iter().find(|s| s.name == p.tracks[i].name && !s.left.is_empty());
    let Some(s) = s else { bail!("track '{}' is silent in this render (muted, no notes, or soloed out)", p.tracks[i].name) };
    Ok((format!("track {}", p.tracks[i].name), s.left.clone(), s.right.clone()))
}

fn sidechain_compress(e: &mut Engine, a: &Value) -> Result<Value> {
    let ratio = match a.get("ratio") {
        None | Some(Value::Null) => 4.0,
        Some(Value::Number(x)) => x.as_f64().unwrap_or(f64::NAN) as f32,
        Some(Value::String(s)) if matches!(s.trim().to_ascii_lowercase().as_str(), "inf" | "infinity" | "limit") => f32::INFINITY,
        Some(other) => bail!("ratio must be 1..20 or \"inf\" (got {other})"),
    };
    let detector = match s_opt(a, "detector").unwrap_or_else(|| "peak".into()).to_ascii_lowercase().as_str() {
        "peak" => Detector::Peak,
        "rms" => Detector::Rms,
        other => bail!("detector must be peak or rms (got '{other}')"),
    };
    let c = KeyedComp {
        threshold_db: f_opt(a, "threshold_db").unwrap_or(-24.0),
        ratio,
        knee_db: f_opt(a, "knee_db").unwrap_or(6.0),
        attack_ms: f_opt(a, "attack_ms").unwrap_or(5.0),
        release_ms: f_opt(a, "release_ms").unwrap_or(150.0),
        makeup_db: f_opt(a, "makeup_db").unwrap_or(0.0),
        key_hpf_hz: f_opt(a, "key_hpf_hz").unwrap_or(0.0),
        detector,
    };
    let lookahead_ms = f_opt(a, "lookahead_ms").unwrap_or(0.0);
    if !(-60.0..=0.0).contains(&c.threshold_db) { bail!("threshold_db must be -60..0 (got {})", c.threshold_db); }
    if !(c.ratio.is_infinite() || (1.0..=20.0).contains(&c.ratio)) { bail!("ratio must be 1..20 or \"inf\" (got {})", c.ratio); }
    if !(0.0..=24.0).contains(&c.knee_db) { bail!("knee_db must be 0..24 (got {})", c.knee_db); }
    if !(0.1..=100.0).contains(&c.attack_ms) { bail!("attack_ms must be 0.1..100 (got {})", c.attack_ms); }
    if !(10.0..=1000.0).contains(&c.release_ms) { bail!("release_ms must be 10..1000 (got {})", c.release_ms); }
    if !(lookahead_ms == 0.0 || (1.0..=5.0).contains(&lookahead_ms)) { bail!("lookahead_ms must be 0 (off) or 1..5 (got {lookahead_ms})"); }
    if !(-24.0..=24.0).contains(&c.makeup_db) { bail!("makeup_db must be -24..24 (got {})", c.makeup_db); }
    if !(0.0..=1000.0).contains(&c.key_hpf_hz) { bail!("key_hpf_hz must be 0..1000 (got {})", c.key_hpf_hz); }
    let (tname, mut tl, mut tr) = source_audio(e, a, "target", "target_path")?;
    // no key given (or the target named as its own key): internal detection,
    // keyed by the target's own signal like an ordinary compressor
    let external = s_opt(a, "key").is_some() || s_opt(a, "key_path").is_some();
    let (kname, kl, kr) = if external { source_audio(e, a, "key", "key_path")? } else { (tname.clone(), tl.clone(), tr.clone()) };
    let internal = kname == tname;
    let kname = if internal { format!("internal ({tname}'s own signal)") } else { kname };
    let lufs_before = read_meter(Meter::Lufs, &tl, &tr).unwrap_or(-70.0);
    let n = tl.len().min(tr.len());
    // Lookahead, offline: instead of delaying the target (which would shift it
    // against the rest of the mix) the gain path is read `la` samples ahead, so
    // the gain lands that much before each key transient and the output keeps
    // the target's timing exactly (0 samples of latency).
    let la = (lookahead_ms * 0.001 * SR).round() as usize;
    let mut g = keyed_gain(&kl, &kr, &c);
    // past the key's end the key is silent: the reduction releases to 0
    let tail = 10f32.powf(c.makeup_db / 20.0);
    let ar = (-1.0 / (c.release_ms * 0.001 * SR)).exp();
    let mut cur = g.last().copied().unwrap_or(tail) / tail;
    while g.len() < n + la {
        cur = 1.0 - ar * (1.0 - cur);
        g.push(cur * tail);
    }
    let g = &g[la..la + n];
    let mut max_gr = 0.0f32;
    let blk = (0.05 * SR) as usize;
    let mut env = Vec::new();
    for i in 0..n {
        tl[i] *= g[i];
        tr[i] *= g[i];
        let gr = 20.0 * g[i].max(1e-9).log10() - c.makeup_db;
        max_gr = max_gr.min(gr);
        if i % blk == 0 {
            env.push(r2(gr));
        }
    }
    let gr_db: Vec<f32> = g.iter().map(|v| 20.0 * v.max(1e-9).log10() - c.makeup_db).collect();
    let mean = |v: &[f32]| if v.is_empty() { 0.0 } else { v.iter().sum::<f32>() / v.len() as f32 };
    // how the reduction splits between where the key is over the threshold and where it rests
    let mut key_db = key_level_db(&kl, &kr, &c);
    key_db.resize(n + la, -180.0);
    let (mut on, mut off) = (Vec::new(), Vec::new());
    for i in 0..n {
        if key_db[i + la] > c.threshold_db { on.push(gr_db[i]) } else { off.push(gr_db[i]) }
    }
    let mut v = json!({
        "target": tname, "key": kname,
        "detector": match c.detector { Detector::Peak => format!("peak (max |key| held {PEAK_HOLD_MS} ms, stereo-linked)"), Detector::Rms => format!("rms ({RMS_WINDOW_MS} ms rectangular window, L and R averaged)") },
        "threshold_db": c.threshold_db, "ratio": if c.ratio.is_infinite() { json!("inf") } else { json!(c.ratio) },
        "knee_db": c.knee_db, "attack_ms": c.attack_ms, "release_ms": c.release_ms, "makeup_db": c.makeup_db, "key_hpf_hz": c.key_hpf_hz,
        "lookahead_ms": lookahead_ms,
        "lookahead_samples": la,
        "latency_samples": 0,
        "latency": "none: lookahead reads the gain path ahead instead of delaying the target, so the output stays time-aligned with the mix",
        "max_gain_reduction_db": r2(max_gr),
        "mean_gain_reduction_db": r2(mean(&gr_db)),
        "mean_gr_while_key_over_threshold_db": r2(mean(&on)),
        "key_over_threshold_percent": r2(100.0 * on.len() as f32 / n.max(1) as f32),
        "mean_gr_elsewhere_db": r2(mean(&off)),
        "gain_reduction_every_50ms_db": env,
        "lufs_before_after": [r2(lufs_before), r2(read_meter(Meter::Lufs, &tl, &tr).unwrap_or(-70.0))],
        "design": "feed-forward, stereo-linked peak-hold or RMS detector on the key, soft-knee static curve and branching attack/release smoothing (Giannoulis, Massberg & Reiss, JAES 2012)",
    });
    match s_opt(a, "out") {
        Some(out) => {
            let path = e.resolve(&out);
            crate::render::write_wav(&path, &tl, &tr)?;
            v["out"] = json!(path.display().to_string());
        }
        None => v["note"] = json!("measured only; pass out to write the compressed target (16-bit wav)"),
    }
    Ok(v)
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
    Tool {
        name: "sidechain_compress",
        description: "Keyed (sidechain) compressor: compresses a target by another source's level, e.g. a kick keying the 808/bass so it ducks on every hit, or a vocal keying a reverb throw so the tail blooms between phrases. target/key = track names (post-fader output) or target_path/key_path = audio files; no key = internal detection on the target itself. Feed-forward, stereo-linked; detector peak (max |key| held 10 ms, default) or rms (20 ms window), optional key_hpf_hz; soft-knee curve, attack/release smoothing. threshold_db -60..0 (-24), ratio 1..20 or \"inf\" to limit (4), knee_db 0..24 (6), attack_ms 0.1..100 (5), release_ms 10..1000 (150), makeup_db (0). lookahead_ms 0 or 1..5 (0): the gain path is read that far ahead instead of delaying the target, so transients duck without overshoot and the output stays time-aligned with the mix (0 latency). Writes the compressed target to out (16-bit wav) if given. Reports max/mean gain reduction, reduction while the key is over the threshold vs elsewhere, a 50 ms envelope and LUFS before/after. Unlike the trigger-driven sidechain effect this follows the key's audio. Does not change the project.",
        mutates: false,
        schema: || {
            obj(
                json!({
                    "target": {"type": "string", "description": "Track to compress"},
                    "target_path": {"type": "string", "description": "Audio file to compress instead"},
                    "key": {"type": "string", "description": "Track whose level drives the compressor (none: the target keys itself)"},
                    "key_path": {"type": "string", "description": "Audio file that keys it instead"},
                    "detector": {"type": "string", "enum": ["peak", "rms"], "description": "peak: max |key| held 10 ms (default); rms: 20 ms rectangular window"},
                    "threshold_db": {"type": "number", "description": "Key level where reduction starts, -60..0 (default -24)"},
                    "ratio": {"type": ["number", "string"], "description": "1..20, or \"inf\" to limit (default 4)"},
                    "knee_db": {"type": "number", "description": "Soft-knee width, 0..24 (default 6)"},
                    "attack_ms": {"type": "number", "description": "0.1..100 (default 5)"},
                    "release_ms": {"type": "number", "description": "10..1000 (default 150)"},
                    "lookahead_ms": {"type": "number", "description": "0 = off, or 1..5 ms (default 0); no latency, the output stays aligned"},
                    "makeup_db": {"type": "number", "description": "-24..24 (default 0)"},
                    "key_hpf_hz": {"type": "number", "description": "High-pass on the key detector, 0 = off (default 0)"},
                    "out": {"type": "string", "description": "Where to write the compressed target"}
                }),
                &[],
            )
        },
        run: sidechain_compress,
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

    #[test]
    fn sidechain_compress_follows_the_key_not_the_target() {
        let dir = std::env::temp_dir().join("bb_sc_compress");
        let _ = std::fs::create_dir_all(&dir);
        let n = (2.0 * SR) as usize;
        let half = n / 2;
        let tone = |f: f32, a: f32, i: usize| a * (2.0 * std::f32::consts::PI * f * i as f32 / SR).sin();
        let target: Vec<f32> = (0..n).map(|i| tone(200.0, 0.25, i)).collect();
        let key_a: Vec<f32> = (0..n).map(|i| if i < half { tone(1000.0, 0.9, i) } else { 0.0 }).collect();
        let key_b: Vec<f32> = (0..n).map(|i| if i >= half { tone(1000.0, 0.9, i) } else { 0.0 }).collect();
        let (tp, ka, kb) = (dir.join("t.wav"), dir.join("ka.wav"), dir.join("kb.wav"));
        crate::render::write_wav(&tp, &target, &target).unwrap();
        crate::render::write_wav(&ka, &key_a, &key_a).unwrap();
        crate::render::write_wav(&kb, &key_b, &key_b).unwrap();
        let mut e = Engine::new(dir.clone());
        let env = |v: &Value| v["gain_reduction_every_50ms_db"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect::<Vec<f64>>();
        let mean = |x: &[f64]| x.iter().sum::<f64>() / x.len() as f64;
        let m = (1.0 / 0.05) as usize; // envelope points per second
        let out = dir.join("ducked.wav");
        let a = e.call("sidechain_compress", &json!({"target_path": tp.display().to_string(), "key_path": ka.display().to_string(), "release_ms": 50.0, "out": out.display().to_string()})).unwrap();
        let ea = env(&a);
        // key A plays in the first second: about 0.75 x 23 dB of reduction there, none once it has released
        assert!(mean(&ea[2..m - 1]) < -12.0, "{a}");
        assert!(mean(&ea[m + 8..]) > -0.05, "{a}");
        assert!(a["mean_gr_while_key_over_threshold_db"].as_f64().unwrap() < -12.0, "{a}");
        assert!(a["mean_gr_elsewhere_db"].as_f64().unwrap() > -3.0, "{a}");
        let (l, _) = crate::samples::decode_stereo(&out).unwrap();
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        assert!(rms(&l[half / 4..half - 2000]) < 0.3 * rms(&l[half + half / 2..]), "first half ducked, second half untouched");
        // key B plays in the second second: the reduction moves with it
        let b = e.call("sidechain_compress", &json!({"target_path": tp.display().to_string(), "key_path": kb.display().to_string(), "release_ms": 50.0})).unwrap();
        let eb = env(&b);
        assert!(mean(&eb[2..m - 1]) > -0.05, "{b}");
        assert!(mean(&eb[m + 2..2 * m - 1]) < -12.0, "{b}");
        // tracks: the named key track drives the reduction and another track does not.
        // The tool's envelope equals the gain computed from that track's own stem.
        let mut s = song("bb_sc_compress_song");
        let p = s.project.clone();
        let stems = s.render_stems_all(&p).unwrap();
        let stem = |n: &str| stems.stems.iter().find(|x| x.name == n).unwrap();
        let blk = (0.05 * SR) as usize;
        let c = KeyedComp { threshold_db: -30.0, ratio: 4.0, knee_db: 6.0, attack_ms: 1.0, release_ms: 30.0, makeup_db: 0.0, key_hpf_hz: 0.0, detector: Detector::Peak };
        let expect = |n: &str| -> Vec<f64> {
            let k = stem(n);
            let g = keyed_gain(&k.left, &k.right, &c);
            (0..g.len()).step_by(blk).map(|i| r2(20.0 * g[i].max(1e-9).log10()) as f64).collect()
        };
        for (key, other) in [("kick", "hat"), ("hat", "kick")] {
            let v = s.call("sidechain_compress", &json!({"target": "keys", "key": key, "threshold_db": -30.0, "attack_ms": 1.0, "release_ms": 30.0})).unwrap();
            let got = env(&v);
            let (own, oth) = (expect(key), expect(other));
            let len = got.len().min(own.len()).min(oth.len());
            let dev = |x: &[f64]| (0..len).map(|i| (got[i] - x[i]).abs()).fold(0.0, f64::max);
            eprintln!("key {key}: max |env - own key| {:.3} dB, max |env - {other}| {:.3} dB", dev(&own), dev(&oth));
            assert!(dev(&own) <= 0.011, "{key}: {v}");
            assert!(dev(&oth) > 1.0, "{key} vs {other}");
            assert!(v["max_gain_reduction_db"].as_f64().unwrap() < -1.0, "{v}");
        }
        // no key: internal detection on the target itself (the same as naming it as its own key)
        let own = s.call("sidechain_compress", &json!({"target": "keys", "attack_ms": 1.0, "release_ms": 30.0})).unwrap();
        assert!(own["key"].as_str().unwrap().starts_with("internal"), "{own}");
        let same = s.call("sidechain_compress", &json!({"target": "keys", "key": "keys", "attack_ms": 1.0, "release_ms": 30.0})).unwrap();
        assert_eq!(env(&own), env(&same));
        let ks = stem("keys");
        let gk = keyed_gain(&ks.left, &ks.right, &KeyedComp { threshold_db: -24.0, ..c });
        let want: Vec<f64> = (0..gk.len()).step_by(blk).map(|i| r2(20.0 * gk[i].max(1e-9).log10()) as f64).collect();
        assert!(env(&own).iter().zip(&want).all(|(x, y)| (x - y).abs() <= 0.011), "internal keying follows the target's own level");
        assert!(s.call("sidechain_compress", &json!({"key": "kick"})).is_err());
        for bad in [json!({"ratio": 0.5}), json!({"ratio": 21.0}), json!({"ratio": "lots"}), json!({"attack_ms": 0.05}), json!({"attack_ms": 101.0}),
                    json!({"release_ms": 5.0}), json!({"release_ms": 1001.0}), json!({"lookahead_ms": 0.5}), json!({"lookahead_ms": 6.0}),
                    json!({"threshold_db": 3.0}), json!({"knee_db": 30.0}), json!({"detector": "vu"})] {
            let mut a = json!({"target": "keys", "key": "kick"});
            a.as_object_mut().unwrap().extend(bad.as_object().unwrap().clone());
            assert!(s.call("sidechain_compress", &a).is_err(), "{a}");
        }
        assert_eq!(s.call("sidechain_compress", &json!({"target": "keys", "key": "kick", "ratio": "inf"})).unwrap()["ratio"], "inf");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidechain_burst_key_attenuates_by_the_curve_and_releases_on_its_time_constant() {
        let dir = std::env::temp_dir().join("bb_sc_burst");
        let _ = std::fs::create_dir_all(&dir);
        // target: a steady 200 Hz tone; key: a 1 kHz burst at 0.5 peak (-6.02 dBFS) from 0.5 s to 1.5 s
        let n = (3.0 * SR) as usize;
        let (b0, b1) = ((0.5 * SR) as usize, (1.5 * SR) as usize);
        let t = sine(200.0, 0.25, 3.0, 0.0);
        let k1 = sine(1000.0, 0.5, 3.0, 0.0);
        let key: Vec<f32> = (0..n).map(|i| if (b0..b1).contains(&i) { k1[i] } else { 0.0 }).collect();
        let (tp, kp, op) = (dir.join("t.wav"), dir.join("k.wav"), dir.join("o.wav"));
        crate::render::write_wav(&tp, &t, &t).unwrap();
        crate::render::write_wav(&kp, &key, &key).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("sidechain_compress", &json!({"target_path": tp.display().to_string(), "key_path": kp.display().to_string(), "out": op.display().to_string()})).unwrap();
        let (o, _) = crate::samples::decode_stereo(&op).unwrap();
        let rms = |x: &[f32]| (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
        // settled part of the burst (0.7..1.4 s): the target drops by exactly the static curve at -6.02 dB
        let (s0, s1) = ((0.7 * SR) as usize, (1.4 * SR) as usize);
        let att = 20.0 * (rms(&o[s0..s1]) / rms(&t[s0..s1])).log10();
        let c = KeyedComp { threshold_db: -24.0, ratio: 4.0, knee_db: 6.0, attack_ms: 5.0, release_ms: 150.0, makeup_db: 0.0, key_hpf_hz: 0.0, detector: Detector::Peak };
        let want = gain_computer(20.0 * 0.5f32.log10(), &c) as f64;
        eprintln!("1 kHz burst: target attenuated {att:.4} dB, curve {want:.4} dB");
        assert!((att - want).abs() <= 0.05, "{att} vs {want}");
        // before the burst the target is untouched
        assert!((20.0 * (rms(&o[..b0 - 1000]) / rms(&t[..b0 - 1000])).log10()).abs() <= 0.01);
        // release: once the 10 ms hold lets go of the last peak, the reduction (in dB)
        // decays as exp(-t / release): 1/e after one time constant, e^-3 after three
        for rel in [50.0f32, 150.0, 400.0] {
            let c = KeyedComp { release_ms: rel, ..c };
            let g: Vec<f32> = keyed_gain(&key, &key, &c).iter().map(|v| 20.0 * v.log10()).collect();
            let lvl = key_level_db(&key, &key, &c);
            let i0 = (b1..n).find(|&i| gain_computer(lvl[i], &c) == 0.0).unwrap(); // first sample the curve asks for no reduction
            let g0 = g[i0 - 1];
            let tau_n = (rel * 0.001 * SR) as usize;
            let k = (i0..n).find(|&i| g[i] >= g0 / std::f32::consts::E).unwrap() - (i0 - 1);
            let tau_ms = k as f32 / SR * 1000.0;
            let at3 = g[i0 - 1 + 3 * tau_n];
            eprintln!("release {rel} ms: GR {g0:.3} dB at release start ({:.2} ms after the burst), measured time constant {tau_ms:.3} ms, at 3 tau {at3:.4} dB (expect {:.4})", (i0 - b1) as f32 / SR * 1000.0, g0 * (-3.0f32).exp());
            // tolerance: one sample (0.023 ms) plus 0.1 % for the f32 smoothing coefficient
            assert!((tau_ms - rel).abs() <= 0.03 + 0.001 * rel, "{tau_ms} vs {rel}");
            assert!((at3 - g0 * (-3.0f32).exp()).abs() <= 0.01, "{at3}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidechain_gain_on_a_steady_key_sits_on_the_static_curve() {
        // the curve itself, by hand: above the knee GR = (x - t)(1/r - 1); at the
        // knee centre (1/r - 1) w / 8; an infinite ratio pins the output at t
        let base = KeyedComp { threshold_db: -24.0, ratio: 4.0, knee_db: 6.0, attack_ms: 5.0, release_ms: 150.0, makeup_db: 0.0, key_hpf_hz: 0.0, detector: Detector::Peak };
        assert!((gain_computer(-6.0, &base) + 18.0 * 0.75).abs() < 1e-4);
        assert!((gain_computer(-24.0, &base) + 0.75 * 6.0 / 8.0).abs() < 1e-4);
        assert_eq!(gain_computer(-30.0, &base), 0.0);
        assert!((gain_computer(-6.0, &KeyedComp { ratio: f32::INFINITY, ..base }) + 18.0).abs() < 1e-4);
        // a steady 1 kHz key at 0.5 peak: peak -6.02 dBFS, RMS -9.03 dBFS
        let key = sine(1000.0, 0.5, 3.0, 0.0);
        let pk = 20.0 * 0.5f32.log10();
        let rms = pk - 10.0 * 2f32.log10();
        let cases = [
            (base, pk),
            (KeyedComp { detector: Detector::Rms, ..base }, rms),
            (KeyedComp { threshold_db: -7.0, ..base }, pk),
            (KeyedComp { ratio: f32::INFINITY, ..base }, pk),
            (KeyedComp { ratio: 20.0, knee_db: 0.0, attack_ms: 0.1, release_ms: 10.0, detector: Detector::Rms, ..base }, rms),
        ];
        for (c, lvl) in cases {
            let g = keyed_gain(&key, &key, &c);
            let last: Vec<f32> = g[g.len() - SR as usize..].iter().map(|v| 20.0 * v.log10()).collect();
            let (lo, hi) = last.iter().fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
            let curve = gain_computer(lvl, &c);
            eprintln!("{:?} t {} r {} w {}: measured {lo:.4}..{hi:.4} dB, curve {curve:.4} dB", c.detector, c.threshold_db, c.ratio, c.knee_db);
            assert!((lo - curve).abs() <= 0.1 && (hi - curve).abs() <= 0.1, "{c:?}");
        }
        // and through the tool, from files
        let dir = std::env::temp_dir().join("bb_sc_curve");
        let _ = std::fs::create_dir_all(&dir);
        let (kp, tp) = (dir.join("k.wav"), dir.join("t.wav"));
        crate::render::write_wav(&kp, &key, &key).unwrap();
        let t = sine(200.0, 0.25, 3.0, 0.0);
        crate::render::write_wav(&tp, &t, &t).unwrap();
        let mut e = Engine::new(dir.clone());
        let v = e.call("sidechain_compress", &json!({"target_path": tp.display().to_string(), "key_path": kp.display().to_string()})).unwrap();
        let tail = v["gain_reduction_every_50ms_db"].as_array().unwrap().last().unwrap().as_f64().unwrap();
        assert!((tail as f32 - gain_computer(pk, &base)).abs() <= 0.1, "{v}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidechain_rms_and_peak_detectors_differ_as_expected_on_a_crest_heavy_key() {
        // a click every 10 ms at 0.9: the peak detector (10 ms hold) always holds one
        // click, -0.92 dBFS; the 20 ms RMS window always holds two, 0.9 sqrt(2/882) = -27.36 dBFS
        let n = 3 * SR as usize;
        let key: Vec<f32> = (0..n).map(|i| if i % 441 == 0 { 0.9 } else { 0.0 }).collect();
        let base = KeyedComp { threshold_db: -24.0, ratio: 4.0, knee_db: 6.0, attack_ms: 1.0, release_ms: 100.0, makeup_db: 0.0, key_hpf_hz: 0.0, detector: Detector::Peak };
        let pk_lvl = 20.0 * 0.9f32.log10();
        let rms_lvl = 20.0 * (0.9 * (2.0f32 / 882.0).sqrt()).log10();
        let settled = |c: &KeyedComp| -> f32 { let g = keyed_gain(&key, &key, c); 20.0 * g[n - 100].log10() };
        let (gp, gr) = (settled(&base), settled(&KeyedComp { detector: Detector::Rms, ..base }));
        eprintln!("click train: peak mode {gp:.3} dB (curve {:.3}), rms mode {gr:.3} dB (curve {:.3})", gain_computer(pk_lvl, &base), gain_computer(rms_lvl, &base));
        assert!((gp - gain_computer(pk_lvl, &base)).abs() <= 0.1, "{gp}");
        assert!((gr - gain_computer(rms_lvl, &base)).abs() <= 0.1, "{gr}");
        assert!(gp < -15.0 && gr > -0.1, "the peak detector clamps hard on the clicks, the RMS one barely moves");
    }

    #[test]
    fn sidechain_lookahead_ducks_the_transient_without_overshoot_and_keeps_timing() {
        let dir = std::env::temp_dir().join("bb_sc_lookahead");
        let _ = std::fs::create_dir_all(&dir);
        // 0.5 s of silence, then a 1 kHz tone at 0.5 peak for 1 s: target and key are the same audio in two files
        let n0 = (0.5 * SR) as usize;
        let mut x = vec![0.0f32; n0];
        x.extend(sine(1000.0, 0.5, 1.0, 0.0));
        x.resize(n0 + (1.5 * SR) as usize, 0.0);
        let (tp, kp) = (dir.join("t.wav"), dir.join("k.wav"));
        crate::render::write_wav(&tp, &x, &x).unwrap();
        crate::render::write_wav(&kp, &x, &x).unwrap();
        let mut e = Engine::new(dir.clone());
        let c = KeyedComp { threshold_db: -24.0, ratio: 4.0, knee_db: 0.0, attack_ms: 0.5, release_ms: 100.0, makeup_db: 0.0, key_hpf_hz: 0.0, detector: Detector::Peak };
        let pk = 20.0 * 0.5f32.log10();
        let curve_out = pk + gain_computer(pk, &c);
        let run = |e: &mut Engine, la: f64, name: &str| -> Vec<f32> {
            let out = dir.join(name);
            let v = e.call("sidechain_compress", &json!({"target_path": tp.display().to_string(), "key_path": kp.display().to_string(),
                "threshold_db": -24.0, "ratio": 4.0, "knee_db": 0.0, "attack_ms": 0.5, "release_ms": 100.0, "lookahead_ms": la, "out": out.display().to_string()})).unwrap();
            assert_eq!(v["latency_samples"], 0);
            crate::samples::decode_stereo(&out).unwrap().0
        };
        // worst 1 ms block peak over the first 30 ms of the tone, against the curve's output level
        let over = |y: &[f32]| -> f32 {
            (0..30).map(|b| { let s = n0 + b * 44; let p = y[s..s + 44].iter().fold(0.0f32, |m, v| m.max(v.abs())); 20.0 * p.max(1e-9).log10() - curve_out }).fold(f32::MIN, f32::max)
        };
        let (with, without) = (run(&mut e, 5.0, "la5.wav"), run(&mut e, 0.0, "la0.wav"));
        eprintln!("transient vs curve: lookahead 5 ms {:+.4} dB, none {:+.4} dB", over(&with), over(&without));
        assert!(over(&with) <= 0.0 + 0.01, "with lookahead the transient sits on or under the curve");
        assert!(over(&without) > 1.0, "without it the first cycles overshoot");
        // timing is kept: the output starts on the same sample as the target
        let first = |y: &[f32]| y.iter().position(|v| v.abs() > 1e-4).unwrap();
        assert_eq!(first(&with), first(&x));
        assert_eq!(with.len(), x.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
