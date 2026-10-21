//! Native timeline editing on audio clips, addressed by stable id (or index):
//! split, trim, move, duplicate, slip, fade, crossfade, consolidate and
//! ripple delete. Every edit is one undo step and keeps clip ids: a split
//! keeps the id on the left part and gives the right part a new one, a
//! duplicate gets new ids, consolidate replaces its sources with one new
//! clip. Times are song beats (tempo changes respected) or seconds of
//! source audio where noted. The studio's clip menu calls these same tools.

use crate::dsp::SR;
use crate::engine::Engine;
use crate::project::{audio_clip_index, AudioClip, Project};
use crate::samples::SampleInfo;
use crate::tempo_curve::TempoCurve;
use crate::tools::{b_or, f_opt, obj, register_sample, s_opt, u_or, Tool};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

/// Shortest clip an edit may leave (seconds).
const MIN_LEN_S: f32 = 0.01;
/// Edge fade a split puts on each new cut, so the cut does not click (ms).
const CUT_FADE_MS: f32 = 5.0;

fn sample_secs(e: &mut Engine, name: &str) -> Result<f32> {
    e.bank.sync(&e.project.samples);
    e.bank
        .get(name)
        .map(|d| d.len() as f32 / SR)
        .ok_or_else(|| anyhow!("sample '{name}' is not loaded (list_samples)"))
}

/// Seconds of source audio clip `i` plays, and the sample's total length.
fn clip_len(e: &mut Engine, i: usize) -> Result<(f32, f32)> {
    let c = e.project.audio_clips[i].clone();
    let total = sample_secs(e, &c.sample)?;
    let avail = (total - c.offset_s.max(0.0)).max(0.0);
    Ok((
        c.length_s.map(|l| l.clamp(0.0, avail)).unwrap_or(avail),
        total,
    ))
}

fn secs_at(p: &Project, beat: f32) -> f64 {
    let t = TempoCurve::new(p);
    if beat < 0.0 {
        return beat as f64 * 60.0 / p.bpm as f64;
    }
    t.secs_at_beat(beat as f64)
}

fn beat_at(p: &Project, secs: f64) -> f32 {
    if secs < 0.0 {
        return (secs * p.bpm as f64 / 60.0) as f32;
    }
    TempoCurve::new(p).beat_at_secs(secs) as f32
}

/// Song beat where clip `i` ends.
fn end_beat(p: &Project, c: &AudioClip, len_s: f32) -> f32 {
    beat_at(p, secs_at(p, c.start_beat) + len_s as f64)
}

fn clip_json(e: &mut Engine, i: usize) -> Value {
    let (len, _) = clip_len(e, i).unwrap_or((0.0, 0.0));
    let c = &e.project.audio_clips[i];
    let end = end_beat(&e.project, c, len * c.stretch);
    json!({"id": c.id, "index": i, "track": c.track, "sample": c.sample, "start_beat": c.start_beat,
        "end_beat": (end * 1000.0).round() / 1000.0, "offset_s": c.offset_s, "length_s": (len * 1000.0).round() / 1000.0,
        "gain_db": c.gain_db, "pitch_st": c.pitch_st, "pitch_voice": c.pitch_voice, "stretch": c.stretch, "fade_in_ms": c.fade_in_ms, "fade_out_ms": c.fade_out_ms})
}

fn id_prop() -> Value {
    json!({"type": "string", "description": "the clip's stable id (\"clip3\", list_audio_clips); an index also works"})
}

/// Split clip `i` at song beat `at`: the left part keeps the id, the right
/// part (returned) gets a new one.
pub fn split(e: &mut Engine, i: usize, at: f32) -> Result<(String, String)> {
    let (len, _) = clip_len(e, i)?;
    let c = e.project.audio_clips[i].clone();
    let t = (secs_at(&e.project, at) - secs_at(&e.project, c.start_beat)) as f32;
    if t < MIN_LEN_S || t > len - MIN_LEN_S {
        bail!(
            "beat {at} is not inside clip '{}' (beats {} to {:.3})",
            c.id,
            c.start_beat,
            end_beat(&e.project, &c, len)
        );
    }
    let rid = crate::ids::next_id(&mut e.project, "clip");
    let mut right = c.clone();
    right.id = rid.clone();
    right.start_beat = at;
    right.offset_s = c.offset_s.max(0.0) + t;
    right.length_s = Some(len - t);
    right.fade_in_ms = Some(CUT_FADE_MS);
    let left = &mut e.project.audio_clips[i];
    left.length_s = Some(t);
    left.fade_out_ms = Some(CUT_FADE_MS);
    e.project.audio_clips.insert(i + 1, right);
    Ok((c.id, rid))
}

/// Trim the clip so it starts at `new_start` (song beat), keeping the audio
/// where it was on the timeline.
fn trim_start(e: &mut Engine, i: usize, new_start: f32) -> Result<()> {
    let (len, _) = clip_len(e, i)?;
    let c = e.project.audio_clips[i].clone();
    let d = (secs_at(&e.project, new_start) - secs_at(&e.project, c.start_beat)) as f32;
    let d = d.max(-c.offset_s.max(0.0));
    if d > len - MIN_LEN_S {
        bail!(
            "trimming clip '{}' to start at beat {new_start} would leave nothing",
            c.id
        );
    }
    let start_s = secs_at(&e.project, c.start_beat) + d as f64;
    let start = beat_at(&e.project, start_s);
    let cl = &mut e.project.audio_clips[i];
    cl.offset_s = c.offset_s.max(0.0) + d;
    cl.length_s = Some(len - d);
    cl.start_beat = start;
    Ok(())
}

