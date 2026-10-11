//! make_beat: the forgiving one-call beat tool. A free-text prompt and/or
//! lyrics become a producer plan that produce_track builds, listens to and
//! revises. analyze_lyrics shows how the lyrics were read.

use crate::engine::Engine;
use crate::prompt_beat::{analyze_lyrics, parse_prompt};
use crate::tools::{find, obj, s_opt, u_or, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

fn make_beat(e: &mut Engine, a: &Value) -> Result<Value> {
    make_beat_with(e, a, None)
}

/// make_beat with the performance's word clips supplied (produce_song: words
/// cut from a recording) instead of a TTS guide voice.
pub fn make_beat_with(e: &mut Engine, a: &Value, clips: Option<Vec<Vec<crate::speech_song::Clip>>>) -> Result<Value> {
    let prompt = s_opt(a, "prompt").unwrap_or_default();
    let lyrics = s_opt(a, "lyrics").unwrap_or_default();
    if prompt.trim().is_empty() && lyrics.trim().is_empty() {
        bail!("say what you want: prompt (e.g. 'Bohemia type beat with a beat switch') and/or lyrics");
    }
    let p = parse_prompt(&prompt);
    let mut lr = (!lyrics.trim().is_empty()).then(|| analyze_lyrics(&lyrics));
    let mut reasons = p.reasons.clone();
    // song form (critic, beat 8: "the only hook is at 0:11"): a song whose
    // words end on a verse comes back to the hook once more at the end
    if let Some(l) = lr.as_mut() {
        let hooks: Vec<usize> = (0..l.sections.len()).filter(|&i| l.sections[i].kind == "hook").collect();
        if clips.is_none() && hooks.len() == 1 && l.sections.len() >= 2 && l.sections.last().map(|s| s.kind != "hook").unwrap_or(false) {
            let h = l.sections[hooks[0]].clone();
            l.total_bars += h.bars;
            l.sections.push(h);
            reasons.push("song form: the hook comes back after the last verse".into());
        }
    }
    let mut args = Map::new();
    let brief = if prompt.is_empty() { format!("a beat for these {} lyrics", lr.as_ref().map(|l| l.delivery.as_str()).unwrap_or("")) } else { prompt.clone() };
    args.insert("brief".into(), json!(brief));
    // the prompt wins over what the lyrics suggest; lyrics fill the gaps
    let genre = p.genre.clone().or_else(|| lr.as_ref().map(|l| l.genre.clone()));
    if let Some(g) = &genre {
        args.insert("genre".into(), json!(g));
    }
    let bpm = p.bpm.or_else(|| lr.as_ref().map(|l| l.bpm));
    if let Some(b) = bpm {
        args.insert("bpm".into(), json!(b));
    }
    if let Some(k) = &p.key {
        args.insert("key".into(), json!(k));
    }
    if let Some(s) = &p.scale {
        args.insert("scale".into(), json!(s));
    }
    let mood = p.mood.clone().or_else(|| lr.as_ref().map(|l| l.mood.clone()));
    let mut intent = Map::new();
    if let Some(m) = &mood {
        args.insert("mood".into(), json!(m));
        intent.insert("mood".into(), json!(m));
    }
    let mut contrasts = p.contrasts.clone();
    let mut duration = p.duration_s;
    if let Some(l) = &lr {
        reasons.extend(l.reasons.iter().cloned());
        // rap leaves space for the flow; sung lyrics get fuller harmony
        if l.delivery == "rap" {
            intent.insert("density".into(), json!(p.density.clone().unwrap_or_else(|| "sparse".into())));
            intent.insert("hero".into(), json!("groove"));
        } else {
            intent.insert("density".into(), json!(p.density.clone().unwrap_or_else(|| "balanced".into())));
            intent.insert("hero".into(), json!("motif"));
        }
        if duration.is_none() {
            let bar_s = 240.0 / bpm.unwrap_or(l.bpm);
            duration = Some((l.total_bars as f32 * bar_s).clamp(30.0, 300.0));
        }
        if l.sections.iter().filter(|s| s.kind == "hook").count() >= 1 && !contrasts.iter().any(|c| c == "sparse_to_dense") {
            contrasts.push("sparse_to_dense".into());
        }
        // the arrangement follows the words: intro, one section per stanza, outro
        if l.sections.len() >= 2 && p.duration_s.is_none() {
            let mut form = vec![json!({"kind": "intro", "bars": 4})];
            for s in &l.sections {
                form.push(json!({"kind": s.kind, "bars": s.bars}));
            }
            // a slow song's outro is ~6 s, not 11 (2 bars under 100 BPM)
            let outro_bars = if bpm.unwrap_or(l.bpm) < 100.0 { 2 } else { 4 };
            form.push(json!({"kind": "outro", "bars": outro_bars}));
            reasons.push(format!(
                "arrangement from the lyrics: intro 4 > {} > outro {outro_bars}",
                l.sections.iter().map(|s| format!("{} {}", s.kind, s.bars)).collect::<Vec<_>>().join(" > ")
            ));
            intent.insert("form".into(), json!(form));
        }
    } else if let Some(d) = &p.density {
        intent.insert("density".into(), json!(d));
    }
    if let Some(en) = p.energy {
        intent.insert("energy".into(), json!(en));
    }
    // the quiet section goes in before the switch is placed (the switch comes out of it)
    contrasts.sort_by_key(|c| match c.as_str() {
        "quiet_section" => 0,
        "beat_switch" => 1,
        _ => 2,
    });
    intent.insert("target_lufs".into(), json!(a["target_lufs"].as_f64().unwrap_or(-14.0)));
    if !contrasts.is_empty() {
        intent.insert("contrasts".into(), json!(contrasts));
    }
    if !p.feel.is_empty() {
        let allowed = ["half_time", "backbeat", "triplet", "swing", "straight", "bounce", "driving", "rolling", "sparse_hats"];
        let f: Vec<&String> = p.feel.iter().filter(|x| allowed.contains(&x.as_str())).collect();
        if !f.is_empty() {
            intent.insert("rhythmic_feel".into(), json!(f));
        }
    }
    if !p.palette.is_empty() {
        intent.insert("palette".into(), Value::Object(p.palette.clone()));
    }
    args.insert("intent".into(), Value::Object(intent));
    if let Some(d) = duration {
        args.insert("duration_s".into(), json!(d.round()));
    }
    for k in ["seed", "out_dir", "max_iterations", "reference_path"] {
        if let Some(v) = a.get(k) {
            args.insert(k.into(), v.clone());
        }
    }
    if !args.contains_key("max_iterations") {
        args.insert("max_iterations".into(), json!(u_or(a, "quality", 2).clamp(1, 6)));
    }
    let run = find("produce_track").expect("produce_track").run;
    let res = run(e, &Value::Object(args.clone()))?;
    // lyrics get performed: sung (melodic) or rapped (rap) by a guide voice
    let mut vocal = Value::Null;
    let mut files = res.get("files").cloned().unwrap_or(Value::Null);
    let vmode = s_opt(a, "vocal").unwrap_or_else(|| "auto".into());
    if let Some(l) = &lr {
        if vmode != "none" && !l.sections.is_empty() {
            let mode = match vmode.as_str() {
                "rap" | "sing" => vmode.clone(),
                _ => if l.delivery == "rap" { "rap".to_string() } else { "sing".to_string() },
            };
            let plan = crate::producer::plan_from_value(&res["plan"])?;
            let t = std::time::Instant::now();
            match sing_over_plan(e, &plan, &l.sections, &mode, plan.seed, clips.clone()) {
                Ok(mut v) => {
                    // re-export the full song with the vocal in it
                    // the song (with the voice) is the main out file; the
                    // beat alone moves to <name>_inst.mp3 (critic v18: the
                    // main file was the instrumental and got sent as the song)
                    if let Some(audio) = res["files"]["audio"].as_str().map(String::from) {
                        let p = std::path::Path::new(&audio);
                        let inst = p.with_file_name(format!("{}_inst.{}", p.file_stem().and_then(|x| x.to_str()).unwrap_or("song"), p.extension().and_then(|x| x.to_str()).unwrap_or("mp3")));
                        if std::fs::rename(p, &inst).is_ok() {
                            files["instrumental"] = json!(inst.to_string_lossy());
                        }
                        let lufs = a["target_lufs"].as_f64().unwrap_or(-14.0);
                        let ex = (find("export_audio").expect("export").run)(e, &json!({"path": audio, "format": "mp3", "target_lufs": lufs, "true_peak_ceiling": -1.2}))?;
                        files["audio"] = json!(audio);
                        files["with_vocal"] = json!(true);
                        v["file"] = json!(audio);
                        v["export"] = ex.get("after").cloned().unwrap_or(Value::Null);
                    }
                    v["total_seconds"] = json!((t.elapsed().as_secs_f32() * 10.0).round() / 10.0);
                    vocal = v;
                }
                Err(err) => vocal = json!({"skipped": format!("{err:#}")}),
            }
        }
    }
    // what was asked but not delivered is said, not hidden (song worker: C minor became E)
    let mut not_honoured: Vec<String> = Vec::new();
    if let Some(k) = &p.key {
        let same = match (crate::theory::pitch_class(k), crate::theory::pitch_class(&e.project.key_root)) {
            (Ok(x), Ok(y)) => x == y,
            _ => true,
        };
        if !same {
            not_honoured.push(format!("key {k} was asked; the beat is in {} {}", e.project.key_root, e.project.scale));
        }
    }
    if let Some(d) = duration {
        let got = e.project.song_seconds();
        if (got - d).abs() > d * 0.15 + 8.0 {
            not_honoured.push(format!("length {d:.0} s was asked; the song is {got:.0} s"));
        }
    }
    // compact answer for small models; the full producer result rides under "details"
    let summary = json!({
        "not_honoured": not_honoured,
        "genre": genre.clone().unwrap_or_else(|| e.project.name.clone()),
        "bpm": e.project.bpm,
        "key": format!("{} {}", e.project.key_root, e.project.scale),
        "seconds": (e.project.song_seconds() * 10.0).round() / 10.0,
        "sections": e.project.song_sections().iter().map(|s| s.pattern.clone()).collect::<Vec<_>>(),
    });
    Ok(json!({
        "summary": summary,
        "how_i_read_it": reasons,
        "lyrics": lr,
        "vocal": vocal,
        "plan_args": args,
        "files": files,
        "score": res.get("score").cloned().or_else(|| res.get("best").cloned()).unwrap_or(Value::Null),
        "next": ["export_audio {path:'song.mp3'} to save it", "make_beat again with a different seed for another take", "set_mixer / generate_drums {pattern} to change a part"],
        "details": res,
    }))
}


/// Sing (or rap) the lyrics over the beat the producer just built: a local TTS
/// guide voice speaks each word, the words are placed on the grid section by
/// section, and in sing mode every syllable is tuned onto a melody written
/// over the plan's chords. The vocal lands on a "vocal" track with a chain.
/// Energy above 5 kHz relative to the whole voice (dB): below about -40 the
/// source is band-limited (an old or phone recording).
fn air_ratio_db(y: &[f32]) -> f32 {
    let ps = crate::analysis::power_spectrum(y);
    let hz = crate::dsp::SR / (2.0 * ps.len() as f32);
    let tot: f32 = ps.iter().skip((80.0 / hz) as usize).sum();
    let air: f32 = ps.iter().skip((5000.0 / hz) as usize).take(((11000.0) / hz) as usize).sum();
    10.0 * (air.max(1e-12) / tot.max(1e-12)).log10()
}

pub fn sing_over_plan(e: &mut Engine, plan: &crate::producer::Plan, sections: &[crate::prompt_beat::LyricSection], mode: &str, seed: u64, given: Option<Vec<Vec<crate::speech_song::Clip>>>) -> Result<Value> {
    use crate::speech_song as ss;
    use crate::theory;
    let t0 = std::time::Instant::now();
    let bpm = plan.bpm;
    // lyric sections -> plan sections (the form was intro + stanzas + outro;
    // otherwise match verse/hook kinds in order)
    let mut map: Vec<usize> = Vec::new();
    let body: Vec<usize> = (0..plan.sections.len()).filter(|&i| !matches!(plan.sections[i].kind.as_str(), "intro" | "outro")).collect();
    if body.len() >= sections.len() {
        let mut used = vec![false; plan.sections.len()];
        for ls in sections {
            let pick = body.iter().copied().find(|&i| !used[i] && plan.sections[i].kind == ls.kind).or_else(|| body.iter().copied().find(|&i| !used[i]));
            if let Some(i) = pick {
                used[i] = true;
                map.push(i);
            }
        }
    }
    if map.len() < sections.len() {
        bail!("the beat has fewer sections ({}) than the lyrics ({})", body.len(), sections.len());
    }
    let mut starts = Vec::new();
    let mut bars_v = Vec::new();
    let mut b = 0u32;
    for s in &plan.sections {
        starts.push(b);
        bars_v.push(s.bars);
        b += s.bars;
    }
    let lines_text: Vec<Vec<String>> = sections
        .iter()
        .flat_map(|s| s.text.iter())
        .map(|l| l.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'').to_string()).filter(|w| !w.is_empty()).collect())
        .collect();
    let from_tts = given.is_none();
    let clips = match given {
        Some(c) => c,
        None => ss::tts_words(&e.workdir, &lines_text, &format!("{seed}"))?,
    };
    let t_tts = t0.elapsed().as_secs_f32();
    let mut lines = Vec::new();
    let mut li = 0;
    for (si, s) in sections.iter().enumerate() {
        for _ in &s.text {
            if let Some(c) = clips.get(li) {
                if !c.is_empty() {
                    lines.push(ss::Line { clips: c.clone(), section: map[si] });
                }
            }
            li += 1;
        }
    }
    let mut perf = ss::plan(&lines, &starts, &bars_v, bpm, mode);
    let key_pc = theory::pitch_class(&plan.key)?;
    let href = crate::producer::harmony_ref(&plan.scale);
    let iv: Vec<u8> = theory::scale_intervals(&plan.scale)?.to_vec();
    let prog_for = |s: &crate::producer::PlanSection| -> String {
        let sw = s.tags.iter().any(|t| t == "switch") && !plan.progression_switch.is_empty() && matches!(s.kind.as_str(), "verse" | "hook");
        if sw {
            plan.progression_switch.clone()
        } else if s.kind == "hook" {
            plan.progression_hook.clone()
        } else if matches!(s.kind.as_str(), "bridge" | "breakdown") && !plan.progression_bridge.is_empty() {
            plan.progression_bridge.clone()
        } else {
            plan.progression_verse.clone()
        }
    };
    let mut sec_chords: Vec<Vec<theory::Chord>> = Vec::new();
    for s in &plan.sections {
        sec_chords.push(theory::parse_progression(&prog_for(s), key_pc, href).unwrap_or_default());
    }
    let bpc = plan.bars_per_chord.max(0.25);
    let chord_at = |beat: f32| -> Option<theory::Chord> {
        let bar = beat / 4.0;
        let si = (0..starts.len()).rev().find(|&i| starts[i] as f32 <= bar + 1e-3)?;
        let ch = &sec_chords[si];
        if ch.is_empty() {
            return None;
        }
        let k = ((bar - starts[si] as f32) / bpc).floor() as usize % ch.len();
        Some(ch[k].clone())
    };
    let all: Vec<f32> = lines.iter().flat_map(|l| l.clips.iter()).flat_map(|c| c.audio.iter().copied()).collect();
    let center = ss::median_pitch(&all);
    let hook_beats: Vec<(f32, f32)> = plan.sections.iter().enumerate().filter(|(_, s)| s.kind == "hook").map(|(i, s)| (starts[i] as f32 * 4.0, s.bars as f32 * 4.0)).collect();
    if mode == "sing" {
        ss::write_melody(&mut perf, &chord_at, key_pc, &iv, center, &hook_beats);
    }
    let refs: Vec<&ss::Clip> = lines.iter().flat_map(|l| l.clips.iter()).collect();
    let mut y = ss::render(&perf, &refs, bpm, 0.95);
    // hooks are sung out: +3 dB over the verses (40 ms ramps)
    {
        let beat_s = 60.0 / bpm;
        let ramp = 0.04 * crate::dsp::SR;
        let g = 10f32.powf(3.0 / 20.0) - 1.0;
        for &(b0, len) in &hook_beats {
            let (s0, s1) = (b0 * beat_s * crate::dsp::SR, (b0 + len) * beat_s * crate::dsp::SR);
            let (i0, i1) = ((s0 as usize).min(y.len()), (s1 as usize).min(y.len()));
            for i in i0..i1 {
                let x = i as f32;
                let w = ((x - s0) / ramp).min((s1 - x) / ramp).clamp(0.0, 1.0);
                y[i] *= 1.0 + g * w;
            }
        }
    }
    // gain staging (critic round 2: the stem clipped at +0.1 dBTP because the
    // hooks were lifted after levelling): repair splice clicks, then peak the
    // stem at -6 dBFS in float and give the level back on the fader
    let clicks_fixed = ss::declick(&mut y);
    let peak = y.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    let stem_gain_db = 20.0 * (0.5 / peak).log10();
    for v in y.iter_mut() {
        *v *= 0.5 / peak;
    }
    let t_render = t0.elapsed().as_secs_f32() - t_tts;
    std::fs::create_dir_all(e.samples_dir())?;
    let p = e.samples_dir().join(format!("vocal_{mode}_{seed}.wav"));
    crate::render::write_wav(&p, &y, &y)?;
    let info = crate::samples::SampleInfo {
        name: crate::samples::sample_name(&format!("vocal_{mode}_{seed}")),
        path: p.to_string_lossy().into(),
        source: format!("guide vocal: local TTS voice, {mode} performance by beatbox"),
        license: "generated".into(),
        author: String::new(),
        duration: 0.0,
    };
    let reg = crate::tools::register_sample(e, info)?;
    let sample = reg["sample"].as_str().unwrap_or("vocal").to_string();
    let inst = crate::instruments::Instrument::Sampler(crate::instruments::SamplerParams { sample: sample.clone(), one_shot: true, ..Default::default() });
    crate::tools::ensure_track(&mut e.project, "vocal", "pad", Some(inst))?;
    e.project.audio_clips.retain(|c| c.track != "vocal");
    e.project.audio_clips.push(crate::project::AudioClip { track: "vocal".into(), sample, start_beat: 0.0, offset_s: 0.0, length_s: None, gain_db: 0.0 });
    crate::tools_vocal::vocal_chain(e)?;
    // the voice sits on top (measured: at 0 dB the words were masked; +5 dB
    // made them intelligible to a speech recogniser), the melodic parts step back
    if let Ok(i) = e.project.track_index("vocal") {
        // critic (JFK rap/sing, beat 8 sung): at +5 dB the vocal still sat up to
        // 3 dB under the beat; vocal-first means ~3 dB over it
        // (sing measured +5.6 dB over the beat in round 2, rap +3.1: aim ~+3)
        let base = if mode == "sing" { 7.5 } else { 10.0 };
        e.project.tracks[i].volume_db = base - stem_gain_db;
    }
    for (t, db) in [("lead", -6.0), ("counter", -4.0), ("texture", -3.0), ("perc", -4.0), ("hat", -2.0), ("open_hat", -2.0)] {
        if let Ok(i) = e.project.track_index(t) {
            e.project.tracks[i].volume_db += db;
        }
    }
    // the melodic bed ducks under the words (keyed from the vocal) and leaves
    // the consonant range (2-4 kHz) to the voice
    let crate_call = |e: &mut Engine, name: &str, args: Value| (find(name).expect("tool").run)(e, &args);
    for t in ["chords", "lead", "counter", "texture"] {
        if e.project.track_index(t).is_ok() {
            let _ = crate_call(e, "add_effect", json!({"track": t, "type": "sidechain", "params": {"source": "vocal", "amount": 0.5, "attack_ms": 10.0, "release_ms": 450.0}}));
            // the voice's body (250-800 Hz) and its consonants (1-4 kHz) belong to the voice;
            // the static ~300 Hz pad line sat right on it (critic: low-mid +7.5 dB)
            // keyed, not static (critic, JFK r3 and beat 8 r3): the bed keeps its
            // body between lines and steps out of 300-900 Hz (-6 dB) under the words
            let _ = crate_call(e, "add_effect", json!({"track": t, "type": "dynamic_eq", "params": {
                "source": "vocal", "freq": 520.0, "q": 0.85, "range_db": -6.0, "attack_ms": 15.0, "release_ms": 400.0}}));
            let _ = crate_call(e, "add_effect", json!({"track": t, "type": "parametric_eq", "params": {"bands": [
                {"kind": "bell", "freq": 300.0, "gain_db": -2.0, "q": 1.2},
                {"kind": "bell", "freq": 2500.0, "gain_db": -4.0, "q": 0.6}
            ]}}));
        }
    }
    // the consonant band (2-8 kHz) belongs to the words: hats and perc duck
    // about 6 dB under every vocal onset and lose their top above 9 kHz
    for t in ["hat", "open_hat", "perc", "shaker"] {
        if e.project.track_index(t).is_ok() {
            let _ = crate_call(e, "add_effect", json!({"track": t, "type": "sidechain", "params": {"source": "vocal", "amount": 0.5, "attack_ms": 5.0, "release_ms": 300.0}}));
            let _ = crate_call(e, "add_effect", json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "high_cut", "freq": 9000.0, "gain_db": 0.0, "q": 0.7}]}}));
        }
    }
    // the voice itself: presence 2.5 kHz, harmonics restored on a band-limited
    // source (an old or phone recording with nothing above ~4 kHz), and a
    // limiter so the stem never goes past -1 dBTP
    let air = air_ratio_db(&y);
    if e.project.track_index("vocal").is_ok() {
        if air < -40.0 {
            // no exciter (critic round 3: the vocal chain cost ~18 points of
            // word recognition, the exciter suspected first): a +3 dB shelf
            let _ = crate_call(e, "add_effect", json!({"track": "vocal", "type": "parametric_eq", "params": {"bands": [{"kind": "high_shelf", "freq": 3000.0, "gain_db": 3.0, "q": 0.7}]}}));
            // the beat steps out of the band this voice lives in (250-1150 Hz
            // on the 1962 tape): the beds' static chord partials covered it
            for t in ["chords", "counter", "texture", "lead"] {
                if e.project.track_index(t).is_ok() {
                    let _ = crate_call(e, "add_effect", json!({"track": t, "type": "parametric_eq", "params": {"bands": [{"kind": "bell", "freq": 600.0, "gain_db": -6.0, "q": 0.6}]}}));
                }
            }
            // an old tape's voice is all body (rolloff ~1.2 kHz): thin the
            // 350 Hz boxiness so it stops filling the beat's low-mids
            let _ = crate_call(e, "add_effect", json!({"track": "vocal", "type": "parametric_eq", "params": {"bands": [{"kind": "bell", "freq": 350.0, "gain_db": -3.0, "q": 0.9}]}}));
        }
        // critic v18 (beat 10 r1): the voice's energy sat at 150-250 Hz and
        // led the beat by only +3.8 dB at 2-5 kHz: 200-300 Hz -3 dB and a
        // broad +3 dB presence lift over 2.5-4 kHz, on the vocal only
        let _ = crate_call(e, "add_effect", json!({"track": "vocal", "type": "parametric_eq", "params": {"bands": [
            {"kind": "bell", "freq": 250.0, "gain_db": -3.0, "q": 1.0},
            {"kind": "bell", "freq": 3200.0, "gain_db": 3.0, "q": 0.7}]}}));
        let _ = crate_call(e, "add_effect", json!({"track": "vocal", "type": "limiter", "params": {"ceiling_db": -1.5, "release_ms": 150.0}}));
        // the lo-fi tape (crusher + dusty top cut) stays on the beat
        let _ = keep_vocal_off_tape(e);
        // with the voice on, the hooks lifted only ~1.2 LU over the verses
        // (critic v18/v19, target 2.5-3): the drums (+3 dB) and the voice
        // (+1.5 dB) rise in every hook, and a quiet double joins the voice there
        let hooks: Vec<String> = plan.sections.iter().filter(|s| s.kind == "hook").map(|s| s.name.clone()).collect();
        if let Ok(bi) = e.project.bus_index("drums") {
            let base = e.project.buses[bi].volume_db;
            for h in &hooks {
                let _ = crate_call(e, "set_section_mix", json!({"section": h, "track": "drums", "volume_db": base + HOOK_DRUM_LIFT_DB, "all_occurrences": true}));
            }
        }
        let _ = hook_double(e, &hooks);
        if let Ok(i) = e.project.track_index("vocal") {
            let base = e.project.tracks[i].volume_db;
            for h in &hooks {
                let _ = crate_call(e, "set_section_mix", json!({"section": h, "track": "vocal", "volume_db": base + HOOK_VOCAL_LIFT_DB, "all_occurrences": true}));
            }
        }
    }
    // the project changed outside the tool layer: drop cached renders
    e.revision += 1;
    let notes: usize = perf.words.iter().map(|w| w.notes.len()).sum();
    Ok(json!({
        "mode": mode,
        "words": perf.words.len(),
        "lines": perf.lines,
        "sung_notes": notes,
        "source_air_db": (air * 10.0).round() / 10.0,
        "vocal_stem": p.to_string_lossy(),
        "clicks_repaired": clicks_fixed,
        "stem_peak_dbfs": -6.0,
        "voice_center_midi": (center * 10.0).round() / 10.0,
        "stretch_range": [(perf.stretch_range.0 * 100.0).round() / 100.0, (perf.stretch_range.1 * 100.0).round() / 100.0],
        "seconds": {"tts": (t_tts * 10.0).round() / 10.0, "perform": (t_render * 10.0).round() / 10.0},
        "voice": if from_tts { "local TTS guide voice (piper en_US-lessac-medium); English voice, so Punjabi words are anglicised" } else { "the recording's own voice" },
        "first_words": perf.words.iter().take(6).map(|w| json!({"word": w.text, "beat": w.beat, "notes": w.notes})).collect::<Vec<_>>(),
    }))
}


