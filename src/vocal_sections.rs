//! Melodic vs rap: find which parts of a take are sung (stable, voiced
//! pitch held on notes) and which are rapped or spoken, then give the sung
//! parts their own chain: soft key-locked tuning (never on the rap), a plate,
//! tempo-synced delay with throws on phrase ends, and a double. The rap and
//! sung parts land on separate tracks, so every send and effect switches
//! per section and stays hand-editable.

use crate::dsp::SR;
use crate::engine::Engine;
use crate::project::AudioClip;
use crate::tools::{b_or, f_opt, f_or, obj, s_opt, s_req, Tool};
use crate::vocal_flex::{phrases, Phrase};
use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize)]
pub struct Styled {
    pub start_s: f32,
    pub end_s: f32,
    pub style: String,
    /// 0..1: how sung it is
    pub melodic: f32,
    pub voiced_pct: f32,
    pub held_pct: f32,
    pub phrases: usize,
}

/// How sung one stretch is: the share of frames voiced, the share of
/// voiced frames held within half a semitone of the frame 60 ms before,
/// and the longest held run.
pub fn melodic_score(x: &[f32]) -> (f32, f32, f32) {
    let fr = crate::vocal::pitch_track(x, SR);
    if fr.len() < 8 {
        return (0.0, 0.0, 0.0);
    }
    let loud: Vec<&crate::vocal::PitchFrame> = {
        let mx = fr.iter().map(|f| f.rms).fold(0.0f32, f32::max).max(1e-6);
        fr.iter().filter(|f| f.rms > mx * 0.08).collect()
    };
    if loud.len() < 4 {
        return (0.0, 0.0, 0.0);
    }
    let dt = if fr.len() > 1 { (fr[1].t - fr[0].t).max(1e-3) } else { 0.01 };
    let lag = ((0.06 / dt).round() as usize).max(1);
    let voiced: Vec<usize> = (0..fr.len()).filter(|&i| fr[i].midi > 0.0 && fr[i].rms > 0.0).collect();
    let v = voiced.len() as f32 / loud.len().max(1) as f32;
    let mut held = 0usize;
    let mut run = 0usize;
    let mut best = 0usize;
    for &i in &voiced {
        if i >= lag && fr[i - lag].midi > 0.0 && (fr[i].midi - fr[i - lag].midi).abs() < 0.5 {
            held += 1;
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    let h = held as f32 / voiced.len().max(1) as f32;
    let longest = best as f32 * dt;
    // sung: mostly voiced, mostly held, notes that last
    // voicing only counts when notes are held (a gliding rap line is voiced too)
    let gate = (h / 0.25).clamp(0.0, 1.0);
    let score = (0.35 * gate * ((v - 0.45) / 0.35).clamp(0.0, 1.0) + 0.45 * ((h - 0.35) / 0.35).clamp(0.0, 1.0) + 0.2 * ((longest - 0.12) / 0.3).clamp(0.0, 1.0)).clamp(0.0, 1.0);
    (score, v.min(1.0), h)
}

/// Label each phrase melodic/rap, then merge neighbours of the same style
/// into sections (a section shorter than `min_s` joins its neighbour).
pub fn styles(x: &[f32], min_pause_s: f32, threshold: f32, min_s: f32) -> Vec<Styled> {
    let ph: Vec<Phrase> = phrases(x, min_pause_s, None);
    let mut v: Vec<Styled> = ph
        .iter()
        .map(|p| {
            let (s0, s1) = ((p.start_s * SR) as usize, ((p.end_s * SR) as usize).min(x.len()));
            let (m, vo, h) = melodic_score(&x[s0..s1]);
            Styled { start_s: p.start_s, end_s: p.end_s, style: if m >= threshold { "melodic".into() } else { "rap".into() }, melodic: m, voiced_pct: vo * 100.0, held_pct: h * 100.0, phrases: 1 }
        })
        .collect();
    // short islands take their neighbours' style
    let n = v.len();
    for i in 0..n {
        if v[i].end_s - v[i].start_s < min_s * 0.5 && i > 0 && i + 1 < n && v[i - 1].style == v[i + 1].style && v[i].style != v[i - 1].style {
            v[i].style = v[i - 1].style.clone();
        }
    }
    let mut out: Vec<Styled> = Vec::new();
    for s in v {
        match out.last_mut() {
            Some(l) if l.style == s.style => {
                let w0 = l.phrases as f32;
                l.melodic = (l.melodic * w0 + s.melodic) / (w0 + 1.0);
                l.voiced_pct = (l.voiced_pct * w0 + s.voiced_pct) / (w0 + 1.0);
                l.held_pct = (l.held_pct * w0 + s.held_pct) / (w0 + 1.0);
                l.end_s = s.end_s;
                l.phrases += 1;
            }
            _ => out.push(s),
        }
    }
    for s in out.iter_mut() {
        s.melodic = (s.melodic * 100.0).round() / 100.0;
        s.voiced_pct = s.voiced_pct.round();
        s.held_pct = s.held_pct.round();
    }
    out
}

fn r3(x: f32) -> f32 {
    (x * 1000.0).round() / 1000.0
}

fn detect_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (name, l, r) = crate::tools_sound::audio_for(e, a)?;
    if s_opt(a, "sample").is_none() && s_opt(a, "path").is_none() {
        bail!("give the vocal: sample or path");
    }
    let x: Vec<f32> = l.iter().zip(r.iter()).map(|(p, q)| 0.5 * (p + q)).collect();
    let st = styles(&x, f_or(a, "min_pause_s", 0.35), f_or(a, "threshold", 0.3), f_or(a, "min_section_s", 2.0));
    let bpm = e.project.bpm;
    let sb = f_opt(a, "start_beat");
    let rows: Vec<Value> = st
        .iter()
        .map(|s| {
            let mut v = json!({"style": s.style, "start_s": r3(s.start_s), "end_s": r3(s.end_s), "melodic": s.melodic, "voiced_pct": s.voiced_pct, "held_pct": s.held_pct, "phrases": s.phrases});
            if let Some(b) = sb {
                v["start_beat"] = json!(r3(b + s.start_s * bpm / 60.0));
                v["end_beat"] = json!(r3(b + s.end_s * bpm / 60.0));
            }
            v
        })
        .collect();
    let sung: f32 = st.iter().filter(|s| s.style == "melodic").map(|s| s.end_s - s.start_s).sum();
    let all: f32 = st.iter().map(|s| s.end_s - s.start_s).sum::<f32>().max(1e-3);
    Ok(json!({"source": name, "sections": rows, "melodic_share_pct": (sung / all * 100.0).round(), "note": "melodic = voiced and held on notes (sung); rap = short, gliding or unvoiced syllables. threshold (default 0.3) moves the line."}))
}

/// Tune only inside `ranges` (seconds), with 30 ms crossfades at the edges.
pub fn tune_ranges(x: &[f32], ranges: &[(f32, f32)], key_pc: u8, scale: &[u8], amount: f32, speed_ms: f32) -> (Vec<f32>, usize) {
    let (tuned, stats) = crate::vocal::autotune(x, SR, key_pc, scale, amount, 0.0, speed_ms);
    let n = x.len().min(tuned.len());
    let mut w = vec![0.0f32; n];
    let xf = (0.03 * SR) as usize;
    for &(a, b) in ranges {
        let (s0, s1) = (((a * SR) as usize).min(n), ((b * SR) as usize).min(n));
        for i in s0..s1 {
            let up = ((i - s0) as f32 / xf as f32).min(1.0);
            let down = ((s1 - i) as f32 / xf as f32).min(1.0);
            w[i] = w[i].max(up.min(down));
        }
    }
    let mut y = x.to_vec();
    for i in 0..n {
        let g = (w[i] * std::f32::consts::FRAC_PI_2).sin();
        let h = (w[i] * std::f32::consts::FRAC_PI_2).cos();
        y[i] = x[i] * h + tuned[i] * g;
    }
    (y, stats.notes_moved)
}

fn ensure_bus(e: &mut Engine, name: &str, effects: &[Value]) -> Result<bool> {
    if e.project.buses.iter().any(|b| b.name.eq_ignore_ascii_case(name)) {
        return Ok(false);
    }
    e.call("add_bus", &json!({"name": name, "preset": "none"}))?;
    for fx in effects {
        let mut f = fx.clone();
        f["track"] = json!(name);
        e.call("add_effect", &f)?;
    }
    Ok(true)
}

fn ensure_audio_track(e: &mut Engine, name: &str, like: usize, volume_db: f32) -> Result<String> {
    if let Ok(i) = e.project.track_index(name) {
        return Ok(e.project.tracks[i].name.clone());
    }
    e.call("add_track", &json!({"name": name, "preset": "pad", "volume_db": volume_db}))?;
    let ni = e.project.track_index(name)?;
    // the same channel as the lead vocal (its EQ / compression / de-ess)
    let fx = e.project.tracks[like].effects.clone();
    e.project.tracks[ni].effects = fx;
    Ok(e.project.tracks[ni].name.clone())
}

fn chain_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let sample = s_req(a, "sample")?;
    let (info, x) = crate::tools_sound::sample_data(e, &sample)?;
    let track = s_opt(a, "track").unwrap_or_else(|| "vocal".into());
    let ti = e.project.track_index(&track)?;
    let tname = e.project.tracks[ti].name.clone();
    let bpm = e.project.bpm;
    let clips: Vec<usize> = (0..e.project.audio_clips.len()).filter(|&i| e.project.audio_clips[i].track.eq_ignore_ascii_case(&tname) && e.project.audio_clips[i].sample.eq_ignore_ascii_case(&info.name)).collect();
    if clips.is_empty() {
        bail!("no clip of '{}' on '{tname}': add_audio_clip the take first (or place_vocal_phrases)", info.name);
    }
    let st = styles(&x, f_or(a, "min_pause_s", 0.35), f_or(a, "threshold", 0.3), 2.0);
    let mel: Vec<(f32, f32)> = st.iter().filter(|s| s.style == "melodic").map(|s| (s.start_s, s.end_s)).collect();
    if mel.is_empty() {
        return Ok(json!({"melodic_sections": 0, "note": "no sung part found in the take (all rap); nothing changed", "sections": st}));
    }
    // 1. soft tune, melodic parts only
    let key_pc = crate::tools::key_pc(&e.project);
    let scale = crate::theory::scale_intervals(&e.project.scale)?.to_vec();
    let tune = b_or(a, "tune", true);
    let (tuned_name, moved) = if tune {
        let (y, moved) = tune_ranges(&x, &mel, key_pc, &scale, f_or(a, "tune_amount", 0.8).clamp(0.0, 1.0), f_or(a, "retune_ms", 15.0).clamp(1.0, 200.0));
        let nn = crate::samples::sample_name(&format!("{}_mtune", info.name));
        let path = e.samples_dir().join(format!("{nn}.wav"));
        crate::render::write_wav(&path, &y, &y)?;
        let si = crate::samples::SampleInfo { name: nn, path: path.to_string_lossy().into(), source: format!("{} (soft tune on the sung parts)", info.source), license: info.license.clone(), author: info.author.clone(), duration: 0.0 };
        let reg = crate::tools::register_sample(e, si)?;
        (reg["sample"].as_str().unwrap_or("").to_string(), moved)
    } else {
        (info.name.clone(), 0)
    };
    // 2. returns: plate and a synced delay (HPF/LPF in the bus, ducked by the dry vocal)
    let plate = s_opt(a, "plate_bus").unwrap_or_else(|| "vox_plate".into());
    let delay = s_opt(a, "delay_bus").unwrap_or_else(|| "vox_delay".into());
    let delay_steps = f_or(a, "delay_steps", 6.0).clamp(1.0, 16.0);
    let new_plate = ensure_bus(e, &plate, &[json!({"type": "reverb", "params": {"mode": "fdn_plate", "decay_s": f_or(a, "plate_s", 2.0).clamp(0.6, 5.0), "predelay_ms": 40.0, "mix": 1.0, "low_cut_hz": 250.0}}), json!({"type": "parametric_eq", "params": {"bands": [{"kind": "high_cut", "freq": 9000.0}]}})])?;
    let mel_track = ensure_audio_track(e, &format!("{tname}_melodic"), ti, e.project.tracks[ti].volume_db)?;
    let new_delay = ensure_bus(e, &delay, &[json!({"type": "delay", "params": {"steps": delay_steps, "feedback": 0.35, "mix": 1.0, "ping_pong": true, "tone": 4500.0}}), json!({"type": "parametric_eq", "params": {"bands": [{"kind": "low_cut", "freq": 300.0}, {"kind": "high_cut", "freq": 7000.0}]}}), json!({"type": "sidechain", "params": {"source": mel_track.clone(), "amount": 0.5, "release_ms": 180.0}})])?;
    // 3. split the take's clips: sung stretches move to the melodic track (tuned)
    let mut moved_clips = 0;
    let mut new_clips: Vec<AudioClip> = Vec::new();
    let mut keep: Vec<AudioClip> = Vec::new();
    for (i, c) in e.project.audio_clips.iter().enumerate() {
        if !clips.contains(&i) {
            keep.push(c.clone());
            continue;
        }
        let s = c.offset_s.max(0.0);
        let end = c.length_s.map(|l| s + l).unwrap_or(x.len() as f32 / SR);
        let at = |t: f32| c.start_beat + (t - s) * bpm / 60.0;
        let mut cuts: Vec<(f32, f32, bool)> = Vec::new();
        let mut t = s;
        for &(m0, m1) in &mel {
            let (a0, a1) = (m0.max(s), m1.min(end));
            if a1 <= a0 + 0.05 {
                continue;
            }
            if a0 > t + 0.02 {
                cuts.push((t, a0, false));
            }
            cuts.push((a0, a1, true));
            t = a1;
        }
        if end > t + 0.02 {
            cuts.push((t, end, false));
        }
        for (c0, c1, sung) in cuts {
            let mut nc = c.clone();
            nc.start_beat = at(c0);
            nc.offset_s = c0;
            nc.length_s = Some(c1 - c0);
            nc.fade_in_ms = Some(10.0);
            nc.fade_out_ms = Some(10.0);
            if sung {
                nc.track = mel_track.clone();
                nc.sample = tuned_name.clone();
                moved_clips += 1;
                new_clips.push(nc);
            } else {
                keep.push(nc);
            }
        }
    }
    let sung_clips = new_clips.clone();
    keep.extend(new_clips);
    e.project.audio_clips = keep;
    // 4. sends from the sung track
    e.call("set_send", &json!({"track": mel_track, "bus": plate, "db": f_or(a, "plate_send_db", -9.0)}))?;
    e.call("set_send", &json!({"track": mel_track, "bus": delay, "db": f_or(a, "delay_send_db", -18.0)}))?;
    // 5. throws: the last 350 ms of each sung phrase, heard only in the delay
    let mut throws = 0;
    if b_or(a, "throws", true) {
        let th = ensure_audio_track(e, &format!("{tname}_throw"), ti, -60.0)?;
        let thi = e.project.track_index(&th)?;
        e.project.tracks[thi].effects.clear();
        e.project.tracks[thi].volume_db = -60.0;
        for c in &sung_clips {
            let len = c.length_s.unwrap_or(0.0);
            if len < 0.6 {
                continue;
            }
            let mut t = c.clone();
            t.track = th.clone();
            t.offset_s = c.offset_s + len - 0.35;
            t.length_s = Some(0.35);
            t.start_beat = c.start_beat + (len - 0.35) * bpm / 60.0;
            e.project.audio_clips.push(t);
            throws += 1;
        }
        e.call("set_send", &json!({"track": th, "bus": delay, "db": f_or(a, "throw_db", -4.0), "pre_fader": true}))?;
    }
    // 6. a double (chorus + haas, -9 dB, 15 ms late) on the sung parts
    let mut doubled = false;
    if b_or(a, "double", true) {
        let dn = ensure_audio_track(e, &format!("{tname}_double"), ti, e.project.tracks[ti].volume_db - 9.0)?;
        let di = e.project.track_index(&dn)?;
        if !e.project.tracks[di].effects.iter().any(|f| f.type_name() == "haas") {
            e.call("add_effect", &json!({"track": dn, "type": "chorus", "params": {"rate_hz": 0.4, "depth_ms": 4.0, "mix": 0.6}}))?;
            e.call("add_effect", &json!({"track": dn, "type": "haas", "params": {"delay_ms": 14.0, "mix": 1.0}}))?;
        }
        for c in &sung_clips {
            let mut d = c.clone();
            d.track = dn.clone();
            d.start_beat += 0.015 * bpm / 60.0;
            e.project.audio_clips.push(d);
        }
        e.call("set_send", &json!({"track": dn, "bus": plate, "db": -12.0}))?;
        doubled = true;
    }
    e.revision += 1;
    Ok(json!({
        "sections": st,
        "melodic_sections": mel.len(),
        "tuned_sample": tuned_name,
        "notes_tuned": moved,
        "melodic_track": mel_track,
        "clips_moved_to_melodic": moved_clips,
        "buses_created": {"plate": new_plate, "delay": new_delay},
        "delay": format!("{} 16ths ({})", delay_steps, if (delay_steps - 6.0).abs() < 0.01 { "dotted 1/4" } else if (delay_steps - 3.0).abs() < 0.01 { "dotted 1/8" } else { "custom" }),
        "throws": throws,
        "doubled": doubled,
        "note": "rap parts stay dry on the original track (no tuning); sung parts get soft tune, plate, delay, throws and a double on their own tracks; everything stays editable (set_send, tweak_effect, remove_audio_clip)",
    }))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "detect_vocal_styles",
            description: "Melodic vs rap sections in a vocal take: each phrase is scored by voicing and how steadily pitch is held (sung notes) vs gliding/short syllables (rap), then merged into time ranges (start_s/end_s, and song beats when start_beat is given). threshold 0..1 (default 0.3). Read-only; sample or path.",
            mutates: false,
            schema: || obj(json!({"sample": {"type": "string"}, "path": {"type": "string"}, "threshold": {"type": "number"}, "min_pause_s": {"type": "number"}, "min_section_s": {"type": "number"}, "start_beat": {"type": "number"}}), &[]),
            run: detect_tool,
        },
        Tool {
            name: "melodic_vocal_chain",
            description: "Give the sung parts of a vocal take their own sound, leaving the rap dry: finds melodic sections, soft-tunes only those (key-locked, retune_ms default 15, tune_amount 0.8; never on rap), moves them to <track>_melodic (same channel FX), sends them to a plate (vox_plate, plate_s 2.0, 40 ms pre-delay) and a tempo-synced delay (vox_delay, delay_steps 6 = dotted 1/4, HPF/LPF, ducked by the voice), adds delay throws on phrase ends (<track>_throw) and a double (<track>_double: chorus + haas, -9 dB). The take must already be on track (default 'vocal'). Options: tune, throws, double (all default true), plate_send_db -9, delay_send_db -18.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "track": {"type": "string"}, "threshold": {"type": "number"}, "min_pause_s": {"type": "number"}, "tune": {"type": "boolean"}, "tune_amount": {"type": "number"}, "retune_ms": {"type": "number"}, "plate_bus": {"type": "string"}, "delay_bus": {"type": "string"}, "plate_s": {"type": "number"}, "delay_steps": {"type": "number"}, "plate_send_db": {"type": "number"}, "delay_send_db": {"type": "number"}, "throws": {"type": "boolean"}, "throw_db": {"type": "number"}, "double": {"type": "boolean"}}), &["sample"]),
            run: chain_tool,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1.5 s held notes (sung) then fast gliding/noisy syllables (rap).
    fn sung_then_rap() -> Vec<f32> {
        let mut x = Vec::new();
        for (k, f) in [220.0f32, 247.0, 262.0].iter().enumerate() {
            for i in 0..(0.9 * SR) as usize {
                let t = i as f32 / SR;
                let env = (t / 0.02).min(1.0) * ((0.9 - t) / 0.05).min(1.0);
                x.push(0.3 * env * ((t * f * std::f32::consts::TAU).sin() + 0.4 * (t * 2.0 * f * std::f32::consts::TAU).sin()));
            }
            let _ = k;
            x.extend(std::iter::repeat(0.0).take((0.45 * SR) as usize));
        }
        x.extend(std::iter::repeat(0.0).take((0.3 * SR) as usize));
        let mut rng = crate::dsp::Rng::new(4);
        for p in 0..3 {
            for s in 0..8 {
                // 90 ms syllables, pitch sliding fast, plus noise
                let f0 = 180.0 + 60.0 * ((s + p) % 3) as f32;
                let mut ph = 0.0f32;
                for i in 0..(0.09 * SR) as usize {
                    let t = i as f32 / SR;
                    let f = f0 * (1.0 + 2.5 * t);
                    ph += f / SR;
                    let env = (t / 0.01).min(1.0) * ((0.09 - t) / 0.02).min(1.0);
                    x.push(0.3 * env * ((ph * std::f32::consts::TAU).sin() * 0.6 + 0.4 * rng.bipolar()));
                }
                x.extend(std::iter::repeat(0.0).take((0.03 * SR) as usize));
            }
            x.extend(std::iter::repeat(0.0).take((0.45 * SR) as usize));
        }
        x
    }

    #[test]
    fn sung_and_rapped_parts_are_told_apart() {
        let x = sung_then_rap();
        let st = styles(&x, 0.35, 0.3, 2.0);
        assert!(st.len() >= 2, "{st:?}");
        assert_eq!(st[0].style, "melodic", "{st:?}");
        assert_eq!(st.last().unwrap().style, "rap", "{st:?}");
        assert!(st[0].end_s < 4.5, "{st:?}");
    }

    #[test]
    fn chain_moves_sung_parts_and_sends_them() {
        let dir = std::env::temp_dir().join("beatbox_vocal_styles_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("new_project", &json!({"name": "mel", "bpm": 90})).unwrap();
        let x = sung_then_rap();
        let wav = dir.join("take.wav");
        crate::render::write_wav(&wav, &x, &x).unwrap();
        e.call("import_sample", &json!({"path": wav.to_string_lossy(), "name": "take"})).unwrap();
        e.call("add_track", &json!({"name": "vocal", "preset": "pad"})).unwrap();
        e.call("add_audio_clip", &json!({"track": "vocal", "sample": "take", "start_beat": 0})).unwrap();
        let r = e.call("melodic_vocal_chain", &json!({"sample": "take"})).unwrap();
        assert!(r["clips_moved_to_melodic"].as_u64().unwrap() >= 1, "{r}");
        assert!(e.project.audio_clips.iter().any(|c| c.track == "vocal_melodic"));
        assert!(e.project.audio_clips.iter().any(|c| c.track == "vocal"), "rap stays on vocal");
        assert!(e.project.buses.iter().any(|b| b.name == "vox_plate"));
        let m = e.mix().unwrap();
        assert!(m.left.iter().all(|v| v.is_finite()));
    }
}
