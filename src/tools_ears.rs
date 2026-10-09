//! "AI ears" tools: per-section analysis, artifact detection, spectrogram
//! images, audio previews, waveform peaks, spectra and EQ curves. Images
//! and audio come back as MCP content blocks (see `media`).

use crate::analysis;
use crate::dsp::SR;
use crate::ears::{self, ArtifactOptions};
use crate::engine::Engine;
use crate::fx::{Effect, FxContext};
use crate::media;
use crate::project::Project;
use crate::render::{self, Mix, RenderOptions};
use crate::tools::{b_or, f_opt, obj, s_opt, u_or, Tool};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;

/// JSON-schema properties shared by every tool that takes a time window.
pub(crate) fn region_props() -> Value {
    json!({
        "section": {"description": "Arrangement section: index (0-based) or pattern name, e.g. 'hook'", "type": ["string", "integer"]},
        "start_beat": {"type": "number", "description": "Window start in song beats (quarter notes)"},
        "end_beat": {"type": "number"},
        "start_s": {"type": "number", "description": "Window start in seconds"},
        "end_s": {"type": "number"},
    })
}

fn with_props(mut base: Value, extra: Value) -> Value {
    if let (Value::Object(b), Value::Object(e)) = (&mut base, extra) {
        for (k, v) in e {
            b.insert(k, v);
        }
    }
    base
}

/// Resolve a time window (samples) from section / beats / seconds args.
pub(crate) fn region(p: &Project, a: &Value, total: usize) -> Result<(usize, usize, String)> {
    let bs = ears::beat_secs(p);
    let sec = a.get("section").and_then(|v| {
        v.as_str()
            .map(String::from)
            .or_else(|| v.as_u64().map(|n| n.to_string()))
    });
    let (mut s0, mut s1, label) = if let Some(s) = sec {
        let (b0, b1) = p.section_beats(&s)?;
        (b0 * bs, b1 * bs, format!("section {s}"))
    } else if f_opt(a, "start_beat").is_some() || f_opt(a, "end_beat").is_some() {
        let b0 = f_opt(a, "start_beat").unwrap_or(0.0);
        let b1 = f_opt(a, "end_beat").unwrap_or(total as f32 / SR / bs);
        (b0 * bs, b1 * bs, format!("beats {b0}-{b1}"))
    } else if f_opt(a, "start_s").is_some() || f_opt(a, "end_s").is_some() {
        let a0 = f_opt(a, "start_s").unwrap_or(0.0);
        let a1 = f_opt(a, "end_s").unwrap_or(total as f32 / SR);
        (a0, a1, format!("{a0}-{a1} s"))
    } else {
        (0.0, total as f32 / SR, "whole".into())
    };
    if let Some(t) = f_opt(a, "tail_s") {
        s1 += t.max(0.0);
    }
    s0 = s0.max(0.0);
    let (x0, x1) = ears::clamp_range(total, (s0 * SR) as usize, (s1 * SR) as usize);
    if x1 <= x0 {
        bail!(
            "empty window ({label}): the render is {:.2} s long",
            total as f32 / SR
        );
    }
    Ok((x0, x1, label))
}

fn has_region(a: &Value) -> bool {
    ["section", "start_beat", "end_beat", "start_s", "end_s"]
        .iter()
        .any(|k| a.get(*k).is_some())
}

/// Render one track alone (solo), with its buses, through the master chain.
pub(crate) fn render_track(e: &mut Engine, name: &str) -> Result<Mix> {
    let mut p = e.project.clone();
    let ti = p.track_index(name)?;
    for (i, t) in p.tracks.iter_mut().enumerate() {
        t.solo = i == ti;
        if i == ti {
            t.mute = false;
        }
    }
    e.bank.sync(&p.samples);
    render::render(&p, &e.bank, &RenderOptions::default())
}

/// Audio source for the ears: sample, file path, a single track, or the mix.
pub(crate) fn source(e: &mut Engine, a: &Value) -> Result<(String, Vec<f32>, Vec<f32>, bool)> {
    if let Some(t) = s_opt(a, "track") {
        let m = render_track(e, &t)?;
        return Ok((format!("track {t}"), m.left, m.right, true));
    }
    let is_mix = s_opt(a, "sample").is_none() && s_opt(a, "path").is_none();
    let (n, l, r) = crate::tools_sound::audio_for(e, a)?;
    Ok((n, l, r, is_mix))
}

