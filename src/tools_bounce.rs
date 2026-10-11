//! Bounce (FL "render to audio clip" / freeze): a track's own output, with
//! its instrument, effects, fader, pan and automation, rendered once to a
//! WAV, registered as a sample and placed back on the timeline as an audio
//! clip on a new track that routes and sends the same way. The original is
//! muted (not deleted), so it can be un-bounced. The bounced audio is then
//! editable like any sample: edit_sample (reverse, chop), stretch_sample,
//! slice_sample, vocal_chop, flip_sample.

use crate::engine::Engine;
use crate::instruments::{Instrument, SamplerParams};
use crate::project::{AudioClip, Track};
use crate::samples::SampleInfo;
use crate::render::{self, RenderOptions};
use crate::tools::{b_or, obj, register_sample, s_opt, s_req, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};

fn bounce(e: &mut Engine, a: &Value) -> Result<Value> {
    let src = s_req(a, "track")?;
    let ti = e.project.track_index(&src)?;
    let src = e.project.tracks[ti].name.clone();
    let name = s_opt(a, "name").unwrap_or_else(|| format!("{src}_bounce"));
    if e.project.track_index(&name).is_ok() {
        bail!("track '{name}' already exists (give another name)");
    }
    let mut p = e.project.clone();
    // the source plays alone, unmuted and unsoloed, so its stem is what it
    // sounds like in the mix
    for (i, t) in p.tracks.iter_mut().enumerate() {
        t.solo = false;
        if i == ti {
            t.mute = false;
        }
    }
    e.bank.sync(&p.samples);
    let tail = a.get("tail").and_then(|v| v.as_f64()).unwrap_or(2.0).clamp(0.0, 30.0) as f32;
    let mix = render::render(&p, &e.bank, &RenderOptions { keep_stems: true, tail, ..Default::default() })?;
    let stem = mix.stems.get(ti).ok_or_else(|| anyhow::anyhow!("no stem for '{src}'"))?;
    let peak = stem.left.iter().chain(stem.right.iter()).fold(0.0f32, |m, x| m.max(x.abs()));
    if peak < 1e-5 {
        bail!("'{src}' renders silent (no notes or clips?); nothing was bounced");
    }
    let dir = e.workdir.join("bounces");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.wav", crate::samples::sample_name(&name)));
    render::write_wav(&file, &stem.left, &stem.right)?;
    let reg = register_sample(
        e,
        SampleInfo {
            name: crate::samples::sample_name(&name),
            path: file.to_string_lossy().into(),
            source: format!("bounce of track '{src}'"),
            license: "own render".into(),
            author: String::new(),
            duration: 0.0,
        },
    )?;
    let sample = reg["sample"].as_str().unwrap_or(&name).to_string();
    let orig = e.project.tracks[ti].clone();
    let mut t = Track::new(&name, Instrument::Sampler(SamplerParams { sample: sample.clone(), ..Default::default() }));
    // the stem is post-fader and post-pan: the new channel sits at unity,
    // routed and sending like the original
    t.volume_db = 0.0;
    t.pan = 0.0;
    t.output = orig.output.clone();
    t.sends = orig.sends.clone();
    e.project.tracks.push(t);
    e.project.audio_clips.push(AudioClip { track: name.clone(), sample: sample.clone(), start_beat: 0.0, offset_s: 0.0, length_s: None, gain_db: 0.0, fade_in_ms: None, fade_out_ms: None });
    let mute = b_or(a, "mute_original", true);
    if mute {
        e.project.tracks[ti].mute = true;
    }
    Ok(json!({
        "bounced": src, "track": name, "sample": sample, "file": file.to_string_lossy(),
        "seconds": (stem.left.len() as f32 / crate::dsp::SR * 100.0).round() / 100.0,
        "peak_db": (crate::dsp::gain_to_db(peak) * 10.0).round() / 10.0,
        "original_muted": mute,
        "next": "edit_sample / stretch_sample / slice_sample on the sample; unmute the original and remove_track the bounce to undo"
    }))
}

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "bounce_track",
        description: "Render a track to audio (FL 'render to audio clip' / freeze): its instrument, effects, fader, pan and automation are printed to a WAV, registered as a sample and placed at beat 0 as an audio clip on a new track (name, default '<track>_bounce') that routes and sends like the original; the original is muted (mute_original=false keeps it). Then edit the audio: edit_sample reverse/chop, stretch_sample, slice_sample.",
        mutates: true,
        schema: || obj(json!({
            "track": {"type": "string", "description": "Track to bounce"},
            "name": {"type": "string", "description": "New track (and sample) name, default <track>_bounce"},
            "mute_original": {"type": "boolean", "description": "Mute the source track (default true)"},
            "tail": {"type": "number", "description": "Seconds of ring-out kept (default 2, max 30)"}
        }), &["track"]),
        run: bounce,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounce_prints_a_track_to_an_audio_clip() {
        let dir = std::env::temp_dir().join("bb_bounce_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir);
        e.call("new_project", &json!({"name": "b", "bpm": 120})).unwrap();
        e.call("add_track", &json!({"name": "keys", "preset": "piano"})).unwrap();
        e.call("add_notes", &json!({"track": "keys", "notes": [{"start": 0, "pitch": 60, "len": 8}]})).unwrap();
        let before = e.mix().unwrap();
        let r = e.call("bounce_track", &json!({"track": "keys"})).unwrap();
        assert_eq!(r["track"], "keys_bounce");
        assert!(e.project.tracks[0].mute);
        let after = e.mix().unwrap();
        // the bounce replaces the original at the same level (within 1 dB)
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let n = before.left.len().min(after.left.len());
        let (a, b) = (rms(&before.left[..n]), rms(&after.left[..n]));
        assert!(b > 0.0 && (crate::dsp::gain_to_db(b) - crate::dsp::gain_to_db(a)).abs() < 1.0, "{a} vs {b}");
        assert!(e.call("bounce_track", &json!({"track": "keys"})).is_err(), "same name twice");
    }
}
