//! Vocal-to-song tools: transcribe_lyrics, analyze_vocal, vocal_to_song,
//! plus timeline audio clips (add_audio_clip / list_audio_clips /
//! remove_audio_clip) that carry a recorded take through a track's channel.

use crate::dsp::{Rng, SR};
use crate::engine::Engine;
use crate::project::{AudioClip, Note, Pattern, Section, VocalMap, STEPS_PER_BAR};
use crate::samples::{self, SampleInfo};
use crate::theory::{self, Chord};
use crate::tools::{b_or, ensure_track, f_opt, find, obj, profile, register_sample, s_opt, s_req, seed_of, u_or, Tool};
use crate::vocal::{self, Lyrics, VocalAnalysis};
use anyhow::{bail, Result};
use serde_json::{json, Value};

fn call(e: &mut Engine, name: &str, args: Value) -> Result<Value> {
    (find(name).expect("tool").run)(e, &args)
}

fn lyrics_for(e: &Engine, path: &std::path::Path, a: &Value) -> (Option<Lyrics>, Option<String>) {
    if !b_or(a, "lyrics", true) {
        return (None, None);
    }
    let model = s_opt(a, "model").unwrap_or_else(|| "base".into());
    match vocal::transcribe(path, &e.workdir, &model, s_opt(a, "language").as_deref(), s_opt(a, "known_lyrics").as_deref()) {
        Ok(l) => (Some(l), None),
        Err(err) => (None, Some(format!("lyrics skipped: {err}"))),
    }
}

fn analysis_json(a: &VocalAnalysis, notes_max: usize) -> Value {
    json!({
        "duration_s": (a.duration * 100.0).round() / 100.0,
        "key": format!("{} {}", a.key_root, a.scale),
        "key_confidence": (a.key_confidence * 100.0).round() / 100.0,
        "key_alternatives": a.key_alternatives,
        "bpm": a.tempo.bpm,
        "tempo_candidates": a.tempo.candidates,
        "tempo_confidence": (a.tempo.confidence * 100.0).round() / 100.0,
        "first_downbeat_s": (a.tempo.downbeat * 1000.0).round() / 1000.0,
        "timing_spread_ms": a.tempo.timing_spread_ms.round(),
        "tuning_cents": a.tuning_cents,
        "range": [a.range.0, a.range.1],
        "voiced_percent": a.voiced_percent,
        "phrases": a.phrases.iter().map(|p| [((p.0 * 100.0).round() / 100.0), ((p.1 * 100.0).round() / 100.0)]).collect::<Vec<_>>(),
        "note_count": a.notes.len(),
        "notes": a.notes.iter().take(notes_max).map(|n| json!({"t": (n.start * 100.0).round() / 100.0, "dur": (n.dur() * 100.0).round() / 100.0, "note": theory::note_name(n.pitch)})).collect::<Vec<_>>(),
    })
}

/// A section of the song built around the vocal.
#[derive(Clone, Debug)]
struct Sec {
    name: String,
    kind: &'static str,
    bar0: u32,
    bars: u32,
    lyrics: String,
}

fn quantize_notes(notes: &[(f32, f32, u8)], bar0: u32, bars: u32) -> Vec<Note> {
    // notes in song beats -> steps inside the section, 16th grid
    let s0 = (bar0 * STEPS_PER_BAR) as f32;
    let s1 = s0 + (bars * STEPS_PER_BAR) as f32;
    let mut out: Vec<Note> = Vec::new();
    for &(b, len, p) in notes {
        let st = (b * 4.0).round();
        if st < s0 || st >= s1 {
            continue;
        }
        let l = (len * 4.0).round().max(1.0).min(s1 - st);
        if let Some(prev) = out.last_mut() {
            if (prev.start - (st - s0)).abs() < 0.5 {
                continue;
            }
            prev.len = prev.len.min(st - s0 - prev.start).max(1.0);
        }
        out.push(Note::new(st - s0, l, p, 0.78));
    }
    out
}

