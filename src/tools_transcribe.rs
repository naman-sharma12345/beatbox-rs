//! Transcription tools: a recording (a stem from separate_stems, or a whole
//! mix) in, notes out. Drums per bar, bass line, chords, a riff or melody,
//! each as a MIDI file plus a readable summary, all on one shared bar grid so
//! the parts line up when they are played back on the engine's instruments.

use crate::engine::Engine;
use crate::tools::{b_or, f_opt, obj, s_opt, s_req, u_or, Tool};
use crate::transcribe::{self as tr, Grid};
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn load(e: &Engine, a: &Value) -> Result<(PathBuf, Vec<f32>)> {
    let path = e.resolve(&s_req(a, "path")?);
    if !path.exists() {
        bail!("no file at {}", path.display());
    }
    let x = crate::samples::decode_file(&path)?;
    tr::check_audio(&x, 2.0)?;
    Ok((path, x))
}

fn file_key(p: &Path) -> String {
    let md = std::fs::metadata(p).ok();
    let len = md.as_ref().map(|m| m.len()).unwrap_or(0);
    let mt = md
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}|{len}|{mt}", p.display())
}

/// The grid a transcription runs on: bpm (+ downbeat_s) as given, else the
/// grid of `grid_from` (e.g. the full mix, so every stem shares its bars),
/// else the input's own. Estimated grids are cached per file.
pub fn grid_for(e: &Engine, a: &Value, path: &Path, x: &[f32]) -> Result<(Grid, String)> {
    let bpb = u_or(a, "beats_per_bar", 4) as usize;
    if let Some(bpm) = f_opt(a, "bpm") {
        return Ok((
            Grid::new(bpm, f_opt(a, "downbeat_s").unwrap_or(0.0), bpb),
            "given".into(),
        ));
    }
    let (src, own) = match s_opt(a, "grid_from") {
        Some(g) => (e.resolve(&g), false),
        None => (path.to_path_buf(), true),
    };
    let cache = e.workdir.join(".beatbox_grid_cache.json");
    let key = file_key(&src);
    let mut map: Map<String, Value> = std::fs::read_to_string(&cache)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if let Some(v) = map.get(&key) {
        if let Ok(g) = serde_json::from_value::<Grid>(v.clone()) {
            return Ok((
                Grid::new(g.bpm, g.downbeat_s, bpb),
                format!("estimated from {} (cached)", src.display()),
            ));
        }
    }
    let (g, _conf) = if own {
        tr::estimate_grid(x)?
    } else {
        if !src.exists() {
            bail!("grid_from: no file at {}", src.display());
        }
        tr::estimate_grid(&crate::samples::decode_file(&src)?)?
    };
    map.insert(key, serde_json::to_value(g)?);
    let _ = std::fs::write(&cache, serde_json::to_string(&map)?);
    Ok((
        Grid::new(g.bpm, g.downbeat_s, bpb),
        format!("estimated from {}", src.display()),
    ))
}

fn out_base(e: &Engine, a: &Value, path: &Path, kind: &str) -> Result<PathBuf> {
    let dir = match s_opt(a, "out_dir") {
        Some(d) => e.resolve(&d),
        None => e.renders_dir().join("transcriptions"),
    };
    std::fs::create_dir_all(&dir)?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("audio");
    Ok(dir.join(format!("{stem}_{kind}")))
}

fn steps_arg(a: &Value) -> Result<usize> {
    let s = u_or(a, "steps_per_bar", 16) as usize;
    if ![8, 12, 16, 24, 32].contains(&s) {
        bail!("steps_per_bar must be 8, 12, 16, 24 or 32");
    }
    Ok(s)
}

fn grid_json(g: &Grid, src: &str) -> Value {
    json!({"bpm": (g.bpm * 100.0).round() / 100.0, "downbeat_s": (g.downbeat_s * 1000.0).round() / 1000.0, "beats_per_bar": g.beats_per_bar, "from": src})
}

