//! Metronome (FL's metronome + count-in, as a file): a click track in the
//! project's tempo, written to a WAV for recording a vocal against, or
//! placed in the song as a muted guide track. Downbeats are accented.

use crate::dsp::SR;
use crate::engine::Engine;
use crate::tools::{b_or, f_opt, obj, s_opt, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::f32::consts::PI;

/// One click: a short decaying sine blip (accent = higher and louder).
fn blip(accent: bool) -> Vec<f32> {
    let (f, g) = if accent { (1600.0, 0.8) } else { (1000.0, 0.5) };
    let n = (0.03 * SR) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let env = (-t / 0.006).exp() * (i as f32 / 24.0).min(1.0);
            g * env * (2.0 * PI * f * t).sin()
        })
        .collect()
}

/// A mono click track: `beats_per_bar` clicks per bar for `bars` bars.
pub fn click_track(bpm: f32, bars: u32, beats_per_bar: u32, accent: bool) -> Vec<f32> {
    let beat = 60.0 / bpm.clamp(20.0, 400.0);
    let total = ((bars * beats_per_bar) as f32 * beat * SR) as usize + (0.05 * SR) as usize;
    let mut out = vec![0.0f32; total];
    for k in 0..bars * beats_per_bar {
        let b = blip(accent && k % beats_per_bar == 0);
        let at = (k as f32 * beat * SR).round() as usize;
        for (j, v) in b.iter().enumerate() {
            if at + j < out.len() {
                out[at + j] += v;
            }
        }
    }
    out
}

/// A click track that follows the song itself: each pattern's time signature
/// (accented bar lines, dotted quarters in 6/8) and the tempo automation,
/// after `count_in` bars of the first pattern's meter at the song bpm.
/// Returns (samples, seconds of count-in, bars).
pub fn song_click(p: &crate::project::Project, count_in: u32, accent: bool) -> (Vec<f32>, f32, u32) {
    let curve = crate::tempo_curve::TempoCurve::new(p);
    let first = p.song_sections().first().and_then(|s| p.pattern_index(&s.pattern).ok()).map(|i| p.patterns[i].meter()).unwrap_or((4, 4));
    let step_s = p.step_secs() as f64;
    let ci_steps = count_in as f64 * crate::project::meter_steps(first) as f64;
    let lead = ci_steps * step_s;
    let mut hits: Vec<(f64, bool)> = Vec::new();
    for b in 0..count_in {
        for (s, down) in crate::project::meter_clicks(first) {
            hits.push(((b * crate::project::meter_steps(first) + s) as f64 * step_s, down));
        }
    }
    let mut off = 0u32;
    let mut bars = 0u32;
    for sec in p.song_sections() {
        let Ok(pi) = p.pattern_index(&sec.pattern) else { continue };
        let pat = &p.patterns[pi];
        let (m, spb) = (pat.meter(), pat.steps_per_bar());
        for _ in 0..sec.repeats.max(1) * pat.bars {
            for (s, down) in crate::project::meter_clicks(m) {
                hits.push((lead + curve.secs_at_step((off + s) as f64), down));
            }
            off += spb;
            bars += 1;
        }
    }
    let total = ((lead + curve.secs_at_step(off as f64)) * SR as f64) as usize + (0.05 * SR) as usize;
    let mut out = vec![0.0f32; total];
    for (t, down) in hits {
        let b = blip(accent && down);
        let at = (t * SR as f64).round() as usize;
        for (j, v) in b.iter().enumerate() {
            if at + j < out.len() {
                out[at + j] += v;
            }
        }
    }
    (out, lead as f32, bars)
}

