//! Groove tools: extract_groove (the feel of a loop, a reference or the
//! singer as a 16-slot template) and steal_groove (lay it onto the beat).

use crate::engine::Engine;
use crate::groove_extract::{self, GrooveTemplate};
use crate::samples;
use crate::tools::{b_or, f_opt, obj, s_opt, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};

fn template_of(e: &Engine, a: &Value) -> Result<GrooveTemplate> {
    if let Some(t) = a.get("template") {
        return Ok(serde_json::from_value(t.clone())?);
    }
    if let Some(p) = s_opt(a, "path") {
        let path = e.resolve(&p);
        let x = samples::decode_file(&path)?;
        let sr = crate::dsp::SR;
        let bpm = f_opt(a, "bpm").unwrap_or_else(|| crate::audio_edit::estimate_bpm(&x).0);
        let on = crate::sc_dsp::detect_transients(&x, 0.5);
        let words: Vec<crate::vocal::Word> = on.iter().map(|&s| crate::vocal::Word { start: s as f32 / sr, end: s as f32 / sr + 0.05, word: String::new(), prob: 1.0 }).collect();
        let te = crate::vocal::estimate_tempo(&[], &words, x.len() as f32 / sr, Some(bpm));
        let step_s = 60.0 / te.bpm / 4.0;
        let pts: Vec<(f32, f32)> = on
            .iter()
            .map(|&s| {
                let pk = x[s..(s + (0.012 * sr) as usize).min(x.len())].iter().fold(0.0f32, |m, v| m.max(v.abs()));
                ((s as f32 / sr - te.downbeat) / step_s, pk)
            })
            .collect();
        return Ok(groove_extract::from_steps(&pts, te.bpm));
    }
    if s_opt(a, "from").as_deref() == Some("vocal") || a.get("from").is_none() {
        let Some(m) = &e.project.vocal_map else {
            bail!("give path (an audio loop / reference), template, or from:'vocal' after vocal_to_song");
        };
        let pts: Vec<(f32, f32)> = m.notes.iter().map(|n| (n.0 * 4.0, n.1.min(2.0).sqrt())).collect();
        return Ok(groove_extract::from_steps(&pts, e.project.bpm));
    }
    bail!("unknown source")
}

/// Dominant low partial of a drum hit (Hz) between lo and hi, read from its
/// body (after the click).
pub fn drum_fundamental(x: &[f32], sr: f32, lo: f32, hi: f32) -> Option<f32> {
    let s0 = (0.012 * sr) as usize;
    let s1 = ((0.22 * sr) as usize).min(x.len());
    if s1 <= s0 + 256 {
        return None;
    }
    let dec = 8usize;
    let y: Vec<f32> = x[s0..s1].chunks(dec).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect();
    let fs = sr / dec as f32;
    let mut best = (0.0f32, 0.0f32);
    let mut f = lo;
    while f <= hi {
        let w = std::f32::consts::TAU * f / fs;
        let (mut re, mut im) = (0.0f32, 0.0f32);
        for (n, v) in y.iter().enumerate() {
            let a = w * n as f32;
            re += v * a.cos();
            im += v * a.sin();
        }
        let p = re * re + im * im;
        if p > best.1 {
            best = (f, p);
        }
        f *= 1.003;
    }
    (best.1 > 1e-6).then_some(best.0)
}

