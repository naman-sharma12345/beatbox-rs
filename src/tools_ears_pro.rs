//! MCP tools for AI ears v3: stereo_image, punch, vocal_pocket,
//! reference_match (v2 of compare_to_reference).

use crate::dsp::SR;
use crate::ears;
use crate::ears_pro as ep;
use crate::engine::Engine;
use crate::render::{self, RenderOptions};
use crate::samples;
use crate::tools::{b_or, f_opt, obj, s_opt, Tool};
use crate::tools_ears::{region, render_track};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

fn src_props() -> Value {
    json!({
        "track": {"type": "string", "description": "Analyse this track alone (soloed through the master chain)"},
        "path": {"type": "string", "description": "Analyse an audio file instead of the project"},
        "snapshot": {"type": "string", "description": "Analyse a saved snapshot (name) instead of the current project"},
        "section": {"description": "Only this arrangement section (index or pattern name)", "type": ["string", "integer"]},
        "start_s": {"type": "number"}, "end_s": {"type": "number"},
    })
}

/// (label, left, right, is_project, project used)
fn source(e: &mut Engine, a: &Value) -> Result<(String, Vec<f32>, Vec<f32>, bool)> {
    if let Some(p) = s_opt(a, "path") {
        let path = e.resolve(&p);
        let (l, r) =
            samples::decode_stereo(&path).with_context(|| format!("reading {}", path.display()))?;
        return Ok((path.display().to_string(), l, r, false));
    }
    if let Some(t) = s_opt(a, "track") {
        let m = render_track(e, &t)?;
        return Ok((format!("track {t}"), m.left, m.right, true));
    }
    let m = e.mix()?;
    Ok(("mix".into(), m.left.clone(), m.right.clone(), true))
}

/// Run `f` with the engine switched to a snapshot (when `snapshot` is set).
fn with_snapshot<T>(
    e: &mut Engine,
    a: &Value,
    f: impl FnOnce(&mut Engine) -> Result<T>,
) -> Result<T> {
    let Some(name) = s_opt(a, "snapshot") else {
        return f(e);
    };
    let p = e.version(&name)?;
    let saved = std::mem::replace(&mut e.project, p);
    e.revision += 1;
    let r = f(e);
    e.project = saved;
    e.revision += 1;
    r
}

fn window(e: &Engine, a: &Value, n: usize, is_project: bool) -> Result<(usize, usize, String)> {
    if is_project {
        region(&e.project, a, n)
    } else {
        let s0 = ((f_opt(a, "start_s").unwrap_or(0.0).max(0.0)) * SR) as usize;
        let s1 = f_opt(a, "end_s")
            .map(|x| (x * SR) as usize)
            .unwrap_or(n)
            .min(n);
        if s1 <= s0.min(n) {
            return Err(anyhow!("empty window"));
        }
        Ok((s0.min(n), s1, "file".into()))
    }
}

fn section_ranges(e: &Engine, n: usize) -> Vec<(String, usize, usize)> {
    ears::spans(&e.project)
        .iter()
        .map(|s| {
            let a = ((s.start_s * SR) as usize).min(n);
            let b = ((s.end_s * SR) as usize).min(n);
            (s.pattern.clone(), a, b)
        })
        .filter(|x| x.2 > x.1 + 4410)
        .collect()
}

fn stereo_tracks(e: &Engine) -> Vec<(String, f32)> {
    e.project
        .tracks
        .iter()
        .filter(|t| {
            matches!(
                crate::tools_mix::role_of(&t.name, &t.instrument),
                "bass" | "kick"
            )
        })
        .map(|t| {
            let fx_w = t
                .effects
                .iter()
                .filter_map(|x| match x {
                    crate::fx::Effect::Width(w) => Some(w.amount),
                    _ => None,
                })
                .fold(0.0f32, f32::max);
            (
                t.name.clone(),
                t.instrument.stereo_spread() + fx_w + t.pan.abs(),
            )
        })
        .collect()
}

pub fn stereo_image_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    with_snapshot(e, a, |e| {
        let (name, l, r, is_p) = source(e, a)?;
        let n = l.len().min(r.len());
        let (s0, s1, label) = window(e, a, n, is_p)?;
        let secs = if is_p && b_or(a, "per_section", true) {
            section_ranges(e, n)
                .into_iter()
                .filter(|x| x.1 >= s0 && x.2 <= s1)
                .map(|(nm, x0, x1)| (nm, x0 - s0, x1 - s0))
                .collect()
        } else {
            Vec::new()
        };
        let hint = if is_p { stereo_tracks(e) } else { Vec::new() };
        let mut v = ep::stereo_image(&l[s0..s1], &r[s0..s1], &secs, &hint);
        v["source"] = json!(name);
        v["window"] = json!(label);
        Ok(v)
    })
}

