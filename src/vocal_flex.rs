//! Vocal-to-beat alignment (flex time / elastic audio): find a take's
//! syllable onsets and its phrases (split at pauses), pull the onsets onto the
//! song grid with a strength knob (pitch kept, WSOLA), place whole phrases on
//! beats with their inner flow untouched, and measure how a vocal sits on the
//! beat (level against the beat, timing against the grid).

use crate::dsp::{Biquad, BiquadKind, SR};
use crate::engine::Engine;
use crate::project::AudioClip;
use crate::tools::{b_or, f_opt, f_or, obj, s_opt, s_req, u_or, Tool};
use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Value};

/// Analysis hop: 5 ms.
pub const HOP: f32 = 0.005;

/// Voice-band level per 5 ms hop, dB (250 Hz - 3.5 kHz, 20 ms RMS window).
pub fn band_db(x: &[f32]) -> Vec<f32> {
    let mut hp = Biquad::new(BiquadKind::LowCut, 250.0, 0.707, 0.0);
    let mut lp = Biquad::new(BiquadKind::HighCut, 3500.0, 0.707, 0.0);
    let y: Vec<f32> = x.iter().map(|v| lp.process(hp.process(*v))).collect();
    let hop = (HOP * SR) as usize;
    let win = (0.02 * SR) as usize;
    let mut out = Vec::with_capacity(y.len() / hop + 1);
    let mut i = 0;
    while i < y.len() {
        let a = i.saturating_sub(win / 2);
        let b = (i + win / 2).min(y.len());
        let e: f32 = y[a..b].iter().map(|v| v * v).sum::<f32>() / (b - a).max(1) as f32;
        out.push(10.0 * e.max(1e-12).log10());
        i += hop;
    }
    out
}

