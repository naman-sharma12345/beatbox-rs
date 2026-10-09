//! Offline renderer: arrangement -> per-track audio -> FX -> mix -> master.

use crate::automation::{self, parse_target, AutomationLane, Target};
use crate::dsp::*;
use crate::fx::{Effect, FxContext};
use crate::instruments::{render_note_slide, Instrument};
use crate::project::Project;
use crate::samples::SampleBank;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

/// One scheduled note.
#[derive(Clone, Debug)]
pub struct Event {
    pub start: usize,
    pub gate: f32,
    pub pitch: f32,
    pub vel: f32,
    /// Glide target pitch (808 / synth slides).
    pub slide_to: Option<f32>,
}

pub struct Stem {
    pub name: String,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

/// Per-track measurements taken during the render (no audio kept), so the
/// analyzer works on 30+ track songs without holding every stem in RAM.
#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub name: String,
    pub stats: crate::analysis::Stats,
    /// Sum of squares over both channels.
    pub energy: f64,
    /// RMS (dBFS) measured only over 400 ms blocks where the track sounds.
    pub active_rms_db: f32,
    /// Percent of the song where the track sounds.
    pub active_percent: f32,
    /// Mean-square energy per 400 ms block (for per-section analysis).
    pub blocks: Vec<f32>,
}

/// Length of the analysis blocks in `TrackInfo::blocks`.
pub const INFO_BLOCK: usize = (SR as usize) * 2 / 5;

pub fn track_info(name: &str, l: &[f32], r: &[f32]) -> TrackInfo {
    let stats = crate::analysis::stats(l, r);
    let n = l.len().min(r.len());
    let mut energy = 0.0f64;
    let mut blocks = Vec::with_capacity(n / INFO_BLOCK + 1);
    for c in 0..n.div_ceil(INFO_BLOCK) {
        let (a, b) = (c * INFO_BLOCK, ((c + 1) * INFO_BLOCK).min(n));
        let mut e = 0.0f64;
        for i in a..b {
            e += (l[i] as f64).powi(2) + (r[i] as f64).powi(2);
        }
        energy += e;
        blocks.push((e / (2.0 * (b - a).max(1) as f64)) as f32);
    }
    let gate = 1e-6f32; // -60 dBFS mean square
    let active: Vec<f32> = blocks.iter().copied().filter(|b| *b > gate).collect();
    let active_rms_db = if active.is_empty() {
        -120.0
    } else {
        let ms = active.iter().sum::<f32>() / active.len() as f32;
        ((10.0 * ms.max(1e-12).log10()) * 10.0).round() / 10.0
    };
    TrackInfo {
        name: name.to_string(),
        stats,
        energy,
        active_rms_db,
        active_percent: (100.0 * active.len() as f32 / blocks.len().max(1) as f32).round(),
        blocks,
    }
}

pub struct Mix {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub stems: Vec<Stem>,
    /// Per-track measurements (with `keep_stems` or `track_stats`).
    pub track_info: Vec<TrackInfo>,
    /// Post-fader output of every bus (only with `keep_stems`).
    pub bus_stems: Vec<Stem>,
    pub seconds: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    /// Play the whole song this many times.
    pub loops: u32,
    /// Seconds of tail after the last bar (reverb/delay ring-out).
    pub tail: f32,
    pub keep_stems: bool,
    /// Measure every track (TrackInfo) without keeping its audio.
    pub track_stats: bool,
    /// Render only this section range of the song (in steps), if set.
    pub step_range: Option<(u32, u32)>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            loops: 1,
            tail: 1.5,
            keep_stems: false,
            track_stats: false,
            step_range: None,
        }
    }
}