fn export_click(e: &mut Engine, a: &Value) -> Result<Value> {
    // no explicit grid asked for: follow the song (meters + tempo automation)
    if f_opt(a, "bpm").is_none() && f_opt(a, "bars").is_none() && f_opt(a, "beats_per_bar").is_none() {
        let count_in = f_opt(a, "count_in_bars").unwrap_or(1.0).clamp(0.0, 8.0) as u32;
        let (x, lead, bars) = song_click(&e.project, count_in, b_or(a, "accent", true));
        let path = match s_opt(a, "path") {
            Some(p) => e.resolve(&p),
            None => e.renders_dir().join(format!("{}_click.wav", crate::samples::sample_name(&e.project.name))),
        };
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        crate::render::write_wav(&path, &x, &x)?;
        let meters: Vec<String> = e.project.patterns.iter().map(|p| { let (n, d) = p.meter(); format!("{}: {n}/{d}", p.name) }).collect();
        return Ok(json!({
            "path": path.to_string_lossy(), "follows": "song (time signatures + tempo automation)", "bars": bars,
            "count_in_bars": count_in, "meters": meters, "tempo_points": e.project.tempo_points.len(),
            "seconds": (x.len() as f32 / SR * 100.0).round() / 100.0, "song_starts_at_s": (lead * 1000.0).round() / 1000.0,
            "note": "record against this; the song's beat 0 is after the count-in (song_starts_at_s)"
        }));
    }
    let bpm = f_opt(a, "bpm").map(|x| x as f32).unwrap_or(e.project.bpm);
    let song_bars = (e.project.song_steps() as f32 / 16.0).ceil().max(1.0) as u32;
    let count_in = f_opt(a, "count_in_bars").unwrap_or(1.0).clamp(0.0, 8.0) as u32;
    let bars = f_opt(a, "bars").map(|x| x.clamp(1.0, 999.0) as u32).unwrap_or(song_bars);
    let bpb = f_opt(a, "beats_per_bar").unwrap_or(4.0).clamp(1.0, 16.0) as u32;
    let accent = b_or(a, "accent", true);
    if !(20.0..=400.0).contains(&bpm) {
        bail!("bpm {bpm} outside 20..400");
    }
    let x = click_track(bpm, bars + count_in, bpb, accent);
    let path = match s_opt(a, "path") {
        Some(p) => e.resolve(&p),
        None => e.renders_dir().join(format!("{}_click_{}bpm.wav", crate::samples::sample_name(&e.project.name), bpm.round())),
    };
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::render::write_wav(&path, &x, &x)?;
    Ok(json!({
        "path": path.to_string_lossy(), "bpm": bpm, "bars": bars, "count_in_bars": count_in,
        "beats_per_bar": bpb, "seconds": (x.len() as f32 / SR * 100.0).round() / 100.0,
        "song_starts_at_s": (count_in * bpb) as f32 * 60.0 / bpm,
        "note": "record against this; the song's beat 0 is after the count-in (song_starts_at_s)"
    }))
}

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "export_click",
        description: "Metronome: write a click track WAV to record a vocal or instrument against. With no bpm/bars/beats_per_bar it follows the song: every pattern's time signature (accented bar lines; dotted quarters in 6/8, 9/8, 12/8) and the tempo automation, after count_in_bars (default 1). With bpm/bars/beats_per_bar it writes a fixed grid instead. Returns where the song's beat 0 falls (song_starts_at_s) so the take lines up with add_audio_clip.",
        mutates: false,
        schema: || obj(json!({
            "path": {"type": "string", "description": "Output WAV (default renders/<project>_click_<bpm>bpm.wav)"},
            "bpm": {"type": "number", "description": "Default: the project tempo"},
            "bars": {"type": "integer"},
            "count_in_bars": {"type": "integer", "description": "0..8, default 1"},
            "beats_per_bar": {"type": "integer", "description": "1..16, default 4"},
            "accent": {"type": "boolean", "description": "Accent each downbeat (default true)"}
        }), &[]),
        run: export_click,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_lands_on_every_beat_with_accented_downbeats() {
        let x = click_track(120.0, 2, 4, true);
        let beat = (0.5 * SR) as usize;
        let peak = |at: usize| x[at..at + 400].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        for k in 0..8 {
            assert!(peak(k * beat) > 0.3, "no click on beat {k}");
            assert!(x[k * beat + beat / 2..k * beat + beat / 2 + 400].iter().all(|v| v.abs() < 1e-3), "noise between clicks");
        }
        assert!(peak(0) > peak(beat) * 1.3, "downbeat not accented");
        let dir = std::env::temp_dir().join("bb_click_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        let r = e.call("export_click", &json!({"bars": 2, "count_in_bars": 1})).unwrap();
        assert!(r["seconds"].as_f64().unwrap() > 5.0);
        assert!(std::path::Path::new(r["path"].as_str().unwrap()).exists());
    }

    #[test]
    fn song_click_follows_meter_and_tempo() {
        let mut p = crate::project::Project::new("w", 120.0);
        p.patterns[0].meter = Some((3, 4));
        // 4 bars of 3/4 = 12 clicks, 4 accented; no count-in; 6 s at 120 bpm
        let (x, lead, bars) = song_click(&p, 0, true);
        assert_eq!((bars, lead), (4, 0.0));
        let beat = (0.5 * SR) as usize;
        let peak = |at: usize| x[at..at + 400].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak(0) > peak(beat) * 1.3 && peak(3 * beat) > peak(beat) * 1.3, "bar 2 starts on beat 3");
        assert!((x.len() as f32 / SR - 6.05).abs() < 0.02);
        // half tempo from beat 6: the song gets longer
        p.tempo_points.push(crate::project::TempoPoint { beat: 6.0, bpm: 60.0, ramp: false });
        let (y, _, _) = song_click(&p, 1, true);
        assert!(y.len() > x.len() + (4.0 * SR) as usize);
    }
}