/// How much the drums bus rises in the hooks of a sung song (dB; critic v19).
pub const HOOK_DRUM_LIFT_DB: f32 = 3.0;
/// How much the voice rises in the hooks (dB; critic v19).
pub const HOOK_VOCAL_LIFT_DB: f32 = 1.5;
/// The hook double: this many dB under the voice, this late (ms).
pub const DOUBLE_DB: f32 = -9.0;
pub const DOUBLE_DELAY_MS: f32 = 22.0;

/// A quiet double of the voice that only plays in the hooks: the vocal track
/// cloned (same chain and sends) as "vocal_double", its clip 22 ms late,
/// 9 dB under, a slow chorus so it is a second take rather than a comb, and
/// silent outside the hooks (fader at -100 dB, section mix opens it).
pub fn hook_double(e: &mut Engine, hooks: &[String]) -> Result<()> {
    if hooks.is_empty() {
        return Ok(());
    }
    let vi = e.project.track_index("vocal")?;
    let Some(clip) = e.project.audio_clips.iter().find(|c| c.track == "vocal").cloned() else {
        return Ok(());
    };
    let mut t = e.project.tracks[vi].clone();
    let level = t.volume_db + DOUBLE_DB;
    t.name = "vocal_double".into();
    t.volume_db = -100.0;
    t.pan = 0.2;
    e.project.tracks.retain(|x| x.name != "vocal_double");
    e.project.audio_clips.retain(|c| c.track != "vocal_double");
    e.project.automation.retain(|l| l.target != "vocal_double");
    e.project.tracks.push(t);
    let mut c = clip;
    c.track = "vocal_double".into();
    c.start_beat += DOUBLE_DELAY_MS / 1000.0 * e.project.bpm / 60.0;
    e.project.audio_clips.push(c);
    e.call_from("add_effect", &json!({"track": "vocal_double", "type": "chorus", "params": {"rate_hz": 0.4, "depth_ms": 6.0, "mix": 0.6}}), "producer")?;
    for h in hooks {
        e.call_from("set_section_mix", &json!({"section": h, "track": "vocal_double", "volume_db": level, "all_occurrences": true}), "producer")?;
    }
    Ok(())
}

