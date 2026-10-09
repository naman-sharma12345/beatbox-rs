//! Delivery tools: codec-aware export (wav/flac/mp3), stems, the master
//! assistant and reference analysis/comparison.

use crate::analysis;
use crate::audio_edit;
use crate::dsp::{db_to_gain, SR};
use crate::export;
use crate::fx::{Effect, FxContext, GainFx, LimiterFx};
use crate::render::{self, RenderOptions};
use crate::samples;
use crate::tools::{b_or, f_opt, obj, s_opt, s_req, u_or, Tool};
use crate::tools_ears::{region, region_props};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct DeliverOpts {
    pub format: String,
    pub bits: u32,
    pub dither: bool,
    pub noise_shaping: bool,
    pub target_lufs: Option<f32>,
    pub ceiling_dbtp: f32,
    pub limit: bool,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
}

impl Default for DeliverOpts {
    fn default() -> Self {
        DeliverOpts {
            format: "wav".into(),
            bits: 24,
            dither: true,
            noise_shaping: false,
            target_lufs: None,
            ceiling_dbtp: -1.0,
            limit: true,
            bitrate_kbps: 320,
            sample_rate: 44_100,
        }
    }
}

pub fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn scale(l: &mut [f32], r: &mut [f32], db: f32) {
    let g = db_to_gain(db);
    for v in l.iter_mut().chain(r.iter_mut()) {
        *v *= g;
    }
}

fn tp_db(l: &[f32], r: &[f32]) -> f32 {
    crate::dsp::gain_to_db(analysis::true_peak(l).max(analysis::true_peak(r)))
}

/// Encode once to `path` and return the decoded audio (what a listener gets).
fn encode_to(path: &Path, l: &[f32], r: &[f32], o: &DeliverOpts) -> Result<(Vec<f32>, Vec<f32>)> {
    if let Some(d) = path.parent() {
        if !d.as_os_str().is_empty() {
            std::fs::create_dir_all(d)?;
        }
    }
    let dither = o.dither && o.bits < 32;
    match o.format.as_str() {
        "wav" => std::fs::write(
            path,
            export::wav(l, r, o.sample_rate, o.bits, dither, o.noise_shaping)?,
        )?,
        "flac" => std::fs::write(
            path,
            export::flac(l, r, o.sample_rate, o.bits.min(24), dither, o.noise_shaping)?,
        )?,
        "mp3" => {
            if !ffmpeg_available() {
                bail!("mp3 export needs ffmpeg on PATH; export wav or flac instead");
            }
            let tmp = path.with_extension("tmp.wav");
            std::fs::write(&tmp, export::wav(l, r, o.sample_rate, 32, false, false)?)?;
            let out = std::process::Command::new("ffmpeg")
                .args(["-y", "-loglevel", "error", "-i"])
                .arg(&tmp)
                .args([
                    "-codec:a",
                    "libmp3lame",
                    "-b:a",
                    &format!("{}k", o.bitrate_kbps),
                ])
                .arg(path)
                .output()
                .context("running ffmpeg")?;
            let _ = std::fs::remove_file(&tmp);
            if !out.status.success() {
                bail!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
            }
        }
        f => bail!("unknown format '{f}' (wav, flac, mp3)"),
    }
    if o.sample_rate != SR as u32 {
        // the decoder resamples to the engine rate; measure the in-memory audio instead
        return Ok((l.to_vec(), r.to_vec()));
    }
    samples::decode_stereo(path)
}

