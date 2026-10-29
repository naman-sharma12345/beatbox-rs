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
use crate::waveform_png;
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
    // Keep the constant-tempo arithmetic unchanged. Automated song positions
    // share the renderer's curve; seconds windows remain literal seconds.
    let curve = (!p.tempo_points.is_empty()).then(|| crate::tempo_curve::TempoCurve::new(p));
    let seconds = |beat: f32| {
        curve
            .as_ref()
            .map_or(beat * bs, |c| c.secs_at_beat(beat as f64) as f32)
    };
    let sec = a.get("section").and_then(|v| {
        v.as_str()
            .map(String::from)
            .or_else(|| v.as_u64().map(|n| n.to_string()))
    });
    let (mut s0, mut s1, label) = if let Some(s) = sec {
        let (b0, b1) = p.section_beats(&s)?;
        (seconds(b0), seconds(b1), format!("section {s}"))
    } else if f_opt(a, "start_beat").is_some() || f_opt(a, "end_beat").is_some() {
        let b0 = f_opt(a, "start_beat").unwrap_or(0.0);
        let end = f_opt(a, "end_beat");
        let b1 = end.unwrap_or_else(|| {
            curve.as_ref().map_or(total as f32 / SR / bs, |c| {
                c.beat_at_secs(total as f64 / SR as f64) as f32
            })
        });
        // An omitted end includes the entire available render, including tails.
        let s1 = if curve.is_some() && end.is_none() {
            total as f32 / SR
        } else {
            seconds(b1)
        };
        (seconds(b0), s1, format!("beats {b0}-{b1}"))
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

// Artifact checks use literal windows, never preview ring-out semantics.
const ARTIFACT_MAX_EVENTS: usize = 256;

fn artifact_number(a: &Value, key: &str) -> Result<Option<f64>> {
    a.get(key)
        .map(|v| {
            let x = v
                .as_f64()
                .ok_or_else(|| anyhow!("{key} must be a finite nonnegative number"))?;
            if !x.is_finite() || x < 0.0 || !(x as f32).is_finite() {
                bail!("{key} must be a finite nonnegative number representable as f32");
            }
            Ok(x)
        })
        .transpose()
}

fn artifact_event_budget(n: u64) -> Result<usize> {
    if !(1..=ARTIFACT_MAX_EVENTS as u64).contains(&n) {
        bail!("max_events must be an integer from 1 to {ARTIFACT_MAX_EVENTS}");
    }
    Ok(n as usize)
}

fn artifact_options(a: &Value) -> Result<ArtifactOptions> {
    let max_events = match a.get("max_events") {
        None => 20,
        Some(v) => artifact_event_budget(v.as_u64().ok_or_else(|| {
            anyhow!("max_events must be an integer from 1 to {ARTIFACT_MAX_EVENTS}")
        })?)?,
    };
    Ok(ArtifactOptions {
        clicks: b_or(a, "clicks", true),
        max_events,
        tail_s: artifact_number(a, "tail_s")?.unwrap_or(6.0) as f32,
    })
}

fn validate_artifact_selectors(a: &Value) -> Result<()> {
    if !a.is_object() {
        bail!("artifact arguments must be an object");
    }
    for key in ["clicks", "per_track"] {
        if a.get(key).is_some_and(|v| !v.is_boolean()) {
            bail!("{key} must be a boolean");
        }
    }
    let mut sources = 0;
    for k in ["track", "sample", "path"] {
        if let Some(v) = a.get(k) {
            if v.as_str().is_none_or(|s| s.trim().is_empty()) {
                bail!("{k} must be a nonempty string");
            }
            sources += 1;
        }
    }
    if sources > 1 {
        bail!("choose only one of track, sample or path");
    }
    let seconds = a.get("start_s").is_some() || a.get("end_s").is_some();
    let beats = a.get("start_beat").is_some() || a.get("end_beat").is_some();
    let section = a.get("section").is_some();
    if (seconds as u8 + beats as u8 + section as u8) > 1 {
        bail!("choose only one window coordinate system: seconds, beats or section");
    }
    if (a.get("path").is_some() || a.get("sample").is_some()) && (beats || section) {
        bail!("file/sample windows accept seconds only, not timeline beats or sections");
    }
    if let Some(v) = a.get("section") {
        if !(v.as_str().is_some_and(|s| !s.trim().is_empty()) || v.as_u64().is_some()) {
            bail!("section must be a nonempty name or nonnegative integer index");
        }
    }
    for (start, end) in [("start_s", "end_s"), ("start_beat", "end_beat")] {
        let lo = artifact_number(a, start)?.unwrap_or(0.0);
        if let Some(hi) = artifact_number(a, end)? {
            if hi <= lo {
                bail!("{end} must be greater than {start}");
            }
        }
    }
    Ok(())
}

fn artifact_seconds_bounds(total: usize, start: f64, end: f64) -> Result<(usize, usize)> {
    if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
        bail!("artifact window must have finite, nonnegative, ordered nonempty bounds");
    }
    let source_end = total as f64 / SR as f64;
    if start >= source_end {
        bail!("artifact window starts outside the available source");
    }
    let lo = (start * SR as f64) as usize;
    let hi = if end >= source_end {
        total
    } else {
        (end * SR as f64) as usize
    };
    if hi <= lo {
        bail!("artifact window contains no samples");
    }
    Ok((lo, hi))
}