/// The level that separates voice from room: the 15th percentile of the
/// band level plus 12 dB, never more than 35 dB under the loud parts.
pub fn voice_threshold(db: &[f32], rel_db: Option<f32>) -> f32 {
    if db.is_empty() {
        return -60.0;
    }
    let mut s = db.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = s[(s.len() as f32 * 0.15) as usize];
    let loud = s[((s.len() as f32 * 0.95) as usize).min(s.len() - 1)];
    match rel_db {
        Some(r) => loud + r.min(-3.0),
        None => (floor + 12.0).max(loud - 35.0).min(loud - 6.0),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Phrase {
    pub start_s: f32,
    pub end_s: f32,
}

/// Phrases: voiced stretches separated by pauses of at least `min_pause_s`
/// (hysteresis: on at the threshold, off 4 dB under it). Phrases shorter
/// than 120 ms are dropped (a click or a breath).
pub fn phrases(x: &[f32], min_pause_s: f32, rel_db: Option<f32>) -> Vec<Phrase> {
    let db = band_db(x);
    let on = voice_threshold(&db, rel_db);
    let off = on - 4.0;
    let mut v: Vec<(usize, usize)> = Vec::new();
    let mut cur: Option<usize> = None;
    let mut last_voiced = 0usize;
    let gap = (min_pause_s / HOP).round().max(1.0) as usize;
    for (i, &d) in db.iter().enumerate() {
        match cur {
            None => {
                if d >= on {
                    cur = Some(i);
                    last_voiced = i;
                }
            }
            Some(s) => {
                if d >= off {
                    last_voiced = i;
                } else if i - last_voiced >= gap {
                    v.push((s, last_voiced + 1));
                    cur = None;
                }
            }
        }
    }
    if let Some(s) = cur {
        v.push((s, last_voiced + 1));
    }
    let len_s = x.len() as f32 / SR;
    v.into_iter()
        .map(|(a, b)| Phrase { start_s: (a as f32 * HOP - 0.03).max(0.0), end_s: (b as f32 * HOP + 0.06).min(len_s) })
        .filter(|p| p.end_s - p.start_s >= 0.12)
        .collect()
}

/// Syllable onsets (seconds): rises of the voice-band level of at least
/// 6 dB over the 40 ms before, above the voice threshold, at least
/// `min_gap_s` apart.
pub fn syllable_onsets(x: &[f32], sensitivity: f32, min_gap_s: f32) -> Vec<f32> {
    let db = band_db(x);
    let thr = voice_threshold(&db, None);
    let rise = 9.0 - 6.0 * sensitivity.clamp(0.0, 1.0);
    let back = (0.04 / HOP) as usize;
    let gap = (min_gap_s / HOP).max(1.0) as usize;
    let mut out: Vec<usize> = Vec::new();
    let mut i = back;
    while i + 1 < db.len() {
        let prev_min = db[i - back..i].iter().cloned().fold(f32::INFINITY, f32::min);
        let local_peak = db[i] >= db[i - 1] && db[i] >= db[i + 1];
        if db[i] > thr && db[i] - prev_min >= rise && (local_peak || db[i + 1] - db[i] < 0.5) {
            // the onset is where the rise starts climbing (first hop 3 dB over the dip)
            let mut j = i;
            while j > i - back && db[j - 1] > prev_min + 3.0 {
                j -= 1;
            }
            if out.last().map(|&l| j >= l + gap).unwrap_or(true) {
                out.push(j);
                i += gap;
                continue;
            }
        }
        i += 1;
    }
    out.into_iter().map(|h| h as f32 * HOP).collect()
}

/// One onset's move: where it was and where it goes (seconds in the take).
#[derive(Clone, Debug, Serialize)]
pub struct Move {
    pub from_s: f32,
    pub to_s: f32,
    pub grid_beat: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct FlexResult {
    pub moves: Vec<Move>,
    pub mean_offset_ms_before: f32,
    pub mean_offset_ms_after: f32,
    pub on_grid_before: f32,
    pub on_grid_after: f32,
    pub min_stretch: f32,
    pub max_stretch: f32,
}

/// Distance (s) from time `t` (take seconds; take 0 = song beat `start_beat`)
/// to the nearest grid line, and that line's song beat.
pub fn nearest_grid(t: f32, bpm: f32, start_beat: f32, grid_beats: f32) -> (f32, f32) {
    let beat = start_beat + t * bpm / 60.0;
    let g = (beat / grid_beats).round() * grid_beats;
    let tg = (g - start_beat) * 60.0 / bpm;
    (tg - t, g)
}

/// Share of onsets within `tol_s` of a grid line, and their mean |offset| (ms).
pub fn grid_stats(onsets: &[f32], bpm: f32, start_beat: f32, grid_beats: f32, tol_s: f32) -> (f32, f32) {
    if onsets.is_empty() {
        return (0.0, 0.0);
    }
    let offs: Vec<f32> = onsets.iter().map(|&t| nearest_grid(t, bpm, start_beat, grid_beats).0.abs()).collect();
    let on = offs.iter().filter(|&&o| o <= tol_s).count() as f32 / offs.len() as f32;
    let mean = offs.iter().sum::<f32>() / offs.len() as f32 * 1000.0;
    ((on * 1000.0).round() / 10.0, (mean * 10.0).round() / 10.0)
}

/// Pull each anchor toward its grid line by `strength` (0..1), capped at
/// `max_shift_s`; anchors keep their order and every segment's stretch stays
/// within 0.5..2.0. Returns the warped take (same start; pitch kept).
pub fn flex(x: &[f32], anchors: &[f32], bpm: f32, start_beat: f32, grid_beats: f32, strength: f32, max_shift_s: f32) -> (Vec<f32>, FlexResult) {
    let len_s = x.len() as f32 / SR;
    let st = strength.clamp(0.0, 1.0);
    let mut moves: Vec<Move> = Vec::new();
    // pin the take's start and end
    let mut pts: Vec<(f32, f32)> = vec![(0.0, 0.0)];
    for &t in anchors {
        if t <= 0.02 || t >= len_s - 0.02 {
            continue;
        }
        let (d, g) = nearest_grid(t, bpm, start_beat, grid_beats);
        let shift = (d * st).clamp(-max_shift_s, max_shift_s);
        let to = t + shift;
        let (pi, po) = *pts.last().unwrap();
        let (din, dout) = (t - pi, to - po);
        if din < 0.03 || dout < 0.015 || dout / din > 2.0 || dout / din < 0.5 {
            continue; // too close to the last anchor to move cleanly; it rides along
        }
        pts.push((t, to));
        moves.push(Move { from_s: t, to_s: to, grid_beat: g });
    }
    let (li, lo) = *pts.last().unwrap();
    pts.push((len_s, lo + (len_s - li)));
    let out_len = pts.last().unwrap().1;
    let (mut mn, mut mx) = (1.0f32, 1.0f32);
    for w in pts.windows(2) {
        let r = (w[1].1 - w[0].1) / (w[1].0 - w[0].0).max(1e-6);
        mn = mn.min(r);
        mx = mx.max(r);
    }
    let map = |o: f32| -> f32 {
        for w in pts.windows(2) {
            if o <= w[1].1 {
                let f = (o - w[0].1) / (w[1].1 - w[0].1).max(1e-6);
                return w[0].0 + f * (w[1].0 - w[0].0);
            }
        }
        len_s
    };
    let y = if moves.is_empty() { x.to_vec() } else { crate::vocal::warp_audio(x, SR, out_len, map) };
    let tol = (grid_beats * 60.0 / bpm * 0.25).min(0.03);
    let (ob, mb) = grid_stats(anchors, bpm, start_beat, grid_beats, tol);
    let after: Vec<f32> = anchors
        .iter()
        .map(|&t| moves.iter().find(|m| (m.from_s - t).abs() < 1e-4).map(|m| m.to_s).unwrap_or_else(|| {
            // an anchor that rode along moves with its segment
            let mut v = t;
            for w in pts.windows(2) {
                if t <= w[1].0 {
                    let f = (t - w[0].0) / (w[1].0 - w[0].0).max(1e-6);
                    v = w[0].1 + f * (w[1].1 - w[0].1);
                    break;
                }
            }
            v
        }))
        .collect();
    let (oa, ma) = grid_stats(&after, bpm, start_beat, grid_beats, tol);
    (y, FlexResult { moves, mean_offset_ms_before: mb, mean_offset_ms_after: ma, on_grid_before: ob, on_grid_after: oa, min_stretch: (mn * 1000.0).round() / 1000.0, max_stretch: (mx * 1000.0).round() / 1000.0 })
}

/// Where each phrase goes when whole phrases are placed on the grid: each
/// starts on the grid line nearest its natural time (gaps kept), but never
/// before the previous phrase has ended. Returns song beats.
pub fn place_phrases(ph: &[Phrase], bpm: f32, start_beat: f32, grid_beats: f32, keep_gaps: bool) -> Vec<f32> {
    let bps = bpm / 60.0;
    let mut out = Vec::new();
    let mut free = f32::NEG_INFINITY;
    let mut drift = 0.0f32;
    for p in ph {
        let natural = start_beat + p.start_s * bps + drift;
        let want = if keep_gaps { natural } else { free.max(start_beat) };
        let mut g = (want / grid_beats).round() * grid_beats;
        while g < free - 1e-4 {
            g += grid_beats;
        }
        if keep_gaps {
            drift = g - (start_beat + p.start_s * bps);
        }
        out.push(g);
        free = g + (p.end_s - p.start_s) * bps;
    }
    out
}

fn take(e: &mut Engine, a: &Value) -> Result<(String, Vec<f32>)> {
    if s_opt(a, "sample").is_some() || s_opt(a, "path").is_some() {
        let (name, l, r) = crate::tools_sound::audio_for(e, a)?;
        let m: Vec<f32> = l.iter().zip(r.iter()).map(|(x, y)| 0.5 * (x + y)).collect();
        return Ok((name, m));
    }
    bail!("give the vocal: sample (a registered sample) or path (a wav/mp3 file)")
}

fn grid_of(a: &Value, default: &str) -> Result<f32> {
    let v = a.get("grid").cloned().unwrap_or_else(|| json!(default));
    if let Some(s) = v.as_str() {
        match s {
            "beat" => return Ok(1.0),
            "half_bar" => return Ok(2.0),
            "bar" => return Ok(4.0),
            _ => {}
        }
    }
    let g = crate::tools_studio::parse_rate(Some(&v))?;
    if !(0.05..=16.0).contains(&g) {
        bail!("grid {v} is out of range (1/32 .. 4 bars)");
    }
    Ok(g)
}

fn r3(x: f32) -> f32 {
    (x * 1000.0).round() / 1000.0
}

fn register(e: &mut Engine, name: &str, y: &[f32], source: String) -> Result<Value> {
    let name = crate::samples::sample_name(name);
    std::fs::create_dir_all(e.samples_dir())?;
    let path = e.samples_dir().join(format!("{name}.wav"));
    crate::render::write_wav(&path, y, y)?;
    let info = crate::samples::SampleInfo { name, path: path.to_string_lossy().into(), source, license: "own recording".into(), author: String::new(), duration: 0.0 };
    crate::tools::register_sample(e, info)
}

fn split_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (name, x) = take(e, a)?;
    let ph = phrases(&x, f_or(a, "min_pause_s", 0.3).clamp(0.08, 3.0), f_opt(a, "threshold_db"));
    if ph.is_empty() {
        bail!("no voiced phrase found (try threshold_db -45 or a shorter min_pause_s)");
    }
    let prefix = s_opt(a, "prefix").unwrap_or_else(|| format!("{name}_ph"));
    let save = b_or(a, "save", true);
    let mut out = Vec::new();
    for (i, p) in ph.iter().enumerate() {
        let (s0, s1) = ((p.start_s * SR) as usize, ((p.end_s * SR) as usize).min(x.len()));
        let mut seg = x[s0..s1].to_vec();
        crate::audio_edit::fade(&mut seg, 5.0, 20.0);
        let mut row = json!({"index": i, "start_s": r3(p.start_s), "end_s": r3(p.end_s), "seconds": r3(p.end_s - p.start_s)});
        if save {
            let reg = register(e, &format!("{prefix}{:02}", i + 1), &seg, format!("phrase {} of {name} ({:.2}-{:.2} s)", i + 1, p.start_s, p.end_s))?;
            row["sample"] = reg["sample"].clone();
        }
        out.push(row);
    }
    let pauses: Vec<Value> = ph.windows(2).map(|w| json!({"from_s": r3(w[0].end_s), "to_s": r3(w[1].start_s)})).collect();
    if save {
        e.revision += 1;
    }
    Ok(json!({"source": name, "phrases": out, "pauses": pauses, "count": ph.len(), "next": ["add_audio_clip each phrase where you want it", "place_vocal_phrases to put them all on the grid in one go"]}))
}

fn onsets_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (name, x) = take(e, a)?;
    let on = syllable_onsets(&x, f_or(a, "sensitivity", 0.5), f_or(a, "min_gap_ms", 90.0) / 1000.0);
    let ph = phrases(&x, f_or(a, "min_pause_s", 0.3), None);
    let bpm = f_opt(a, "bpm").unwrap_or(e.project.bpm);
    let sb = f_or(a, "start_beat", 0.0);
    let gb = grid_of(a, "1/16")?;
    let tol = (gb * 60.0 / bpm * 0.25).min(0.03);
    let (on_grid, mean_ms) = grid_stats(&on, bpm, sb, gb, tol);
    let max = u_or(a, "max", 400) as usize;
    Ok(json!({
        "source": name,
        "seconds": r3(x.len() as f32 / SR),
        "onsets_s": on.iter().take(max).map(|t| r3(*t)).collect::<Vec<_>>(),
        "onset_count": on.len(),
        "syllables_per_s": r3(on.len() as f32 / (x.len() as f32 / SR).max(0.1)),
        "phrases": ph,
        "timing": {"bpm": bpm, "start_beat": sb, "grid_beats": gb, "on_grid_pct": on_grid, "mean_offset_ms": mean_ms, "tolerance_ms": r3(tol * 1000.0)},
    }))
}

fn align_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let sample = s_req(a, "sample")?;
    let (info, x) = crate::tools_sound::sample_data(e, &sample)?;
    let bpm = f_opt(a, "bpm").unwrap_or(e.project.bpm);
    let sb = f_or(a, "start_beat", 0.0);
    let gb = grid_of(a, "1/16")?;
    let strength = f_or(a, "strength", 0.7);
    let max_shift = f_or(a, "max_shift_ms", 120.0).clamp(5.0, 400.0) / 1000.0;
    let unit = s_opt(a, "unit").unwrap_or_else(|| "syllable".into());
    let anchors: Vec<f32> = match unit.as_str() {
        "phrase" => phrases(&x, f_or(a, "min_pause_s", 0.3), None).iter().map(|p| p.start_s + 0.03).collect(),
        "syllable" | "word" => syllable_onsets(&x, f_or(a, "sensitivity", 0.5), 0.09),
        u => bail!("unit '{u}': use syllable or phrase"),
    };
    if anchors.is_empty() {
        bail!("no onsets found in '{}'", info.name);
    }
    let (y, res) = flex(&x, &anchors, bpm, sb, gb, strength, max_shift);
    let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_flex", info.name));
    let reg = register(e, &nn, &y, format!("{} (flex: {unit} onsets to {gb} beats, strength {strength})", info.source))?;
    let new = reg["sample"].as_str().unwrap_or(&nn).to_string();
    // clips that played the old take now play the aligned one
    let mut switched = 0;
    if b_or(a, "replace_clips", true) {
        for c in e.project.audio_clips.iter_mut() {
            if c.sample.eq_ignore_ascii_case(&info.name) && (c.start_beat - sb).abs() < 1e-3 && c.offset_s.abs() < 1e-6 {
                c.sample = new.clone();
                switched += 1;
            }
        }
    }
    e.revision += 1;
    Ok(json!({
        "sample": new,
        "unit": unit,
        "grid_beats": gb,
        "strength": strength,
        "moved": res.moves.len(),
        "anchors": anchors.len(),
        "on_grid_pct": {"before": res.on_grid_before, "after": res.on_grid_after},
        "mean_offset_ms": {"before": res.mean_offset_ms_before, "after": res.mean_offset_ms_after},
        "stretch_range": [res.min_stretch, res.max_stretch],
        "clips_switched": switched,
        "moves": res.moves.iter().take(u_or(a, "max_moves", 24) as usize).map(|m| json!({"from_s": r3(m.from_s), "to_s": r3(m.to_s), "grid_beat": m.grid_beat})).collect::<Vec<_>>(),
        "note": "take time 0 sits at song beat start_beat; pitch is kept (WSOLA)",
    }))
}