/// The lead-vocal chain: HPF, EQ (less boxiness, more presence and air),
/// compression, de-essing, reverb and delay sends, and the vocal's presence
/// range carved out of the chords and lead so the words cut through.
pub fn vocal_chain(e: &mut Engine) -> Result<()> {
    call(e, "add_effect", json!({"track": "vocal", "type": "filter", "params": {"mode": "highpass", "cutoff": 95.0}}))?;
    call(e, "add_effect", json!({"track": "vocal", "type": "parametric_eq", "params": {"bands": [
        {"kind": "bell", "freq": 300.0, "gain_db": -2.5, "q": 1.0},
        {"kind": "bell", "freq": 3200.0, "gain_db": 2.0, "q": 0.9},
        {"kind": "high_shelf", "freq": 10000.0, "gain_db": 2.5, "q": 0.7}]}}))?;
    call(e, "add_effect", json!({"track": "vocal", "type": "compressor", "params": {"threshold_db": -20.0, "ratio": 3.5, "attack_ms": 6.0, "release_ms": 90.0, "makeup_db": 4.0}}))?;
    call(e, "add_effect", json!({"track": "vocal", "type": "deesser", "params": {"freq": 6500.0, "threshold_db": -26.0}}))?;
    call(e, "add_bus", json!({"name": "vox_verb"}))?;
    call(e, "add_effect", json!({"track": "vox_verb", "type": "reverb", "params": {"size": 0.75, "mix": 1.0, "predelay_ms": 40.0, "low_cut_hz": 250.0}}))?;
    call(e, "add_bus", json!({"name": "vox_delay"}))?;
    call(e, "add_effect", json!({"track": "vox_delay", "type": "delay", "params": {"steps": 3.0, "feedback": 0.3, "mix": 1.0, "ping_pong": true}}))?;
    call(e, "set_send", json!({"track": "vocal", "bus": "vox_verb", "db": -11.0}))?;
    call(e, "set_send", json!({"track": "vocal", "bus": "vox_delay", "db": -17.0}))?;
    // carve the vocal's presence range out of the chords/lead
    for t in ["chords", "lead"] {
        if e.project.track_index(t).is_ok() {
            call(e, "add_effect", json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "bell", "freq": 2800.0, "gain_db": -3.0, "q": 0.8}]}}))?;
        }
    }
    Ok(())
}