pub fn punch_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    with_snapshot(e, a, |e| {
        if let Some(p) = s_opt(a, "path") {
            let path = e.resolve(&p);
            let (l, r) = samples::decode_stereo(&path)?;
            let on = ep::audio_onsets(&l, &r);
            let pm = ep::punch_at(&l, &r, &on);
            return Ok(
                json!({"source": path.display().to_string(), "master": ep::punch_json("file", &pm), "onsets": on.len(), "findings": ep::punch_findings(&pm, None, false)}),
            );
        }
        let kicks = ep::role_onsets(&e.project, &["kick"]);
        let snares = ep::role_onsets(&e.project, &["snare"]);
        let both: Vec<usize> = {
            let mut v = kicks.clone();
            v.extend(&snares);
            v.sort_unstable();
            v
        };
        if both.is_empty() {
            return Err(anyhow!(
                "no kick/snare hits in the project: punch is measured at drum onsets"
            ));
        }
        let m = e.mix()?;
        let pm = ep::punch_at(&m.left, &m.right, &both);
        let pk = ep::punch_at(&m.left, &m.right, &kicks);
        let ps = ep::punch_at(&m.left, &m.right, &snares);
        drop(m);
        // pre-master render: what the master chain does to the transients
        let pre = if b_or(a, "compare_premaster", true) {
            let mut p = e.project.clone();
            p.master_effects.clear();
            e.bank.sync(&p.samples);
            let mm = render::render(&p, &e.bank, &RenderOptions::default())?;
            Some(ep::punch_at(&mm.left, &mm.right, &both))
        } else {
            None
        };
        let mut tracks = Vec::new();
        let names: Vec<String> = match a.get("tracks").and_then(|v| v.as_array()) {
            Some(v) => v
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect(),
            None => Vec::new(),
        };
        for t in names {
            let ti = e.project.track_index(&t)?;
            let tr = e.project.tracks[ti].clone();
            let (ev, _) = render::schedule(&e.project, &Default::default());
            let on: Vec<usize> = ev[ti].iter().map(|x| x.start).collect();
            let mm = render_track(e, &tr.name)?;
            tracks.push(ep::punch_json(&t, &ep::punch_at(&mm.left, &mm.right, &on)));
        }
        let drum_bus = e.project.buses.iter().any(|b| b.name == "drums");
        let loss = pre.as_ref().map(|p| r1(p.transient_to_sustain_db - pm.transient_to_sustain_db));
        Ok(json!({
            "master": ep::punch_json("kick+snare", &pm),
            "kick": ep::punch_json("kick", &pk),
            "snare": ep::punch_json("snare", &ps),
            "pre_master": pre.as_ref().map(|p| ep::punch_json("pre-master kick+snare", p)),
            "limiter_transient_loss_db": loss,
            "tracks": tracks,
            "findings": ep::punch_findings(&pm, pre.as_ref(), drum_bus),
            "note": "punch_index_db = first 10 ms over the median of the surrounding 200 ms at kick/snare onsets (5+ dB reads as punchy); transient_to_sustain_db = peak 0-15 ms over RMS 30-150 ms",
        }))
    })
}

