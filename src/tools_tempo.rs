//! Tempo automation (FL's tempo automation clip): an AI can slow a song into
//! its outro, push a drop, or ramp a build in one call. Points live on the song
//! timeline in beats; the project's bpm is the tempo at beat 0.

use crate::engine::Engine;
use crate::project::TempoPoint;
use crate::tempo_curve::{bpm_at, TempoCurve};
use crate::tools::{f_opt, obj, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};

fn beat_arg(e: &Engine, a: &Value, beat_key: &str, bar_key: &str) -> Option<f32> {
    match (f_opt(a, beat_key), f_opt(a, bar_key)) {
        (Some(b), _) => Some(b.max(0.0)),
        (None, Some(bar)) => Some(e.project.beat_at_bar(bar.max(1.0))),
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
        .map(|t| json!({"beat": t.beat, "bar": p.bar_at_beat(t.beat), "bpm": t.bpm, "ramp": t.ramp, "seconds": (curve.secs_at_beat(t.beat as f64) * 100.0).round() / 100.0}))
        .collect();
    let mut beat = 0.0f32;
    let mut secs = Vec::new();
    for s in &p.arrangement {
        let beats = p.patterns.iter().find(|x| x.name == s.pattern).map(|x| x.steps() as f32 / 4.0).unwrap_or(16.0);
        secs.push(json!({"section": s.pattern, "bar": p.bar_at_beat(beat), "bpm": (bpm_at(p.bpm, &p.tempo_points, beat) * 10.0).round() / 10.0, "seconds": (curve.secs_at_beat(beat as f64) * 100.0).round() / 100.0}));
        beat += beats * s.repeats.max(1) as f32;
    }
    let len = curve.secs_at_beat(p.song_beats() as f64);
    json!({"base_bpm": p.bpm, "points": pts, "sections": secs, "song_seconds": (len * 100.0).round() / 100.0})
}

/// Re-fit a pattern's notes from one bar length to another: each note keeps
/// its bar and its place in the bar; notes past the new bar end are dropped
/// (a 4/4 groove becomes its first three beats in 3/4), held notes are
/// clipped at the new bar line or scaled when they span bars.
fn refit(notes: &mut Vec<crate::project::Note>, old_spb: f32, new_spb: f32) -> usize {
    let before = notes.len();
    notes.retain_mut(|n| {
        let bar = (n.start / old_spb).floor();
        let inb = n.start - bar * old_spb;
        if inb >= new_spb - 1e-4 {
            return false;
        }
        n.start = bar * new_spb + inb;
        n.len = if n.len <= old_spb - inb + 1e-4 { n.len.min(new_spb - inb) } else { n.len * new_spb / old_spb };
        true
    });
    before - notes.len()
}

fn set_time_signature(e: &mut Engine, a: &Value) -> Result<Value> {
    let ts = crate::tools::s_req(a, "time_signature")?;
    let Some(m) = crate::project::parse_meter(&ts) else { bail!("time_signature '{ts}' (like 4/4, 3/4, 6/8, 7/8, 5/4)") };
    let which = crate::tools::s_opt(a, "pattern").unwrap_or_else(|| "all".into());
    let do_refit = a.get("refit").and_then(|v| v.as_bool()).unwrap_or(true);
    let new_spb = crate::project::meter_steps(m);
    let mut changed = Vec::new();
    let mut dropped = 0usize;
    for pat in e.project.patterns.iter_mut() {
        if which != "all" && !pat.name.eq_ignore_ascii_case(&which) {
            continue;
        }
        let old_spb = pat.steps_per_bar();
        if do_refit && old_spb != new_spb {
            for notes in pat.clips.values_mut() {
                dropped += refit(notes, old_spb as f32, new_spb as f32);
            }
        }
        pat.meter = if m == (4, 4) { None } else { Some(m) };
        changed.push(pat.name.clone());
    }
    if changed.is_empty() {
        bail!("no pattern '{which}' (patterns: {})", e.project.patterns.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", "));
    }
    let p = &e.project;
    let curve = TempoCurve::new(p);
    Ok(json!({
        "time_signature": ts, "steps_per_bar": new_spb, "patterns": changed, "notes_dropped": dropped,
        "clicks_per_bar": crate::project::meter_clicks(m).len(),
        "song_seconds": (curve.secs_at_beat(p.song_beats() as f64) * 100.0).round() / 100.0
    }))
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
                let Some(beat) = beat_arg(e, a, "beat", "bar") else { bail!("add_tempo_point: give beat or bar") };
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
                let (Some(b0), Some(b1)) = (beat_arg(e, a, "from_beat", "from_bar"), beat_arg(e, a, "to_beat", "to_bar")) else {
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
            name: "set_time_signature",
            description: "Play a pattern (or all patterns) in a time signature: 3/4, 6/8, 7/8, 5/4, 12/8... A bar then holds numerator*16/denominator steps (3/4 and 6/8 = 12, 7/8 = 14), so the song, the playlist, the click and the bar grid follow it. refit (default true) keeps each note's bar and place in the bar and drops what falls past the new bar line (a 4/4 groove becomes its first three beats in 3/4). export_click accents each bar and clicks dotted quarters in 6/8, 9/8, 12/8.",
            mutates: true,
            schema: || obj(json!({
                "time_signature": {"type": "string", "description": "e.g. 3/4, 6/8, 7/8, 5/4 (4/4 resets)"},
                "pattern": {"type": "string", "description": "pattern name, or 'all' (default)"},
                "refit": {"type": "boolean", "description": "move notes to the new bar length (default true)"}
            }), &["time_signature"]),
            run: set_time_signature,
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
                let Some(beat) = beat_arg(e, a, "beat", "bar") else { bail!("remove_tempo_point: give beat, bar or all") };
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

    #[test]
    fn time_signature_changes_the_bar_length() {
        let dir = std::env::temp_dir().join("bb_meter_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("new_project", &json!({"name": "w", "bpm": 120})).unwrap();
        e.call("add_track", &json!({"name": "keys", "preset": "piano"})).unwrap();
        // one note per beat over 2 bars of 4/4
        let notes: Vec<Value> = (0..8).map(|k| json!({"start": k * 4, "pitch": 60, "len": 2})).collect();
        e.call("add_notes", &json!({"track": "keys", "notes": notes})).unwrap();
        let r = e.call("set_time_signature", &json!({"time_signature": "3/4"})).unwrap();
        assert_eq!(r["steps_per_bar"], 12);
        assert_eq!(r["notes_dropped"], 2, "beat 4 of each bar goes");
        let pat = &e.project.patterns[0];
        assert_eq!(pat.steps(), 4 * 12);
        let starts: Vec<f32> = pat.clips["keys"].iter().map(|n| n.start).collect();
        assert_eq!(&starts[..4], &[0.0, 4.0, 8.0, 12.0]);
        // a 4-bar song in 3/4 at 120 bpm lasts 4 * 3 beats = 6 s
        let (_, len) = crate::render::schedule(&e.project, &crate::render::RenderOptions::default());
        assert!((len as f32 / crate::dsp::SR - 6.0).abs() < 0.01);
        assert_eq!(crate::project::meter_clicks((6, 8)).len(), 2, "6/8 clicks dotted quarters");
        assert_eq!(crate::project::meter_steps((7, 8)), 14);
        assert!(e.call("set_time_signature", &json!({"time_signature": "3/5"})).is_err());
        e.call("set_time_signature", &json!({"time_signature": "4/4", "refit": false})).unwrap();
        assert!(e.project.patterns[0].meter.is_none());
    }
}