fn place_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let sample = s_req(a, "sample")?;
    let (info, x) = crate::tools_sound::sample_data(e, &sample)?;
    let ti = e.project.track_index(&s_req(a, "track")?)?;
    let track = e.project.tracks[ti].name.clone();
    let bpm = e.project.bpm;
    let sb = f_or(a, "start_beat", 0.0);
    let gb = grid_of(a, "beat")?;
    let mut ph = phrases(&x, f_or(a, "min_pause_s", 0.3), f_opt(a, "threshold_db"));
    if let (Some(f), Some(t)) = (f_opt(a, "from_s"), f_opt(a, "to_s")) {
        ph.retain(|p| p.start_s >= f - 1e-3 && p.end_s <= t + 0.1);
    }
    if ph.is_empty() {
        bail!("no voiced phrase found in '{}'", info.name);
    }
    let keep = b_or(a, "keep_gaps", true);
    let at = place_phrases(&ph, bpm, sb, gb, keep);
    if b_or(a, "replace", false) {
        e.project.audio_clips.retain(|c| !(c.track == track && c.sample.eq_ignore_ascii_case(&info.name)));
    }
    let gain = f_or(a, "gain_db", 0.0);
    let mut placed = Vec::new();
    for (p, &beat) in ph.iter().zip(at.iter()) {
        e.project.audio_clips.push(AudioClip { track: track.clone(), sample: info.name.clone(), start_beat: beat, offset_s: p.start_s, length_s: Some(p.end_s - p.start_s), gain_db: gain });
        let natural = sb + p.start_s * bpm / 60.0;
        placed.push(json!({"from_s": r3(p.start_s), "to_s": r3(p.end_s), "beat": r3(beat), "bar": (beat / 4.0).floor() as i64 + 1, "moved_ms": r3((beat - natural) * 60.0 / bpm * 1000.0)}));
    }
    e.revision += 1;
    Ok(json!({"track": track, "sample": info.name, "grid_beats": gb, "keep_gaps": keep, "phrases": placed.len(), "placed": placed, "clips": e.project.audio_clips.len(), "next": ["align_vocal_to_grid for word-level tightening", "measure_vocal_vs_beat to check level and timing"]}))
}