fn source_props() -> Value {
    json!({
        "track": {"type": "string", "description": "Analyse this track alone (soloed render)"},
        "sample": {"type": "string", "description": "A registered sample"},
        "path": {"type": "string", "description": "Any audio file (wav/mp3/flac/ogg)"},
    })
}

fn section_marks(p: &Project, s0: usize, s1: usize) -> Vec<f32> {
    let (t0, t1) = (s0 as f32 / SR, s1 as f32 / SR);
    ears::spans(p)
        .iter()
        .map(|s| s.start_s)
        .filter(|t| *t > t0 && *t < t1)
        .map(|t| t - t0)
        .collect()
}

/// Build a spectrogram PNG of a window; returns (png bytes, path).
fn spectrogram_png(
    e: &Engine,
    l: &[f32],
    r: &[f32],
    w: usize,
    h: usize,
    floor_db: f32,
    marks: &[f32],
    path: Option<String>,
    stem: &str,
) -> Result<(Vec<u8>, std::path::PathBuf)> {
    let rgb = ears::spectrogram_rgb(l, r, w, h, floor_db, marks);
    let png = media::png_rgb(w as u32, h as u32, &rgb)?;
    let path = match path {
        Some(p) => e.resolve(&p),
        None => e.renders_dir().join(format!("{stem}.png")),
    };
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&path, &png)?;
    Ok((png, path))
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

/// A cropped copy of the mix (audio + per-track blocks) for windowed analysis.
pub(crate) fn window_mix(m: &Mix, s0: usize, s1: usize) -> Mix {
    Mix {
        left: m.left[s0..s1].to_vec(),
        right: m.right[s0..s1].to_vec(),
        stems: Vec::new(),
        track_info: window_infos(m, s0, s1),
        bus_stems: Vec::new(),
        seconds: (s1 - s0) as f32 / SR,
    }
}

/// Window the mix's per-track 400 ms blocks to [s0, s1).
fn window_infos(m: &Mix, s0: usize, s1: usize) -> Vec<render::TrackInfo> {
    let (b0, b1) = (s0 / render::INFO_BLOCK, s1.div_ceil(render::INFO_BLOCK));
    m.track_info
        .iter()
        .map(|t| {
            let hi = b1.min(t.blocks.len());
            let lo = b0.min(hi);
            let blocks = t.blocks[lo..hi].to_vec();
            let energy: f64 = blocks
                .iter()
                .map(|v| *v as f64 * 2.0 * render::INFO_BLOCK as f64)
                .sum();
            let active: Vec<f32> = blocks.iter().copied().filter(|b| *b > 1e-6).collect();
            let mut ti = t.clone();
            ti.energy = energy;
            ti.active_percent = (100.0 * active.len() as f32 / blocks.len().max(1) as f32).round();
            ti.active_rms_db = if active.is_empty() {
                -120.0
            } else {
                let ms = active.iter().sum::<f32>() / active.len() as f32;
                ((10.0 * ms.max(1e-12).log10()) * 10.0).round() / 10.0
            };
            ti.blocks = blocks;
            ti
        })
        .collect()
}