fn trim_end(e: &mut Engine, i: usize, new_end: f32) -> Result<()> {
    let (_, total) = clip_len(e, i)?;
    let c = e.project.audio_clips[i].clone();
    let want = (secs_at(&e.project, new_end) - secs_at(&e.project, c.start_beat)) as f32;
    let avail = (total - c.offset_s.max(0.0)).max(0.0);
    if want < MIN_LEN_S {
        bail!(
            "beat {new_end} is before clip '{}' starts (beat {})",
            c.id,
            c.start_beat
        );
    }
    e.project.audio_clips[i].length_s = Some(want.min(avail));
    Ok(())
}

/// Indices of the clips a range edit touches on the chosen tracks.
fn clips_on(p: &Project, tracks: &Option<Vec<String>>) -> Vec<usize> {
    (0..p.audio_clips.len())
        .filter(|&i| {
            tracks.as_ref().is_none_or(|t| {
                t.iter()
                    .any(|n| n.eq_ignore_ascii_case(&p.audio_clips[i].track))
            })
        })
        .collect()
}

/// Remove [from, to) (song beats) from the chosen tracks' clips and pull
/// everything after it earlier by the gap. Returns (removed, trimmed, moved).
fn ripple(
    e: &mut Engine,
    tracks: Option<Vec<String>>,
    from: f32,
    to: f32,
) -> Result<(usize, usize, usize)> {
    if to <= from {
        bail!("ripple range is empty (from {from} to {to})");
    }
    let gap_beats = to - from;
    let (mut removed, mut trimmed, mut moved) = (0, 0, 0);
    // split clips that straddle a range edge, so every clip is in or out
    for edge in [from, to] {
        let mut i = 0;
        while i < e.project.audio_clips.len() {
            if clips_on(&e.project, &tracks).contains(&i) {
                let (len, _) = clip_len(e, i)?;
                let c = e.project.audio_clips[i].clone();
                let end = end_beat(&e.project, &c, len);
                if c.start_beat < edge - 1e-4 && end > edge + 1e-4 {
                    split(e, i, edge)?;
                    trimmed += 1;
                }
            }
            i += 1;
        }
    }
    let mut keep = Vec::new();
    for (i, c) in e.project.audio_clips.clone().into_iter().enumerate() {
        let on = clips_on(&e.project, &tracks).contains(&i);
        if on && c.start_beat >= from - 1e-4 && c.start_beat < to - 1e-4 {
            removed += 1;
            continue;
        }
        let mut c = c;
        if on && c.start_beat >= to - 1e-4 {
            c.start_beat -= gap_beats;
            moved += 1;
        }
        keep.push(c);
    }
    e.project.audio_clips = keep;
    Ok((removed, trimmed, moved))
}

fn track_filter(e: &Engine, a: &Value) -> Result<Option<Vec<String>>> {
    if b_or(a, "all_tracks", false) {
        return Ok(None);
    }
    match s_opt(a, "track") {
        Some(t) => {
            let ti = e.project.track_index(&t)?;
            Ok(Some(vec![e.project.tracks[ti].name.clone()]))
        }
        None => Ok(None),
    }
}