fn mono(l: &[f32], r: &[f32]) -> Vec<f32> {
    l.iter().zip(r.iter()).map(|(x, y)| 0.5 * (x + y)).collect()
}

fn band_rms_db(x: &[f32], lo: f32, hi: f32) -> f32 {
    let mut h = Biquad::new(BiquadKind::LowCut, lo, 0.707, 0.0);
    let mut l = Biquad::new(BiquadKind::HighCut, hi, 0.707, 0.0);
    let e: f64 = x.iter().map(|v| { let y = l.process(h.process(*v)); (y * y) as f64 }).sum::<f64>() / x.len().max(1) as f64;
    (10.0 * e.max(1e-12).log10()) as f32
}

fn measure_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let tname = s_opt(a, "track").unwrap_or_else(|| "vocal".into());
    let vi = e.project.track_index(&tname)?;
    let full = e.mix()?;
    let mut solo = e.project.clone();
    for (i, t) in solo.tracks.iter_mut().enumerate() {
        t.mute = i != vi;
        t.solo = false;
    }
    let voc = e.render_version(&solo)?;
    let n = full.left.len().min(voc.left.len());
    let bpm = e.project.bpm;
    let bs = 60.0 / bpm;
    let s0 = ((f_or(a, "from_beat", 0.0) * bs * SR) as usize).min(n);
    let s1 = f_opt(a, "to_beat").map(|b| ((b * bs * SR) as usize).min(n)).unwrap_or(n);
    if s1 <= s0 + (SR * 0.5) as usize {
        bail!("the window is shorter than half a second");
    }
    let v = mono(&voc.left[s0..s1], &voc.right[s0..s1]);
    let m = mono(&full.left[s0..s1], &full.right[s0..s1]);
    // the beat as heard under the vocal (ducking included): the mix minus the vocal
    let beat: Vec<f32> = m.iter().zip(v.iter()).map(|(x, y)| x - y).collect();
    // only where the vocal is actually singing
    let db = band_db(&v);
    let thr = voice_threshold(&db, None);
    let hop = (HOP * SR) as usize;
    let mask = |x: &[f32]| -> Vec<f32> {
        let mut o = Vec::new();
        for (i, d) in db.iter().enumerate() {
            if *d >= thr {
                let a0 = i * hop;
                let a1 = (a0 + hop).min(x.len());
                if a0 < a1 {
                    o.extend_from_slice(&x[a0..a1]);
                }
            }
        }
        o
    };
    let (vm, bm) = (mask(&v), mask(&beat));
    if vm.len() < (SR * 0.3) as usize {
        bail!("the '{tname}' track is silent in this window");
    }
    let full_db = band_rms_db(&vm, 40.0, 16000.0) - band_rms_db(&bm, 40.0, 16000.0);
    let pres_db = band_rms_db(&vm, 1000.0, 4000.0) - band_rms_db(&bm, 1000.0, 4000.0);
    let body_db = band_rms_db(&vm, 200.0, 800.0) - band_rms_db(&bm, 200.0, 800.0);
    let gb = grid_of(a, "1/16")?;
    let on: Vec<f32> = syllable_onsets(&v, 0.5, 0.09);
    let start_beat = s0 as f32 / SR / bs;
    let tol = (gb * bs * 0.25).min(0.03);
    let (on_grid, mean_ms) = grid_stats(&on, bpm, start_beat, gb, tol);
    // signed offsets: early (-) or late (+) against the nearest grid line
    let mut signed: Vec<f32> = on.iter().map(|&t| -nearest_grid(t, bpm, start_beat, gb).0 * 1000.0).collect();
    signed.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let median = signed.get(signed.len() / 2).copied().unwrap_or(0.0);
    let mut verdict = Vec::new();
    if pres_db < 3.0 {
        verdict.push(format!("the vocal is only {pres_db:+.1} dB over the beat at 1-4 kHz: words will be hard to hear (aim for +6 or more)"));
    }
    if full_db < -2.0 {
        verdict.push(format!("the vocal is {full_db:+.1} dB under the beat overall"));
    }
    if on_grid < 50.0 {
        verdict.push(format!("only {on_grid}% of syllables land within {:.0} ms of the grid: try align_vocal_to_grid or place_vocal_phrases", tol * 1000.0));
    }
    if median.abs() > 15.0 {
        verdict.push(format!("the vocal sits {} by about {:.0} ms (median)", if median > 0.0 { "late" } else { "early" }, median.abs()));
    }
    if verdict.is_empty() {
        verdict.push("the vocal sits over the beat and on the grid".into());
    }
    Ok(json!({
        "track": tname,
        "window_s": [r3(s0 as f32 / SR), r3(s1 as f32 / SR)],
        "vocal_over_beat_db": {"full_band": (full_db * 10.0).round() / 10.0, "presence_1_4k": (pres_db * 10.0).round() / 10.0, "body_200_800": (body_db * 10.0).round() / 10.0},
        "timing": {"grid_beats": gb, "syllables": on.len(), "on_grid_pct": on_grid, "mean_offset_ms": mean_ms, "median_signed_ms": (median * 10.0).round() / 10.0, "tolerance_ms": r3(tol * 1000.0)},
        "verdict": verdict,
        "note": "levels are measured only where the vocal sounds; the beat is the mix minus the vocal, so sidechain ducking counts",
    }))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "detect_vocal_onsets",
            description: "Find a vocal take's syllable onsets and phrases (voiced stretches split at pauses) from a sample or a file, and how close the onsets sit to the song grid (on_grid_pct, mean offset). start_beat = the song beat where the take's time 0 sits. Read-only.",
            mutates: false,
            schema: || obj(json!({"sample": {"type": "string"}, "path": {"type": "string"}, "sensitivity": {"type": "number", "description": "0..1 (default 0.5); higher finds more onsets"}, "min_gap_ms": {"type": "number"}, "min_pause_s": {"type": "number", "description": "pause that ends a phrase (default 0.3)"}, "bpm": {"type": "number"}, "start_beat": {"type": "number"}, "grid": {"description": "grid to measure against (default '1/16')"}, "max": {"type": "integer"}}), &[]),
            run: onsets_tool,
        },
        Tool {
            name: "split_vocal_at_pauses",
            description: "Split a vocal take at its pauses into one sample per phrase (<prefix>01, 02, ...), with each phrase's start/end in the take. min_pause_s (default 0.3) is the shortest gap that splits; threshold_db (e.g. -30) sets the voice level against the loudest parts (default: from the noise floor). save:false only lists them.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "path": {"type": "string"}, "min_pause_s": {"type": "number"}, "threshold_db": {"type": "number"}, "prefix": {"type": "string"}, "save": {"type": "boolean"}}), &[]),
            run: split_tool,
        },
        Tool {
            name: "align_vocal_to_grid",
            description: "Flex time / elastic audio: pull a vocal's onsets onto the song grid, pitch kept. unit 'syllable' (default) moves every syllable onset, 'phrase' moves only phrase starts (the flow inside a phrase stays). strength 0..1 (default 0.7; 1 = hard quantize, 0.3 = a light tighten), max_shift_ms caps any move (default 120), grid default '1/16' (also '1/8', '1/8t', 'beat'). start_beat = the song beat where the take starts. Writes <sample>_flex and switches clips that played the take from start_beat. Reports on-grid share and mean offset before/after.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "start_beat": {"type": "number"}, "bpm": {"type": "number"}, "grid": {"description": "default '1/16'"}, "strength": {"type": "number"}, "unit": {"type": "string", "enum": ["syllable", "phrase"]}, "max_shift_ms": {"type": "number"}, "sensitivity": {"type": "number"}, "min_pause_s": {"type": "number"}, "new_name": {"type": "string"}, "replace_clips": {"type": "boolean"}, "max_moves": {"type": "integer"}}), &["sample"]),
            run: align_tool,
        },
        Tool {
            name: "place_vocal_phrases",
            description: "Beat-synced phrase placement: split a take at its pauses and put each phrase on a track as an audio clip starting on a grid line (grid 'beat' default, 'half_bar', 'bar', or '1/8'...). The words inside each phrase keep their natural flow. keep_gaps (default true) keeps the pauses close to the original, false packs phrases back to back. from_s/to_s limit it to part of the take; replace removes this sample's old clips on the track first.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "track": {"type": "string"}, "start_beat": {"type": "number"}, "grid": {"description": "default 'beat'"}, "keep_gaps": {"type": "boolean"}, "min_pause_s": {"type": "number"}, "threshold_db": {"type": "number"}, "from_s": {"type": "number"}, "to_s": {"type": "number"}, "gain_db": {"type": "number"}, "replace": {"type": "boolean"}}), &["sample", "track"]),
            run: place_tool,
        },
        Tool {
            name: "measure_vocal_vs_beat",
            description: "How a vocal track sits in the song: its level over the beat where it sounds (full band, 1-4 kHz presence, 200-800 Hz body; the beat is the mix minus the vocal, so ducking counts) and its timing against the grid (on-grid share, mean and median early/late offset), with plain-word findings. track default 'vocal'; from_beat/to_beat set a window.",
            mutates: false,
            schema: || obj(json!({"track": {"type": "string"}, "from_beat": {"type": "number"}, "to_beat": {"type": "number"}, "grid": {"description": "default '1/16'"}}), &[]),
            run: measure_tool,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic "rap take": 120 ms voiced bursts (300 Hz buzz) at given times.
    fn take_at(times: &[f32], len_s: f32) -> Vec<f32> {
        let mut x = vec![0.0f32; (len_s * SR) as usize];
        let mut rng = crate::dsp::Rng::new(3);
        for v in x.iter_mut() {
            *v = 0.0005 * rng.bipolar();
        }
        for &t in times {
            let s = (t * SR) as usize;
            for i in 0..(0.12 * SR) as usize {
                if s + i < x.len() {
                    let ph = i as f32 / SR;
                    let env = (i as f32 / (0.005 * SR)).min(1.0) * (1.0 - i as f32 / (0.12 * SR));
                    x[s + i] += 0.4 * env * ((ph * 300.0 * std::f32::consts::TAU).sin() + 0.5 * (ph * 900.0 * std::f32::consts::TAU).sin());
                }
            }
        }
        x
    }

    #[test]
    fn onsets_and_phrases_are_found() {
        let times = [0.5, 0.8, 1.1, 2.5, 2.8];
        let x = take_at(&times, 3.5);
        let on = syllable_onsets(&x, 0.5, 0.09);
        assert_eq!(on.len(), 5, "{on:?}");
        for (a, b) in on.iter().zip(times.iter()) {
            assert!((a - b).abs() < 0.02, "onset {a} vs {b}");
        }
        let ph = phrases(&x, 0.3, None);
        assert_eq!(ph.len(), 2, "{ph:?}");
        assert!(ph[0].start_s < 0.5 && ph[0].end_s > 1.2 && ph[0].end_s < 1.5);
        assert!(ph[1].start_s > 2.3 && ph[1].start_s < 2.5);
    }

    #[test]
    fn flex_pulls_onsets_onto_the_grid() {
        // 120 bpm: 16ths every 0.125 s. Onsets 40 ms late.
        let times = [0.54, 1.04, 1.29, 1.79, 2.29];
        let x = take_at(&times, 3.0);
        let on = syllable_onsets(&x, 0.5, 0.09);
        let (y, r) = flex(&x, &on, 120.0, 0.0, 0.25, 1.0, 0.12);
        assert!(r.mean_offset_ms_before > 25.0, "{r:?}");
        assert!(r.mean_offset_ms_after < 8.0, "{r:?}");
        let on2 = syllable_onsets(&y, 0.5, 0.09);
        let (pct, mean) = grid_stats(&on2, 120.0, 0.0, 0.25, 0.02);
        assert!(pct >= 80.0 && mean < 15.0, "after: {on2:?} {pct} {mean}");
        // strength 0 leaves it alone
        let (_, r0) = flex(&x, &on, 120.0, 0.0, 0.25, 0.0, 0.12);
        assert!((r0.mean_offset_ms_after - r0.mean_offset_ms_before).abs() < 1.0);
    }

    #[test]
    fn phrases_land_on_beats_with_their_gaps() {
        let ph = vec![Phrase { start_s: 0.13, end_s: 1.2 }, Phrase { start_s: 1.9, end_s: 2.6 }];
        let at = place_phrases(&ph, 120.0, 4.0, 1.0, true);
        assert_eq!(at, vec![4.0, 8.0]);
        let packed = place_phrases(&ph, 120.0, 4.0, 1.0, false);
        assert_eq!(packed[0], 4.0);
        assert!(packed[1] >= 4.0 + 1.07 * 2.0 && packed[1] <= 7.0, "{packed:?}");
    }

    #[test]
    fn tools_place_and_measure_a_take() {
        let dir = std::env::temp_dir().join("beatbox_vocal_flex_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("new_project", &json!({"name": "flex", "bpm": 120})).unwrap();
        let x = take_at(&[0.53, 0.83, 1.13, 2.53, 2.83], 3.5);
        let wav = dir.join("take.wav");
        crate::render::write_wav(&wav, &x, &x).unwrap();
        e.call("import_sample", &json!({"path": wav.to_string_lossy(), "name": "take"})).unwrap();
        let s = e.call("split_vocal_at_pauses", &json!({"sample": "take"})).unwrap();
        assert_eq!(s["count"], 2, "{s}");
        e.call("add_track", &json!({"name": "vocal", "preset": "pad"})).unwrap();
        let p = e.call("place_vocal_phrases", &json!({"sample": "take", "track": "vocal", "start_beat": 0})).unwrap();
        assert_eq!(p["phrases"], 2, "{p}");
        let a = e.call("align_vocal_to_grid", &json!({"sample": "take", "strength": 1.0})).unwrap();
        assert!(a["mean_offset_ms"]["after"].as_f64().unwrap() <= a["mean_offset_ms"]["before"].as_f64().unwrap(), "{a}");
        let m = e.call("measure_vocal_vs_beat", &json!({"track": "vocal"})).unwrap();
        assert!(m["timing"]["syllables"].as_u64().unwrap() >= 3, "{m}");
    }
}