fn tune_drums(e: &mut Engine, a: &Value) -> Result<Value> {
    use crate::instruments::{DrumKind, Instrument};
    let key_pc = crate::theory::pitch_class(&e.project.key_root)? as i32;
    let ivs = crate::theory::scale_intervals(&e.project.scale)?;
    let third = ivs.get(2).copied().unwrap_or(4) as i32;
    let want: Vec<String> = a.get("tracks").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let mut rows = Vec::new();
    for ti in 0..e.project.tracks.len() {
        let t = &e.project.tracks[ti];
        if !want.is_empty() && !want.iter().any(|w| w.eq_ignore_ascii_case(&t.name)) {
            continue;
        }
        let Instrument::Drum(d) = &t.instrument else { continue };
        let (lo, hi, targets): (f32, f32, Vec<i32>) = match d.kind {
            DrumKind::Kick => (30.0, 160.0, vec![0, 7]),
            DrumKind::Tom => (60.0, 400.0, vec![0, third, 7]),
            DrumKind::Snare => (120.0, 400.0, vec![0, third, 7]),
            _ => continue,
        };
        let hit = crate::instruments::render_note(&t.instrument, 60.0, 0.9, 0.3, &e.bank, 1);
        let Some(f0) = drum_fundamental(&hit, crate::dsp::SR, lo, hi) else { continue };
        let m = 69.0 + 12.0 * (f0 / 440.0).log2();
        // nearest key target (root / fifth / third) in any octave
        let mut best = (0.0f32, f32::INFINITY, 0i32);
        for &iv in &targets {
            let pc = (key_pc + iv).rem_euclid(12);
            for oct in 0..8 {
                let c = (oct * 12 + pc) as f32;
                let dd = c - m;
                if dd.abs() < best.1.abs() || best.1.is_infinite() {
                    best = (c, dd, iv);
                }
            }
        }
        let shift = best.1.clamp(-6.0, 6.0);
        let name = t.name.clone();
        let old = d.tune;
        if let Instrument::Drum(dm) = &mut e.project.tracks[ti].instrument {
            dm.tune = ((old + shift) * 100.0).round() / 100.0;
        }
        rows.push(json!({"track": name, "measured_hz": (f0 * 10.0).round() / 10.0, "measured_note": crate::theory::note_name(m.round().clamp(0.0, 127.0) as u8), "target": crate::theory::note_name(best.0 as u8), "degree": match best.2 { 0 => "root", 7 => "fifth", _ => "third" }, "shift_semitones": (shift * 100.0).round() / 100.0}));
    }
    if rows.is_empty() {
        bail!("no kick / tom / snare drum tracks to tune");
    }
    Ok(json!({"key": format!("{} {}", e.project.key_root, e.project.scale), "tuned": rows}))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "tune_drums_to_key",
            description: "Tune the kick (to the root or fifth), toms and snare body (root / third / fifth) to the song key: renders a hit, measures its fundamental, shifts the drum's tune by the nearest interval (<= 6 semitones). Kicks that sit in key make the low end stop fighting the bass. tracks narrows it.",
            mutates: true,
            schema: || obj(json!({"tracks": {"type": "array", "items": {"type": "string"}}}), &[]),
            run: tune_drums,
        },
        Tool {
            name: "extract_groove",
            description: "Measure the feel of a recording as a 16-slot groove template: per-16th microtiming (steps, + = late), accents, swing and looseness. Source: path (a drum loop or reference track; bpm optional) or from:'vocal' (the singer of vocal_to_song). Feed the template to steal_groove.",
            mutates: false,
            schema: || obj(json!({"path": {"type": "string"}, "bpm": {"type": "number"}, "from": {"type": "string", "enum": ["vocal"]}}), &[]),
            run: |e, a| Ok(json!({"template": template_of(e, a)?})),
        },
        Tool {
            name: "steal_groove",
            description: "Lay a recording's feel onto the beat: notes on each 16th take that slot's measured timing offset and accent (blended by amount, default 0.8). Source as extract_groove (path / from:'vocal' / template). tracks default = every drum track; all_patterns default true. Drums that breathe with the singer: steal_groove {from:'vocal'}.",
            mutates: true,
            schema: || obj(json!({"path": {"type": "string"}, "bpm": {"type": "number"}, "from": {"type": "string", "enum": ["vocal"]}, "template": {"type": "object"}, "tracks": {"type": "array", "items": {"type": "string"}}, "amount": {"type": "number", "minimum": 0, "maximum": 1}, "all_patterns": {"type": "boolean"}, "pattern": {"type": "string"}}), &[]),
            run: |e, a| {
                let t = template_of(e, a)?;
                let tracks: Vec<String> = match a.get("tracks").and_then(|v| v.as_array()) {
                    Some(v) => v.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                    None => e.project.tracks.iter().filter(|t| t.instrument.is_drum()).map(|t| t.name.clone()).collect(),
                };
                let pats: Vec<usize> = if b_or(a, "all_patterns", true) && s_opt(a, "pattern").is_none() {
                    (0..e.project.patterns.len()).collect()
                } else {
                    vec![e.project.pattern_index(&s_opt(a, "pattern").unwrap_or_default())?]
                };
                let moved = groove_extract::apply(&mut e.project, &t, &tracks, &pats, f_opt(a, "amount").unwrap_or(0.8));
                Ok(json!({"notes_moved": moved, "tracks": tracks, "swing": t.swing, "looseness_ms": t.looseness_ms, "template": t}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_kick_like_fundamental() {
        let sr = crate::dsp::SR;
        let x: Vec<f32> = (0..(sr * 0.4) as usize).map(|i| { let t = i as f32 / sr; (t * 55.0 * std::f32::consts::TAU).sin() * (-t * 6.0).exp() }).collect();
        let f = drum_fundamental(&x, sr, 30.0, 160.0).unwrap();
        assert!((f - 55.0).abs() < 1.5, "{f}");
    }

    #[test]
    fn kick_lands_on_a_key_tone() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_tune_drums"));
        e.call("new_project", &json!({"name": "t", "bpm": 90})).unwrap();
        e.call("set_key", &json!({"root": "F", "scale": "minor"})).unwrap();
        e.call("add_track", &json!({"name": "kick", "preset": "kick"})).unwrap();
        let r = e.call("tune_drums_to_key", &json!({})).unwrap();
        let row = &r["tuned"][0];
        let t = row["target"].as_str().unwrap();
        assert!(t.starts_with('F') || t.starts_with('C'), "{r}");
        // re-measuring after the shift lands within a quarter tone of the target
        let inst = e.project.tracks[0].instrument.clone();
        let hit = crate::instruments::render_note(&inst, 60.0, 0.9, 0.3, &e.bank, 1);
        let f = drum_fundamental(&hit, crate::dsp::SR, 30.0, 160.0).unwrap();
        let m = 69.0 + 12.0 * (f / 440.0).log2();
        let pc = (m.round() as i32).rem_euclid(12);
        assert!(pc == 5 || pc == 0, "kick at {f} Hz ({m}) {r}");
    }
}
