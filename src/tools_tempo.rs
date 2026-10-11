//! Tempo automation (FL's tempo automation clip): an AI can slow a song into
//! its outro, push a drop, or ramp a build in one call. Points live on the song
//! timeline in beats; the project's bpm is the tempo at beat 0.

use crate::engine::Engine;
use crate::project::TempoPoint;
use crate::tempo_curve::{bpm_at, TempoCurve};
use crate::tools::{f_opt, obj, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};

fn beat_arg(a: &Value, beat_key: &str, bar_key: &str) -> Option<f32> {
    match (f_opt(a, beat_key), f_opt(a, bar_key)) {
        (Some(b), _) => Some(b.max(0.0)),
        (None, Some(bar)) => Some((bar - 1.0).max(0.0) * 4.0),
        _ => None,
    }
}

fn put(e: &mut Engine, beat: f32, bpm: f32, ramp: bool) -> Result<()> {
    if !(20.0..=400.0).contains(&bpm) {
        bail!("bpm {bpm} out of range (20-400)");
    }
    if beat <= 0.0 && !ramp {
        // a jump at beat 0 is the song tempo itself
        e.project.bpm = bpm;
        return Ok(());
    }
    let pts = &mut e.project.tempo_points;
    pts.retain(|p| (p.beat - beat).abs() > 1e-4);
    pts.push(TempoPoint { beat, bpm, ramp });
    pts.sort_by(|a, b| a.beat.partial_cmp(&b.beat).unwrap_or(std::cmp::Ordering::Equal));
    Ok(())
}