/// Codec-aware delivery: loudness-normalise to a target, true-peak limit,
/// encode, re-measure the decoded file and trim gain until the decoded true
/// peak meets the ceiling.
pub fn deliver(l: &[f32], r: &[f32], path: &Path, o: &DeliverOpts) -> Result<Value> {
    let before = analysis::loudness(l, r);
    let mut steps = Vec::new();
    let trig = HashMap::new();
    let ctx = FxContext {
        step_secs: 0.125,
        triggers: &trig,
    };
    let limiter = Effect::Limiter(LimiterFx {
        ceiling_db: o.ceiling_dbtp - 0.5,
        release_ms: 80.0,
        true_peak: true,
        bypass: false,
    });
    // gain -> (limit) -> measure, repeated so the limiter's loudness loss is made up
    let mut gain = o
        .target_lufs
        .map(|t| t - before.integrated_lufs)
        .unwrap_or(0.0);
    let (mut l, mut r) = (l.to_vec(), r.to_vec());
    for pass in 0..4 {
        let (mut wl, mut wr) = (l.clone(), r.clone());
        scale(&mut wl, &mut wr, gain);
        let limited = o.limit && tp_db(&wl, &wr) > o.ceiling_dbtp - 0.3;
        if limited {
            limiter.process(&mut wl, &mut wr, &ctx);
        }
        let now = analysis::loudness(&wl, &wr).integrated_lufs;
        steps.push(json!({"step": "gain_stage", "pass": pass + 1, "gain_db": (gain * 100.0).round() / 100.0, "limited": limited, "lufs": now}));
        let err = o.target_lufs.map(|t| t - now).unwrap_or(0.0);
        if err.abs() <= 0.3 || pass == 3 {
            l = wl;
            r = wr;
            break;
        }
        gain += err * if limited { 1.2 } else { 1.0 };
    }
    if o.sample_rate != SR as u32 {
        l = crate::resample::resample(&l, SR as u32, o.sample_rate);
        r = crate::resample::resample(&r, SR as u32, o.sample_rate);
        steps.push(json!({"step": "resample", "to_hz": o.sample_rate}));
    }
    let mut trim_total = 0.0f32;
    let mut decoded_tp = 0.0;
    for pass in 0..4 {
        let (dl, dr) = encode_to(path, &l, &r, o)?;
        decoded_tp = tp_db(&dl, &dr);
        if decoded_tp <= o.ceiling_dbtp || o.format == "wav" && o.bits == 32 && pass > 0 {
            break;
        }
        let trim = -(decoded_tp - o.ceiling_dbtp + 0.1);
        scale(&mut l, &mut r, trim);
        trim_total += trim;
        steps.push(json!({"step": "codec_trim", "pass": pass + 1, "decoded_true_peak": decoded_tp, "trim_db": (trim * 100.0).round() / 100.0}));
    }
    let (dl, dr) = if o.sample_rate == SR as u32 {
        samples::decode_stereo(path)?
    } else {
        (l.clone(), r.clone())
    };
    let after = analysis::loudness(&dl, &dr);
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let lufs_ok = o
        .target_lufs
        .map(|t| (after.integrated_lufs - t).abs() <= 1.0)
        .unwrap_or(true);
    Ok(json!({
        "path": path, "format": o.format, "bits": if o.format == "mp3" { Value::Null } else { json!(o.bits) },
        "bitrate_kbps": if o.format == "mp3" { json!(o.bitrate_kbps) } else { Value::Null },
        "sample_rate": o.sample_rate, "dither": o.dither && o.bits < 32 && o.format != "mp3",
        "bytes": bytes, "seconds": (dl.len() as f32 / SR * 100.0).round() / 100.0,
        "before": {"integrated_lufs": before.integrated_lufs, "true_peak_dbtp": before.true_peak_dbtp},
        "after": {"integrated_lufs": after.integrated_lufs, "true_peak_dbtp": (decoded_tp * 100.0).round() / 100.0, "short_term_max_lufs": after.short_term_max_lufs, "loudness_range_lu": after.loudness_range_lu},
        "codec_trim_db": (trim_total * 100.0).round() / 100.0,
        "steps": steps,
        "passed": decoded_tp <= o.ceiling_dbtp + 0.05 && lufs_ok,
    }))
}