/// Deterministic per-pass dice roll for a note with probability `p`
/// (same project = same render, but each pattern pass rolls differently).
pub fn chance_hit(track: usize, note: usize, pass_offset: u32, p: f32) -> bool {
    let mut x = (track as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (note as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (pass_offset as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    ((x >> 11) as f64 / (1u64 << 53) as f64) < p as f64
}

/// Build the event list for every track (index aligned with project.tracks).
pub fn schedule(p: &Project, opts: &RenderOptions) -> (Vec<Vec<Event>>, usize) {
    let step = p.step_secs();
    let swing_steps = p.swing.clamp(0.0, 1.0) * 0.5;
    let mut events: Vec<Vec<Event>> = vec![Vec::new(); p.tracks.len()];
    let mut offset: u32 = 0;
    let (r0, r1) = opts.step_range.unwrap_or((0, u32::MAX));
    for _ in 0..opts.loops.max(1) {
        for sec in p.song_sections() {
            let Ok(pi) = p.pattern_index(&sec.pattern) else {
                continue;
            };
            let pat = &p.patterns[pi];
            for _ in 0..sec.repeats.max(1) {
                for (ti, track) in p.tracks.iter().enumerate() {
                    for (ni, n) in pat.notes(&track.name).iter().enumerate() {
                        if n.prob < 1.0 && !chance_hit(ti, ni, offset, n.prob) {
                            continue;
                        }
                        let mut s = n.start;
                        if s.fract() == 0.0 && (s as i64) % 2 == 1 {
                            s += swing_steps;
                        }
                        s += n.offset.clamp(-0.5, 0.5);
                        let abs = (offset as f32 + s).max(0.0);
                        if abs < r0 as f32 || abs >= r1 as f32 {
                            continue;
                        }
                        let rel = abs - r0 as f32;
                        events[ti].push(Event {
                            start: (rel * step * SR) as usize,
                            gate: (n.len * step).max(0.01),
                            pitch: n.pitch as f32,
                            vel: n.vel,
                            slide_to: n.slide_to.map(|x| x as f32),
                        });
                    }
                }
                offset += pat.steps();
            }
        }
    }
    let end_step = offset.min(r1).saturating_sub(r0);
    let len = (end_step as f32 * step * SR) as usize;
    for e in events.iter_mut() {
        e.sort_by_key(|x| x.start);
    }
    (events, len)
}

/// Song-time mapping for automation during one render.
#[derive(Clone, Copy, Debug)]
pub struct Timeline {
    /// Song beat at sample 0 of this render.
    pub beat0: f32,
    pub beats_per_sample: f32,
    /// Song length in beats (automation wraps on multi-loop renders).
    pub song_beats: f32,
    /// Number of loops rendered; past the last one (the reverb tail) every
    /// lane holds its final value instead of wrapping back to the start.
    pub loops: u32,
}

impl Timeline {
    pub fn new(p: &Project, opts: &RenderOptions) -> Self {
        let r0 = opts.step_range.map(|r| r.0).unwrap_or(0);
        Timeline {
            beat0: r0 as f32 / 4.0,
            beats_per_sample: 1.0 / (4.0 * p.step_secs() * SR),
            song_beats: p.song_beats(),
            loops: opts.loops.max(1),
        }
    }
    pub fn beat_at(&self, sample: usize) -> f32 {
        let b = self.beat0 + sample as f32 * self.beats_per_sample;
        if self.song_beats <= 0.0 || b < self.song_beats {
            return b;
        }
        if b >= self.song_beats * self.loops as f32 {
            // tail: hold the end of the song
            return self.song_beats;
        }
        b % self.song_beats
    }
}

/// Automation is evaluated once per block and interpolated per sample.
pub const AUTO_BLOCK: usize = 64;

fn owner_lanes<'a>(p: &'a Project, owner: &str) -> Vec<&'a AutomationLane> {
    p.automation
        .iter()
        .filter(|l| l.enabled && !l.points.is_empty() && l.target.eq_ignore_ascii_case(owner))
        .collect()
}

/// Per-sample values of a lane (block-evaluated, linearly smoothed).
fn lane_curve(
    lane: &AutomationLane,
    n: usize,
    tl: &Timeline,
    map: impl Fn(f32) -> f32,
) -> Vec<f32> {
    let blocks: Vec<f32> = (0..=n / AUTO_BLOCK + 1)
        .map(|k| map(lane.value_at(tl.beat_at(k * AUTO_BLOCK)).unwrap_or(0.0)))
        .collect();
    (0..n)
        .map(|i| {
            let k = i / AUTO_BLOCK;
            let f = (i % AUTO_BLOCK) as f32 / AUTO_BLOCK as f32;
            blocks[k] + (blocks[k + 1] - blocks[k]) * f
        })
        .collect()
}

/// Run an effect chain, honouring `fx.<i>.<param>` automation lanes.
///
/// Effects are whole-buffer processors with internal state, so an automated
/// effect is rendered at a small grid of parameter values (each a normal,
/// continuous render) and the outputs are blended per sample with
/// multilinear weights from the automation curve. Up to three automated
/// parameters per effect are honoured.
pub fn process_chain(
    effects: &[Effect],
    lanes: &[&AutomationLane],
    l: &mut Vec<f32>,
    r: &mut Vec<f32>,
    ctx: &FxContext,
    tl: &Timeline,
) {
    for (fi, fx) in effects.iter().enumerate() {
        let mine: Vec<(&AutomationLane, Vec<String>)> = lanes
            .iter()
            .filter_map(|lane| match parse_target(&lane.param) {
                Some(Target::Fx(i, path)) if i == fi => Some((*lane, path)),
                _ => None,
            })
            .take(3)
            .collect();
        let base = serde_json::to_value(fx).unwrap_or_default();
        let mine: Vec<_> = mine
            .into_iter()
            .filter(|(_, path)| automation::get_path(&base, path).is_some())
            .collect();
        if mine.is_empty() {
            fx.process(l, r, ctx);
            continue;
        }
        let n = l.len();
        let k_per = match mine.len() {
            1 => 10,
            2 => 4,
            _ => 3,
        };
        // per lane: the level values and the per-sample fractional level index
        let mut levels: Vec<Vec<f32>> = Vec::new();
        let mut pos: Vec<Vec<f32>> = Vec::new();
        for (lane, path) in &mine {
            let (lo, hi) = lane.range().unwrap_or((0.0, 0.0));
            let log = automation::is_log_param(&lane.param) && lo > 0.0;
            let k = if (hi - lo).abs() < 1e-9 { 1 } else { k_per };
            let to_axis = |v: f32| if log { v.max(1e-6).ln() } else { v };
            let (a0, a1) = (to_axis(lo), to_axis(hi));
            let lv: Vec<f32> = (0..k)
                .map(|j| {
                    let t = if k == 1 {
                        0.0
                    } else {
                        j as f32 / (k - 1) as f32
                    };
                    let x = a0 + (a1 - a0) * t;
                    if log {
                        x.exp()
                    } else {
                        x
                    }
                })
                .collect();
            let span = (a1 - a0).abs().max(1e-12);
            let kk = (k - 1) as f32;
            pos.push(lane_curve(lane, n, tl, |v| {
                if k == 1 {
                    0.0
                } else {
                    ((to_axis(v) - a0) / span * kk).clamp(0.0, kk)
                }
            }));
            levels.push(lv);
            let _ = path;
        }
        let dims: Vec<usize> = levels.iter().map(|v| v.len()).collect();
        let total: usize = dims.iter().product();
        let (src_l, src_r) = (l.clone(), r.clone());
        let mut acc_l = vec![0.0f32; n];
        let mut acc_r = vec![0.0f32; n];
        for combo in 0..total {
            // decode the multi-index
            let mut idx = Vec::with_capacity(dims.len());
            let mut c = combo;
            for d in &dims {
                idx.push(c % d);
                c /= d;
            }
            // weights: product of hat functions
            let mut w = vec![1.0f32; n];
            for (li, &j) in idx.iter().enumerate() {
                for (wi, p) in w.iter_mut().zip(pos[li].iter()) {
                    *wi *= (1.0 - (p - j as f32).abs()).max(0.0);
                }
            }
            if w.iter().all(|x| *x <= 1e-6) {
                continue;
            }
            let mut v = base.clone();
            for (li, (_, path)) in mine.iter().enumerate() {
                automation::set_path(&mut v, path, levels[li][idx[li]]);
            }
            let Ok(fxv) = serde_json::from_value::<Effect>(v) else {
                continue;
            };
            let (mut tl_, mut tr_) = (src_l.clone(), src_r.clone());
            fxv.process(&mut tl_, &mut tr_, ctx);
            for i in 0..n {
                acc_l[i] += w[i] * tl_[i];
                acc_r[i] += w[i] * tr_[i];
            }
        }
        *l = acc_l;
        *r = acc_r;
    }
}

/// Apply fader (with optional `volume` lane, in dB) and stereo balance
/// (with optional `pan` lane) to a stereo buffer.
fn apply_fader(
    l: &mut [f32],
    r: &mut [f32],
    volume_db: f32,
    lanes: &[&AutomationLane],
    tl: &Timeline,
    balance: Option<f32>,
) {
    let n = l.len();
    let vol_lane = lanes
        .iter()
        .find(|x| parse_target(&x.param) == Some(Target::Volume));
    match vol_lane {
        Some(lane) => {
            let g = lane_curve(lane, n, tl, |db| db_to_gain(db.clamp(-120.0, 24.0)));
            for i in 0..n {
                l[i] *= g[i];
                r[i] *= g[i];
            }
        }
        None => {
            let g = db_to_gain(volume_db);
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                *a *= g;
                *b *= g;
            }
        }
    }
    if let Some(pan) = balance {
        let pan_lane = lanes
            .iter()
            .find(|x| parse_target(&x.param) == Some(Target::Pan));
        let curve = pan_lane.map(|lane| lane_curve(lane, n, tl, |v| v.clamp(-1.0, 1.0)));
        for i in 0..n {
            let p = curve.as_ref().map(|c| c[i]).unwrap_or(pan);
            if p > 0.0 {
                l[i] *= 1.0 - p;
            } else if p < 0.0 {
                r[i] *= 1.0 + p;
            }
        }
    }
}