/// Name of the bus that carries the beat through the lo-fi tape.
pub const TAPE_BUS: &str = "beat_tape";

/// The lo-fi darkening on the master (the crusher, a dusty top cut at or
/// under 10 kHz, vinyl) moves onto a bus that carries every part except the
/// vocal and its returns, so the words keep their consonants (critic v18:
/// keep lo-fi darkening off the vocal bus). Returns the effects moved.
pub fn keep_vocal_off_tape(e: &mut Engine) -> Vec<String> {
    let dark = |x: &crate::fx::Effect| {
        let n = x.type_name();
        if matches!(n.as_str(), "bitcrush" | "vinyl") {
            return true;
        }
        if n == "parametric_eq" {
            let v = serde_json::to_value(x).unwrap_or(Value::Null);
            return v["bands"].as_array().is_some_and(|b| b.iter().any(|b| b["kind"] == "high_cut" && b["freq"].as_f64().unwrap_or(1e9) <= 10000.0));
        }
        false
    };
    if e.project.track_index("vocal").is_err() || !e.project.master_effects.iter().any(|x| dark(x)) {
        return Vec::new();
    }
    let (moved, kept): (Vec<_>, Vec<_>) = e.project.master_effects.drain(..).partition(|x| dark(x));
    e.project.master_effects = kept;
    let names: Vec<String> = moved.iter().map(|x| x.type_name()).collect();
    if e.project.bus_index(TAPE_BUS).is_err() {
        e.project.buses.push(crate::project::Bus::new(TAPE_BUS));
    }
    let bi = e.project.bus_index(TAPE_BUS).expect("tape bus");
    e.project.buses[bi].effects.extend(moved);
    for t in e.project.tracks.iter_mut() {
        if t.output.is_none() && t.name != "vocal" {
            t.output = Some(TAPE_BUS.into());
        }
    }
    for b in e.project.buses.iter_mut() {
        if b.output.is_none() && !matches!(b.name.as_str(), TAPE_BUS | "vox_verb" | "vox_delay") {
            b.output = Some(TAPE_BUS.into());
        }
    }
    e.revision += 1;
    names
}