fn opts_from(a: &Value, default_fmt: &str) -> Result<DeliverOpts> {
    let d = DeliverOpts::default();
    let format = s_opt(a, "format")
        .unwrap_or_else(|| default_fmt.to_string())
        .to_lowercase();
    let bits = u_or(a, "bits", if format == "flac" { 24 } else { d.bits as u64 }) as u32;
    let sr = u_or(a, "sample_rate", 44_100) as u32;
    if !(8_000..=192_000).contains(&sr) {
        bail!("sample_rate must be 8000..192000");
    }
    Ok(DeliverOpts {
        format,
        bits,
        dither: b_or(a, "dither", bits <= 24),
        noise_shaping: b_or(a, "noise_shaping", false),
        target_lufs: f_opt(a, "target_lufs"),
        ceiling_dbtp: f_opt(a, "true_peak_ceiling").unwrap_or(d.ceiling_dbtp),
        limit: b_or(a, "limit", true),
        bitrate_kbps: (u_or(a, "bitrate_kbps", 320) as u32).clamp(64, 320),
        sample_rate: sr,
    })
}

fn ext(fmt: &str) -> &str {
    match fmt {
        "flac" => "flac",
        "mp3" => "mp3",
        _ => "wav",
    }
}

fn delivery_props() -> Value {
    json!({
        "format": {"type": "string", "enum": ["wav", "flac", "mp3"], "description": "default wav"},
        "bits": {"type": "integer", "enum": [16, 24, 32], "description": "wav 16/24/32-float, flac 16/24 (default 24)"},
        "dither": {"type": "boolean", "description": "TPDF dither when reducing to 16/24 bit (default true)"},
        "noise_shaping": {"type": "boolean", "description": "first-order noise-shaped dither (default false)"},
        "sample_rate": {"type": "integer", "description": "44100 (default) or e.g. 48000 (windowed-sinc SRC)"},
        "bitrate_kbps": {"type": "integer", "description": "mp3 bitrate (default 320)"},
    })
}

fn merge_props(mut a: Value, b: Value) -> Value {
    if let (Value::Object(x), Value::Object(y)) = (&mut a, b) {
        x.extend(y);
    }
    a
}