pub fn vocal_pocket_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    with_snapshot(e, a, |e| {
        let vocal_track = s_opt(a, "vocal_track");
        let rel = f_opt(a, "vocal_level_lu").unwrap_or(-3.0);
        // the beat without the vocal, and the vocal alone (or a proxy)
        let (bl, br, vocal, proxy) = match &vocal_track {
            Some(v) => {
                let vi = e.project.track_index(v)?;
                let mut p = e.project.clone();
                p.tracks[vi].mute = true;
                e.bank.sync(&p.samples);
                let beat = render::render(&p, &e.bank, &RenderOptions::default())?;
                let voc = render_track(e, v)?;
                let mono: Vec<f32> = voc
                    .left
                    .iter()
                    .zip(&voc.right)
                    .map(|(x, y)| 0.5 * (x + y))
                    .collect();
                (beat.left, beat.right, mono, false)
            }
            None => {
                let m = e.mix()?;
                let mut v = ep::proxy_vocal(m.left.len(), e.project.bpm, 7);
                ep::level_match(&m.left, &m.right, &mut v, rel);
                (m.left.clone(), m.right.clone(), v, true)
            }
        };
        let n = bl.len().min(br.len()).min(vocal.len());
        let mut secs = section_ranges(e, n);
        if secs.is_empty() {
            secs.push(("whole".into(), 0, n));
        }
        if let Some(s) = s_opt(a, "section") {
            secs.retain(|x| x.0 == s);
        }
        let mut out = Vec::new();
        let mut worst: Option<(f32, String)> = None;
        for (name, s0, s1) in &secs {
            let ps = ep::vocal_pocket_section(name, &bl[*s0..*s1], &br[*s0..*s1], &vocal[*s0..*s1]);
            if worst.as_ref().map(|w| ps.free_space < w.0).unwrap_or(true) {
                worst = Some((ps.free_space, name.clone()));
            }
            out.push(json!({"section": name, "free_space_score": ps.free_space,
                "beat_occupancy_db": ps.occupancy_db.iter().map(|(z, d)| json!({"zone": z, "db_of_total": d})).collect::<Vec<_>>(),
                "vocal_masked": ps.masked.iter().map(|(z, m)| json!({"zone": z, "probability": m})).collect::<Vec<_>>()}));
        }
        // who fills the intelligibility zone (1-4 kHz) in the worst section
        let mut competing = Vec::new();
        let mut findings = Vec::new();
        if let Some((score, sec)) = &worst {
            if b_or(a, "per_track", true) {
                let (s0, s1) = secs
                    .iter()
                    .find(|x| &x.0 == sec)
                    .map(|x| (x.1, x.2))
                    .unwrap_or((0, n));
                let cands: Vec<String> = e
                    .project
                    .tracks
                    .iter()
                    .filter(|t| !t.mute && Some(&t.name) != vocal_track.as_ref())
                    .filter(|t| {
                        !matches!(
                            crate::tools_mix::role_of(&t.name, &t.instrument),
                            "kick" | "bass"
                        )
                    })
                    .map(|t| t.name.clone())
                    .collect();
                for t in cands.iter().take(8) {
                    let m = render_track(e, t)?;
                    let s1 = s1.min(m.left.len());
                    if s1 <= s0 {
                        continue;
                    }
                    let mono: Vec<f32> = m.left[s0..s1]
                        .iter()
                        .zip(&m.right[s0..s1])
                        .map(|(x, y)| 0.5 * (x + y))
                        .collect();
                    let z = ep::band(&mono, 1000.0, 4000.0);
                    let en: f64 = z.iter().map(|v| (*v as f64).powi(2)).sum::<f64>()
                        / z.len().max(1) as f64;
                    competing.push((t.clone(), (10.0 * en.max(1e-12).log10()) as f32));
                }
                competing.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap());
            }
            if *score < 60.0 {
                let culprit = competing.first().map(|c| c.0.clone());
                let mut msg = format!(
                    "the beat leaves little room for a rap vocal in '{sec}' (free space {score:.0}/100 in 1-4 kHz)"
                );
                if let Some(c) = &culprit {
                    msg.push_str(&format!("; '{c}' fills that band most"));
                }
                findings.push(json!({"severity": ((60.0 - score) / 60.0).clamp(0.2, 0.8), "message": msg,
                    "suggested_call": culprit.map(|c| json!({"tool": "add_effect", "args": {"track": c, "type": "dynamic_eq", "params": {"freq": 2500.0, "q": 0.9, "threshold_db": -30.0, "range_db": -4.0}}}))}));
            }
        }
        Ok(json!({
            "vocal": if proxy { json!({"proxy": true, "level_lu_vs_beat": rel, "note": "proxy rap vocal (speech-shaped noise on a 16th flow); unvalidated, use to compare versions"}) } else { json!({"track": vocal_track}) },
            "sections": out,
            "worst_section": worst.map(|w| json!({"section": w.1, "free_space_score": w.0})),
            "competing_tracks_1_4khz": competing.iter().map(|(t, d)| json!({"track": t, "level_db": r1(*d)})).collect::<Vec<_>>(),
            "findings": findings,
        }))
    })
}