/// produce_song: a recording (spoken or rapped words, phone-quality is fine)
/// becomes a finished song: denoise, transcribe, cut fillers/repeats/false
/// starts, lay the words on a beat as a rap flow or a sung melody, mix, master.
fn produce_song(e: &mut Engine, a: &Value) -> Result<Value> {
    use crate::speech_song as ss;
    let t0 = std::time::Instant::now();
    let path = e.resolve(&crate::tools::s_req(a, "path")?);
    if !path.exists() {
        bail!("no file at {} (give the recording's path)", path.display());
    }
    let raw = crate::samples::decode_file(&path)?;
    if raw.len() < (crate::dsp::SR * 1.5) as usize {
        bail!("the recording is shorter than 1.5 seconds");
    }
    // 1. clean
    let (clean, red_db) = if crate::tools::b_or(a, "denoise", true) { ss::denoise(&raw, -18.0, 1.6) } else { (raw.clone(), 0.0) };
    std::fs::create_dir_all(e.samples_dir())?;
    let stem = crate::samples::sample_name(path.file_stem().and_then(|s| s.to_str()).unwrap_or("take"));
    let clean_path = e.samples_dir().join(format!("{stem}_clean.wav"));
    crate::render::write_wav(&clean_path, &clean, &clean)?;
    let t_clean = t0.elapsed().as_secs_f32();
    // 2. words
    let lyr = crate::vocal::transcribe(&clean_path, &e.workdir, &s_opt(a, "model").unwrap_or_else(|| "base".into()), s_opt(a, "language").as_deref(), None)?;
    let t_words = t0.elapsed().as_secs_f32() - t_clean;
    let words = lyr.words();
    // 3. edit: fillers, stutters/false starts, low-confidence blips
    let norm = |w: &str| w.to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == '\'').collect::<String>();
    let mut kept: Vec<crate::vocal::Word> = Vec::new();
    let mut removed: Vec<Value> = Vec::new();
    for w in words.iter() {
        let n = norm(&w.word);
        if n.is_empty() {
            continue;
        }
        if ss::FILLERS.contains(&n.as_str()) {
            removed.push(json!({"word": w.word, "at_s": w.start, "why": "filler"}));
            continue;
        }
        if w.prob > 0.0 && w.prob < 0.2 && w.end - w.start < 0.15 {
            removed.push(json!({"word": w.word, "at_s": w.start, "why": "unclear blip"}));
            continue;
        }
        if let Some(prev) = kept.last() {
            let pn = norm(&prev.word);
            // "the the", "we- we choose": keep the second (the real take)
            if (pn == n || (n.starts_with(&pn) && pn.len() >= 1 && pn.len() < n.len() && prev.end - prev.start < 0.25)) && w.start - prev.end < 0.8 {
                removed.push(json!({"word": prev.word, "at_s": prev.start, "why": if pn == n { "repeated word" } else { "false start" }}));
                kept.pop();
            }
        }
        kept.push(w.clone());
    }
    if kept.len() < 3 {
        bail!("heard only {} usable words in the recording", kept.len());
    }
    // 4. lines: phrase breaks at pauses and punctuation, at most 9 words
    let mut lines: Vec<Vec<crate::vocal::Word>> = vec![Vec::new()];
    for (i, w) in kept.iter().enumerate() {
        let cur = lines.last_mut().unwrap();
        cur.push(w.clone());
        let punct = w.word.trim_end().ends_with(['.', ',', '?', '!', ';']);
        let gap = kept.get(i + 1).map(|n| n.start - w.end).unwrap_or(0.0);
        if (punct || gap > 0.35 || cur.len() >= 9) && i + 1 < kept.len() {
            lines.push(Vec::new());
        }
    }
    lines.retain(|l| !l.is_empty());
    // a one- or two-word fragment rides with the line before it (a bar per
    // fragment makes the flow stall)
    let mut merged: Vec<Vec<crate::vocal::Word>> = Vec::new();
    for l in lines {
        match merged.last_mut() {
            Some(prev) if l.len() <= 2 && prev.len() + l.len() <= 10 => prev.extend(l),
            _ => merged.push(l),
        }
    }
    let lines = merged;
    let sr = crate::dsp::SR;
    // keep_flow: whole phrases (split at the take's own pauses) ride as one
    // clip each, so every phrase lands on its bar with the flow inside it
    // untouched (no per-word relock, nothing cut)
    let keep_flow = crate::tools::b_or(a, "keep_flow", false);
    let flow_clips: Option<(Vec<Vec<ss::Clip>>, Vec<String>)> = if keep_flow {
        let ph = crate::vocal_flex::phrases(&clean, crate::tools::f_or(a, "min_pause_s", 0.3).clamp(0.1, 2.0), None);
        let mut cl = Vec::new();
        let mut tx = Vec::new();
        for p in &ph {
            let (s0, s1) = ((p.start_s * sr) as usize, ((p.end_s * sr) as usize).min(clean.len()));
            if s1 <= s0 + (0.1 * sr) as usize {
                continue;
            }
            let mut audio = clean[s0..s1].to_vec();
            crate::audio_edit::fade(&mut audio, 4.0, 15.0);
            let ws: Vec<String> = words.iter().filter(|w| { let m = 0.5 * (w.start + w.end); m >= p.start_s && m < p.end_s }).map(|w| w.word.trim().to_string()).filter(|w| !w.is_empty()).collect();
            let text = if ws.is_empty() { "yeah".to_string() } else { ws.join(" ") };
            cl.push(vec![ss::Clip { syl: ws.len().max(1), text: format!("~{text}"), audio }]);
            tx.push(text);
        }
        if cl.len() < 2 {
            bail!("keep_flow found only {} phrase(s) in the take (try min_pause_s 0.2)", cl.len());
        }
        Some((cl, tx))
    } else {
        None
    };
    let clips: Vec<Vec<ss::Clip>> = lines
        .iter()
        .map(|l| {
            l.iter()
                .filter_map(|w| {
                    let a0 = ((w.start - 0.02).max(0.0) * sr) as usize;
                    let a1 = (((w.end + 0.04) * sr) as usize).min(clean.len());
                    if a1 <= a0 + (0.05 * sr) as usize {
                        return None;
                    }
                    let audio = ss::tighten(&clean[a0..a1], -35.0);
                    let text = norm(&w.word);
                    (!audio.is_empty()).then(|| ss::Clip { syl: ss::syllables(&text), text, audio })
                })
                .collect()
        })
        .collect();
    let (clips, text): (Vec<Vec<ss::Clip>>, Vec<String>) = match flow_clips {
        Some((c, t)) => (c, t),
        None => {
            let t = clips.iter().map(|l| l.iter().map(|c| c.text.clone()).collect::<Vec<_>>().join(" ")).collect();
            (clips, t)
        }
    };
    let lyrics_text = text.join("\n");
    // 5. the song
    let mode = match s_opt(a, "mode").as_deref() {
        Some("sing") => "sing",
        _ => "rap",
    };
    let prompt = s_opt(a, "prompt").unwrap_or_else(|| {
        if mode == "rap" { "hard dark boom bap beat, 90 bpm, punchy drums, heavy 808".into() } else { "smooth emotional rnb beat, 84 bpm, warm keys and pads".into() }
    });
    let mut args = json!({"prompt": prompt, "lyrics": lyrics_text, "vocal": mode, "quality": crate::tools::u_or(a, "quality", 1)});
    for k in ["seed", "out_dir", "target_lufs"] {
        if let Some(v) = a.get(k) {
            args[k] = v.clone();
        }
    }
    let res = make_beat_with(e, &args, Some(clips))?;
    let total = t0.elapsed().as_secs_f32();
    Ok(json!({
        "summary": if keep_flow { format!("{} song from {} phrases (flow kept), {} s", mode, text.len(), e.project.song_seconds().round()) } else { format!("{} song from {} words ({} cut), {} s", mode, kept.len(), removed.len(), e.project.song_seconds().round()) },
        "file": res["vocal"]["file"].clone(),
        "files": {"song_with_vocal": res["vocal"]["file"].clone(), "instrumental": res["files"].get("instrumental").cloned().unwrap_or(Value::Null)},
        "language": lyr.language.clone(),
        "keep_flow": keep_flow,
        "beat_only": res["files"].get("instrumental").cloned().unwrap_or_else(|| res["files"]["audio"].clone()),
        "report": {
            "denoise_db": (red_db * 10.0).round() / 10.0,
            "clean_take": clean_path.to_string_lossy(),
            "heard": lyr.text,
            "kept_lines": text,
            "removed": removed,
            "beat": res["summary"].clone(),
            "vocal": res["vocal"].clone(),
            "seconds": {"clean": (t_clean * 10.0).round() / 10.0, "transcribe": (t_words * 10.0).round() / 10.0, "total": (total * 10.0).round() / 10.0},
        },
        "next": ["produce_song again with mode:'sing' (or 'rap') for the other version", "revise the beat: make_beat with a different prompt/seed", "set_mixer {track:'vocal'} to balance the voice"],
    }))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "make_beat",
            description: "One call, finished beat. prompt: what you want in plain words ('Bohemia type beat with a beat switch and a quiet section', 'dark UK drill 144 bpm with piano'). lyrics: optional text; rap vs sung, mood and hooks shape the beat. Artist references, beat switches, quiet parts, drops, half-time, key changes, instruments, BPM, key and length are understood. Built, mixed, mastered and checked by the producer.",
            mutates: true,
            schema: || obj(json!({"prompt": {"type": "string"}, "lyrics": {"type": "string"}, "seed": {"type": "integer"}, "out_dir": {"type": "string"}, "quality": {"type": "integer", "description": "1-6 listen/revise rounds (default 2)"}, "target_lufs": {"type": "number", "description": "delivery loudness (default -14, streaming)"}, "vocal": {"type": "string", "enum": ["auto", "sing", "rap", "none"], "description": "with lyrics: perform them with a guide voice (auto = sung for melodic lyrics, rapped for rap)"}}), &[]),
            run: make_beat,
        },
        Tool {
            name: "produce_song",
            description: "One call, finished song from a recording of someone talking or rapping (path to wav/mp3/ogg). Cleans noise, transcribes, cuts fillers/repeated words/false starts, then performs the words on a new beat: mode 'rap' locks every word to a 16th-note flow, mode 'sing' writes a melody and tunes the voice onto it; keep_flow:true instead places the take's own phrases on bar lines and keeps the flow inside them. Set language for non-English takes. Mixed and mastered (-14 LUFS). Returns the song file and a production report.",
            mutates: true,
            schema: || obj(json!({"path": {"type": "string"}, "mode": {"type": "string", "enum": ["rap", "sing"], "description": "default rap"}, "prompt": {"type": "string", "description": "the beat you want (default by mode)"}, "seed": {"type": "integer"}, "out_dir": {"type": "string"}, "target_lufs": {"type": "number"}, "denoise": {"type": "boolean"}, "quality": {"type": "integer"}, "language": {"type": "string", "description": "the take's language code, e.g. 'hi' (Hindi), 'pa', 'en' (default: detected; set it for anything but English)"}, "model": {"type": "string", "enum": ["tiny", "base", "small", "medium"], "description": "speech recogniser size (default base; small is better for Hindi/Punjabi)"}, "keep_flow": {"type": "boolean", "description": "true: place the take's own phrases (split at its pauses) on bar lines with the flow inside each phrase untouched and no words cut; false (default): relock every word"}, "min_pause_s": {"type": "number", "description": "keep_flow: the pause that splits phrases (default 0.3)"}}), &["path"]),
            run: produce_song,
        },
        Tool {
            name: "analyze_lyrics",
            description: "Read lyrics (text): rap or sung, syllables per line, rhyme density, mood, sections with the hook found from repeats, bar counts, and the genre/BPM they suggest.",
            mutates: false,
            schema: || obj(json!({"lyrics": {"type": "string"}}), &["lyrics"]),
            run: |_, a| Ok(serde_json::to_value(analyze_lyrics(&s_opt(a, "lyrics").unwrap_or_default()))?),
        },
        Tool {
            name: "read_prompt",
            description: "Show how a beat prompt is understood (genre, BPM, key, mood, contrasts like beat_switch/drum_dropout, instruments, length) without building anything.",
            mutates: false,
            schema: || obj(json!({"prompt": {"type": "string"}}), &["prompt"]),
            run: |_, a| Ok(serde_json::to_value(parse_prompt(&s_opt(a, "prompt").unwrap_or_default()))?),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lofi_tape_moves_off_the_vocal() {
        let mut e = Engine::new(std::env::temp_dir().join("beatbox_prompt_tape_tests"));
        e.call("new_project", &json!({"bpm": 82, "patterns": [{"name": "a", "bars": 1}]})).unwrap();
        e.call("add_track", &json!({"name": "pad", "preset": "warm_pad"})).unwrap();
        e.call("add_track", &json!({"name": "vocal", "preset": "warm_pad"})).unwrap();
        e.call("add_bus", &json!({"name": "verb", "preset": "reverb"})).unwrap();
        e.call("add_bus", &json!({"name": "vox_verb", "preset": "reverb"})).unwrap();
        e.call("add_effect", &json!({"track": "master", "type": "bitcrush", "params": {"bits": 12.0, "downsample": 2, "mix": 0.25}})).unwrap();
        e.call("add_effect", &json!({"track": "master", "type": "parametric_eq", "params": {"bands": [{"kind": "high_cut", "freq": 8000.0, "gain_db": 0.0, "q": 0.707}]}})).unwrap();
        let n0 = e.project.master_effects.len();
        let moved = keep_vocal_off_tape(&mut e);
        assert_eq!(moved, vec!["bitcrush".to_string(), "parametric_eq".to_string()]);
        assert_eq!(e.project.master_effects.len(), n0 - 2);
        assert!(e.project.master_effects.iter().all(|x| x.type_name() != "bitcrush"));
        let tape = &e.project.buses[e.project.bus_index(TAPE_BUS).unwrap()];
        assert_eq!(tape.effects.len(), 2);
        let out = |t: &str| e.project.tracks[e.project.track_index(t).unwrap()].output.clone();
        assert_eq!(out("pad").as_deref(), Some(TAPE_BUS));
        assert_eq!(out("vocal"), None);
        let bus_out = |b: &str| e.project.buses[e.project.bus_index(b).unwrap()].output.clone();
        assert_eq!(bus_out("verb").as_deref(), Some(TAPE_BUS));
        assert_eq!(bus_out("vox_verb"), None);
        assert_eq!(bus_out(TAPE_BUS), None);
        // a second call has nothing left to move
        assert!(keep_vocal_off_tape(&mut e).is_empty());
    }
}