/// Index of the last limiter in a chain.
fn limiter_pos(fx: &[Effect]) -> Option<usize> {
    fx.iter().rposition(|e| matches!(e, Effect::Limiter(_)))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "export_audio",
            description: "Deliver the song (or a section/window): wav (16/24-bit int or 32-bit float), flac (16/24, built-in encoder) or mp3 (ffmpeg/libmp3lame). Optional target_lufs loudness normalisation and a true-peak limiter; the file is then ENCODED, DECODED and RE-MEASURED, and the gain is trimmed until the decoded true peak meets the ceiling (MP3 overshoot included). TPDF dither on bit-depth reduction. Returns before/after LUFS and dBTP, codec trim and pass/fail.",
            mutates: false,
            schema: || obj(merge_props(merge_props(delivery_props(), region_props()), json!({
                "path": {"type": "string", "description": "Output path (default renders/<project>.<ext>)"},
                "target_lufs": {"type": "number", "description": "Normalise integrated loudness to this (e.g. -14 streaming, -9 club). Omit to keep the mix level"},
                "true_peak_ceiling": {"type": "number", "description": "dBTP ceiling after encoding (default -1)"},
                "limit": {"type": "boolean", "description": "Apply a true-peak limiter when the gain would exceed the ceiling (default true)"},
                "tail_s": {"type": "number"},
            })), &[]),
            run: |e, a| {
                let o = opts_from(a, "wav")?;
                let m = e.mix()?;
                let p = e.project.clone();
                let (s0, s1, label) = region(&p, a, m.left.len().min(m.right.len()))?;
                let path = match s_opt(a, "path") {
                    Some(x) => e.resolve(&x),
                    None => e.renders_dir().join(format!("{}.{}", samples::sample_name(&p.name), ext(&o.format))),
                };
                let mut v = deliver(&m.left[s0..s1], &m.right[s0..s1], &path, &o)?;
                v["window"] = json!(label);
                Ok(v)
            },
        },
        Tool {
            name: "export_stems",
            description: "Export every track AND every bus/return (reverb, delay, drum group) as its own file, all the same length and aligned at 0. mode pre_master (default): post-fader, before the master chain; the track stems routed straight to master plus the bus stems SUM BACK to the pre-master mix, and the residual is reported. mode post_master: each stem is also run through the master chain + master fader (approximate: compressors/limiters react to each stem alone, so the sum will not null against the real master; the report says how far off it is). Formats wav/flac/mp3 with dither; no loudness normalisation.",
            mutates: false,
            schema: || obj(merge_props(delivery_props(), json!({
                "dir": {"type": "string", "description": "Output folder (default renders/<project>_stems)"},
                "mode": {"type": "string", "enum": ["pre_master", "post_master"], "description": "default pre_master"},
                "include_buses": {"type": "boolean", "description": "Also export bus/return outputs (default true)"},
                "tracks": {"type": "array", "items": {"type": "string"}, "description": "Only these tracks/buses"},
            })), &[]),
            run: |e, a| {
                let mut o = opts_from(a, "wav")?;
                o.limit = false;
                o.target_lufs = None;
                o.ceiling_dbtp = 0.0;
                let mode = s_opt(a, "mode").unwrap_or_else(|| "pre_master".into());
                if mode != "pre_master" && mode != "post_master" { bail!("mode must be pre_master or post_master"); }
                let p = e.project.clone();
                let dir = match s_opt(a, "dir") {
                    Some(d) => e.resolve(&d),
                    None => e.renders_dir().join(format!("{}_stems", samples::sample_name(&p.name))),
                };
                e.bank.sync(&p.samples);
                let mix = render::render(&p, &e.bank, &RenderOptions { keep_stems: true, ..Default::default() })?;
                // the pre-master reference: same song with an empty master chain at 0 dB
                let mut pm = p.clone();
                pm.master_effects.clear();
                pm.master_volume_db = 0.0;
                pm.automation.retain(|l| !l.target.eq_ignore_ascii_case("master"));
                let pre = render::render(&pm, &e.bank, &RenderOptions::default())?;
                let any_solo = p.tracks.iter().any(|t| t.solo);
                let n = mix.left.len();
                let (mut sl, mut sr) = (vec![0.0f32; n], vec![0.0f32; n]);
                let mut add = |s: &render::Stem| { for i in 0..n.min(s.left.len()) { sl[i] += s.left[i]; sr[i] += s.right[i]; } };
                for (t, s) in p.tracks.iter().zip(mix.stems.iter()) {
                    let audible = !t.mute && (!any_solo || t.solo);
                    let direct = t.output.as_deref().map(|b| p.bus_index(b).is_err()).unwrap_or(true);
                    if audible && direct { add(s); }
                }
                for (b, s) in p.buses.iter().zip(mix.bus_stems.iter()) {
                    if !b.mute { add(s); }
                }
                let resid = |rl: &[f32], rr: &[f32], al: &[f32], ar: &[f32]| -> f32 {
                    let (mut e1, mut e0) = (0.0f64, 0.0f64);
                    for i in 0..rl.len().min(al.len()) {
                        e0 += (rl[i] as f64).powi(2) + (rr[i] as f64).powi(2);
                        e1 += ((rl[i] - al[i]) as f64).powi(2) + ((rr[i] - ar[i]) as f64).powi(2);
                    }
                    if e0 < 1e-12 { -120.0 } else { ((10.0 * (e1.max(1e-20) / e0).log10()) * 10.0).round() as f32 / 10.0 }
                };
                let pre_resid = resid(&pre.left, &pre.right, &sl, &sr);
                let master_fx = |l: &mut Vec<f32>, r: &mut Vec<f32>| {
                    let trig = HashMap::new();
                    let ctx = FxContext { step_secs: p.step_secs(), triggers: &trig };
                    let g = db_to_gain(p.master_volume_db);
                    for v in l.iter_mut().chain(r.iter_mut()) { *v *= g; }
                    for fx in &p.master_effects { fx.process(l, r, &ctx); }
                };
                let only: Option<Vec<String>> = a.get("tracks").and_then(|v| v.as_array()).map(|x| x.iter().filter_map(|s| s.as_str().map(|s| s.to_lowercase())).collect());
                let mut stems: Vec<(&str, &render::Stem, bool)> = p.tracks.iter().zip(mix.stems.iter()).map(|(t, s)| ("track", s, !t.mute && (!any_solo || t.solo))).collect();
                if b_or(a, "include_buses", true) {
                    stems.extend(p.buses.iter().zip(mix.bus_stems.iter()).map(|(b, s)| ("bus", s, !b.mute)));
                }
                let mut out = Vec::new();
                let (mut ql, mut qr) = (vec![0.0f32; n], vec![0.0f32; n]);
                for (kind, s, audible) in stems {
                    if let Some(o) = &only {
                        if !o.contains(&s.name.to_lowercase()) { continue; }
                    }
                    let (mut l, mut r) = (s.left.clone(), s.right.clone());
                    if mode == "post_master" {
                        master_fx(&mut l, &mut r);
                    }
                    let direct = kind == "bus" || p.tracks.iter().find(|t| t.name == s.name).and_then(|t| t.output.as_deref()).map(|b| p.bus_index(b).is_err()).unwrap_or(true);
                    if audible && direct {
                        for i in 0..n.min(l.len()) { ql[i] += l[i]; qr[i] += r[i]; }
                    }
                    let path: PathBuf = dir.join(format!("{}{}.{}", if kind == "bus" { "bus_" } else { "" }, samples::sample_name(&s.name), ext(&o.format)));
                    let v = deliver(&l, &r, &path, &o)?;
                    out.push(json!({"name": s.name, "kind": kind, "path": path, "audible": audible, "in_sum": audible && direct,
                        "routed_to": if kind == "bus" { json!("master") } else { json!(p.tracks.iter().find(|t| t.name == s.name).and_then(|t| t.output.clone()).unwrap_or_else(|| "master".into())) },
                        "lufs": v["after"]["integrated_lufs"], "true_peak_dbtp": v["after"]["true_peak_dbtp"], "silent": v["after"]["integrated_lufs"].as_f64().unwrap_or(-70.0) <= -69.0}));
                }
                let (sum_check, verdict) = if mode == "pre_master" {
                    (pre_resid, if pre_resid < -60.0 { "stems sum back to the pre-master mix (null within -60 dB)" } else { "stems do NOT fully null against the pre-master mix (check pre-fader sends / bus routing)" })
                } else {
                    let r = resid(&mix.left, &mix.right, &ql, &qr);
                    (r, if r < -40.0 { "post-master stems sum close to the master" } else { "post-master stems do not sum to the master: the master chain is nonlinear (compressor/limiter/clipper react to the full mix)" })
                };
                Ok(json!({"dir": dir, "mode": mode, "format": o.format, "count": out.len(), "seconds": (mix.seconds * 100.0).round() / 100.0,
                    "sum_residual_db": sum_check, "sums_to": if mode == "pre_master" { "pre-master mix" } else { "master" }, "verdict": verdict,
                    "master_chain": p.master_effects.iter().map(|x| x.type_name()).collect::<Vec<_>>(), "stems": out}))
            },
        },
        Tool {
            name: "master_assistant",
            description: "Set up the master for delivery: keeps/adds a true-peak limiter LAST on the master chain at the ceiling, an optional style stage (clean: nothing; punchy: gentle glue compressor; loud: soft clipper before the limiter), optional tonal match to a reference file (parametric EQ moves from compare_to_reference), then iterates a gain stage before the limiter (re-rendering) until integrated loudness is within 0.3 LU of target_lufs. dry_run:true reports the plan without changing anything.",
            mutates: true,
            schema: || obj(json!({
                "target_lufs": {"type": "number", "description": "default -14"},
                "true_peak_ceiling": {"type": "number", "description": "default -1 dBTP"},
                "style": {"type": "string", "enum": ["clean", "punchy", "loud"], "description": "default clean"},
                "reference": {"type": "string", "description": "Reference audio path: match its tonal balance (and loudness when target_lufs is omitted)"},
                "max_iterations": {"type": "integer", "description": "default 4"},
                "dry_run": {"type": "boolean"},
            }), &[]),
            run: |e, a| {
                let ceiling = f_opt(a, "true_peak_ceiling").unwrap_or(-1.0);
                let style = s_opt(a, "style").unwrap_or_else(|| "clean".into());
                let mut plan = Vec::new();
                let mut p = e.project.clone();
                let mut target = f_opt(a, "target_lufs");
                // reference tone match
                if let Some(rp) = s_opt(a, "reference") {
                    let path = e.resolve(&rp);
                    let (rl, rr) = samples::decode_stereo(&path).with_context(|| format!("reading reference {}", path.display()))?;
                    let rf = audio_edit::profile("reference", &rl, &rr);
                    if target.is_none() { target = Some(rf.integrated_lufs.clamp(-20.0, -6.0)); }
                    let m = e.render_version(&p)?;
                    let mp = audio_edit::profile("mix", &m.left, &m.right);
                    let (deltas, _) = audio_edit::compare(&mp, &rf);
                    let bands: Vec<Value> = deltas.iter().filter(|d| d.metric.starts_with("band_")).filter_map(|d| d.fix.as_ref()).filter_map(|f| f["args"]["params"]["bands"][0].as_object().cloned().map(Value::Object)).collect();
                    if !bands.is_empty() {
                        let fx: Effect = serde_json::from_value(json!({"type": "parametric_eq", "bands": bands})).context("tone-match eq")?;
                        let at = limiter_pos(&p.master_effects).unwrap_or(p.master_effects.len());
                        p.master_effects.insert(at, fx);
                        plan.push(json!({"step": "tone_match_eq", "bands": bands}));
                    }
                }
                let target = target.unwrap_or(-14.0);
                // style stage
                let has = |p: &crate::project::Project, f: fn(&Effect) -> bool| p.master_effects.iter().any(f);
                match style.as_str() {
                    "punchy" if !has(&p, |e| matches!(e, Effect::Compressor(_))) => {
                        let at = limiter_pos(&p.master_effects).unwrap_or(p.master_effects.len());
                        p.master_effects.insert(at, serde_json::from_value(json!({"type": "compressor", "threshold_db": -14.0, "ratio": 2.0, "attack_ms": 30.0, "release_ms": 150.0}))?);
                        plan.push(json!({"step": "glue_compressor"}));
                    }
                    "loud" if !has(&p, |e| matches!(e, Effect::SoftClipper(_))) => {
                        let at = limiter_pos(&p.master_effects).unwrap_or(p.master_effects.len());
                        p.master_effects.insert(at, serde_json::from_value(json!({"type": "soft_clipper", "threshold_db": -6.0, "ceiling_db": -0.3}))?);
                        plan.push(json!({"step": "soft_clipper"}));
                    }
                    "clean" | "punchy" | "loud" => {}
                    s => bail!("unknown style '{s}' (clean, punchy, loud)"),
                }
                // limiter last
                let lim_ceiling = ceiling - 0.3;
                match limiter_pos(&p.master_effects) {
                    Some(i) => {
                        let mut lim = p.master_effects.remove(i);
                        if let Effect::Limiter(l) = &mut lim { l.ceiling_db = lim_ceiling; l.true_peak = true; l.bypass = false; }
                        p.master_effects.push(lim);
                        plan.push(json!({"step": "limiter_last", "ceiling_db": lim_ceiling, "moved_from": i}));
                    }
                    None => {
                        p.master_effects.push(Effect::Limiter(LimiterFx { ceiling_db: lim_ceiling, release_ms: 80.0, true_peak: true, bypass: false }));
                        plan.push(json!({"step": "add_limiter", "ceiling_db": lim_ceiling}));
                    }
                }
                // gain stage directly before the limiter
                let li = p.master_effects.len() - 1;
                if !(li > 0 && matches!(p.master_effects[li - 1], Effect::Gain(_))) {
                    p.master_effects.insert(li, Effect::Gain(GainFx { db: 0.0, bypass: false }));
                }
                let gi = p.master_effects.len() - 2;
                let mut iters = Vec::new();
                let max_it = u_or(a, "max_iterations", 4).clamp(1, 8);
                let mut last = analysis::loudness(&[], &[]);
                for k in 0..max_it {
                    let m = e.render_version(&p)?;
                    last = analysis::loudness(&m.left, &m.right);
                    let err = target - last.integrated_lufs;
                    let g = if let Effect::Gain(gfx) = &p.master_effects[gi] { gfx.db } else { 0.0 };
                    iters.push(json!({"iteration": k + 1, "gain_db": g, "integrated_lufs": last.integrated_lufs, "true_peak_dbtp": last.true_peak_dbtp}));
                    if err.abs() <= 0.3 || last.integrated_lufs <= -69.0 { break; }
                    // a limiter makes gain->loudness sub-linear; over-step a little when pushing
                    let step = if err > 0.0 { err * 1.15 } else { err };
                    if let Effect::Gain(gfx) = &mut p.master_effects[gi] { gfx.db = (gfx.db + step).clamp(-24.0, 24.0); }
                }
                let reached = (target - last.integrated_lufs).abs() <= 0.5;
                let chain: Vec<String> = p.master_effects.iter().map(|x| x.type_name()).collect();
                let dry = b_or(a, "dry_run", false);
                if !dry {
                    e.project.master_effects = p.master_effects.clone();
                }
                Ok(json!({
                    "applied": !dry, "target_lufs": target, "true_peak_ceiling": ceiling, "style": style,
                    "integrated_lufs": last.integrated_lufs, "true_peak_dbtp": last.true_peak_dbtp,
                    "reached_target": reached, "plan": plan, "iterations": iters, "master_chain": chain,
                    "note": if reached { "Now check_master, then export_audio (it re-measures after encoding)." } else { "Target not reached: the limiter is working hard or the mix is too dynamic. Lower the target, try style 'loud', or fix the arrangement level (analyze_sections)." },
                }))
            },
        },
        Tool {
            name: "analyze_reference",
            description: "Profile a reference track (file path; optional start_s/end_s to profile only e.g. its chorus): integrated/short-term LUFS, loudness range, true peak, crest, centroid, band balance, stereo width (side/mid), BPM and key, plus a 32-point log spectrum. Feed it to compare_to_reference or master_assistant.",
            mutates: false,
            schema: || obj(json!({"path": {"type": "string"}, "start_s": {"type": "number"}, "end_s": {"type": "number"}}), &["path"]),
            run: |e, a| {
                let path = e.resolve(&s_req(a, "path")?);
                let (l, r) = samples::decode_stereo(&path).with_context(|| format!("reading {}", path.display()))?;
                let (s0, s1) = window_s(a, l.len().min(r.len()));
                let mut v = serde_json::to_value(audio_edit::profile(&path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(), &l[s0..s1], &r[s0..s1]))?;
                v["path"] = json!(path);
                let mono: Vec<f32> = (s0..s1).map(|i| 0.5 * (l[i] + r[i])).collect();
                v["spectrum_db"] = json!(analysis::log_spectrum_db(&mono, 32).iter().map(|d| (d * 10.0).round() / 10.0).collect::<Vec<_>>());
                Ok(v)
            },
        },
        Tool {
            name: "compare_to_reference",
            description: "A/B the mix (or one section/window of it) against a reference file (optionally its own start_s/end_s window, e.g. the reference's chorus vs your hook): loudness, per-band tonal balance in dB, crest/dynamics and stereo width, each with a verdict and the exact tool call that closes the gap, plus a 0-100 match score. Matches SOUND only, never composition.",
            mutates: false,
            schema: || obj(merge_props(region_props(), json!({
                "reference": {"type": "string", "description": "Reference audio path"},
                "ref_start_s": {"type": "number"}, "ref_end_s": {"type": "number"},
            })), &["reference"]),
            run: |e, a| {
                let path = e.resolve(&s_req(a, "reference")?);
                let (rl, rr) = samples::decode_stereo(&path).with_context(|| format!("reading {}", path.display()))?;
                let n = rl.len().min(rr.len());
                let r0 = ((f_opt(a, "ref_start_s").unwrap_or(0.0).max(0.0) * SR) as usize).min(n);
                let r1 = (f_opt(a, "ref_end_s").map(|x| (x * SR) as usize).unwrap_or(n)).clamp(r0, n);
                if r1 <= r0 { return Err(anyhow!("empty reference window")); }
                let rf = audio_edit::profile("reference", &rl[r0..r1], &rr[r0..r1]);
                let m = e.mix()?;
                let p = e.project.clone();
                let (s0, s1, label) = region(&p, a, m.left.len().min(m.right.len()))?;
                let mp = audio_edit::profile("mix", &m.left[s0..s1], &m.right[s0..s1]);
                let (deltas, score) = audio_edit::compare(&mp, &rf);
                Ok(json!({"window": label, "reference": path, "score": score, "deltas": deltas, "mix": mp, "reference_profile": rf}))
            },
        },
    ]
}

