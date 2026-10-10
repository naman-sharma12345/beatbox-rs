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

pub fn tools() -> Vec<Tool> {
    vec![
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