fn summary(e: &Engine) -> Value {
    let p = &e.project;
    let curve = TempoCurve::new(p);
    let pts: Vec<Value> = p
        .tempo_points
        .iter()
        .map(|t| json!({"beat": t.beat, "bar": t.beat / 4.0 + 1.0, "bpm": t.bpm, "ramp": t.ramp, "seconds": (curve.secs_at_beat(t.beat as f64) * 100.0).round() / 100.0}))
        .collect();
    let mut beat = 0.0f32;
    let mut secs = Vec::new();
    for s in &p.arrangement {
        let bars = p.patterns.iter().find(|x| x.name == s.pattern).map(|x| x.bars).unwrap_or(4) as f32;
        secs.push(json!({"section": s.pattern, "bar": beat / 4.0 + 1.0, "bpm": (bpm_at(p.bpm, &p.tempo_points, beat) * 10.0).round() / 10.0, "seconds": (curve.secs_at_beat(beat as f64) * 100.0).round() / 100.0}));
        beat += bars * 4.0 * s.repeats.max(1) as f32;
    }
    let len = curve.secs_at_beat(p.song_beats() as f64);
    json!({"base_bpm": p.bpm, "points": pts, "sections": secs, "song_seconds": (len * 100.0).round() / 100.0})
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "add_tempo_point",
            description: "Tempo automation (FL tempo clip): from this song position on, play at bpm. Position as beat (quarter notes from 0) or bar (1-based). ramp=true glides linearly from the previous point (or the song bpm) to this one instead of jumping. A jump at beat 0 sets the song bpm. Notes, automation and audio clips follow the new timing; tempo-synced delays keep the song bpm.",
            mutates: true,
            schema: || obj(json!({
                "beat": {"type": "number"},
                "bar": {"type": "number", "description": "1-based bar (instead of beat)"},
                "bpm": {"type": "number"},
                "ramp": {"type": "boolean", "description": "glide from the previous point (default false: jump)"}
            }), &["bpm"]),
            run: |e, a| {
                let Some(beat) = beat_arg(a, "beat", "bar") else { bail!("add_tempo_point: give beat or bar") };
                let bpm = f_opt(a, "bpm").unwrap_or(0.0);
                let ramp = a.get("ramp").and_then(|v| v.as_bool()).unwrap_or(false);
                put(e, beat, bpm, ramp)?;
                Ok(summary(e))
            },
        },
        Tool {
            name: "tempo_ramp",
            description: "Ramp the tempo between two song positions in one call (a slow-down into an outro, a build that speeds up): holds the tempo up to from_bar, then glides to to_bpm by to_bar and stays there. from_bpm defaults to the tempo already playing at from_bar.",
            mutates: true,
            schema: || obj(json!({
                "from_bar": {"type": "number"}, "to_bar": {"type": "number"},
                "from_beat": {"type": "number"}, "to_beat": {"type": "number"},
                "from_bpm": {"type": "number"}, "to_bpm": {"type": "number"}
            }), &["to_bpm"]),
            run: |e, a| {
                let (Some(b0), Some(b1)) = (beat_arg(a, "from_beat", "from_bar"), beat_arg(a, "to_beat", "to_bar")) else {
                    bail!("tempo_ramp: give from_bar/to_bar (or from_beat/to_beat)")
                };
                if b1 <= b0 {
                    bail!("tempo_ramp: the end ({b1} beats) must come after the start ({b0})");
                }
                let from = f_opt(a, "from_bpm").unwrap_or_else(|| bpm_at(e.project.bpm, &e.project.tempo_points, b0));
                let to = f_opt(a, "to_bpm").unwrap_or(from);
                // points strictly inside the ramp would bend it; they go
                e.project.tempo_points.retain(|p| p.beat <= b0 + 1e-4 || p.beat >= b1 - 1e-4);
                put(e, b0, from, false)?;
                put(e, b1, to, true)?;
                Ok(summary(e))
            },
        },
        Tool {
            name: "list_tempo",
            description: "The tempo map: song bpm, every tempo point (beat, bar, bpm, ramp, seconds), the bpm and start time of each arrangement section, and the song length in seconds with the tempo changes applied.",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| Ok(summary(e)),
        },
        Tool {
            name: "remove_tempo_point",
            description: "Remove tempo automation: the point at beat/bar, or every point with all=true (the song plays at its bpm again).",
            mutates: true,
            schema: || obj(json!({"beat": {"type": "number"}, "bar": {"type": "number"}, "all": {"type": "boolean"}}), &[]),
            run: |e, a| {
                if a.get("all").and_then(|v| v.as_bool()).unwrap_or(false) {
                    e.project.tempo_points.clear();
                    return Ok(summary(e));
                }
                let Some(beat) = beat_arg(a, "beat", "bar") else { bail!("remove_tempo_point: give beat, bar or all") };
                let n = e.project.tempo_points.len();
                e.project.tempo_points.retain(|p| (p.beat - beat).abs() > 1e-3);
                if e.project.tempo_points.len() == n {
                    bail!("no tempo point at beat {beat} (points at: {:?})", e.project.tempo_points.iter().map(|p| p.beat).collect::<Vec<_>>());
                }
                Ok(summary(e))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempo_points_retime_the_render() {
        let dir = std::env::temp_dir().join("bb_tempo_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("new_project", &json!({"name": "t", "bpm": 120})).unwrap();
        e.call("add_track", &json!({"name": "keys", "preset": "piano"})).unwrap();
        // a note on beat 0 and one on beat 8 (step 32, bar 3)
        e.call("add_notes", &json!({"track": "keys", "notes": [{"start": 0, "pitch": 60, "len": 2}, {"start": 32, "pitch": 64, "len": 2}]})).unwrap();
        e.call("set_pattern_length", &json!({"pattern": "A", "bars": 4})).ok();
        let flat = crate::render::schedule(&e.project, &crate::render::RenderOptions::default());
        let at = flat.0[0][1].start as f32 / crate::dsp::SR;
        assert!((at - 4.0).abs() < 0.01, "beat 8 at 120 bpm is 4 s: {at}");
        // half time from bar 2 (beat 4): beat 8 now lands at 2 s + 4 beats at 60 = 6 s
        let r = e.call("add_tempo_point", &json!({"bar": 2, "bpm": 60})).unwrap();
        assert_eq!(r["points"][0]["beat"], 4.0);
        let slow = crate::render::schedule(&e.project, &crate::render::RenderOptions::default());
        let at = slow.0[0][1].start as f32 / crate::dsp::SR;
        assert!((at - 6.0).abs() < 0.01, "after the jump: {at}");
        assert!(slow.1 > flat.1, "the song got longer");
        // a ramp and the list
        e.call("tempo_ramp", &json!({"from_bar": 2, "to_bar": 3, "from_bpm": 120, "to_bpm": 90})).unwrap();
        let l = e.call("list_tempo", &json!({})).unwrap();
        assert_eq!(l["points"].as_array().unwrap().len(), 2);
        assert!(e.call("add_tempo_point", &json!({"bar": 2, "bpm": 900})).is_err());
        e.call("remove_tempo_point", &json!({"all": true})).unwrap();
        assert!(e.project.tempo_points.is_empty());
        // automation follows the curve: the render still runs end to end
        e.call("add_tempo_point", &json!({"bar": 2, "bpm": 100, "ramp": true})).unwrap();
        let m = e.mix().unwrap();
        assert!(m.left.iter().any(|v| v.abs() > 0.01));
    }
}