fn artifact_region(p: &Project, a: &Value, total: usize) -> Result<(usize, usize, String)> {
    // Validate here too so this resolver is safe when called independently.
    validate_artifact_selectors(a)?;
    if a.get("section").is_some() || a.get("start_beat").is_some() || a.get("end_beat").is_some() {
        let mut literal = a.clone();
        literal
            .as_object_mut()
            .ok_or_else(|| anyhow!("arguments must be an object"))?
            .remove("tail_s");
        return region(p, &literal, total);
    }
    let start = artifact_number(a, "start_s")?.unwrap_or(0.0);
    let end = artifact_number(a, "end_s")?.unwrap_or(total as f64 / SR as f64);
    let (lo, hi) = artifact_seconds_bounds(total, start, end)?;
    let label = if a.get("start_s").is_some() || a.get("end_s").is_some() {
        format!("{start}-{end} s")
    } else {
        "whole".to_string()
    };
    Ok((lo, hi, label))
}

fn artifact_endpoint_context(events: &mut Vec<ears::Artifact>, source_end_checked: bool) {
    if !source_end_checked {
        events.retain(|ev| ev.kind != "truncated_tail" && ev.kind != "end_level_jump");
    }
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

/// Waveform image options from tool arguments (shared with the CLI path).
fn waveform_style(a: &Value) -> Result<waveform_png::WaveStyle> {
    let d = waveform_png::WaveStyle::default();
    let colour = |k: &str, def: [u8; 3]| -> Result<[u8; 3]> {
        match s_opt(a, k) {
            None => Ok(def),
            Some(s) => waveform_png::parse_color(&s)
                .ok_or_else(|| anyhow!("{k} must be #rrggbb, got '{s}'")),
        }
    };
    let lanes = match s_opt(a, "lanes") {
        None => waveform_png::Lanes::Mono,
        Some(s) => waveform_png::Lanes::parse(&s)
            .ok_or_else(|| anyhow!("lanes must be 'mono', 'stereo' or 'channels', not '{s}'"))?,
    };
    let center = match s_opt(a, "center") {
        Some(s) if s.eq_ignore_ascii_case("none") => None,
        _ => Some(colour("center", d.center.unwrap_or(d.background))?),
    };
    Ok(waveform_png::WaveStyle {
        width: (u_or(a, "width", d.width as u64) as usize).clamp(16, 8192),
        height: (u_or(a, "height", d.height as u64) as usize).clamp(16, 4096),
        lanes,
        rms: b_or(a, "rms", false),
        background: colour("background", d.background)?,
        wave: colour("wave", d.wave)?,
        rms_color: colour("rms_color", d.rms_color)?,
        center,
    })
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
        offset: 0,
        levels: None,
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
                "max_events": {"type": "integer", "minimum": 1, "maximum": 256, "description": "Per-detector event budget (default 20, range 1..256)"},
                "tail_s": {"type": "number", "description": "Analysis horizon in seconds for outro end-level jumps, not window ring-out (finite, nonnegative; default 6)"},
            })), &[]),
            run: |e, a| {
                validate_artifact_selectors(a)?;
                let o = artifact_options(a)?;
                let (name, l, r, is_mix) = source(e, a)?;
                let p = e.project.clone();
                let n = l.len().min(r.len());
                let (s0, s1, label) = artifact_region(&p, a, n)?;
                let label = if is_mix { label } else { "file".to_string() };
                let source_end_checked = s1 == n;
                // drum attacks are transients, not clicks: scan deeper, then mask them
                let onsets = if let Some(t) = s_opt(a, "track") {
                    ears::artifact_drum_onsets(&p, ears::ArtifactOnsetScope::ExplicitTrack(p.track_index(&t)?))
                } else if is_mix {
                    ears::artifact_drum_onsets(&p, ears::ArtifactOnsetScope::Mix)
                } else {
                    Vec::new()
                };
                let wide = ArtifactOptions { max_events: o.max_events.checked_mul(8).ok_or_else(|| anyhow!("max_events overflow"))?, ..o };
                let (mut events, bed) = ears::detect_artifacts(&l[s0..s1], &r[s0..s1], &wide);
                artifact_endpoint_context(&mut events, source_end_checked);
                let masked = ears::mask_drum_clicks(&mut events, &onsets, s0);
                let mut nclick = 0;
                events.retain(|x| x.kind != "click" || { nclick += 1; nclick <= o.max_events });
                for ev in events.iter_mut() {
                    ev.time_s += s0 as f32 / SR;
                    if let Some(x) = ev.end_s.as_mut() { *x += s0 as f32 / SR; }
                }
                let mut culprits = Vec::new();
                if b_or(a, "per_track", false) && is_mix {
                    let names: Vec<String> = p.tracks.iter().filter(|t| !t.mute).map(|t| t.name.clone()).collect();
                    for t in names {
                        let m = render_track(e, &t)?;
                        let (b0, b1) = ears::clamp_range(m.left.len().min(m.right.len()), s0, s1);
                        if b1 <= b0 { continue; }
                        let (mut ev, mut bed) = ears::detect_artifacts(&m.left[b0..b1], &m.right[b0..b1], &wide);
                        artifact_endpoint_context(&mut ev, b1 == m.left.len().min(m.right.len()));
                        // a solo noise-percussion track is judged between its own hits
                        if bed.detected {
                            if let Some(spans) = ears::own_hit_spans(&p, &e.bank, p.track_index(&t)?) {
                                let local: Vec<(usize, usize)> = spans.iter().filter(|s| s.1 > b0 && s.0 < b1).map(|s| (s.0.saturating_sub(b0), s.1 - b0)).collect();
                                bed = ears::noise_bed_gated(&m.left[b0..b1], &m.right[b0..b1], &local);
                                if !bed.detected {
                                    ev.retain(|x| x.kind != "noise_bed");
                                }
                            }
                        }
                        let own_onsets = ears::artifact_drum_onsets(&p, ears::ArtifactOnsetScope::ExplicitTrack(p.track_index(&t)?));
                        ears::mask_drum_clicks(&mut ev, &own_onsets, b0);
                        let mut nc = 0;
                        ev.retain(|x| x.kind != "click" || { nc += 1; nc <= o.max_events });
                        let worst: Vec<_> = ev.into_iter().filter(|x| x.kind == "noise_bed" || x.kind == "click").collect();
                        if !worst.is_empty() {
                            culprits.push(json!({"track": t, "noise_bed": bed.detected.then_some(&bed), "events": worst}));
                        }
                    }
                }
                let fails = events.iter().filter(|x| x.severity == "fail").count();
                let warns = events.iter().filter(|x| x.severity == "warn").count();
                Ok(json!({"source": name, "window": label, "clean": fails + warns == 0, "fail": fails, "warn": warns, "drum_attack_transients_ignored": masked,
                    "events": events, "noise_bed": bed, "culprits": culprits,
                    "window_bounds": {"start_s": s0 as f64 / SR as f64, "end_s": s1 as f64 / SR as f64,
                        "source_end_s": n as f64 / SR as f64, "source_end_checked": source_end_checked},
                    "scope_note": if source_end_checked { "Findings describe only the checked window; source ending checked." } else { "Excerpt boundary: source ending not checked. Clean applies only to this window, not the whole source." }}))
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
            name: "render_waveform_png",
            description: "Draw a waveform image of the mix, one track, a sample or an audio file (optionally a window) and write it as a PNG, with no GUI. Each column shows the min/max peaks; rms adds the column's RMS as an inner band. lanes 'mono' downmixes into one lane, 'stereo' draws left on top and right below, 'channels' gives every channel of a multichannel file its own lane. A WAV file is scanned from disk in chunks (memory grows with width, not length). Colours are #rrggbb; the default is a dark background with a bright wave. The PNG is written by Beatbox's own encoder (CRC-32, zlib/Adler-32, DEFLATE fixed-Huffman or stored blocks), so the same audio and options always give the same bytes; the result carries the byte count and CRC-32. Returns the path plus the image inline (inline false = link only). Same as the CLI `beatbox waveform`.",
            mutates: false,
            schema: || obj(with_props(with_props(source_props(), region_props()), json!({
                "width": {"type": "integer", "description": "Pixels, default 1200 (16-8192)"},
                "height": {"type": "integer", "description": "Pixels, default 300 (16-4096)"},
                "lanes": {"type": "string", "enum": ["mono", "stereo", "channels"], "description": "mono (default) downmixes; stereo = L over R; channels = one lane per file channel"},
                "rms": {"type": "boolean", "description": "Overlay the RMS band inside the peaks (default false)"},
                "background": {"type": "string", "description": "#rrggbb, default #121418"},
                "wave": {"type": "string", "description": "Peak colour, default #5ac8fa"},
                "rms_color": {"type": "string", "description": "RMS band colour, default #d2f0ff"},
                "center": {"type": "string", "description": "Zero-line colour, default #2e343c; 'none' hides it"},
                "compression": {"type": "string", "enum": ["fixed", "stored"], "description": "DEFLATE mode: fixed Huffman (default, smaller) or stored blocks"},
                "out": {"type": "string", "description": "PNG path (default renders/waveform_<source>.png)"},
                "inline": {"type": "boolean", "description": "Embed the image (default true)"},
            })), &[]),
            run: |e, a| {
                let st = waveform_style(a)?;
                let comp = match s_opt(a, "compression").as_deref() {
                    None | Some("fixed") => waveform_png::Compression::Fixed,
                    Some("stored") => waveform_png::Compression::Stored,
                    Some(o) => bail!("compression must be 'fixed' or 'stored', not '{o}'"),
                };
                // a WAV file is read from disk in chunks (memory O(width)); anything
                // else (mix, track, sample, mp3/flac, beat or section windows) is drawn
                // from its decoded buffer in one pass
                let wav = s_opt(a, "path").filter(|p| p.to_ascii_lowercase().ends_with(".wav") && s_opt(a, "track").is_none() && s_opt(a, "sample").is_none() && a.get("section").is_none() && a.get("start_beat").is_none() && a.get("end_beat").is_none());
                let streamed = match wav {
                    Some(p) => {
                        let window = if a.get("start_s").is_some() || a.get("end_s").is_some() { Some((f_opt(a, "start_s").unwrap_or(0.0) as f64, f_opt(a, "end_s").map(|x| x as f64).unwrap_or(f64::MAX))) } else { None };
                        waveform_png::wav_file_png(&e.resolve(&p), &st, comp, window).ok().map(|(png, info)| {
                            let name = std::path::Path::new(&p).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or(p.clone());
                            let label = if window.is_some() { format!("{:.2}-{:.2} s", info.start_frame as f64 / info.sample_rate as f64, (info.start_frame + info.drawn) as f64 / info.sample_rate as f64) } else { "whole".to_string() };
                            (png, name, label, info.drawn as f64 / info.sample_rate as f64, info.channels)
                        })
                    }
                    None => None,
                };
                let was_streamed = streamed.is_some();
                let (png, name, label, seconds, channels) = match streamed {
                    Some(x) => x,
                    None => {
                        let (name, l, r, is_mix) = source(e, a)?;
                        let p = e.project.clone();
                        let (s0, s1, label) = if is_mix || has_region(a) { region(&p, a, l.len().min(r.len()))? } else { (0, l.len().min(r.len()), "whole".into()) };
                        (waveform_png::waveform_png(&l[s0..s1], &r[s0..s1], &st, comp), name, label, (s1 - s0) as f64 / SR as f64, 2)
                    }
                };
                let path = match s_opt(a, "out") { Some(o) => e.resolve(&o), None => e.renders_dir().join(format!("waveform_{}.png", slug(&name))) };
                if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
                std::fs::write(&path, &png)?;
                let mut v = json!({"path": path, "source": name, "window": label, "width": st.width, "height": st.height,
                    "lanes": st.lanes.name(), "lane_count": st.lanes.count(channels), "rms": st.rms, "seconds": (seconds * 100.0).round() / 100.0,
                    "streamed_from_disk": was_streamed,
                    "compression": if comp == waveform_png::Compression::Fixed { "fixed" } else { "stored" },
                    "bytes": png.len(), "crc32": format!("{:08x}", waveform_png::crc32(&png)),
                    "axis": "x = time, y = amplitude (+1 top, -1 bottom of each lane)"});
                let mut blocks = vec![media::link_block(&path, "image/png")];
                if b_or(a, "inline", true) { blocks.insert(0, media::image_block(&png)); }
                media::attach(&mut v, blocks);
                Ok(v)
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
    use super::*;
    use crate::engine::Engine;

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
    fn waveform_png_tool_mix_and_file() {
        // the project's mix (one section), stereo lanes with the RMS band
        let mut e = eng();
        let v = e.call("render_waveform_png", &json!({"section": 0, "width": 320, "height": 120, "lanes": "stereo", "rms": true, "wave": "#ff8800"})).unwrap();
        let png = std::fs::read(v["path"].as_str().unwrap()).unwrap();
        assert_eq!(&png[..8], &waveform_png::PNG_SIGNATURE);
        assert_eq!(media::png_dims(&png).unwrap(), (320, 120));
        assert_eq!(v["bytes"].as_u64().unwrap() as usize, png.len());
        assert_eq!(v["crc32"], format!("{:08x}", waveform_png::crc32(&png)));
        assert_eq!(v["_content"][0]["type"], "image");
        assert_eq!(v["lanes"], "stereo");
        // the peak colour is really drawn
        assert!(png_pixels(&png).chunks(3).any(|p| p == [0xFF, 0x88, 0x00]));

        // an audio file: a 0.5-amplitude 441 Hz sine, mono lane, stored blocks
        let dir = std::env::temp_dir().join("beatbox_waveform_png_tool");
        std::fs::create_dir_all(&dir).unwrap();
        let s: Vec<f32> = (0..44_100)
            .map(|i| 0.5 * (std::f32::consts::TAU * 441.0 * i as f32 / 44_100.0).sin())
            .collect();
        let wav = dir.join("sine.wav");
        std::fs::write(&wav, media::wav_bytes(&s, &s, 44_100)).unwrap();
        let out = dir.join("sine.png");
        let args = json!({"path": wav, "width": 200, "height": 101, "compression": "stored", "center": "none", "background": "#000000", "wave": "#ffffff", "out": out, "inline": false});
        let a = e.call("render_waveform_png", &args).unwrap();
        assert_eq!(a["_content"][0]["type"], "resource_link");
        let png = std::fs::read(&out).unwrap();
        let px = png_pixels(&png);
        // every column spans rows 25..=75 (0.5 peaks); row 0 and row 100 stay black
        for x in 0..200 {
            assert_eq!(&px[(25 * 200 + x) * 3..][..3], &[255, 255, 255]);
            assert_eq!(&px[(75 * 200 + x) * 3..][..3], &[255, 255, 255]);
            assert_eq!(&px[x * 3..][..3], &[0, 0, 0]);
            assert_eq!(&px[(100 * 200 + x) * 3..][..3], &[0, 0, 0]);
        }
        // same input, same bytes
        let b = e.call("render_waveform_png", &args).unwrap();
        assert_eq!(a["crc32"], b["crc32"]);
        assert_eq!(std::fs::read(&out).unwrap(), png);
        assert!(e
            .call(
                "render_waveform_png",
                &json!({"path": wav, "lanes": "quad"})
            )
            .is_err());
        assert!(e
            .call("render_waveform_png", &json!({"path": wav, "wave": "red"}))
            .is_err());
    }

    fn png_pixels(png: &[u8]) -> Vec<u8> {
        let mut rd = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut buf = vec![0u8; rd.output_buffer_size().unwrap()];
        let info = rd.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        buf
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

    /// Regression for the external review's artifact scan (22 DC-step
    /// warnings, a lone click, noise beds on solo hat tracks): a dense
    /// 144 BPM half-time trap pattern with a gliding 808, a five-note bell
    /// motif, 1/32 hat rolls and open hats must scan clean, mix and every
    /// solo track. Documented false positives allowed here: none.
    #[test]
    fn dense_trap_144_artifact_scan_is_clean() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_artifact_regress"));
        let calls: Vec<Value> =
            serde_json::from_str(include_str!("testdata/dense_trap_144.json")).unwrap();
        for c in &calls {
            e.call(c["tool"].as_str().unwrap(), &c["args"])
                .unwrap_or_else(|err| panic!("{c}: {err:#}"));
        }
        let a = e
            .call(
                "detect_artifacts",
                &json!({"per_track": true, "max_events": 50}),
            )
            .unwrap();
        assert_eq!(a["clean"], true, "{a:#}");
        assert_eq!(a["noise_bed"]["detected"], false, "{a:#}");
        assert!(a["culprits"].as_array().unwrap().is_empty(), "{a:#}");
        // the percussion gating did not just blind the detector: the same
        // hat track with a constant hiss under it is still caught
        let hat = render_track(&mut e, "hat").unwrap();
        let mut rng = crate::dsp::Rng::new(9);
        let hiss: Vec<f32> = (0..hat.left.len()).map(|_| rng.bipolar() * 0.004).collect();
        let l: Vec<f32> = hat.left.iter().zip(&hiss).map(|(a, b)| a + b).collect();
        let r: Vec<f32> = hat.right.iter().zip(&hiss).map(|(a, b)| a + b).collect();
        let ti = e.project.track_index("hat").unwrap();
        let spans = ears::own_hit_spans(&e.project, &e.bank, ti).unwrap();
        let open = spans
            .windows(2)
            .map(|w| w[1].0.saturating_sub(w[0].1))
            .sum::<usize>();
        let bed = ears::noise_bed_gated(&l, &r, &spans);
        if open > (SR * 2.0) as usize {
            assert!(
                bed.detected,
                "hiss under the hats must still be found: {bed:?}"
            );
        }
    }
}

#[cfg(test)]
mod region_tempo_tests {
    use super::*;
    #[test]
    fn region_windows_follow_curve_and_keep_seconds_and_open_end() {
        let mut p = Project {
            bpm: 120.,
            ..Default::default()
        };
        let total = (20. * SR) as usize;
        assert_eq!(
            region(&p, &json!({"start_beat":2,"end_beat":6}), total).unwrap(),
            (SR as usize, (3. * SR) as usize, "beats 2-6".into())
        );
        p.tempo_points = vec![
            crate::project::TempoPoint {
                beat: 4.,
                bpm: 60.,
                ramp: false,
            },
            crate::project::TempoPoint {
                beat: 8.,
                bpm: 100.,
                ramp: true,
            },
        ];
        let c = crate::tempo_curve::TempoCurve::new(&p);
        let at = |b: f64| (c.secs_at_beat(b) as f32 * SR) as usize;
        let r = region(&p, &json!({"start_beat":2,"end_beat":6}), total).unwrap();
        assert_eq!((r.0, r.1), (at(2.), at(6.)));
        let r = region(&p, &json!({"start_beat":6}), total).unwrap();
        assert_eq!((r.0, r.1), (at(6.), total));
        let r = region(&p, &json!({"start_s":1,"end_s":3,"tail_s":0.5}), total).unwrap();
        assert_eq!((r.0, r.1), (SR as usize, (3.5 * SR) as usize));
        assert!(region(&p, &json!({"start_beat":8,"end_beat":2}), total).is_err());
        assert!(region(&p, &json!({"start_beat":200}), total).is_err());
        let r = region(&p, &json!({"section":0}), total).unwrap();
        assert_eq!((r.0, r.1), (0, at(16.)));
        let mut e = Engine::new(std::env::temp_dir().join("regions-meter"));
        e.call("add_pattern", &json!({"name":"odd","bars":2}))
            .unwrap();
        e.call(
            "set_time_signature",
            &json!({"pattern":"odd","time_signature":"7/8"}),
        )
        .unwrap();
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":"odd","repeats":1},{"pattern":"A","repeats":1}]}),
        )
        .unwrap();
        e.project.tempo_points = p.tempo_points;
        let spans = ears::spans(&e.project);
        let c = crate::tempo_curve::TempoCurve::new(&e.project);
        assert_eq!(spans[1].start_beat, 7.);
        assert_eq!(spans[1].start_s, c.secs_at_beat(7.) as f32);
        let r = region(&e.project, &json!({"section":1}), total).unwrap();
        assert_eq!(r.0, (spans[1].start_s * SR) as usize);
        assert_eq!(r.1, (spans[1].end_s * SR) as usize);
    }
}