fn vocal_to_song(e: &mut Engine, a: &Value) -> Result<Value> {
    let path = e.resolve(&s_req(a, "path")?);
    if !path.exists() {
        bail!("no file at {}", path.display());
    }
    let x = samples::decode_file(&path)?;
    if x.len() < (SR * 2.0) as usize {
        bail!("the vocal is shorter than 2 seconds");
    }
    let (lyrics, lyr_note) = lyrics_for(e, &path, a);
    let tune_amount = f_opt(a, "tune").unwrap_or(0.0).clamp(0.0, 1.0);
    let words = lyrics.as_ref().map(|l| l.words()).unwrap_or_default();
    let an = vocal::analyze(&x, SR, &words, f_opt(a, "bpm"));
    if an.notes.len() < 4 {
        bail!("could not hear a sung melody in this file (only {} notes); is it a dry vocal?", an.notes.len());
    }
    let style_in = s_opt(a, "style").unwrap_or_else(|| {
        if an.tempo.bpm >= 125.0 { "house" } else if an.tempo.bpm >= 95.0 { "afrobeats" } else { "boom_bap" }.to_string()
    });
    let g = theory::groove(&style_in)?;
    let prof = profile(g.name);
    let seed = seed_of(a);
    let mut rng = Rng::new(seed);
    // grid tempo: the singer's tempo (double-time when very slow)
    let mut bpm = an.tempo.bpm;
        if bpm < 72.0 && f_opt(a, "bpm").is_none() {
        bpm *= 2.0;
    }
    let key_root = s_opt(a, "key").unwrap_or_else(|| an.key_root.clone());
    let scale = s_opt(a, "scale").unwrap_or_else(|| an.scale.clone());
    let key_pc = theory::pitch_class(&key_root)?;
    let intro_bars = (u_or(a, "intro_bars", 4) as u32).clamp(0, 16);
    let outro_bars = (u_or(a, "outro_bars", 4) as u32).clamp(0, 16);
    let section_bars = (u_or(a, "section_bars", 8) as u32).clamp(2, 16);

    // place the vocal. With warp (default) every sung phrase start is pinned to
    // a bar line and the take is time-stretched between them (pitch kept), so a
    // free-time singer locks to the grid; without it the take keeps its own
    // timing and its first bar line lands on bar `intro_bars`.
    // optional pitch correction to the song key, before the warp
    let mut tune_stats = None;
    let x = if tune_amount > 0.0 {
        let ivs = theory::scale_intervals(if scale == "major" { "major" } else { "minor" })?;
        let (y, st) = vocal::autotune(&x, SR, key_pc, ivs, tune_amount, f_opt(a, "tune_hard").unwrap_or(0.1), 35.0);
        tune_stats = Some(st);
        y
    } else {
        x
    };
    let anchors = vocal::phrase_anchors(&an.notes, 0.28, 1.4);
    let warp = if b_or(a, "warp", true) && anchors.len() >= 3 {
        let w = vocal::fit_warp(&anchors, bpm);
        bpm = w.bpm;
        Some(w)
    } else {
        None
    };
    let beat_s = 60.0 / bpm;
    let bar_s = 4.0 * beat_s;
    let (clip_start_beat, song_beat): (f32, Box<dyn Fn(f32) -> f32>) = match &warp {
        Some(w) => {
            let w2 = w.clone();
            let base = intro_bars as f32 * 4.0;
            (base + w.beat_at(0.0), Box::new(move |t: f32| base + w2.beat_at(t)))
        }
        None => {
            let db = an.tempo.downbeat;
            let first = an.notes[0].start;
            let k = ((first - db) / bar_s).floor();
            let first_bar_t = db + k * bar_s;
            let c = intro_bars as f32 * 4.0 - first_bar_t / beat_s;
            (c, Box::new(move |t: f32| c + t / beat_s))
        }
    };
    let last_end = an.notes.last().map(|n| n.end).unwrap_or(an.duration);
    let vocal_end_bar = (song_beat(last_end) / 4.0).ceil() as u32;
    let vocal_bars = vocal_end_bar.saturating_sub(intro_bars).max(1);

    // melody on the song grid
    let mel: Vec<(f32, f32, u8)> = an.notes.iter().map(|n| (song_beat(n.start), n.dur() / beat_s, n.pitch)).collect();

    // sections of `section_bars` over the vocal
    let mut secs: Vec<Sec> = Vec::new();
    if intro_bars > 0 {
        secs.push(Sec { name: "intro".into(), kind: "intro", bar0: 0, bars: intro_bars, lyrics: String::new() });
    }
    let mut b = intro_bars;
    let end = intro_bars + vocal_bars;
    while b < end {
        let mut n = section_bars.min(end - b);
        if end - (b + n) > 0 && end - (b + n) < section_bars / 2 {
            n = end - b; // fold a short remainder into this section
        }
        let text: Vec<String> = words
            .iter()
            .filter(|w| {
                let sb = song_beat(w.start) / 4.0;
                sb >= b as f32 && sb < (b + n) as f32
            })
            .map(|w| w.word.clone())
            .collect();
        let sung = mel.iter().any(|m| m.0 / 4.0 >= b as f32 && m.0 / 4.0 < (b + n) as f32);
        secs.push(Sec { name: String::new(), kind: if sung { "verse" } else { "break" }, bar0: b, bars: n, lyrics: text.join(" ") });
        b += n;
    }
    // the hook: the vocal section whose lyrics repeat most (or the highest-sung one)
    let vidx: Vec<usize> = (0..secs.len()).filter(|&i| secs[i].kind == "verse").collect();
    let mut rep = vec![0.0f32; secs.len()];
    for &i in &vidx {
        for &j in &vidx {
            if i != j {
                let s = vocal::lyric_similarity(&secs[i].lyrics, &secs[j].lyrics);
                if s >= 0.45 {
                    rep[i] += s;
                }
            }
        }
    }
    let any_rep = vidx.iter().any(|&i| rep[i] > 0.0);
    let mean_pitch = |s: &Sec| -> f32 {
        let v: Vec<f32> = mel.iter().filter(|m| m.0 / 4.0 >= s.bar0 as f32 && m.0 / 4.0 < (s.bar0 + s.bars) as f32).map(|m| m.2 as f32).collect();
        if v.is_empty() { 0.0 } else { v.iter().sum::<f32>() / v.len() as f32 }
    };
    if any_rep {
        let best = vidx.iter().map(|&i| rep[i]).fold(0.0f32, f32::max);
        for &i in &vidx {
            if rep[i] >= best * 0.6 {
                secs[i].kind = "hook";
            }
        }
    } else if vidx.len() >= 2 {
        let mut mp: Vec<f32> = vidx.iter().map(|&i| mean_pitch(&secs[i])).collect();
        let med = { let mut m = mp.clone(); m.sort_by(|a, b| a.partial_cmp(b).unwrap()); m[m.len() / 2] };
        for (k, &i) in vidx.iter().enumerate() {
            if mp[k] > med {
                secs[i].kind = "hook";
            }
        }
        mp.clear();
    }
    // the last vocal section rides home as a hook when nothing else is
    if !secs.iter().any(|s| s.kind == "hook") {
        if let Some(&l) = vidx.last() {
            secs[l].kind = "hook";
        }
    }
    let outro_bar0 = intro_bars + vocal_bars;
    if outro_bars > 0 {
        secs.push(Sec { name: "outro".into(), kind: "outro", bar0: outro_bar0, bars: outro_bars, lyrics: String::new() });
    }
    let mut counts = std::collections::BTreeMap::new();
    for s in secs.iter_mut() {
        if s.name.is_empty() {
            let c = counts.entry(s.kind).or_insert(0);
            *c += 1;
            s.name = format!("{}{}", s.kind, c);
        }
    }

    // harmony: half-bar slots over the whole vocal, Viterbi on the sung notes
    let slot_beats = if bpm >= 128.0 { 4.0 } else { 2.0 };
    let slots_per_bar = (4.0 / slot_beats) as usize;
    let v0 = intro_bars as f32 * 4.0;
    let rel: Vec<(f32, f32, u8)> = mel.iter().map(|m| (m.0 - v0, m.1, m.2)).collect();
    let n_slots = vocal_bars as usize * slots_per_bar;
    let vchords = vocal::harmonize(&rel, n_slots, slot_beats, key_pc, &scale)?;
    let tonic = vocal::candidate_chords(key_pc, &scale)?.remove(0);
    let hook_bar = secs.iter().find(|s| s.kind == "hook").map(|s| s.bar0).unwrap_or(intro_bars);
    let chords_at = |bar: u32, nbars: u32, kind: &str| -> Vec<Chord> {
        let mut out = Vec::new();
        for i in 0..(nbars as usize * slots_per_bar) {
            let c = match kind {
                // instrumental ends borrow the hook's changes and come home to the tonic
                "intro" | "outro" => {
                    let hs = (hook_bar - intro_bars) as usize * slots_per_bar + i % (4 * slots_per_bar);
                    if kind == "outro" && i + slots_per_bar >= nbars as usize * slots_per_bar {
                        tonic.clone()
                    } else {
                        vchords.get(hs).cloned().unwrap_or_else(|| tonic.clone())
                    }
                }
                _ => vchords.get((bar - intro_bars) as usize * slots_per_bar + i).cloned().unwrap_or_else(|| tonic.clone()),
            };
            out.push(c);
        }
        out
    };

    // fresh project
    let name = s_opt(a, "name").unwrap_or_else(|| format!("{} (vocal song)", path.file_stem().and_then(|s| s.to_str()).unwrap_or("vocal")));
    let mut p = crate::project::Project::new(&name, (bpm * 100.0).round() / 100.0);
    p.key_root = key_root.clone();
    p.scale = if scale == "major" { "major".into() } else { "minor".into() };
    p.swing = 0.0;
    p.patterns = secs.iter().map(|s| Pattern::new(&s.name, s.bars)).collect();
    p.arrangement = secs.iter().map(|s| Section { pattern: s.name.clone(), repeats: 1 }).collect();
    e.project = p;

    // vocal sample + track + clip
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("take").to_string();
    let (sample_path, sample_name) = match &warp {
        Some(w) => {
            let b0 = w.beat_at(0.0);
            let out_len = (w.beat_at(an.duration) - b0) * beat_s;
            let y = vocal::warp_audio(&x, SR, out_len, |o| w.time_at_beat(b0 + o / beat_s));
            std::fs::create_dir_all(e.samples_dir())?;
            let p = e.samples_dir().join(format!("{}_warped.wav", samples::sample_name(&stem)));
            crate::render::write_wav(&p, &y, &y)?;
            (p, format!("vocal_{stem}_warped"))
        }
        None if tune_amount > 0.0 => {
            std::fs::create_dir_all(e.samples_dir())?;
            let p = e.samples_dir().join(format!("{}_tuned.wav", samples::sample_name(&stem)));
            crate::render::write_wav(&p, &x, &x)?;
            (p, format!("vocal_{stem}_tuned"))
        }
        None => (path.clone(), format!("vocal_{stem}")),
    };
    let info = SampleInfo {
        name: samples::sample_name(&sample_name),
        path: sample_path.to_string_lossy().into(),
        source: if warp.is_some() { "vocal take (auto-warped to the grid)".into() } else { "vocal take".into() },
        license: s_opt(a, "license").unwrap_or_default(),
        author: String::new(),
        duration: 0.0,
    };
    let reg = register_sample(e, info)?;
    let sample = reg["sample"].as_str().unwrap_or("vocal").to_string();
    let inst = crate::instruments::Instrument::Sampler(crate::instruments::SamplerParams { sample: sample.clone(), one_shot: true, ..Default::default() });
    ensure_track(&mut e.project, "vocal", "pad", Some(inst))?;
    e.project.audio_clips.push(AudioClip { track: "vocal".into(), sample: sample.clone(), start_beat: clip_start_beat, offset_s: 0.0, length_s: None, gain_db: 0.0 });

    // per-section parts
    let drums: Vec<&str> = g.parts.iter().map(|p| p.0).collect();
    let hook_melody = {
        let h = secs.iter().find(|s| s.kind == "hook").cloned();
        h.map(|h| quantize_notes(&mel, h.bar0, h.bars.min(4))).unwrap_or_default()
    };
    let lead_preset = s_opt(a, "lead_preset").unwrap_or_else(|| prof.lead_preset.to_string());
    for s in &secs {
        let pi = e.project.pattern_index(&s.name)?;
        let total = e.project.patterns[pi].steps();
        call(e, "generate_drums", json!({"style": g.name, "pattern": s.name, "set_tempo": false, "fill": s.kind != "outro", "seed": seed ^ (s.bar0 as u64 * 7919)}))?;
        let keep: &[&str] = match s.kind {
            "intro" => &["hat", "shaker", "rim"],
            "outro" => &["kick", "hat", "shaker"],
            "break" => &["hat", "shaker"],
            "verse" => &["kick", "snare", "clap", "hat", "rim", "shaker", "perc"],
            _ => &[],
        };
        if !keep.is_empty() {
            let pat = &mut e.project.patterns[pi];
            for d in &drums {
                if !keep.contains(d) {
                    pat.clips.remove(*d);
                }
            }
        }
        // chords
        let ch = chords_at(s.bar0, s.bars, s.kind);
        let voiced = theory::voice_chords_open(&ch, 4, 59);
        let cstyle = match s.kind {
            "hook" => prof.chord_style,
            "verse" => "block",
            _ => "hold",
        };
        let chord_track = ensure_track(&mut e.project, "chords", prof.chord_preset, None)?;
        let cn = theory::chord_notes(&voiced, slot_beats / 4.0, total, cstyle, if s.kind == "hook" { 0.72 } else { 0.62 })?;
        *e.project.patterns[pi].notes_mut(&chord_track) = cn;
        // bass (not in the intro)
        if s.kind != "intro" && s.kind != "break" {
            let bass_track = ensure_track(&mut e.project, "bass", prof.bass_preset, None)?;
            let bn = theory::bass_notes(&ch, slot_beats / 4.0, total, prof.bass_octave, prof.bass_style, &mut rng)?;
            *e.project.patterns[pi].notes_mut(&bass_track) = bn;
        }
        // the hook melody answers in the instrumental parts
        if matches!(s.kind, "intro" | "outro" | "break") && !hook_melody.is_empty() {
            let lead = ensure_track(&mut e.project, "lead", &lead_preset, None)?;
            let span = (4 * STEPS_PER_BAR) as f32;
            let mut notes = Vec::new();
            let mut off = 0.0;
            while off < total as f32 {
                for n in &hook_melody {
                    if off + n.start < total as f32 {
                        let mut m = n.clone();
                        m.start += off;
                        m.len = m.len.min(total as f32 - m.start);
                        m.pitch = m.pitch.saturating_add(12);
                        notes.push(m);
                    }
                }
                off += span;
            }
            *e.project.patterns[pi].notes_mut(&lead) = notes;
        }
        // the singer's melody as MIDI (muted reference, e.g. to double or harmonize)
        let vm = ensure_track(&mut e.project, "vocal_midi", "pluck_lead", None)?;
        *e.project.patterns[pi].notes_mut(&vm) = quantize_notes(&mel, s.bar0, s.bars);
    }
    if let Ok(i) = e.project.track_index("vocal_midi") {
        e.project.tracks[i].mute = true;
    }

    // mix: a vocal chain, returns, and a beat that leaves the vocal room
    vocal_chain(e)?;
    call(e, "add_effect", json!({"track": "chords", "type": "reverb", "params": {"size": 0.7, "mix": 0.25}}))?;
    if e.project.track_index("lead").is_ok() {
        call(e, "add_effect", json!({"track": "lead", "type": "delay", "params": {"steps": 3.0, "mix": 0.2, "feedback": 0.3}}))?;
    }
    if e.project.track_index("bass").is_ok() && (prof.sidechain || matches!(g.name, "trap" | "drill" | "phonk")) {
        call(e, "add_effect", json!({"track": "bass", "type": "sidechain", "params": {"source": "kick", "amount": 0.5}}))?;
    }
    let beat_tracks: Vec<String> = e.project.tracks.iter().map(|t| t.name.clone()).filter(|n| n != "vocal" && n != "vocal_midi").collect();
    crate::carve::carve(e, &beat_tracks);
    call(e, "set_mixer", json!({"track": "chords", "volume_db": -10.0}))?;
    if e.project.track_index("lead").is_ok() {
        call(e, "set_mixer", json!({"track": "lead", "volume_db": -9.0, "pan": 0.15}))?;
    }
    // drums in the song's key (kick on the root or fifth)
    let drum_tuning = call(e, "tune_drums_to_key", json!({})).ok().map(|v| v["tuned"].clone());
    let vox_db = f_opt(a, "vocal_db").unwrap_or(0.0);
    call(e, "set_mixer", json!({"track": "vocal", "volume_db": vox_db}))?;

    // the vocal map: what was heard, on the song grid
    let mut vmap = VocalMap {
        track: "vocal".into(),
        sample: sample.clone(),
        key: format!("{} {}", e.project.key_root, e.project.scale),
        warped: warp.is_some(),
        phrases_pinned: warp.as_ref().map(|w| w.anchors.len()).unwrap_or(0),
        ..Default::default()
    };
    vmap.words = words.iter().map(|w| (song_beat(w.start), song_beat(w.end), w.word.clone())).collect();
    vmap.notes = mel.clone();
    for s in &secs {
        vmap.sections.push((s.name.clone(), s.kind.to_string(), s.bar0, s.bars));
        for (i, c) in chords_at(s.bar0, s.bars, s.kind).iter().enumerate() {
            let b = s.bar0 as f32 * 4.0 + i as f32 * slot_beats;
            match vmap.chords.last_mut() {
                Some(last) if last.2 == c.label && (last.0 + last.1 - b).abs() < 1e-3 => last.1 += slot_beats,
                _ => vmap.chords.push((b, slot_beats, c.label.clone())),
            }
        }
    }
    e.project.vocal_map = Some(vmap);

    let sec_json: Vec<Value> = secs
        .iter()
        .map(|s| {
            let ch = chords_at(s.bar0, s.bars, s.kind);
            let mut labels: Vec<String> = Vec::new();
            for c in ch.iter() {
                labels.push(c.label.clone());
            }
            json!({"section": s.name, "kind": s.kind, "bars": s.bars, "start_bar": s.bar0 + 1, "chords": labels.join(" "), "lyrics": s.lyrics})
        })
        .collect();
    let mut out = json!({
        "song": e.project.name,
        "style": g.name,
        "bpm": e.project.bpm,
        "key": format!("{} {}", e.project.key_root, e.project.scale),
        "vocal": analysis_json(&an, 0),
        "vocal_clip": {"track": "vocal", "sample": sample, "start_beat": (clip_start_beat * 1000.0).round() / 1000.0},
        "tune": tune_stats,
        "drums_tuned": drum_tuning,
        "warp": warp.as_ref().map(|w| json!({"phrases_pinned": w.anchors.len(), "bpm": w.bpm, "stretch_range": [(w.min_stretch * 1000.0).round() / 1000.0, (w.max_stretch * 1000.0).round() / 1000.0]})),
        "harmonic_rhythm_beats": slot_beats,
        "sections": sec_json,
        "song_seconds": e.project.song_seconds(),
        "lyrics": lyrics.as_ref().map(|l| l.text.clone()),
        "next": "render or export_audio to hear it; vocal_pocket / ears_report to check the vocal sits; tweak with set_mixer, generate_drums {pattern}, or rerun with style/key/bpm overrides"
    });
    if let Some(n) = lyr_note {
        out["notes"] = json!([n]);
    }
    Ok(out)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "transcribe_lyrics",
            description: "Transcribe the lyrics of a sung vocal with word timestamps (local Whisper via faster-whisper; model tiny|base|small, default base). known_lyrics primes the recognizer. Cached next to the file.",
            mutates: false,
            schema: || obj(json!({"path": {"type": "string"}, "model": {"type": "string"}, "language": {"type": "string"}, "known_lyrics": {"type": "string"}}), &["path"]),
            run: |e, a| {
                let path = e.resolve(&s_req(a, "path")?);
                let l = vocal::transcribe(&path, &e.workdir, &s_opt(a, "model").unwrap_or_else(|| "base".into()), s_opt(a, "language").as_deref(), s_opt(a, "known_lyrics").as_deref())?;
                Ok(json!({"language": l.language, "text": l.text, "lines": l.segments.iter().map(|s| json!({"start": s.start, "end": s.end, "text": s.text})).collect::<Vec<_>>(), "words": l.words().len()}))
            },
        },
        Tool {
            name: "analyze_vocal",
            description: "Listen to a sung vocal: pitch-tracked notes, key, tempo + first downbeat, timing tightness, range, tuning, phrases (and lyrics when lyrics=true). The first step of building a song around a singer.",
            mutates: false,
            schema: || obj(json!({"path": {"type": "string"}, "bpm": {"type": "number", "description": "tempo hint if you know it"}, "lyrics": {"type": "boolean", "description": "also transcribe lyrics (default false here)"}, "model": {"type": "string"}, "notes": {"type": "integer", "description": "how many notes to list (default 48)"}}), &["path"]),
            run: |e, a| {
                let path = e.resolve(&s_req(a, "path")?);
                let x = samples::decode_file(&path)?;
                let mut aa = a.clone();
                if aa.get("lyrics").is_none() {
                    aa["lyrics"] = json!(false);
                }
                let (lyrics, note) = lyrics_for(e, &path, &aa);
                let words = lyrics.as_ref().map(|l| l.words()).unwrap_or_default();
                let an = vocal::analyze(&x, SR, &words, f_opt(a, "bpm"));
                let mut v = analysis_json(&an, u_or(a, "notes", 48) as usize);
                if let Some(l) = lyrics {
                    v["lyrics"] = json!(l.segments.iter().map(|s| json!({"start": s.start, "end": s.end, "text": s.text})).collect::<Vec<_>>());
                }
                if let Some(n) = note {
                    v["notes_lyrics"] = json!(n);
                }
                Ok(v)
            },
        },
        Tool {
            name: "vocal_to_song",
            description: "Build a whole song around a sung vocal: transcribes the lyrics, hears the melody, key and tempo, places the take on the bar grid, harmonizes the sung melody (Viterbi over the key's chords), writes drums/bass/chords per section (intro, verses, hooks found from repeated lyrics, breaks, outro), answers the hook melody on a lead in the instrumental parts, and mixes the vocal (HPF, EQ, comp, de-esser, reverb + delay returns, presence carved out of the beat). style picks the groove (trap, house, lofi, boom_bap, afrobeats, ...); key/scale/bpm override what was heard.",
            mutates: true,
            schema: || obj(json!({
                "path": {"type": "string"},
                "style": {"type": "string"},
                "key": {"type": "string"},
                "scale": {"type": "string"},
                "bpm": {"type": "number"},
                "intro_bars": {"type": "integer"},
                "outro_bars": {"type": "integer"},
                "section_bars": {"type": "integer"},
                "lyrics": {"type": "boolean", "description": "transcribe lyrics to find the hook (default true)"},
                "tune": {"type": "number", "description": "auto-tune the take to the key first: 0 = off (default), 1 = full correction"},
                "tune_hard": {"type": "number", "description": "0..1 how much of the singer's wobble/vibrato to flatten (default 0.1)"},
                "warp": {"type": "boolean", "description": "pin each sung phrase to a bar line and time-stretch between them (default true; false keeps the singer's own timing)"},
                "known_lyrics": {"type": "string"},
                "model": {"type": "string"},
                "language": {"type": "string"},
                "lead_preset": {"type": "string"},
                "vocal_db": {"type": "number"},
                "license": {"type": "string"},
                "name": {"type": "string"},
                "seed": {"type": "integer"}
            }), &["path"]),
            run: vocal_to_song,
        },
        Tool {
            name: "tune_vocal",
            description: "Auto-tune a sung take to the key (TD-PSOLA pitch correction, formants kept). Every sung note moves to the nearest scale note; amount 0..1 (default 1) scales the move, hard 0..1 (default 0.15) also flattens wobble/vibrato toward the note (1 = the hard T-Pain effect), speed_ms (default 35) is how fast it follows. Give a sample name (its audio clips switch to the tuned take, undoable) or a path (writes <name>_tuned.wav). key/scale default to the project's.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "path": {"type": "string"}, "key": {"type": "string"}, "scale": {"type": "string"}, "amount": {"type": "number"}, "hard": {"type": "number"}, "speed_ms": {"type": "number"}}), &[]),
            run: |e, a| {
                let key = s_opt(a, "key").unwrap_or_else(|| e.project.key_root.clone());
                let scale = s_opt(a, "scale").unwrap_or_else(|| e.project.scale.clone());
                let key_pc = theory::pitch_class(&key)?;
                let ivs = theory::scale_intervals(&scale)?;
                let (x, src_name, src_path) = match (s_opt(a, "sample"), s_opt(a, "path")) {
                    (Some(n), _) => {
                        let info = e.project.samples.iter().find(|s| s.name == n).cloned().ok_or_else(|| anyhow::anyhow!("no sample '{n}'"))?;
                        let d = e.bank.get(&n).map(|d| d.to_vec()).map(Ok).unwrap_or_else(|| samples::decode_file(std::path::Path::new(&info.path)))?;
                        (d, Some(n), std::path::PathBuf::from(info.path))
                    }
                    (None, Some(p)) => {
                        let p = e.resolve(&p);
                        (samples::decode_file(&p)?, None, p)
                    }
                    _ => bail!("give sample (a project sample name) or path"),
                };
                let (y, st) = vocal::autotune(&x, SR, key_pc, ivs, f_opt(a, "amount").unwrap_or(1.0), f_opt(a, "hard").unwrap_or(0.15), f_opt(a, "speed_ms").unwrap_or(35.0));
                let stem = src_path.file_stem().and_then(|s| s.to_str()).unwrap_or("vocal").to_string();
                std::fs::create_dir_all(e.samples_dir())?;
                let out = e.samples_dir().join(format!("{}_tuned.wav", samples::sample_name(&stem)));
                crate::render::write_wav(&out, &y, &y)?;
                let mut res = json!({"path": out.to_string_lossy(), "key": format!("{key} {scale}"), "stats": st});
                if let Some(n) = src_name {
                    let info = SampleInfo { name: format!("{n}_tuned"), path: out.to_string_lossy().into(), source: format!("{n}, auto-tuned to {key} {scale}"), license: String::new(), author: String::new(), duration: 0.0 };
                    let reg = register_sample(e, info)?;
                    let tuned = reg["sample"].as_str().unwrap_or_default().to_string();
                    let mut switched = 0;
                    for c in e.project.audio_clips.iter_mut().filter(|c| c.sample == n) {
                        c.sample = tuned.clone();
                        switched += 1;
                    }
                    if let Some(m) = e.project.vocal_map.as_mut() {
                        if m.sample == n {
                            m.sample = tuned.clone();
                        }
                    }
                    res["sample"] = json!(tuned);
                    res["clips_switched"] = json!(switched);
                }
                Ok(res)
            },
        },
        Tool {
            name: "get_vocal_map",
            description: "What vocal_to_song heard, on the song grid: sections with their lyrics and chords by bar, so you can edit around the singer (e.g. drop the drums under a line, add a riser before the hook). bars=[from,to] narrows it (1-based).",
            mutates: false,
            schema: || obj(json!({"bars": {"type": "array", "items": {"type": "integer"}}}), &[]),
            run: |e, a| {
                let Some(m) = &e.project.vocal_map else {
                    bail!("no vocal map: run vocal_to_song first");
                };
                let (b0, b1) = match a.get("bars").and_then(|v| v.as_array()) {
                    Some(v) if v.len() == 2 => (v[0].as_u64().unwrap_or(1).max(1) as u32 - 1, v[1].as_u64().unwrap_or(9999) as u32),
                    _ => (0, u32::MAX),
                };
                let mut bars = Vec::new();
                for (name, kind, s0, n) in &m.sections {
                    for b in *s0..s0 + n {
                        if b < b0 || b >= b1 {
                            continue;
                        }
                        let (lo, hi) = (b as f32 * 4.0, (b + 1) as f32 * 4.0);
                        let words: Vec<&str> = m.words.iter().filter(|w| w.0 >= lo && w.0 < hi).map(|w| w.2.as_str()).collect();
                        let chords: Vec<&str> = m.chords.iter().filter(|c| c.0 < hi && c.0 + c.1 > lo).map(|c| c.2.as_str()).collect();
                        bars.push(json!({"bar": b + 1, "section": name, "kind": kind, "chords": chords.join(" "), "lyrics": words.join(" ")}));
                    }
                }
                Ok(json!({"key": m.key, "warped": m.warped, "phrases_pinned": m.phrases_pinned, "sample": m.sample, "track": m.track, "bars": bars}))
            },
        },
        Tool {
            name: "add_audio_clip",
            description: "Place a sample on the song timeline at a beat (a vocal take, a long loop, a bounce), played through a track's channel (its FX, fader, sends). offset_s trims the start, length_s the end.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "sample": {"type": "string"}, "start_beat": {"type": "number"}, "offset_s": {"type": "number"}, "length_s": {"type": "number"}, "gain_db": {"type": "number"}}), &["track", "sample", "start_beat"]),
            run: |e, a| {
                let sample = s_req(a, "sample")?;
                if !e.project.samples.iter().any(|s| s.name == sample) {
                    bail!("no sample '{sample}' (import_sample first; list_samples shows them)");
                }
                let ti = e.project.track_index(&s_req(a, "track")?)?;
                let track = e.project.tracks[ti].name.clone();
                e.project.audio_clips.push(AudioClip {
                    track: track.clone(),
                    sample,
                    start_beat: f_opt(a, "start_beat").unwrap_or(0.0),
                    offset_s: f_opt(a, "offset_s").unwrap_or(0.0),
                    length_s: f_opt(a, "length_s"),
                    gain_db: f_opt(a, "gain_db").unwrap_or(0.0),
                });
                Ok(json!({"index": e.project.audio_clips.len() - 1, "track": track, "clips": e.project.audio_clips.len()}))
            },
        },
        Tool {
            name: "list_audio_clips",
            description: "List the audio clips on the timeline (track, sample, start beat, trims, gain).",
            mutates: false,
            schema: || obj(json!({}), &[]),
            run: |e, _| Ok(json!({"clips": e.project.audio_clips})),
        },
        Tool {
            name: "remove_audio_clip",
            description: "Remove an audio clip by index (see list_audio_clips).",
            mutates: true,
            schema: || obj(json!({"index": {"type": "integer"}}), &["index"]),
            run: |e, a| {
                let i = u_or(a, "index", 0) as usize;
                if i >= e.project.audio_clips.len() {
                    bail!("no clip {i}; there are {}", e.project.audio_clips.len());
                }
                let c = e.project.audio_clips.remove(i);
                Ok(json!({"removed": c, "clips": e.project.audio_clips.len()}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_clip_plays_through_its_track() {
        let dir = std::env::temp_dir().join("beatbox_audio_clip_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("new_project", &json!({"name": "clip", "bpm": 120})).unwrap();
        // one second of a 220 Hz tone as a sample
        let tone: Vec<f32> = (0..SR as usize).map(|i| 0.5 * (i as f32 * 220.0 / SR * std::f32::consts::TAU).sin()).collect();
        let wav = dir.join("tone.wav");
        crate::render::write_wav(&wav, &tone, &tone).unwrap();
        e.call("import_sample", &json!({"path": wav.to_string_lossy(), "name": "tone"})).unwrap();
        e.call("add_track", &json!({"name": "vox", "preset": "pad"})).unwrap();
        e.call("add_audio_clip", &json!({"track": "vox", "sample": "tone", "start_beat": 2.0})).unwrap();
        let mix = e.mix().unwrap();
        let at = |s: f32| mix.left[(s * SR) as usize].abs().max(mix.left[(s * SR) as usize + 30].abs());
        // beat 2 at 120 BPM = 1.0 s
        assert!(at(0.5) < 1e-4, "silent before the clip");
        assert!(at(1.3) > 0.05, "clip sounds at 1.3 s");
        assert!(at(2.3) < 1e-3, "and ends after a second");
    }
}