/// Mix the given clips into one mono buffer from song beat `from` to `to`
/// with their gains and fades, the way the renderer plays them.
fn mixdown(e: &mut Engine, idx: &[usize], from: f32, to: f32) -> Result<Vec<f32>> {
    let t0 = secs_at(&e.project, from);
    let n = (((secs_at(&e.project, to) - t0) * SR as f64).round() as usize).max(1);
    let mut out = vec![0.0f32; n];
    for &i in idx {
        let (len_s, _) = clip_len(e, i)?;
        let c = e.project.audio_clips[i].clone();
        let data = e
            .bank
            .get(&c.sample)
            .ok_or_else(|| anyhow!("sample '{}' not loaded", c.sample))?;
        let off = ((c.offset_s.max(0.0) * SR) as usize).min(data.len());
        let len = ((len_s * SR) as usize).min(data.len() - off);
        let g = crate::dsp::db_to_gain(c.gain_db);
        let start = ((secs_at(&e.project, c.start_beat) - t0) * SR as f64).round() as i64;
        let fi = (c.fade_in_ms.unwrap_or(4.0).max(0.5) / 1000.0 * SR).max(1.0);
        let fo = (c.fade_out_ms.unwrap_or(4.0).max(0.5) / 1000.0 * SR).max(1.0);
        for k in 0..len {
            let o = start + k as i64;
            if o < 0 {
                continue;
            }
            let o = o as usize;
            if o >= n {
                break;
            }
            let env = ((k as f32 / fi).min(1.0) * std::f32::consts::FRAC_PI_2).sin()
                * (((len - k) as f32 / fo).min(1.0) * std::f32::consts::FRAC_PI_2).sin();
            out[o] += data[off + k] * g * env;
        }
    }
    Ok(out)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "split_clip",
            description: "Split an audio clip at a song beat into two clips. The left part keeps the clip's id, the right part gets a new id (returned); each new cut gets a 5 ms fade so it does not click.",
            mutates: true,
            schema: || obj(json!({"id": id_prop(), "at_beat": {"type": "number"}}), &["id", "at_beat"]),
            run: |e, a| {
                let i = audio_clip_index(&e.project, a)?;
                let at = f_opt(a, "at_beat").ok_or_else(|| anyhow!("missing at_beat"))?;
                let (l, r) = split(e, i, at)?;
                Ok(json!({"left": clip_json(e, i), "right": clip_json(e, i + 1), "left_id": l, "right_id": r}))
            },
        },
        Tool {
            name: "trim_clip",
            description: "Trim an audio clip's edges without moving its audio on the timeline: start_beat / end_beat set the new edges in song beats (trim_start_s / trim_end_s cut seconds off instead). Extending an edge reveals more of the take, up to the sample's ends.",
            mutates: true,
            schema: || obj(json!({"id": id_prop(), "start_beat": {"type": "number"}, "end_beat": {"type": "number"},
                "trim_start_s": {"type": "number"}, "trim_end_s": {"type": "number"}}), &["id"]),
            run: |e, a| {
                let i = audio_clip_index(&e.project, a)?;
                let mut did = false;
                if let Some(s) = f_opt(a, "trim_start_s") {
                    let c = e.project.audio_clips[i].clone();
                    let b = beat_at(&e.project, secs_at(&e.project, c.start_beat) + s as f64);
                    trim_start(e, i, b)?;
                    did = true;
                }
                if let Some(s) = f_opt(a, "trim_end_s") {
                    let (len, _) = clip_len(e, i)?;
                    if s >= len - MIN_LEN_S {
                        bail!("trim_end_s {s} would leave nothing of a {len:.3} s clip");
                    }
                    e.project.audio_clips[i].length_s = Some(len - s.max(0.0));
                    did = true;
                }
                if let Some(b) = f_opt(a, "start_beat") {
                    trim_start(e, i, b)?;
                    did = true;
                }
                if let Some(b) = f_opt(a, "end_beat") {
                    trim_end(e, i, b)?;
                    did = true;
                }
                if !did {
                    bail!("give start_beat, end_beat, trim_start_s or trim_end_s");
                }
                Ok(clip_json(e, i))
            },
        },
        Tool {
            name: "move_clip",
            description: "Move an audio clip on the timeline: start_beat (absolute) or by_beats (relative), and/or onto another track's channel.",
            mutates: true,
            schema: || obj(json!({"id": id_prop(), "start_beat": {"type": "number"}, "by_beats": {"type": "number"}, "track": {"type": "string", "description": "move onto this track (name or id)"}}), &["id"]),
            run: |e, a| {
                let i = audio_clip_index(&e.project, a)?;
                if let Some(t) = s_opt(a, "track") {
                    let ti = e.project.track_index(&t)?;
                    e.project.audio_clips[i].track = e.project.tracks[ti].name.clone();
                }
                if let Some(b) = f_opt(a, "start_beat") {
                    e.project.audio_clips[i].start_beat = b;
                }
                if let Some(d) = f_opt(a, "by_beats") {
                    e.project.audio_clips[i].start_beat += d;
                }
                Ok(clip_json(e, i))
            },
        },
        Tool {
            name: "duplicate_clip",
            description: "Duplicate an audio clip: count copies placed back to back after it (or starting at at_beat). Each copy gets a new id.",
            mutates: true,
            schema: || obj(json!({"id": id_prop(), "at_beat": {"type": "number"}, "count": {"type": "integer", "minimum": 1, "maximum": 64}}), &["id"]),
            run: |e, a| {
                let i = audio_clip_index(&e.project, a)?;
                let (len, _) = clip_len(e, i)?;
                let c = e.project.audio_clips[i].clone();
                let n = u_or(a, "count", 1).clamp(1, 64) as usize;
                let mut at = f_opt(a, "at_beat").unwrap_or_else(|| end_beat(&e.project, &c, len));
                let mut made = Vec::new();
                for _ in 0..n {
                    let mut d = c.clone();
                    d.id = crate::ids::next_id(&mut e.project, "clip");
                    d.start_beat = at;
                    at = end_beat(&e.project, &d, len);
                    made.push(d.id.clone());
                    e.project.audio_clips.push(d);
                }
                Ok(json!({"source": c.id, "copies": made, "next_free_beat": at}))
            },
        },
        Tool {
            name: "slip_clip",
            description: "Slip an audio clip: slide the take under the clip by by_s seconds (positive plays later material) while the clip's position and length stay put.",
            mutates: true,
            schema: || obj(json!({"id": id_prop(), "by_s": {"type": "number"}}), &["id", "by_s"]),
            run: |e, a| {
                let i = audio_clip_index(&e.project, a)?;
                let (len, total) = clip_len(e, i)?;
                let by = f_opt(a, "by_s").unwrap_or(0.0);
                let c = &mut e.project.audio_clips[i];
                let max_off = (total - len).max(0.0);
                let new = (c.offset_s.max(0.0) + by).clamp(0.0, max_off);
                let applied = new - c.offset_s.max(0.0);
                c.offset_s = new;
                c.length_s = Some(len);
                let mut v = clip_json(e, i);
                v["slipped_s"] = json!((applied * 1000.0).round() / 1000.0);
                if (applied - by).abs() > 1e-4 {
                    v["note"] = json!("limited by the take's edges");
                }
                Ok(v)
            },
        },
        Tool {
            name: "fade_clip",
            description: "Set an audio clip's fade in / fade out in ms (equal-power; 0 = a hard edge with a tiny anti-click ramp). Fades can be as long as the clip.",
            mutates: true,
            schema: || obj(json!({"id": id_prop(), "fade_in_ms": {"type": "number", "minimum": 0}, "fade_out_ms": {"type": "number", "minimum": 0}}), &["id"]),
            run: |e, a| {
                let i = audio_clip_index(&e.project, a)?;
                let (len, _) = clip_len(e, i)?;
                let max = (len * 1000.0).max(0.5);
                if f_opt(a, "fade_in_ms").is_none() && f_opt(a, "fade_out_ms").is_none() {
                    bail!("give fade_in_ms and/or fade_out_ms");
                }
                let c = &mut e.project.audio_clips[i];
                if let Some(v) = f_opt(a, "fade_in_ms") {
                    c.fade_in_ms = Some(v.clamp(0.5, max));
                }
                if let Some(v) = f_opt(a, "fade_out_ms") {
                    c.fade_out_ms = Some(v.clamp(0.5, max));
                }
                Ok(clip_json(e, i))
            },
        },
        Tool {
            name: "pitch_clip",
            description: "Transpose an audio clip by semitones (-24..24: +12 = an octave up, -7 = a fifth down) without moving it or changing its length - an acapella or loop drops into the song's key while every hit stays on the grid. Render-time pitch, not a bounce: undo or set 0 and the original audio is back bit for bit. voice (default false) switches sung takes to a TD-PSOLA that keeps the singer's formants, so a transposed vocal keeps its tone instead of going chipmunk; best within +-7 semitones.",
            mutates: true,
            schema: || obj(json!({"id": id_prop(), "semitones": {"type": "number", "minimum": -24, "maximum": 24}, "voice": {"type": "boolean"}}), &["id", "semitones"]),
            run: |e, a| {
                let i = audio_clip_index(&e.project, a)?;
                let st = a
                    .get("semitones")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| anyhow!("give semitones (-24..24)"))? as f32;
                let c = &mut e.project.audio_clips[i];
                c.pitch_st = st.clamp(-24.0, 24.0);
                if let Some(v) = a.get("voice").and_then(|v| v.as_bool()) {
                    c.pitch_voice = v;
                }
                Ok(clip_json(e, i))
            },
        },
        Tool {
            name: "stretch_clip",
            description: "Time-stretch an audio clip in place (0.25..4: 2 = twice as long, 0.5 = half as long) while its pitch stays put - a fast loop slows into the song without going dark, a take fills a longer slot. The start stays on its beat; the end follows the stretch. Render-time, not a bounce: undo or factor 1 restores the original bit for bit. For an exact length on the grid use fit_clip.",
            mutates: true,
            schema: || obj(json!({"id": id_prop(), "factor": {"type": "number", "minimum": 0.25, "maximum": 4}}), &["id", "factor"]),
            run: |e, a| {
                let i = audio_clip_index(&e.project, a)?;
                let f = a
                    .get("factor")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| anyhow!("give factor (0.25..4)"))? as f32;
                let c = &mut e.project.audio_clips[i];
                c.stretch = f.clamp(0.25, 4.0);
                Ok(clip_json(e, i))
            },
        },
        Tool {
            name: "fit_clip",
            description: "Stretch an audio clip so it fills exactly beats on the timeline (measured from its natural length after offset_s/length_s, at the project tempo): a loop that runs 7.3 beats becomes exactly 8, hits and phrasing intact. Sets the clip's stretch; undo or stretch_clip 1 restores the original.",
            mutates: true,
            schema: || obj(json!({"id": id_prop(), "beats": {"type": "number", "exclusiveMinimum": 0}}), &["id", "beats"]),
            run: |e, a| {
                let i = audio_clip_index(&e.project, a)?;
                let beats = a
                    .get("beats")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| anyhow!("give beats"))? as f32;
                if beats <= 0.0 {
                    bail!("beats must be positive");
                }
                let (len, _) = clip_len(e, i)?;
                if len <= 0.0 {
                    bail!("clip has no audio to fit");
                }
                let want = beats * e.project.step_secs() * 4.0;
                let factor = (want / len).clamp(0.25, 4.0);
                let c = &mut e.project.audio_clips[i];
                c.stretch = factor;
                Ok(clip_json(e, i))
            },
        },
        Tool {
            name: "crossfade_clips",
            description: "Crossfade two touching or overlapping audio clips on the same track (first, then second): the first is extended and the second starts earlier into the take so they overlap by ms, with an equal-power fade out / fade in across the overlap.",
            mutates: true,
            schema: || obj(json!({"first": id_prop(), "second": id_prop(), "ms": {"type": "number", "minimum": 1, "description": "overlap length (default 30)"}}), &["first", "second"]),
            run: |e, a| {
                let ia = audio_clip_index(&e.project, &json!({"id": a.get("first").cloned().unwrap_or(Value::Null)}))?;
                let ib = audio_clip_index(&e.project, &json!({"id": a.get("second").cloned().unwrap_or(Value::Null)}))?;
                if ia == ib {
                    bail!("first and second are the same clip");
                }
                let (ca, cb) = (e.project.audio_clips[ia].clone(), e.project.audio_clips[ib].clone());
                if !ca.track.eq_ignore_ascii_case(&cb.track) {
                    bail!("the clips are on different tracks ({} and {})", ca.track, cb.track);
                }
                let (len_a, total_a) = clip_len(e, ia)?;
                let a_end = secs_at(&e.project, ca.start_beat) + len_a as f64;
                let b_start = secs_at(&e.project, cb.start_beat);
                if b_start < secs_at(&e.project, ca.start_beat) {
                    bail!("'{}' starts before '{}': pass them in timeline order", cb.id, ca.id);
                }
                let gap = b_start - a_end;
                if gap > 0.002 {
                    bail!("the clips are {:.3} s apart; move them together first (move_clip)", gap);
                }
                let want = (f_opt(a, "ms").unwrap_or(30.0).max(1.0) / 1000.0) as f64;
                let have = (a_end - b_start).max(0.0);
                if have < want {
                    let need = want - have;
                    // half from each side, as far as the takes allow
                    let a_room = (total_a - ca.offset_s.max(0.0) - len_a).max(0.0) as f64;
                    let b_room = cb.offset_s.max(0.0) as f64;
                    let mut ext_a = (need / 2.0).min(a_room);
                    let mut ext_b = (need - ext_a).min(b_room);
                    ext_a = (need - ext_b).min(a_room);
                    if ext_a + ext_b < need - 1e-6 {
                        ext_b = ext_b.min(b_room);
                    }
                    e.project.audio_clips[ia].length_s = Some(len_a + ext_a as f32);
                    let nb_start = b_start - ext_b;
                    let (len_b, _) = clip_len(e, ib)?;
                    let cbm = &mut e.project.audio_clips[ib];
                    cbm.offset_s = cb.offset_s.max(0.0) - ext_b as f32;
                    cbm.length_s = Some(len_b + ext_b as f32);
                    let nb = beat_at(&e.project, nb_start);
                    e.project.audio_clips[ib].start_beat = nb;
                }
                let (len_a2, _) = clip_len(e, ia)?;
                let a_end2 = secs_at(&e.project, e.project.audio_clips[ia].start_beat) + len_a2 as f64;
                let overlap = (a_end2 - secs_at(&e.project, e.project.audio_clips[ib].start_beat)).max(0.0);
                if overlap < 0.0005 {
                    bail!("no room in the takes for a crossfade (both clips already sit at their sample's edge)");
                }
                let ms = (overlap * 1000.0) as f32;
                e.project.audio_clips[ia].fade_out_ms = Some(ms);
                e.project.audio_clips[ib].fade_in_ms = Some(ms);
                let mut v = json!({"first": clip_json(e, ia), "second": clip_json(e, ib), "overlap_ms": (ms * 10.0).round() / 10.0});
                if overlap + 1e-4 < want {
                    v["note"] = json!(format!("only {:.1} ms of overlap fits in the takes", ms));
                }
                Ok(v)
            },
        },
        Tool {
            name: "consolidate_clips",
            description: "Consolidate audio clips on one track into a single new clip: the chosen clips (ids, or every clip on the track inside from_beat..to_beat, default all) are mixed with their trims, gains and fades into a new sample, and replaced by one clip with a new id.",
            mutates: true,
            schema: || obj(json!({"track": {"type": "string"}, "ids": {"type": "array", "items": {"type": "string"}},
                "from_beat": {"type": "number"}, "to_beat": {"type": "number"}, "name": {"type": "string", "description": "name of the new sample"}}), &["track"]),
            run: |e, a| {
                let ti = e.project.track_index(&s_opt(a, "track").unwrap_or_default())?;
                let track = e.project.tracks[ti].name.clone();
                let ids: Vec<String> = a.get("ids").and_then(|v| v.as_array()).map(|v| v.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                let mut idx = Vec::new();
                let mut lo = f32::MAX;
                let mut hi = f32::MIN;
                for i in 0..e.project.audio_clips.len() {
                    let c = e.project.audio_clips[i].clone();
                    if !c.track.eq_ignore_ascii_case(&track) || (!ids.is_empty() && !ids.contains(&c.id)) {
                        continue;
                    }
                    let (len, _) = clip_len(e, i)?;
                    let end = end_beat(&e.project, &c, len);
                    if f_opt(a, "from_beat").is_some_and(|f| end <= f) || f_opt(a, "to_beat").is_some_and(|t| c.start_beat >= t) {
                        continue;
                    }
                    lo = lo.min(c.start_beat);
                    hi = hi.max(end);
                    idx.push(i);
                }
                if idx.is_empty() {
                    bail!("no audio clips on '{track}' match");
                }
                let from = f_opt(a, "from_beat").map(|f| f.max(lo)).unwrap_or(lo);
                let to = f_opt(a, "to_beat").map(|t| t.min(hi)).unwrap_or(hi);
                let buf = mixdown(e, &idx, from, to)?;
                let name = crate::samples::sample_name(&s_opt(a, "name").unwrap_or_else(|| format!("{track}_consolidated")));
                let dir = e.workdir.join("consolidated");
                std::fs::create_dir_all(&dir)?;
                let file = dir.join(format!("{name}.wav"));
                crate::render::write_wav(&file, &buf, &buf)?;
                let reg = register_sample(e, SampleInfo {
                    name: name.clone(),
                    path: file.to_string_lossy().into(),
                    source: format!("consolidated clips on '{track}'"),
                    license: "own render".into(),
                    author: String::new(),
                    duration: 0.0,
                })?;
                let sample = reg["sample"].as_str().unwrap_or(&name).to_string();
                let gone: Vec<String> = idx.iter().map(|&i| e.project.audio_clips[i].id.clone()).collect();
                let mut k = 0;
                e.project.audio_clips.retain(|_| {
                    let drop = idx.contains(&k);
                    k += 1;
                    !drop
                });
                let id = crate::ids::next_id(&mut e.project, "clip");
                e.project.audio_clips.push(AudioClip { track: track.clone(), sample: sample.clone(), start_beat: from, offset_s: 0.0, length_s: None, gain_db: 0.0, pitch_st: 0.0, pitch_voice: false, stretch: 1.0, fade_in_ms: None, fade_out_ms: None, id: id.clone() });
                Ok(json!({"id": id, "track": track, "sample": sample, "file": file.to_string_lossy(), "replaced": gone,
                    "start_beat": from, "seconds": (buf.len() as f32 / SR * 1000.0).round() / 1000.0}))
            },
        },
        Tool {
            name: "ripple_delete",
            description: "Ripple delete: remove an audio clip (id) or a time range (from_beat..to_beat) and pull every later audio clip earlier to close the gap. Applies to the clip's track (or `track`), or every track with all_tracks. Clips crossing the range edges are split, so only the range is removed. Pattern notes and automation are not shifted.",
            mutates: true,
            schema: || obj(json!({"id": id_prop(), "from_beat": {"type": "number"}, "to_beat": {"type": "number"},
                "track": {"type": "string"}, "all_tracks": {"type": "boolean"}}), &[]),
            run: |e, a| {
                let (tracks, from, to) = if a.get("id").is_some() || a.get("index").is_some() {
                    let i = audio_clip_index(&e.project, a)?;
                    let (len, _) = clip_len(e, i)?;
                    let c = e.project.audio_clips[i].clone();
                    let tracks = if b_or(a, "all_tracks", false) { None } else { Some(vec![c.track.clone()]) };
                    (tracks, c.start_beat, end_beat(&e.project, &c, len))
                } else {
                    let (Some(f), Some(t)) = (f_opt(a, "from_beat"), f_opt(a, "to_beat")) else {
                        bail!("give the clip's id, or from_beat and to_beat");
                    };
                    (track_filter(e, a)?, f, t)
                };
                let (removed, split_edges, moved) = ripple(e, tracks.clone(), from, to)?;
                Ok(json!({"from_beat": from, "to_beat": to, "gap_beats": to - from, "removed": removed, "split_at_edges": split_edges,
                    "moved_earlier": moved, "tracks": tracks.map(|t| json!(t)).unwrap_or(json!("all"))}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A per-test directory, removed when the test ends (std only).
    struct TestDir(std::path::PathBuf);

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 2 s of a ramp so positions in the take are measurable; 120 BPM => 1 beat = 0.5 s.
    ///
    /// Each call gets its own directory (pid + test name + counter): the tests
    /// run in parallel inside one process, and a shared per-pid directory let
    /// them overwrite each other's take.wav and consolidated clips.
    fn eng() -> (Engine, TestDir) {
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let test: String = std::thread::current()
            .name()
            .unwrap_or("test")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let dir = std::env::temp_dir().join(format!(
            "beatbox_timeline_{}_{}_{}",
            std::process::id(),
            test,
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("new_project", &json!({"name": "tl", "bpm": 120}))
            .unwrap();
        let tone: Vec<f32> = (0..(2.0 * SR) as usize)
            .map(|i| 0.5 * (i as f32 * 220.0 / SR * std::f32::consts::TAU).sin())
            .collect();
        let wav = dir.join("take.wav");
        crate::render::write_wav(&wav, &tone, &tone).unwrap();
        e.call(
            "import_sample",
            &json!({"path": wav.to_string_lossy(), "name": "take"}),
        )
        .unwrap();
        e.call("add_track", &json!({"name": "vox", "preset": "pad"}))
            .unwrap();
        e.call(
            "add_audio_clip",
            &json!({"track": "vox", "sample": "take", "start_beat": 0.0}),
        )
        .unwrap();
        (e, TestDir(dir))
    }

    #[test]
    fn pitch_clip_transposes_keeps_time_and_restores_bit_identically() {
        let (mut e, _dir) = eng();
        let id = e.project.audio_clips[0].id.clone();
        let opts = crate::render::RenderOptions::default();
        let base = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        let crossings = |m: &crate::render::Mix, secs: f32| -> usize {
            let n = ((secs * SR) as usize).min(m.left.len());
            m.left[..n]
                .windows(2)
                .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
                .count()
        };
        let base_x = crossings(&base, 0.4);
        assert!(base_x > 50, "220 Hz tone gives crossings, got {base_x}");
        // +12 semitones doubles the 220 Hz tone to 440
        e.call("pitch_clip", &json!({"id": id, "semitones": 12}))
            .unwrap();
        assert_eq!(e.project.audio_clips[0].pitch_st, 12.0);
        let up = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(up.left.len(), base.left.len(), "render length kept");
        let up_x = crossings(&up, 0.4);
        assert!(
            up_x > (base_x as f32 * 1.7) as usize,
            "220 Hz {base_x} crossings -> {up_x} (want ~double)"
        );
        // back to 0: the original audio is back bit for bit
        e.call("pitch_clip", &json!({"id": id, "semitones": 0}))
            .unwrap();
        let back = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(
            back.left.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            base.left.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            "pitch 0 restores the untouched render"
        );
        // unknown id errors
        assert!(e
            .call("pitch_clip", &json!({"id": "clip999", "semitones": 3}))
            .is_err());
    }

    #[test]
    fn pitch_clip_voice_mode_also_transposes_and_keeps_length() {
        let (mut e, _dir) = eng();
        let id = e.project.audio_clips[0].id.clone();
        let opts = crate::render::RenderOptions::default();
        let base = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        let crossings = |m: &crate::render::Mix, secs: f32| -> usize {
            let n = ((secs * SR) as usize).min(m.left.len());
            m.left[..n]
                .windows(2)
                .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
                .count()
        };
        let base_x = crossings(&base, 0.4);
        e.call(
            "pitch_clip",
            &json!({"id": id, "semitones": 12, "voice": true}),
        )
        .unwrap();
        assert!(e.project.audio_clips[0].pitch_voice);
        let up = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(up.left.len(), base.left.len(), "render length kept");
        let up_x = crossings(&up, 0.4);
        assert!(
            up_x > (base_x as f32 * 1.5) as usize,
            "voice mode 220 Hz {base_x} crossings -> {up_x} (want higher)"
        );
        // both fields survive a project save/load round-trip
        let json = serde_json::to_string(&e.project).unwrap();
        let back: crate::project::Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.audio_clips[0].pitch_st, 12.0);
        assert!(back.audio_clips[0].pitch_voice);
    }

    #[test]
    fn stretch_clip_and_fit_clip_resize_on_the_grid_and_keep_pitch() {
        let (mut e, _dir) = eng();
        let id = e.project.audio_clips[0].id.clone();
        let opts = crate::render::RenderOptions {
            tail: 4.5,
            ..Default::default()
        };
        let base = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        let crossings = |m: &crate::render::Mix, from: f32, secs: f32| -> usize {
            let a = ((from * SR) as usize).min(m.left.len());
            let b = (((from + secs) * SR) as usize).min(m.left.len());
            m.left[a..b]
                .windows(2)
                .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
                .count()
        };
        let rms = |m: &crate::render::Mix, from: f32, secs: f32| -> f32 {
            let a = ((from * SR) as usize).min(m.left.len());
            let b = (((from + secs) * SR) as usize).min(m.left.len());
            (m.left[a..b].iter().map(|s| s * s).sum::<f32>() / (b - a).max(1) as f32).sqrt()
        };
        // the unstretched 2 s take has decayed by 3.4 s (default tail 4.5 s)
        let tail_base = rms(&base, 3.4, 0.8);
        // stretch 2x: pitch kept (still ~88 crossings per 0.4 s), the take
        // now plays to 4 s, and clip_json reports the stretch and new end
        let v = e
            .call("stretch_clip", &json!({"id": id, "factor": 2.0}))
            .unwrap();
        assert_eq!(e.project.audio_clips[0].stretch, 2.0);
        assert_eq!(v["stretch"], 2.0);
        assert_eq!(v["end_beat"], 8.0);
        let up = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(up.left.len(), base.left.len(), "render length kept");
        let x = crossings(&up, 0.0, 0.4);
        assert!(
            (x as f32 - 88.0).abs() < 20.0,
            "stretch keeps pitch: {x} crossings in 0.4s (want ~88)"
        );
        let tail_up = rms(&up, 3.4, 0.8);
        assert!(
            tail_up > 0.05 && tail_base < tail_up * 0.5,
            "stretched take plays past the old end: {tail_base} -> {tail_up}"
        );
        // fit_clip to exactly 2 beats (1 s at 120 bpm): factor 0.5
        e.call("fit_clip", &json!({"id": id, "beats": 2.0}))
            .unwrap();
        assert!((e.project.audio_clips[0].stretch - 0.5).abs() < 1e-3);
        // back to 1: the original render is back bit for bit
        e.call("stretch_clip", &json!({"id": id, "factor": 1.0}))
            .unwrap();
        let back = crate::render::render(&e.project, &e.bank, &opts).unwrap();
        assert_eq!(
            back.left.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            base.left.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            "stretch 1 restores the untouched render"
        );
        // the field survives a project save/load round-trip
        e.call("stretch_clip", &json!({"id": id, "factor": 1.5}))
            .unwrap();
        let json = serde_json::to_string(&e.project).unwrap();
        let rt: crate::project::Project = serde_json::from_str(&json).unwrap();
        assert!((rt.audio_clips[0].stretch - 1.5).abs() < 1e-6);
        // unknown id errors on both tools
        assert!(e
            .call("stretch_clip", &json!({"id": "clip999", "factor": 2.0}))
            .is_err());
        assert!(e
            .call("fit_clip", &json!({"id": "clip999", "beats": 4.0}))
            .is_err());
    }

    #[test]
    fn split_trim_move_duplicate_slip_fade_keep_ids() {
        let (mut e, _dir) = eng(); // keep the directory until the test ends
        let id = e.project.audio_clips[0].id.clone();
        assert!(id.starts_with("clip"), "{id}");
        // split at beat 2 (1.0 s)
        let v = e
            .call("split_clip", &json!({"id": id, "at_beat": 2.0}))
            .unwrap();
        let right = v["right_id"].as_str().unwrap().to_string();
        assert_ne!(right, id);
        assert_eq!(e.project.audio_clips.len(), 2);
        let r = e
            .project
            .audio_clips
            .iter()
            .find(|c| c.id == right)
            .unwrap();
        assert!((r.offset_s - 1.0).abs() < 1e-3 && (r.length_s.unwrap() - 1.0).abs() < 1e-3);
        assert!((e.project.audio_clips[0].length_s.unwrap() - 1.0).abs() < 1e-3);
        // undo restores one clip with the original id; redo brings the same right id back
        e.call("undo", &json!({})).unwrap();
        assert_eq!(e.project.audio_clips.len(), 1);
        assert_eq!(e.project.audio_clips[0].id, id);
        e.call("redo", &json!({})).unwrap();
        assert!(e.project.audio_clips.iter().any(|c| c.id == right));
        // trim the right part's start to beat 2.5 (0.25 s in)
        e.call("trim_clip", &json!({"id": right, "start_beat": 2.5}))
            .unwrap();
        let r = e
            .project
            .audio_clips
            .iter()
            .find(|c| c.id == right)
            .unwrap()
            .clone();
        assert!(
            (r.start_beat - 2.5).abs() < 1e-3
                && (r.offset_s - 1.25).abs() < 1e-3
                && (r.length_s.unwrap() - 0.75).abs() < 1e-3
        );
        // move it
        e.call("move_clip", &json!({"id": right, "by_beats": 1.5}))
            .unwrap();
        assert!(
            (e.project
                .audio_clips
                .iter()
                .find(|c| c.id == right)
                .unwrap()
                .start_beat
                - 4.0)
                .abs()
                < 1e-4
        );
        // duplicate twice: two new ids, back to back
        let d = e
            .call("duplicate_clip", &json!({"id": right, "count": 2}))
            .unwrap();
        let copies: Vec<String> = d["copies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        assert_eq!(copies.len(), 2);
        let c0 = e
            .project
            .audio_clips
            .iter()
            .find(|c| c.id == copies[0])
            .unwrap();
        assert!((c0.start_beat - 5.5).abs() < 1e-3, "{}", c0.start_beat);
        // slip: offset moves, start does not
        let before = e
            .project
            .audio_clips
            .iter()
            .find(|c| c.id == right)
            .unwrap()
            .clone();
        e.call("slip_clip", &json!({"id": right, "by_s": -0.5}))
            .unwrap();
        let after = e
            .project
            .audio_clips
            .iter()
            .find(|c| c.id == right)
            .unwrap()
            .clone();
        assert_eq!(after.start_beat, before.start_beat);
        assert!((after.offset_s - 0.75).abs() < 1e-3);
        // fade
        e.call(
            "fade_clip",
            &json!({"id": right, "fade_in_ms": 300, "fade_out_ms": 10000}),
        )
        .unwrap();
        let f = e
            .project
            .audio_clips
            .iter()
            .find(|c| c.id == right)
            .unwrap();
        assert_eq!(f.fade_in_ms, Some(300.0));
        assert!(
            (f.fade_out_ms.unwrap() - 750.0).abs() < 1.0,
            "fade capped at the clip length"
        );
    }

    #[test]
    fn crossfade_consolidate_and_ripple() {
        let (mut e, _dir) = eng(); // keep the directory until the test ends
        let id = e.project.audio_clips[0].id.clone();
        let v = e
            .call("split_clip", &json!({"id": id, "at_beat": 2.0}))
            .unwrap();
        let right = v["right_id"].as_str().unwrap().to_string();
        let x = e
            .call(
                "crossfade_clips",
                &json!({"first": id, "second": right, "ms": 40}),
            )
            .unwrap();
        assert!(
            (x["overlap_ms"].as_f64().unwrap() - 40.0).abs() < 1.0,
            "{x}"
        );
        let a = e
            .project
            .audio_clips
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .clone();
        let b = e
            .project
            .audio_clips
            .iter()
            .find(|c| c.id == right)
            .unwrap()
            .clone();
        assert!((a.length_s.unwrap() - 1.02).abs() < 2e-3 && (b.offset_s - 0.98).abs() < 2e-3);
        // the crossfaded pair renders without a dip or a bump at the joint
        let mix = e.mix().unwrap();
        let rms = |s: f32| {
            let i0 = (s * SR) as usize;
            (mix.left[i0..i0 + 2000].iter().map(|x| x * x).sum::<f32>() / 2000.0).sqrt()
        };
        let (mid, joint) = (rms(0.5), rms(0.99));
        assert!(
            (joint / mid - 1.0).abs() < 0.25,
            "joint {joint} vs body {mid}"
        );
        // consolidate both into one new clip
        let c = e
            .call("consolidate_clips", &json!({"track": "vox"}))
            .unwrap();
        assert_eq!(e.project.audio_clips.len(), 1);
        let nid = c["id"].as_str().unwrap().to_string();
        assert!(nid != id && nid != right);
        assert!((c["seconds"].as_f64().unwrap() - 2.0).abs() < 0.01, "{c}");
        // ripple delete a range in the middle: the tail moves earlier
        e.call(
            "add_audio_clip",
            &json!({"track": "vox", "sample": "take", "start_beat": 8.0}),
        )
        .unwrap();
        let later = e.project.audio_clips[1].id.clone();
        let r = e
            .call(
                "ripple_delete",
                &json!({"track": "vox", "from_beat": 1.0, "to_beat": 2.0}),
            )
            .unwrap();
        assert_eq!(r["split_at_edges"], 2, "{r}");
        let l = e
            .project
            .audio_clips
            .iter()
            .find(|c| c.id == later)
            .unwrap();
        assert!((l.start_beat - 7.0).abs() < 1e-4);
        // ripple delete a whole clip by id
        e.call("ripple_delete", &json!({"id": later})).unwrap();
        assert!(e.project.audio_clips.iter().all(|c| c.id != later));
    }
}