#[cfg(test)]
mod region_output_tests {
    use super::*;
    #[test]
    fn automated_beat_export_matches_seconds_export_decoded_samples() {
        let dir = std::env::temp_dir().join(format!("region-output-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        e.call(
            "add_notes",
            &json!({"track":"keys","notes":[{"start":8,"len":4,"pitch":60,"vel":0.8}]}),
        )
        .unwrap();
        e.call("add_tempo_point", &json!({"beat":4,"bpm":60}))
            .unwrap();
        e.call("add_tempo_point", &json!({"beat":8,"bpm":100,"ramp":true}))
            .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let c = crate::tempo_curve::TempoCurve::new(&e.project);
        let (s0, s1) = (c.secs_at_beat(2.), c.secs_at_beat(6.));
        e.call(
            "export_audio",
            &json!({"path":"beats.wav","start_beat":2,"end_beat":6,"bits":24}),
        )
        .unwrap();
        e.call(
            "export_audio",
            &json!({"path":"seconds.wav","start_s":s0,"end_s":s1,"bits":24}),
        )
        .unwrap();
        let a = crate::samples::decode_stereo_native(&dir.join("beats.wav")).unwrap();
        let b = crate::samples::decode_stereo_native(&dir.join("seconds.wav")).unwrap();
        assert_eq!(a, b);
        assert!((a.0.len() as f64 / a.2 as f64 - (s1 - s0)).abs() < 0.001);
        assert!(a.0.iter().any(|s| s.abs() > 0.001));
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod region_integration_tests {
    use super::*;
    #[test]
    fn sections_preview_and_waveform_share_automated_windows() {
        let dir = std::path::PathBuf::from("/tmp/region-integration");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("add_track", &json!({"name":"keys","preset":"epiano"}))
            .unwrap();
        for (name, meter) in [("first", "3/4"), ("second", "7/8")] {
            e.call("add_pattern", &json!({"name":name,"bars":1}))
                .unwrap();
            e.call(
                "set_time_signature",
                &json!({"pattern":name,"time_signature":meter}),
            )
            .unwrap();
            e.call("add_notes",&json!({"pattern":name,"track":"keys","notes":[{"start":0,"len":4,"pitch":60,"vel":0.8}]})).unwrap();
        }
        e.call(
            "set_arrangement",
            &json!({"sections":[{"pattern":"first","repeats":1},{"pattern":"second","repeats":1}]}),
        )
        .unwrap();
        e.call("add_tempo_point", &json!({"beat":2,"bpm":60}))
            .unwrap();
        e.call("add_tempo_point", &json!({"beat":6,"bpm":100,"ramp":true}))
            .unwrap();
        let before = serde_json::to_value(&e.project).unwrap();
        let c = crate::tempo_curve::TempoCurve::new(&e.project);
        let (s0, s1) = (c.secs_at_beat(3.), c.secs_at_beat(6.5));
        let a = e.call("analyze_sections", &json!({})).unwrap();
        let m = &a["sections"][1]["metrics"];
        println!("{}", a);
        assert!((m["start_s"].as_f64().unwrap() - s0).abs() < 0.006);
        assert!((m["end_s"].as_f64().unwrap() - s1).abs() < 0.006);
        let a=e.call("render_preview",&json!({"section":"second","out":"section.wav","inline_audio":false,"spectrogram":false})).unwrap();
        let b=e.call("render_preview",&json!({"start_s":s0,"end_s":s1,"out":"seconds.wav","inline_audio":false,"spectrogram":false})).unwrap();
        assert_eq!(a["seconds"], b["seconds"]);
        assert_eq!(
            std::fs::read(dir.join("section.wav")).unwrap(),
            std::fs::read(dir.join("seconds.wav")).unwrap()
        );
        let a = e
            .call("waveform_peaks", &json!({"section":"second","columns":20}))
            .unwrap();
        let b = e
            .call(
                "waveform_peaks",
                &json!({"start_s":s0,"end_s":s1,"columns":20}),
            )
            .unwrap();
        assert_eq!(a["peaks"], b["peaks"]);
        assert_eq!(serde_json::to_value(&e.project).unwrap(), before);
    }
}

#[cfg(test)]
mod artifact_window_tests {
    use super::*;
    #[test]
    fn literal_windows_and_preview_ringout_stay_distinct() {
        let p = Project::default();
        let n = (10.0 * SR) as usize;
        for args in [
            json!({"start_s":1,"end_s":2}),
            json!({"start_beat":2,"end_beat":4}),
            json!({"section":0}),
        ] {
            let a = artifact_region(&p, &args, n).unwrap();
            let mut explicit = args.clone();
            explicit["tail_s"] = json!(6);
            assert_eq!(a, artifact_region(&p, &explicit, n).unwrap());
        }
        let args = json!({"start_s":1,"end_s":2,"tail_s":6});
        assert_eq!(
            artifact_region(&p, &args, n).unwrap().1,
            (2.0 * SR) as usize
        );
        assert_eq!(region(&p, &args, n).unwrap().1, (8.0 * SR) as usize);
    }
    #[test]
    fn unequal_channels_and_nonfinite_bounds_are_safe() {
        let l = [0.0; 8];
        let r = [0.0; 4];
        let (lo, hi) = artifact_seconds_bounds(l.len().min(r.len()), 0.0, 1.0).unwrap();
        assert_eq!((l[lo..hi].len(), r[lo..hi].len()), (4, 4));
        for (lo, hi) in [
            (f64::NAN, 1.0),
            (0.0, f64::INFINITY),
            (-1.0, 1.0),
            (1.0, 1.0),
            (0.0, 0.0),
        ] {
            assert!(artifact_seconds_bounds(44100, lo, hi).is_err());
        }
        assert!(artifact_seconds_bounds(0, 0.0, 1.0).is_err());
    }
    #[test]
    fn endpoint_context_removes_only_endpoint_findings() {
        let kinds = [
            "click",
            "dc_step",
            "noise_bed",
            "truncated_tail",
            "end_level_jump",
        ];
        let events = || {
            kinds
                .iter()
                .map(|kind| ears::Artifact {
                    kind: kind.to_string(),
                    severity: "warn".into(),
                    time_s: 1.0,
                    end_s: None,
                    detail: String::new(),
                    track: None,
                })
                .collect::<Vec<_>>()
        };
        let mut all = events();
        artifact_endpoint_context(&mut all, true);
        assert_eq!(all.len(), 5);
        let mut excerpt = events();
        artifact_endpoint_context(&mut excerpt, false);
        assert_eq!(
            excerpt.iter().map(|x| x.kind.as_str()).collect::<Vec<_>>(),
            vec!["click", "dc_step", "noise_bed"]
        );
    }
}