fn window_s(a: &Value, n: usize) -> (usize, usize) {
    let s0 = ((f_opt(a, "start_s").unwrap_or(0.0).max(0.0) * SR) as usize).min(n);
    let s1 = f_opt(a, "end_s")
        .map(|x| (x * SR) as usize)
        .unwrap_or(n)
        .clamp(s0, n);
    if s1 <= s0 {
        (0, n)
    } else {
        (s0, s1)
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::Engine;
    use serde_json::json;

    fn eng(dir: &str) -> Engine {
        let mut e = Engine::new(std::env::temp_dir().join(dir));
        e.call("generate_beat", &json!({"style": "lofi", "seed": 11}))
            .unwrap();
        e
    }

    #[test]
    fn export_formats_meet_ceiling() {
        let mut e = eng("beatbox_delivery_tests");
        let w = e.call("export_audio", &json!({"format": "wav", "bits": 16, "target_lufs": -10, "path": "out/a.wav", "section": 0})).unwrap();
        assert_eq!(w["passed"], true, "{w}");
        assert!(w["after"]["true_peak_dbtp"].as_f64().unwrap() <= -0.95);
        assert!(
            (w["after"]["integrated_lufs"].as_f64().unwrap() + 10.0).abs() < 1.0,
            "{w}"
        );
        let f = e
            .call(
                "export_audio",
                &json!({"format": "flac", "path": "out/a.flac", "section": 0}),
            )
            .unwrap();
        assert!(f["bytes"].as_u64().unwrap() > 1000);
        if super::ffmpeg_available() {
            let m = e.call("export_audio", &json!({"format": "mp3", "target_lufs": -9, "true_peak_ceiling": -1, "path": "out/a.mp3", "section": 0})).unwrap();
            assert!(
                m["after"]["true_peak_dbtp"].as_f64().unwrap() <= -0.95,
                "{m}"
            );
        }
        let s = e
            .call(
                "export_stems",
                &json!({"dir": "out/stems", "format": "flac", "bits": 16}),
            )
            .unwrap();
        assert!(s["count"].as_u64().unwrap() >= 3, "{s}");
    }

    #[test]
    fn master_assistant_hits_target_and_reference_compare() {
        let mut e = eng("beatbox_master_tests");
        let r = e
            .call(
                "master_assistant",
                &json!({"target_lufs": -11, "style": "punchy"}),
            )
            .unwrap();
        assert_eq!(r["reached_target"], true, "{r}");
        assert_eq!(
            e.project.master_effects.last().unwrap().type_name(),
            "limiter"
        );
        e.call("export_audio", &json!({"path": "ref.wav", "bits": 24}))
            .unwrap();
        let p = e
            .call("analyze_reference", &json!({"path": "ref.wav"}))
            .unwrap();
        assert!(p["integrated_lufs"].as_f64().unwrap() > -13.0, "{p}");
        let c = e
            .call("compare_to_reference", &json!({"reference": "ref.wav"}))
            .unwrap();
        assert!(c["score"].as_u64().unwrap() >= 90, "{c}");
    }
}