/// Impulse response magnitude (dB) of the linear EQ/filter effects in a chain.
pub(crate) fn eq_curve(effects: &[Effect], step_secs: f32, points: usize) -> Vec<(f32, f32)> {
    const N: usize = 16384;
    let mut l = vec![0.0f32; N];
    l[0] = 1.0;
    let mut r = l.clone();
    let trig = HashMap::new();
    let ctx = FxContext {
        step_secs,
        triggers: &trig,
    };
    for fx in effects {
        if matches!(
            fx,
            Effect::Filter(_) | Effect::Eq(_) | Effect::ParametricEq(_) | Effect::Gain(_)
        ) {
            fx.process(&mut l, &mut r, &ctx);
        }
    }
    let mut im = vec![0.0f32; N];
    analysis::fft(&mut l, &mut im);
    let bin = SR / N as f32;
    (0..points)
        .map(|i| {
            let f = 20.0 * 1000f32.powf(i as f32 / (points - 1).max(1) as f32);
            let k = ((f / bin).round() as usize).min(N / 2);
            let mag = (l[k] * l[k] + im[k] * im[k]).sqrt();
            (
                f.round(),
                (20.0 * mag.max(1e-9).log10() * 10.0).round() / 10.0,
            )
        })
        .collect()
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "analyze_sections",
            description: "Per-section listening report of the arranged song: for every arrangement section its integrated LUFS, short-term max, true peak, crest, centroid, band balance, top tracks (rms + energy share), deltas vs the previous section, plus musical flags such as 'hook not louder than verse', 'low end drops out in hook', 'intro louder than hook', section overs and silent sections. Use it to check contrast and energy flow before mastering.",
            mutates: false,
            schema: || obj(json!({"top_tracks": {"type": "integer", "description": "Tracks listed per section (default 4)"}}), &[]),
            run: |e, a| {
                let m = e.mix()?;
                let p = e.project.clone();
                Ok(ears::analyze_sections(&p, &m.left, &m.right, &m.track_info, u_or(a, "top_tracks", 4) as usize))
            },
        },
        Tool {
            name: "detect_artifacts",
            description: "Hunt for audio defects in the mix (or one track / sample / file, optionally a window): isolated clicks/pops, DC steps, end-level jumps after a fade-out, truncated tails (audio ending while still sounding) and sustained broadband noise/hiss beds (6-16 kHz floor, spectral flatness, coverage, looped-bed period). per_track:true also renders each track solo to name the culprit of a hiss bed. Returns events with times, severity and a summary.",
            mutates: false,
            schema: || obj(with_props(with_props(source_props(), region_props()), json!({
                "per_track": {"type": "boolean", "description": "Also scan every track solo for noise beds/clicks (slower)"},
                "clicks": {"type": "boolean", "description": "Run the click detector (default true)"},
                "max_events": {"type": "integer"},
                "tail_s": {"type": "number", "description": "Seconds at the end treated as outro for end-level jumps (default 6)"},
            })), &[]),
            run: |e, a| {
                let (name, l, r, is_mix) = source(e, a)?;
                let p = e.project.clone();
                let (s0, s1, label) = if is_mix { region(&p, a, l.len())? } else {
                    let n = l.len().min(r.len());
                    (((f_opt(a, "start_s").unwrap_or(0.0)) * SR) as usize, ((f_opt(a, "end_s").map(|x| x * SR).unwrap_or(n as f32)) as usize).min(n), "file".to_string())
                };
                let o = ArtifactOptions {
                    clicks: b_or(a, "clicks", true),
                    max_events: u_or(a, "max_events", 20) as usize,
                    tail_s: f_opt(a, "tail_s").unwrap_or(6.0),
                };
                let (mut events, bed) = ears::detect_artifacts(&l[s0..s1], &r[s0..s1], &o);
                for ev in events.iter_mut() {
                    ev.time_s += s0 as f32 / SR;
                    if let Some(x) = ev.end_s.as_mut() { *x += s0 as f32 / SR; }
                }
                let mut culprits = Vec::new();
                if b_or(a, "per_track", false) && is_mix {
                    let names: Vec<String> = p.tracks.iter().filter(|t| !t.mute).map(|t| t.name.clone()).collect();
                    for t in names {
                        let m = render_track(e, &t)?;
                        let (b0, b1) = ears::clamp_range(m.left.len(), s0, s1);
                        let (ev, bed) = ears::detect_artifacts(&m.left[b0..b1], &m.right[b0..b1], &o);
                        let worst: Vec<_> = ev.into_iter().filter(|x| x.kind == "noise_bed" || x.kind == "click").collect();
                        if !worst.is_empty() {
                            culprits.push(json!({"track": t, "noise_bed": bed.detected.then_some(&bed), "events": worst}));
                        }
                    }
                }
                let fails = events.iter().filter(|x| x.severity == "fail").count();
                let warns = events.iter().filter(|x| x.severity == "warn").count();
                Ok(json!({"source": name, "window": label, "clean": fails + warns == 0, "fail": fails, "warn": warns,
                    "events": events, "noise_bed": bed, "culprits": culprits}))
            },
        },
        Tool {
            name: "render_spectrogram",
            description: "Draw a log-frequency spectrogram (20 Hz bottom .. 20 kHz top, magma colours, faint lines at 100 Hz/1 kHz/10 kHz, white lines at section boundaries) of the mix, one track, a sample or a file, optionally a window. Saves a PNG and returns it as an MCP image so you can SEE hiss beds, masking, harsh resonances, clicks and empty spectrum.",
            mutates: false,
            schema: || obj(with_props(with_props(source_props(), region_props()), json!({
                "width": {"type": "integer", "description": "pixels (default 800, max 2400)"},
                "height": {"type": "integer", "description": "pixels (default 300, max 1200)"},
                "floor_db": {"type": "number", "description": "darkest level (default -100)"},
                "out": {"type": "string", "description": "PNG path (default renders/spectrogram_<source>.png)"},
                "inline": {"type": "boolean", "description": "Return the PNG as an image block (default true)"},
            })), &[]),
            run: |e, a| {
                let (name, l, r, is_mix) = source(e, a)?;
                let p = e.project.clone();
                let (s0, s1, label) = if is_mix || has_region(a) { region(&p, a, l.len().min(r.len()))? } else { (0, l.len().min(r.len()), "whole".into()) };
                let w = (u_or(a, "width", 800) as usize).clamp(16, 2400);
                let h = (u_or(a, "height", 300) as usize).clamp(16, 1200);
                let marks = if is_mix { section_marks(&p, s0, s1) } else { Vec::new() };
                let (png, path) = spectrogram_png(e, &l[s0..s1], &r[s0..s1], w, h, f_opt(a, "floor_db").unwrap_or(-100.0).min(-10.0), &marks, s_opt(a, "out"), &format!("spectrogram_{}", slug(&name)))?;
                let mut v = json!({"path": path, "source": name, "window": label, "width": w, "height": h, "seconds": ((s1 - s0) as f32 / SR * 100.0).round() / 100.0, "axis": "x = time, y = log frequency 20 Hz (bottom) to 20 kHz (top)"});
                let mut blocks = vec![media::link_block(&path, "image/png")];
                if b_or(a, "inline", true) { blocks.insert(0, media::image_block(&png)); }
                media::attach(&mut v, blocks);
                Ok(v)
            },
        },
        Tool {
            name: "render_preview",
            description: "Listen to a region: render the mix (or one track) for a section / beat range / seconds window (+ tail_s of ring-out), save a WAV and return it as an MCP audio block plus a spectrogram image and the window's loudness metrics. Keep previews short (max_seconds, default 20) so they stay small.",
            mutates: false,
            schema: || obj(with_props(region_props(), json!({
                "track": {"type": "string", "description": "Preview only this track (soloed)"},
                "tail_s": {"type": "number", "description": "Extra seconds after the window (ring-out)"},
                "max_seconds": {"type": "number", "description": "Cap on the preview length (default 20, max 60)"},
                "out": {"type": "string", "description": "WAV path (default renders/preview_<label>.wav)"},
                "spectrogram": {"type": "boolean", "description": "Also return a spectrogram image (default true)"},
                "inline_audio": {"type": "boolean", "description": "Embed the audio (default true; false = file link only)"},
            })), &[]),
            run: |e, a| {
                let (name, l, r) = match s_opt(a, "track") {
                    Some(t) => { let m = render_track(e, &t)?; (format!("track {t}"), m.left, m.right) }
                    None => { let m = e.mix()?; ("mix".to_string(), m.left.clone(), m.right.clone()) }
                };
                let p = e.project.clone();
                let (s0, mut s1, label) = region(&p, a, l.len().min(r.len()))?;
                let max = (f_opt(a, "max_seconds").unwrap_or(20.0).clamp(1.0, 60.0) * SR) as usize;
                let capped = s1 - s0 > max;
                if capped { s1 = s0 + max; }
                let (wl, wr) = (&l[s0..s1], &r[s0..s1]);
                let path = match s_opt(a, "out") { Some(o) => e.resolve(&o), None => e.renders_dir().join(format!("preview_{}.wav", slug(&format!("{name}_{label}")))) };
                let wav = media::wav_bytes(wl, wr, SR as u32);
                if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
                std::fs::write(&path, &wav)?;
                let metrics = ears::measure(wl, wr, 0, wl.len());
                let mut v = json!({"path": path, "source": name, "window": label, "start_s": (s0 as f32 / SR * 100.0).round() / 100.0, "seconds": ((s1 - s0) as f32 / SR * 100.0).round() / 100.0, "capped": capped, "metrics": metrics});
                let mut blocks = Vec::new();
                if b_or(a, "inline_audio", true) { blocks.push(media::audio_block(&wav, "audio/wav")); }
                blocks.push(media::link_block(&path, "audio/wav"));
                if b_or(a, "spectrogram", true) {
                    let marks = section_marks(&p, s0, s1);
                    let (png, pp) = spectrogram_png(e, wl, wr, 600, 220, -100.0, &marks, None, &format!("preview_{}", slug(&format!("{name}_{label}"))))?;
                    v["spectrogram"] = json!(pp);
                    blocks.push(media::image_block(&png));
                }
                media::attach(&mut v, blocks);
                Ok(v)
            },
        },
        Tool {
            name: "waveform_peaks",
            description: "Min/max waveform peaks of the mix, one track, a sample or a file (optionally a window) in N columns, plus an RMS envelope in dBFS: a compact way to SEE dynamics, gaps, fades and where hits land without audio.",
            mutates: false,
            schema: || obj(with_props(with_props(source_props(), region_props()), json!({"columns": {"type": "integer", "description": "default 120, max 2000"}})), &[]),
            run: |e, a| {
                let (name, l, r, is_mix) = source(e, a)?;
                let p = e.project.clone();
                let (s0, s1, label) = if is_mix || has_region(a) { region(&p, a, l.len().min(r.len()))? } else { (0, l.len().min(r.len()), "whole".into()) };
                let cols = (u_or(a, "columns", 120) as usize).clamp(4, 2000);
                let pk = ears::peaks(&l[s0..s1], &r[s0..s1], cols);
                let hop = ((s1 - s0) / cols).max(1);
                let env: Vec<f32> = ears::envelope_db(&l[s0..s1], &r[s0..s1], hop).into_iter().take(cols).map(|d| (d.max(-120.0) * 10.0).round() / 10.0).collect();
                Ok(json!({"source": name, "window": label, "columns": cols, "seconds_per_column": hop as f32 / SR, "peaks": pk, "rms_db": env}))
            },
        },
        Tool {
            name: "spectrum",
            description: "Average log-spaced spectrum (dB, 20 Hz-20 kHz) of the mix, a track, a sample or a file (optionally a window), plus band shares and centroid. Use to compare tonal balance between sections or against a reference.",
            mutates: false,
            schema: || obj(with_props(with_props(source_props(), region_props()), json!({"bins": {"type": "integer", "description": "default 48, max 512"}})), &[]),
            run: |e, a| {
                let (name, l, r, is_mix) = source(e, a)?;
                let p = e.project.clone();
                let (s0, s1, label) = if is_mix || has_region(a) { region(&p, a, l.len().min(r.len()))? } else { (0, l.len().min(r.len()), "whole".into()) };
                let bins = (u_or(a, "bins", 48) as usize).clamp(4, 512);
                let mono: Vec<f32> = (s0..s1).map(|i| 0.5 * (l[i] + r[i])).collect();
                let db = analysis::log_spectrum_db(&mono, bins);
                let pts: Vec<Value> = db.iter().enumerate().map(|(i, d)| json!([(20.0 * 1000f32.powf((i as f32 + 0.5) / bins as f32)).round(), (d * 10.0).round() / 10.0])).collect();
                let st = analysis::stats(&l[s0..s1], &r[s0..s1]);
                Ok(json!({"source": name, "window": label, "points_hz_db": pts, "bands": st.bands, "spectral_centroid_hz": st.spectral_centroid_hz}))
            },
        },
        Tool {
            name: "eq_curve",
            description: "Frequency response (dB vs Hz) of the linear tone-shaping effects (filter, eq, parametric_eq, gain) on a track, bus or master, measured with an impulse. index limits it to one effect. Use it to verify an EQ move does what you meant.",
            mutates: false,
            schema: || obj(json!({
                "track": {"type": "string", "description": "Track, bus or 'master'"},
                "index": {"type": ["integer", "string"], "description": "Only this effect (position or stable id)"},
                "points": {"type": "integer", "description": "default 40"},
            }), &["track"]),
            run: |e, a| {
                let t = s_opt(a, "track").ok_or_else(|| anyhow!("missing 'track'"))?;
                let step = e.project.step_secs();
                let fx = crate::tools::effects_of(&mut e.project, &t)?.clone();
                let chain: Vec<Effect> = match a.get("index").and_then(|v| v.as_u64()) {
                    Some(i) => vec![fx.get(i as usize).cloned().ok_or_else(|| anyhow!("no effect {i} on '{t}' ({} effects)", fx.len()))?],
                    None => fx,
                };
                let used: Vec<String> = chain.iter().filter(|f| matches!(f, Effect::Filter(_) | Effect::Eq(_) | Effect::ParametricEq(_) | Effect::Gain(_))).map(|f| f.type_name()).collect();
                let pts = eq_curve(&chain, step, (u_or(a, "points", 40) as usize).clamp(4, 400));
                Ok(json!({"track": t, "effects_measured": used, "points_hz_db": pts.iter().map(|(f, d)| json!([f, d])).collect::<Vec<_>>()}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use crate::engine::Engine;
    use serde_json::json;

    fn eng() -> Engine {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_ears_tests"));
        e.call("generate_beat", &json!({"style": "trap", "seed": 3}))
            .unwrap();
        e
    }

    #[test]
    fn sections_spectrogram_preview() {
        let mut e = eng();
        let s = e.call("analyze_sections", &json!({})).unwrap();
        assert!(s["count"].as_u64().unwrap() >= 2, "{s}");
        assert!(s["sections"][0]["metrics"]["integrated_lufs"]
            .as_f64()
            .is_some());
        assert!(s["sections"][1]["delta_vs_previous"]["lufs"].is_number());
        let sp = e
            .call(
                "render_spectrogram",
                &json!({"section": 0, "width": 200, "height": 80}),
            )
            .unwrap();
        let blocks = sp["_content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "image");
        let pr = e
            .call(
                "render_preview",
                &json!({"start_beat": 0, "end_beat": 4, "tail_s": 0.5}),
            )
            .unwrap();
        assert!(
            (pr["seconds"].as_f64().unwrap() - (4.0 * 60.0 / e.project.bpm as f64 + 0.5)).abs()
                < 0.05,
            "{pr}"
        );
        assert_eq!(pr["_content"][0]["type"], "audio");
        let wp = e.call("waveform_peaks", &json!({"columns": 50})).unwrap();
        assert_eq!(wp["peaks"].as_array().unwrap().len(), 50);
        let spc = e
            .call("spectrum", &json!({"bins": 16, "section": 0}))
            .unwrap();
        assert_eq!(spc["points_hz_db"].as_array().unwrap().len(), 16);
        let art = e
            .call("detect_artifacts", &json!({"clicks": true}))
            .unwrap();
        assert!(art["events"].is_array());
        let w = e.call("analyze_mix", &json!({"section": 0})).unwrap();
        assert!(w["window"].is_object(), "{w}");
    }

    #[test]
    fn eq_curve_reads_a_cut() {
        let mut e = eng();
        let t = e.project.tracks[0].name.clone();
        e.call("add_effect", &json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "low_cut", "freq": 200, "stages": 2}]}})).unwrap();
        let c = e
            .call("eq_curve", &json!({"track": t, "points": 20}))
            .unwrap();
        let pts = c["points_hz_db"].as_array().unwrap();
        assert!(pts[0][1].as_f64().unwrap() < -20.0, "{c}");
        assert!(pts[19][1].as_f64().unwrap().abs() < 1.0, "{c}");
    }
}