pub fn render(p: &Project, bank: &SampleBank, opts: &RenderOptions) -> Result<Mix> {
    let (events, body_len) = schedule(p, opts);
    let total = body_len + (opts.tail.max(0.0) * SR) as usize;
    let any_solo = p.tracks.iter().any(|t| t.solo);
    let tl = Timeline::new(p, opts);

    let triggers: HashMap<String, Vec<usize>> = p
        .tracks
        .iter()
        .zip(events.iter())
        .map(|(t, ev)| (t.name.to_lowercase(), ev.iter().map(|e| e.start).collect()))
        .collect();
    let ctx = FxContext {
        step_secs: p.step_secs(),
        triggers: &triggers,
    };

    let mut ml = vec![0.0f32; total];
    let mut mr = vec![0.0f32; total];
    let mut bus_in: Vec<(Vec<f32>, Vec<f32>)> = p
        .buses
        .iter()
        .map(|_| (vec![0.0f32; total], vec![0.0f32; total]))
        .collect();
    let mut stems = Vec::new();
    let mut track_infos = Vec::new();

    for (ti, track) in p.tracks.iter().enumerate() {
        let audible = !track.mute && (!any_solo || track.solo);
        if !audible && !opts.keep_stems && !opts.track_stats {
            continue;
        }
        let lanes = owner_lanes(p, &track.name);
        let inst_lanes: Vec<(&AutomationLane, Vec<String>)> = lanes
            .iter()
            .filter_map(|l| match parse_target(&l.param) {
                Some(Target::Instrument(path)) => Some((*l, path)),
                _ => None,
            })
            .collect();
        let base_inst = if inst_lanes.is_empty() {
            Value::Null
        } else {
            serde_json::to_value(&track.instrument).unwrap_or_default()
        };
        let spread = track.instrument.stereo_spread().clamp(0.0, 1.0);
        let mut mono = vec![0.0f32; total];
        // decorrelated second render for stereo unison spread
        let mut mono2 = if spread > 0.0 {
            vec![0.0f32; total]
        } else {
            Vec::new()
        };
        let mut cache: HashMap<(i32, u8, u32, Vec<i64>, i32), (Vec<f32>, Vec<f32>)> =
            HashMap::new();
        for (k, e) in events[ti].iter().enumerate() {
            if e.start >= total {
                continue;
            }
            // instrument automation is sampled at note-on
            let beat = tl.beat_at(e.start);
            let vals: Vec<f32> = inst_lanes
                .iter()
                .map(|(l, _)| l.value_at(beat).unwrap_or(0.0))
                .collect();
            let key = (
                (e.pitch * 10.0) as i32,
                (e.vel * 127.0) as u8,
                (e.gate * 1000.0) as u32,
                vals.iter().map(|v| (v * 1000.0).round() as i64).collect(),
                e.slide_to.map(|x| (x * 10.0) as i32).unwrap_or(-1),
            );
            let seed = (ti as u64) << 32 | (k as u64 % 7);
            let (buf, buf2) = cache.entry(key).or_insert_with(|| {
                let inst: Instrument = if inst_lanes.is_empty() {
                    track.instrument.clone()
                } else {
                    let mut v = base_inst.clone();
                    for ((_, path), x) in inst_lanes.iter().zip(vals.iter()) {
                        automation::set_path(&mut v, path, *x);
                    }
                    serde_json::from_value(v).unwrap_or_else(|_| track.instrument.clone())
                };
                let a = render_note_slide(&inst, e.pitch, e.vel, e.gate, bank, seed, e.slide_to);
                let b = if spread > 0.0 {
                    render_note_slide(
                        &inst,
                        e.pitch,
                        e.vel,
                        e.gate,
                        bank,
                        seed ^ 0xA5A5_5A5A,
                        e.slide_to,
                    )
                } else {
                    Vec::new()
                };
                (a, b)
            });
            let end = (e.start + buf.len()).min(total);
            for (o, s) in mono[e.start..end].iter_mut().zip(buf.iter()) {
                *o += *s;
            }
            if spread > 0.0 {
                let end = (e.start + buf2.len()).min(total);
                for (o, s) in mono2[e.start..end].iter_mut().zip(buf2.iter()) {
                    *o += *s;
                }
            }
        }
        // pan (equal power), optionally automated
        let pan_lane = lanes
            .iter()
            .find(|x| parse_target(&x.param) == Some(Target::Pan));
        let pan_curve = pan_lane.map(|lane| lane_curve(lane, total, &tl, |v| v.clamp(-1.0, 1.0)));
        let gains = |pan: f32| {
            let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
            (
                angle.cos() * std::f32::consts::SQRT_2,
                angle.sin() * std::f32::consts::SQRT_2,
            )
        };
        let (gl, gr) = gains(track.pan);
        let mut l = Vec::with_capacity(total);
        let mut r = Vec::with_capacity(total);
        for (i, x) in mono.iter().enumerate() {
            let (a, b) = match &pan_curve {
                Some(c) => gains(c[i]),
                None => (gl, gr),
            };
            let xr = if spread > 0.0 {
                // left = voice set A, right = blend toward the independent set B
                x * (1.0 - spread) + mono2[i] * spread
            } else {
                *x
            };
            l.push(x * a);
            r.push(xr * b);
        }
        drop(mono);
        drop(mono2);
        process_chain(&track.effects, &lanes, &mut l, &mut r, &ctx, &tl);
        let pre: Option<(Vec<f32>, Vec<f32>)> =
            if audible && track.sends.iter().any(|s| s.pre_fader) {
                Some((l.clone(), r.clone()))
            } else {
                None
            };
        apply_fader(&mut l, &mut r, track.volume_db, &lanes, &tl, None);
        if audible {
            for s in &track.sends {
                let Ok(bi) = p.bus_index(&s.bus) else {
                    continue;
                };
                let g = db_to_gain(s.db.clamp(-120.0, 12.0));
                let (sl, sr) = match (&pre, s.pre_fader) {
                    (Some((pl, pr)), true) => (pl, pr),
                    _ => (&l, &r),
                };
                let (bl, br) = &mut bus_in[bi];
                for i in 0..total {
                    bl[i] += sl[i] * g;
                    br[i] += sr[i] * g;
                }
            }
            let dest = track.output.as_deref().and_then(|b| p.bus_index(b).ok());
            let (dl, dr) = match dest {
                Some(bi) => {
                    let (bl, br) = &mut bus_in[bi];
                    (bl, br)
                }
                None => (&mut ml, &mut mr),
            };
            for i in 0..total {
                dl[i] += l[i];
                dr[i] += r[i];
            }
        }
        if opts.keep_stems || opts.track_stats {
            track_infos.push(track_info(&track.name, &l, &r));
        }
        if opts.keep_stems {
            stems.push(Stem {
                name: track.name.clone(),
                left: l,
                right: r,
            });
        }
    }

    // buses: own chain + fader + balance, then into the master
    let mut bus_stems = Vec::new();
    for (bus, (mut l, mut r)) in p.buses.iter().zip(bus_in) {
        let lanes = owner_lanes(p, &bus.name);
        process_chain(&bus.effects, &lanes, &mut l, &mut r, &ctx, &tl);
        apply_fader(&mut l, &mut r, bus.volume_db, &lanes, &tl, Some(bus.pan));
        if !bus.mute {
            for i in 0..total {
                ml[i] += l[i];
                mr[i] += r[i];
            }
        }
        if opts.keep_stems {
            bus_stems.push(Stem {
                name: bus.name.clone(),
                left: l,
                right: r,
            });
        }
    }

    let master_lanes = owner_lanes(p, "master");
    apply_fader(
        &mut ml,
        &mut mr,
        p.master_volume_db,
        &master_lanes,
        &tl,
        None,
    );
    // DC block before the master chain so the limiter has the last word
    let (mut dl, mut dr) = (DcBlock::default(), DcBlock::default());
    for (a, b) in ml.iter_mut().zip(mr.iter_mut()) {
        *a = dl.process(*a);
        *b = dr.process(*b);
    }
    process_chain(
        &p.master_effects,
        &master_lanes,
        &mut ml,
        &mut mr,
        &ctx,
        &tl,
    );
    for (a, b) in ml.iter_mut().zip(mr.iter_mut()) {
        if !a.is_finite() {
            *a = 0.0;
        }
        if !b.is_finite() {
            *b = 0.0;
        }
    }
    Ok(Mix {
        seconds: total as f32 / SR,
        left: ml,
        right: mr,
        stems,
        track_info: track_infos,
        bus_stems,
    })
}

/// Write 16-bit stereo PCM WAV.
pub fn write_wav(path: &Path, l: &[f32], r: &[f32]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let n = l.len().min(r.len());
    let data_len = (n * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&2u16.to_le_bytes()); // stereo
    out.extend_from_slice(&(SR as u32).to_le_bytes());
    out.extend_from_slice(&((SR as u32) * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        for s in [l[i], r[i]] {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(&out)?;
    Ok(())
}