pub fn reference_match_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let rsrc = s_opt(a, "reference").ok_or_else(|| anyhow!("missing 'reference'"))?;
    // the reference: an audio file, or a snapshot of this project
    let (rname, rl, rr) = if let Ok(p) = e.version(&rsrc) {
        e.bank.sync(&p.samples);
        let m = render::render(&p, &e.bank, &RenderOptions::default())?;
        (format!("snapshot {rsrc}"), m.left, m.right)
    } else {
        let path = e.resolve(&rsrc);
        let (l, r) =
            samples::decode_stereo(&path).with_context(|| format!("reading {}", path.display()))?;
        (path.display().to_string(), l, r)
    };
    let m = e.mix()?;
    let (ml, mr) = (m.left.clone(), m.right.clone());
    drop(m);
    let n = ml.len().min(mr.len());
    // section alignment: our hook / verse vs the reference's loudest / median windows
    let spans = ears::spans(&e.project);
    let pick = |kind: &str| {
        spans
            .iter()
            .filter(|s| ears::role(&s.pattern) == kind)
            .max_by(|x, y| (x.end_s - x.start_s).partial_cmp(&(y.end_s - y.start_s)).unwrap())
            .map(|s| (((s.start_s * SR) as usize).min(n), ((s.end_s * SR) as usize).min(n)))
    };
    let hook = pick("hook");
    let verse = pick("verse");
    let len_s = hook
        .map(|h| (h.1 - h.0) as f32 / SR)
        .unwrap_or(16.0)
        .clamp(6.0, 30.0);
    let rwin = ep::ref_windows(&rl, &rr, len_s);
    let mut pairs = Vec::new();
    for (kind, mine) in [("hook", hook), ("verse", verse)] {
        if let (Some((a0, a1)), Some(rw)) = (mine, rwin.iter().find(|w| w.0 == kind)) {
            pairs.push((kind, a0, a1, rw.1, rw.2));
        }
    }
    if pairs.is_empty() {
        let rw = &rwin[0];
        pairs.push(("whole", 0, n, rw.1, rw.2));
    }
    let names: Vec<&str> = ep::TONAL.iter().map(|t| t.0).collect();
    let mut per_section = Vec::new();
    let mut actions: Vec<(f32, Value)> = Vec::new();
    let mut penalty = 0.0f32;
    for (kind, a0, a1, b0, b1) in &pairs {
        let (xl, xr) = (&ml[*a0..*a1], &mr[*a0..*a1]);
        let (yl, yr) = (&rl[*b0..*b1], &rr[*b0..*b1]);
        let tm = ep::tonal_shape(xl, xr);
        let tr = ep::tonal_shape(yl, yr);
        let tonal: Vec<Value> = names
            .iter()
            .enumerate()
            .map(|(i, nm)| json!({"band": nm, "delta_db": r1(tm[i] - tr[i])}))
            .collect();
        let im = ep::image(xl, xr);
        let ir = ep::image(yl, yr);
        let lm = crate::analysis::loudness(xl, xr);
        let lr = crate::analysis::loudness(yl, yr);
        let (pm, pr) = (ep::psr(xl, xr), ep::psr(yl, yr));
        let punch_m = ep::punch_at(xl, xr, &ep::audio_onsets(xl, xr));
        let punch_r = ep::punch_at(yl, yr, &ep::audio_onsets(yl, yr));
        if *kind != "verse" {
            for (i, nm) in names.iter().enumerate() {
                let d = tm[i] - tr[i];
                if d.abs() > 2.0 && tr[i] > -40.0 {
                    penalty += (d.abs() - 2.0).min(8.0) * 1.5;
                    let (kindq, freq) = match *nm {
                        "sub" => ("low_shelf", 50.0),
                        "bass" => ("bell", 120.0),
                        "low_mid" => ("bell", 450.0),
                        "mid" => ("bell", 1600.0),
                        "presence" => ("bell", 5000.0),
                        _ => ("high_shelf", 10000.0),
                    };
                    let g = (-d * 0.6).clamp(-4.0, 4.0);
                    actions.push((d.abs() / 6.0, json!({"why": format!("{kind}: {nm} is {d:+.1} dB vs the reference (loudness-matched)"), "tool": "add_effect", "args": {"track": "master", "type": "parametric_eq", "params": {"bands": [{"kind": kindq, "freq": freq, "gain_db": r1(g), "q": 0.8}]}}})));
                }
            }
            let dw = im.width - ir.width;
            if dw.abs() > 0.12 {
                penalty += dw.abs() * 20.0;
                actions.push((dw.abs(), json!({"why": format!("{kind}: stereo width {:.2} vs reference {:.2}", im.width, ir.width), "tool": "add_effect", "args": {"track": "master", "type": "width", "params": {"amount": r1((1.0 - dw).clamp(0.6, 1.5))}}})));
            }
            let dp = pm - pr;
            if dp < -2.0 {
                penalty += (-dp - 2.0) * 2.0;
                actions.push(((-dp) / 6.0, json!({"why": format!("{kind}: PSR {pm:.1} dB vs reference {pr:.1}: the master is squashed harder"), "tool": "master_assistant", "args": {"target_lufs": r1(lr.integrated_lufs.max(lm.integrated_lufs - 1.0))}})));
            }
            if punch_r.hits > 2 && punch_m.hits > 2 && punch_m.punch_index_db + 2.0 < punch_r.punch_index_db {
                penalty += 5.0;
                actions.push((0.5, json!({"why": format!("{kind}: drums punch {:.1} dB vs reference {:.1}", punch_m.punch_index_db, punch_r.punch_index_db), "tool": "add_effect", "args": {"track": "kick", "type": "transient", "params": {"attack": 0.4}}})));
            }
        }
        per_section.push(json!({
            "pair": {"mine": kind, "mine_s": [r1(*a0 as f32 / SR), r1(*a1 as f32 / SR)], "reference_s": [r1(*b0 as f32 / SR), r1(*b1 as f32 / SR)]},
            "loudness_lufs": {"mine": r1(lm.integrated_lufs), "reference": r1(lr.integrated_lufs)},
            "tonal_delta_db": tonal,
            "width": {"mine": (im.width * 100.0).round() / 100.0, "reference": (ir.width * 100.0).round() / 100.0},
            "psr_db": {"mine": r1(pm), "reference": r1(pr)},
            "punch_index_db": {"mine": punch_m.punch_index_db, "reference": punch_r.punch_index_db},
        }));
    }
    // hook-over-verse lift, mine vs the reference's
    let lift = |x0: usize, x1: usize, y0: usize, y1: usize, l: &[f32], r: &[f32]| {
        crate::analysis::loudness(&l[x0..x1], &r[x0..x1]).integrated_lufs
            - crate::analysis::loudness(&l[y0..y1], &r[y0..y1]).integrated_lufs
    };
    let mut contrast = Value::Null;
    if pairs.len() == 2 {
        let (h, v) = (&pairs[0], &pairs[1]);
        let mine = lift(h.1, h.2, v.1, v.2, &ml, &mr);
        let theirs = lift(h.3, h.4, v.3, v.4, &rl, &rr);
        contrast = json!({"hook_over_verse_lu": {"mine": r1(mine), "reference": r1(theirs)}});
        if mine + 1.0 < theirs {
            penalty += 6.0;
            actions.push((0.6, json!({"why": format!("hook lifts {mine:.1} LU over the verse, the reference {theirs:.1} LU"), "tool": "set_section_mix", "args": {"section": "verse", "track": "master", "volume_db": r1(-(theirs - mine).min(3.0)), "all_occurrences": true}})));
        }
    }
    // tempo / key for context (sound is matched, composition never copied)
    let mp = crate::audio_edit::profile("mix", &ml, &mr);
    let rp = crate::audio_edit::profile("reference", &rl, &rr);
    actions.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap());
    let li = crate::analysis::loudness(&ml, &mr).integrated_lufs;
    let lri = crate::analysis::loudness(&rl, &rr).integrated_lufs;
    if (li - lri).abs() > 1.0 {
        actions.push((0.3, json!({"why": format!("integrated {li:.1} vs reference {lri:.1} LUFS"), "tool": "master_assistant", "args": {"target_lufs": r1(lri)}})));
    }
    Ok(json!({
        "reference": rname,
        "match_score": (100.0 - penalty).clamp(0.0, 100.0).round(),
        "alignment": {"pairs": pairs.iter().map(|p| p.0).collect::<Vec<_>>(), "reference_windows": "hook = loudest sustained window, verse = median-loud window, each the length of our hook",
            "tempo": {"mine": e.project.bpm, "reference": rp.bpm, "reference_confidence": rp.bpm_confidence},
            "key": {"mine": format!("{} {}", e.project.key_root, e.project.scale), "detected_mine": mp.key, "reference": rp.key}},
        "integrated_lufs": {"mine": r1(li), "reference": r1(lri)},
        "per_section": per_section,
        "contrast": contrast,
        "actions": actions.into_iter().map(|x| x.1).take(8).collect::<Vec<_>>(),
        "note": "tonal deltas are loudness-matched (band share of each window's own energy); actions are ordered by audible impact",
    }))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "stereo_image",
            description: "Stereo image per band (20-120, 120-500, 500-2k, 2k-8k, 8k-20k Hz) of the mix, a soloed track, a snapshot or a file: correlation, width (side/mid), L/R balance, mono-fold loss, whether the low end is mono, out-of-phase windows, and per-section width. Findings come with the tool call that fixes them.",
            mutates: false,
            schema: || {
                let mut p = src_props();
                p["per_section"] = json!({"type": "boolean", "description": "default true"});
                obj(p, &[])
            },
            run: stereo_image_tool,
        },
        Tool {
            name: "punch",
            description: "Transient and punch metrics at the kick/snare onsets: punch index (first 10 ms over the surrounding 200 ms), attack time, transient-to-sustain, micro crest, separately for kick and snare, plus how much the master chain flattens them (pre- vs post-master). Optional per-track solo measurements. Works on a file too (onsets detected from audio).",
            mutates: false,
            schema: || {
                obj(
                    json!({
                        "tracks": {"type": "array", "items": {"type": "string"}, "description": "Also measure these tracks soloed"},
                        "compare_premaster": {"type": "boolean", "description": "Render without the master chain to measure limiter/compressor transient loss (default true)"},
                        "path": {"type": "string", "description": "Measure an audio file instead"},
                        "snapshot": {"type": "string"},
                    }),
                    &[],
                )
            },
            run: punch_tool,
        },
        Tool {
            name: "vocal_pocket",
            description: "How much room the beat leaves for a rap vocal, per section: beat energy in the body (200-500 Hz), intelligibility (1-4 kHz) and sibilance (5-8 kHz) zones, the probability a vocal is masked there, a 0-100 free-space score, and which tracks fill 1-4 kHz most, with a dynamic-EQ carve as the fix. Uses vocal_track when you have one, else a proxy rap vocal (speech-shaped noise on a 16th flow at -3 LU vs the beat).",
            mutates: false,
            schema: || {
                obj(
                    json!({
                        "vocal_track": {"type": "string", "description": "A real vocal track (it is muted in the beat render)"},
                        "vocal_level_lu": {"type": "number", "description": "Proxy vocal level relative to the beat (default -3)"},
                        "section": {"type": "string", "description": "Only this section (pattern name)"},
                        "per_track": {"type": "boolean", "description": "Solo-render the candidates to name who fills 1-4 kHz (default true)"},
                        "snapshot": {"type": "string"},
                    }),
                    &[],
                )
            },
            run: vocal_pocket_tool,
        },
        Tool {
            name: "reference_match",
            description: "reference_match v2: A/B the mix against a reference (audio file or a snapshot name), loudness-matched and section-aligned (our hook vs the reference's loudest sustained window, our verse vs its median-loud window): tonal balance per band, width, PSR (squash), punch, hook-over-verse lift, tempo/key for context, a 0-100 match score and an ordered list of exact tool calls that close the gaps. Matches sound, never composition.",
            mutates: false,
            schema: || {
                obj(
                    json!({"reference": {"type": "string", "description": "Audio file path or snapshot name"}}),
                    &["reference"],
                )
            },
            run: reference_match_tool,
        },
    ]
}

