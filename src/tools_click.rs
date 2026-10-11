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

fn export_click(e: &mut Engine, a: &Value) -> Result<Value> {
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
        description: "Metronome: write a click track WAV in the project's tempo (or bpm) to record a vocal or instrument against: count_in_bars (default 1) before the song, bars (default the song's length), beats_per_bar (default 4), accented downbeats. Returns where the song's beat 0 falls (song_starts_at_s) so the take lines up with add_audio_clip.",
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
}