/// "AAAB AAAC ..." in groups of four bars.
fn letters_text(l: &[char]) -> String {
    l.chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

fn maybe_import(e: &mut Engine, a: &Value, midi: &Path, name: &str) -> Option<Value> {
    if !b_or(a, "import", false) {
        return None;
    }
    Some(
        match e.call_from(
            "import_midi",
            &json!({"path": midi.to_string_lossy(), "pattern": name, "set_tempo": false}),
            "transcribe",
        ) {
            Ok(v) => {
                json!({"pattern": name, "tracks": v.get("tracks").cloned().unwrap_or(Value::Null)})
            }
            Err(err) => json!({"error": format!("{err:#}")}),
        },
    )
}

fn write_midi(p: &Path, smf: &crate::smf::Smf) -> Result<()> {
    std::fs::write(p, crate::smf::write(smf))?;
    Ok(())
}

fn transcribe_drums(e: &mut Engine, a: &Value) -> Result<Value> {
    let (path, x) = load(e, a)?;
    let steps = steps_arg(a)?;
    let (grid, src) = grid_for(e, a, &path, &x)?;
    let s = tr::transcribe_drums_opts(&x, grid, steps, b_or(a, "follow_level", false));
    let base = out_base(e, a, &path, "drums")?;
    let midi = base.with_extension("mid");
    write_midi(&midi, &tr::drums_midi(&s, b_or(a, "keep_timing", false)))?;
    // patterns by letter, most used first
    let mut counts: Vec<(char, usize, usize)> = Vec::new();
    for (b, l) in s.letters.iter().enumerate() {
        if *l == '-' {
            continue;
        }
        match counts.iter_mut().find(|c| c.0 == *l) {
            Some(c) => c.1 += 1,
            None => counts.push((*l, 1, b)),
        }
    }
    counts.sort_by_key(|a| std::cmp::Reverse(a.1));
    let ghost_max = 60u8;
    let pat_json = |b: usize| -> Value {
        let mut m = Map::new();
        for (k, name) in tr::DRUM_KINDS.iter().enumerate() {
            if s.pattern[b][k].iter().any(|v| *v > 0) {
                m.insert(
                    name.to_string(),
                    json!(tr::row_text(
                        &s.pattern[b][k],
                        if k == 1 { ghost_max } else { 0 }
                    )),
                );
            }
        }
        Value::Object(m)
    };
    let mut patterns = Map::new();
    for (l, n, first) in counts.iter().take(8) {
        let mut p = pat_json(*first);
        p["bars"] = json!(n);
        patterns.insert(l.to_string(), p);
    }
    let mut hits = Map::new();
    let mut push = Map::new();
    for (k, name) in tr::DRUM_KINDS.iter().enumerate() {
        let n = s.hits.iter().filter(|h| h.kind == k).count();
        if n > 0 {
            hits.insert(name.to_string(), json!(n));
            push.insert(name.to_string(), json!(s.push_ms[k]));
        }
    }
    let detail = base.with_extension("json");
    let bars: Vec<Value> = (0..s.bars).map(|b| json!({"bar": b + 1, "letter": s.letters[b].to_string(), "t": (grid.bar_t(b) * 1000.0).round() / 1000.0, "fill": s.fills.contains(&b), "pattern": pat_json(b)})).collect();
    std::fs::write(
        &detail,
        serde_json::to_string_pretty(
            &json!({"grid": grid_json(&grid, &src), "steps_per_bar": steps, "swing_pct": s.swing_pct, "bars": bars,
        "hits": s.hits.iter().map(|h| json!({"t": (h.t * 1000.0).round() / 1000.0, "kind": tr::DRUM_KINDS[h.kind], "vel": h.vel, "ghost": h.ghost, "bar": h.bar + 1, "step": h.step, "offset": (h.offset * 100.0).round() / 100.0})).collect::<Vec<_>>()}),
        )?,
    )?;
    let imported = maybe_import(e, a, &midi, "drums_transcribed");
    Ok(
        json!({"grid": grid_json(&grid, &src), "bars": s.bars, "steps_per_bar": steps, "swing_pct": s.swing_pct, "push_ms": push, "hits": hits,
        "ghost_notes": s.hits.iter().filter(|h| h.ghost).count(),
        "patterns": patterns, "bar_letters": letters_text(&s.letters), "fills": s.fills.iter().map(|b| b + 1).collect::<Vec<_>>(),
        "legend": "per step: X accent, x hit, g ghost, . rest; bar_letters groups 4 bars, '-' = no drums",
        "midi": midi.to_string_lossy(), "detail": detail.to_string_lossy(), "imported": imported,
        "next": ["import_midi", "transcribe_bass", "arrangement_map"]}),
    )
}

fn pitched_summary(
    notes: &[tr::PNote],
    grid: &Grid,
    src: &str,
    steps: usize,
    dur: f32,
    midi: &Path,
    detail: &Path,
    imported: Option<Value>,
) -> Value {
    let bars = grid.bars_in(dur);
    let per_bar = tr::notes_per_bar(notes, steps, bars);
    let mut hist = [0usize; 12];
    for n in notes {
        hist[(n.pitch % 12) as usize] += n.len;
    }
    let mut pcs: Vec<(usize, usize)> = hist
        .iter()
        .cloned()
        .enumerate()
        .filter(|x| x.1 > 0)
        .collect();
    pcs.sort_by_key(|a| std::cmp::Reverse(a.1));
    let lo = notes.iter().map(|n| n.pitch).min();
    let hi = notes.iter().map(|n| n.pitch).max();
    json!({"grid": grid_json(grid, src), "notes": notes.len(), "bars": bars,
        "range": match (lo, hi) { (Some(l), Some(h)) => json!(format!("{}-{}", tr::note_name(l), tr::note_name(h))), _ => Value::Null },
        "main_pitch_classes": pcs.iter().take(5).map(|(p, _)| tr::PCN[*p]).collect::<Vec<_>>(),
        "first_bars": per_bar.iter().take(8).enumerate().map(|(i, s)| format!("{}: {s}", i + 1)).collect::<Vec<_>>(),
        "legend": "pitch@step:length in steps",
        "midi": midi.to_string_lossy(), "detail": detail.to_string_lossy(), "imported": imported})
}

fn write_notes_detail(
    p: &Path,
    notes: &[tr::PNote],
    grid: &Grid,
    src: &str,
    steps: usize,
    dur: f32,
) -> Result<()> {
    let per_bar = tr::notes_per_bar(notes, steps, grid.bars_in(dur));
    std::fs::write(
        p,
        serde_json::to_string_pretty(
            &json!({"grid": grid_json(grid, src), "steps_per_bar": steps,
            "per_bar": per_bar,
            "notes": notes.iter().map(|n| json!({"t0": (n.t0 * 1000.0).round() / 1000.0, "t1": (n.t1 * 1000.0).round() / 1000.0, "pitch": tr::note_name(n.pitch), "midi": n.pitch, "vel": n.vel, "bar": n.step / steps + 1, "step": n.step % steps, "len": n.len})).collect::<Vec<_>>()}),
        )?,
    )?;
    Ok(())
}

fn transcribe_bass(e: &mut Engine, a: &Value) -> Result<Value> {
    let (path, x) = load(e, a)?;
    let steps = steps_arg(a)?;
    let (grid, src) = grid_for(e, a, &path, &x)?;
    let mut notes = tr::transcribe_bass(&x, grid, steps);
    let restruck = if b_or(a, "reattacks", false) {
        tr::split_reattacks(
            &tr::log_flux_onsets(&x, f_opt(a, "reattack_delta").unwrap_or(0.07)),
            &mut notes,
            grid,
            steps,
        )
    } else {
        0
    };
    let base = out_base(e, a, &path, "bass")?;
    let midi = base.with_extension("mid");
    write_midi(
        &midi,
        &tr::notes_midi(
            &notes,
            grid,
            steps,
            "Bass",
            Some(33),
            b_or(a, "keep_timing", false),
        ),
    )?;
    let detail = base.with_extension("json");
    let dur = x.len() as f32 / crate::dsp::SR;
    write_notes_detail(&detail, &notes, &grid, &src, steps, dur)?;
    let imported = maybe_import(e, a, &midi, "bass_transcribed");
    let mut v = pitched_summary(&notes, &grid, &src, steps, dur, &midi, &detail, imported);
    if b_or(a, "reattacks", false) {
        v["restruck_notes"] = json!(restruck);
    }
    v["next"] = json!(["import_midi", "transcribe_chords", "match_timbre"]);
    Ok(v)
}

fn transcribe_melody(e: &mut Engine, a: &Value) -> Result<Value> {
    let (path, x) = load(e, a)?;
    let steps = steps_arg(a)?;
    let (grid, src) = grid_for(e, a, &path, &x)?;
    let lo = f_opt(a, "low_hz").unwrap_or(150.0);
    let hi = f_opt(a, "high_hz").unwrap_or(1500.0);
    if !(lo >= 30.0 && hi > lo * 1.5 && hi <= 4000.0) {
        bail!("low_hz/high_hz: need 30 <= low < high/1.5 and high <= 4000");
    }
    let name = s_opt(a, "name").unwrap_or_else(|| "melody".into());
    let mode = s_opt(a, "mode").unwrap_or_else(|| "line".into());
    let notes = match mode.as_str() {
        "line" => tr::transcribe_top_line(&x, grid, steps, lo, hi),
        "picked" => tr::transcribe_picked(
            &x,
            grid,
            steps,
            lo,
            hi,
            f_opt(a, "sensitivity").unwrap_or(0.5),
        ),
        m => {
            bail!("mode '{m}': line (one sustained line) or picked (one note per pluck: arpeggios)")
        }
    };
    let base = out_base(e, a, &path, &name)?;
    let midi = base.with_extension("mid");
    write_midi(
        &midi,
        &tr::notes_midi(
            &notes,
            grid,
            steps,
            &name,
            None,
            b_or(a, "keep_timing", false),
        ),
    )?;
    let detail = base.with_extension("json");
    let dur = x.len() as f32 / crate::dsp::SR;
    write_notes_detail(&detail, &notes, &grid, &src, steps, dur)?;
    let imported = maybe_import(e, a, &midi, &format!("{name}_transcribed"));
    let mut v = pitched_summary(&notes, &grid, &src, steps, dur, &midi, &detail, imported);
    v["next"] = json!(["import_midi", "match_timbre"]);
    Ok(v)
}

fn transcribe_chords(e: &mut Engine, a: &Value) -> Result<Value> {
    let (path, x) = load(e, a)?;
    let (grid, src) = grid_for(e, a, &path, &x)?;
    let (ch, mut bch, en) = tr::beat_chroma(&x, grid);
    // an isolated bass stem names the roots far better than the low end of a mix (kick, riff overtones)
    let mut bass_w = 0.25f32;
    if let Some(bp) = s_opt(a, "bass") {
        let bx = crate::samples::decode_file(&e.resolve(&bp))?;
        // the bass line's own pitch-tracked notes (an FFT can't tell D1 from D#1), weighted by how
        // long each sounds inside each beat
        let notes = tr::transcribe_bass(&bx, grid, 16);
        let nb = bch.len();
        bch = vec![[0.0f32; 12]; nb];
        let bs = grid.beat_s();
        for n in &notes {
            let b0 = ((n.t0 - grid.downbeat_s) / bs).floor().max(0.0) as usize;
            let b1 = ((n.t1 - grid.downbeat_s) / bs).ceil().max(0.0) as usize;
            for b in b0..b1.min(nb) {
                let (s0, s1) = (
                    grid.downbeat_s + b as f32 * bs,
                    grid.downbeat_s + (b + 1) as f32 * bs,
                );
                let ov = n.t1.min(s1) - n.t0.max(s0);
                if ov > 0.0 {
                    bch[b][(n.pitch % 12) as usize] += ov;
                }
            }
        }
        bass_w = 0.6;
    }
    let bass_w = f_opt(a, "bass_weight").unwrap_or(bass_w).clamp(0.0, 2.0);
    let path_st = tr::chords_per_beat(
        &ch,
        &bch,
        &en,
        f_opt(a, "change_cost").unwrap_or(0.15).clamp(0.0, 2.0),
        bass_w,
    );
    let (k, minor, kconf) = tr::key_of(&ch);
    let bpb = grid.beats_per_bar;
    let bars = path_st.len().div_ceil(bpb);
    // per bar: the chord on each beat, written as "Dm . . ." (a dot repeats)
    let mut per_bar = Vec::new();
    let mut main = Vec::new();
    for b in 0..bars {
        let mut cells = Vec::new();
        let mut prev = String::new();
        for i in 0..bpb {
            let s = path_st
                .get(b * bpb + i)
                .map(|s| tr::chord_label(*s))
                .unwrap_or_else(|| "N".into());
            cells.push(if s == prev {
                ".".to_string()
            } else {
                s.clone()
            });
            prev = s;
        }
        // the bar's main chord: the most beats
        let mut cnt: Vec<(usize, usize)> = Vec::new();
        for i in 0..bpb {
            if let Some(s) = path_st.get(b * bpb + i) {
                match cnt.iter_mut().find(|c| c.0 == *s) {
                    Some(c) => c.1 += 1,
                    None => cnt.push((*s, 1)),
                }
            }
        }
        let m = cnt
            .iter()
            .max_by_key(|c| c.1)
            .map(|c| tr::chord_label(c.0))
            .unwrap_or_else(|| "N".into());
        main.push(m);
        per_bar.push(cells.join(" "));
    }
    let (loop_bars, held) = tr::chord_loop(&main);
    let first = main.iter().position(|c| c != "N").unwrap_or(0);
    let loop_txt = if loop_bars > 0 {
        main[first..(first + loop_bars).min(main.len())].join(" | ")
    } else {
        String::new()
    };
    // MIDI: one block chord per chord change (close voicing around C4)
    let ppq = 480u32;
    let mut notes = Vec::new();
    let mut i = 0;
    while i < path_st.len() {
        let s = path_st[i];
        let mut j = i + 1;
        while j < path_st.len() && path_st[j] == s {
            j += 1;
        }
        for p in tr::voicing(s) {
            notes.push(crate::smf::SmfNote {
                tick: i as u32 * ppq,
                len: (j - i) as u32 * ppq - 10,
                pitch: p,
                vel: 80,
                channel: 0,
            });
        }
        i = j;
    }
    let base = out_base(e, a, &path, "chords")?;
    let midi = base.with_extension("mid");
    write_midi(
        &midi,
        &crate::smf::Smf {
            division: ppq as u16,
            bpm: grid.bpm,
            time_sig: (bpb as u8, 4),
            tracks: vec![crate::smf::SmfTrack {
                name: "Chords".into(),
                program: Some(0),
                notes,
            }],
        },
    )?;
    let detail = base.with_extension("json");
    std::fs::write(
        &detail,
        serde_json::to_string_pretty(
            &json!({"grid": grid_json(&grid, &src), "per_bar": per_bar, "main_per_bar": main, "per_beat": path_st.iter().map(|s| tr::chord_label(*s)).collect::<Vec<_>>()}),
        )?,
    )?;
    let imported = maybe_import(e, a, &midi, "chords_transcribed");
    Ok(
        json!({"grid": grid_json(&grid, &src), "key": format!("{} {}", tr::PCN[k as usize], if minor { "minor" } else { "major" }), "key_confidence": (kconf * 100.0).round() / 100.0,
        "bars": bars, "loop": if loop_bars > 0 { json!({"bars": loop_bars, "chords": loop_txt, "holds_pct": (held * 100.0).round()}) } else { Value::Null },
        "first_bars": per_bar.iter().take(8).enumerate().map(|(i, s)| format!("{}: {s}", i + 1)).collect::<Vec<_>>(),
        "legend": "one cell per beat, '.' = same chord, N = no chord",
        "midi": midi.to_string_lossy(), "detail": detail.to_string_lossy(), "imported": imported,
        "next": ["import_midi", "set_key", "transcribe_bass"]}),
    )
}

fn grid_props(mut m: Value) -> Value {
    let g = json!({
        "path": {"type": "string", "description": "audio file: a stem from separate_stems or a full mix"},
        "bpm": {"type": "number", "description": "tempo; omit to estimate"},
        "downbeat_s": {"type": "number", "description": "time of a bar's first beat, with bpm"},
        "grid_from": {"type": "string", "description": "estimate the bar grid from this file instead (pass the full mix so every stem shares its bars)"},
        "beats_per_bar": {"type": "integer"},
        "keep_timing": {"type": "boolean", "description": "MIDI keeps the played timing instead of snapping to the grid"},
        "import": {"type": "boolean", "description": "also import the MIDI into the project as a new pattern"},
        "out_dir": {"type": "string"}
    });
    for (k, v) in g.as_object().unwrap() {
        m[k] = v.clone();
    }
    m
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "transcribe_drums",
            description: "Drum transcription per bar: kick, snare (with ghost notes), closed and open hi-hat, crash and toms on a 16th (or 32nd) grid with velocities, plus swing %, how far each drum sits ahead of or behind the beat, a letter per bar (bars playing the same pattern share one) and the fill bars. Works best on the drums stem from separate_stems; pass the full mix as grid_from so bars match the other parts. Writes a General MIDI drum file and a per-bar JSON; import:true adds it to the project.",
            mutates: true,
            schema: || obj(grid_props(json!({"steps_per_bar": {"type": "integer", "enum": [16, 32]}, "follow_level": {"type": "boolean", "description": "read each drum against the level of the ~8 s around it rather than the whole song, so a section played softer (hats pulled back) keeps its hits. Off by default"}})), &["path"]),
            run: transcribe_drums,
        },
        Tool {
            name: "transcribe_bass",
            description: "Bass line transcription: the notes of a bass part (30-400 Hz, one note at a time) with start, length and velocity snapped to the bar grid. Use the bass stem from separate_stems; pass the full mix as grid_from so bars match the drums. Writes MIDI and a per-bar JSON; import:true adds it to the project.",
            mutates: true,
            schema: || obj(grid_props(json!({"steps_per_bar": {"type": "integer", "enum": [8, 12, 16, 24, 32]}, "reattacks": {"type": "boolean", "description": "split a held note where the same pitch is struck again (pumping 8ths, a re-picked root on the 'and' of a beat); found from the attacks in the stem. Off by default"}, "reattack_delta": {"type": "number", "description": "with reattacks: how far an attack must stand out (0.02-0.3, default 0.07; lower finds softer re-picks)"}})), &["path"]),
            run: transcribe_bass,
        },
        Tool {
            name: "transcribe_melody",
            description: "Riff / melody transcription: the most prominent line in a part (guitar riff, synth lead, piano top line, a sung melody), one note at a time between low_hz and high_hz (default 150-1500), snapped to the bar grid; mode:'picked' follows the picking hand of an arpeggiated part instead. Use the 'other' or 'vocals' stem from separate_stems. Writes MIDI and a per-bar JSON; import:true adds it to the project.",
            mutates: true,
            schema: || obj(grid_props(json!({"steps_per_bar": {"type": "integer", "enum": [8, 12, 16, 24, 32]}, "low_hz": {"type": "number"}, "high_hz": {"type": "number"}, "sensitivity": {"type": "number", "description": "picked mode: 0..1 (default 0.5), higher finds softer plucks under chorus/reverb"}, "mode": {"type": "string", "enum": ["line", "picked"], "description": "line (default): the most prominent sustained line; picked: one note per pluck, the newly struck string at each onset (arpeggiated / picked guitar, harp, plucked keys)"}, "name": {"type": "string", "description": "part name for the files and MIDI track (default melody)"}})), &["path"]),
            run: transcribe_melody,
        },
        Tool {
            name: "transcribe_chords",
            description: "Chord transcription: the chord on every beat (major, minor, 7, maj7, m7, sus2, sus4, dim, power) with the key, the repeating progression loop and the first bars as text. Works on a full mix or the 'other' stem. Writes block-chord MIDI and a per-beat JSON; import:true adds it to the project. change_cost (default 0.15) higher = fewer chord changes.",
            mutates: true,
            schema: || obj(grid_props(json!({"change_cost": {"type": "number"}, "bass": {"type": "string", "description": "the bass stem from separate_stems: its notes name the chord roots (pass the bass+other mix or the full mix as path)"}, "bass_weight": {"type": "number", "description": "how much the bass note decides the root (default 0.6 with bass, 0.25 without)"}})), &["path"]),
            run: transcribe_chords,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcribe_tools_write_midi_on_a_given_grid() {
        let dir = std::env::temp_dir().join("bb_transcribe_tools");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        // two bars at 120 bpm: a click on every beat and a held A2 bass note
        let sr = crate::dsp::SR;
        let n = (sr * 4.5) as usize;
        let mut x = vec![0.0f32; n];
        for b in 0..8 {
            let i0 = (b as f32 * 0.5 * sr) as usize;
            for k in 0..(0.15 * sr) as usize {
                let t = k as f32 / sr;
                if i0 + k < n {
                    x[i0 + k] += 0.8
                        * (2.0 * std::f32::consts::PI * (50.0 + 100.0 * (-t * 30.0).exp()) * t)
                            .sin()
                        * (-t * 20.0).exp();
                }
            }
        }
        crate::render::write_wav(&dir.join("k.wav"), &x, &x).unwrap();
        let v = e
            .call(
                "transcribe_drums",
                &json!({"path": "k.wav", "bpm": 120, "downbeat_s": 0.0}),
            )
            .unwrap();
        assert!(v["hits"]["kick"].as_u64().unwrap_or(0) >= 7, "{v}");
        assert!(Path::new(v["midi"].as_str().unwrap()).exists());
        let c = e
            .call("transcribe_chords", &json!({"path": "k.wav", "bpm": 120}))
            .unwrap();
        assert!(c["key"].is_string(), "{c}");
        assert!(e
            .call(
                "transcribe_drums",
                &json!({"path": "k.wav", "bpm": 120, "steps_per_bar": 7})
            )
            .is_err());
    }
}