#[cfg(test)]
mod tests {
    use crate::engine::Engine;
    use serde_json::json;

    #[test]
    fn ears_v3_tools_on_a_beat() {
        let d = std::env::temp_dir().join("beatbox_ears_pro_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("generate_beat", &json!({"style": "trap", "seed": 4}))
            .unwrap();
        let s = e.call("stereo_image", &json!({})).unwrap();
        assert_eq!(s["bands"].as_array().unwrap().len(), 5);
        assert!(s["low_end_mono_ok"].is_boolean());
        let p = e.call("punch", &json!({"tracks": ["kick"]})).unwrap();
        assert!(p["master"]["hits"].as_u64().unwrap() > 4, "{p}");
        assert!(p["limiter_transient_loss_db"].as_f64().is_some());
        let v = e.call("vocal_pocket", &json!({"per_track": false})).unwrap();
        let sec = v["sections"].as_array().unwrap();
        assert!(!sec.is_empty());
        let fs = sec[0]["free_space_score"].as_f64().unwrap();
        assert!((0.0..=100.0).contains(&fs));
        // a snapshot as the reference: the same project matches itself
        e.call("snapshot", &json!({"name": "a"})).unwrap();
        let r = e.call("reference_match", &json!({"reference": "a"})).unwrap();
        assert!(r["match_score"].as_f64().unwrap() >= 90.0, "{r}");
    }
}
