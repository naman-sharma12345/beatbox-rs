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
    /// Mean-square energy below ~150 Hz per 10 ms block (kick/bass masking).
    pub low_env: Vec<f32>,
}

/// Length of the blocks in `TrackInfo::low_env`.
pub const LOW_BLOCK: usize = 441;

/// Low-band (< ~150 Hz, 2-pole) mean-square envelope in 10 ms blocks.
pub fn low_envelope(l: &[f32], r: &[f32]) -> Vec<f32> {
    let n = l.len().min(r.len());
    let a = (-2.0 * std::f32::consts::PI * 150.0 / SR).exp();
    let (mut y1, mut y2) = (0.0f32, 0.0f32);
    let mut out = Vec::with_capacity(n / LOW_BLOCK + 1);
    let mut acc = 0.0f32;
    for i in 0..n {
        let x = 0.5 * (l[i] + r[i]);
        y1 = x + (y1 - x) * a;
        y2 = y1 + (y2 - y1) * a;
        acc += y2 * y2;
        if (i + 1) % LOW_BLOCK == 0 {
            out.push(acc / LOW_BLOCK as f32);
            acc = 0.0;
        }
    }
    out
}

/// Share of `b`'s low-band energy that sounds while `a` is also loud in the
/// low band (within 6 dB of `b` or louder): the masking that sidechain
/// ducking should remove. 0..1.
pub fn low_overlap(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (mut tot, mut ov) = (0.0f64, 0.0f64);
    for i in 0..n {
        let (x, y) = (a[i] as f64, b[i] as f64);
        tot += y;
        if x > y * 0.25 && x > 1e-7 {
            ov += y.min(x);
        }
    }
    if tot < 1e-12 {
        0.0
    } else {
        (ov / tot) as f32
    }
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
        low_env: low_envelope(l, r),
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

/// Bus processing order: a bus is processed after every bus that feeds
/// it. Cycles (rejected by route_bus) fall back to declaration order.
pub fn bus_order(p: &Project) -> Vec<usize> {
    let n = p.buses.len();
    let out_of = |i: usize| {
        p.buses[i]
            .output
            .as_deref()
            .and_then(|o| p.bus_index(o).ok())
    };
    let mut indeg = vec![0usize; n];
    for i in 0..n {
        if let Some(d) = out_of(i) {
            if d != i {
                indeg[d] += 1;
            }
        }
    }
    let mut order = Vec::with_capacity(n);
    let mut done = vec![false; n];
    while let Some(i) = (0..n).find(|&i| !done[i] && indeg[i] == 0) {
        done[i] = true;
        order.push(i);
        if let Some(d) = out_of(i) {
            if d != i {
                indeg[d] -= 1;
            }
        }
    }
    order.extend((0..n).filter(|&i| !done[i]));
    order
}

/// Fade applied where a voice is cut short (mono choke).
pub const CHOKE_FADE_S: f32 = 0.005;
/// Fade-out applied to the last samples of every voice so a buffer that
/// ends above zero (a truncated tail) never steps to silence.
pub const END_FADE: usize = 96;
/// Fade-in on every voice (0.5 ms): kills the step of a sample/oscillator
/// that starts away from zero without softening the transient audibly.
pub const START_FADE: usize = 24;

/// Mix one voice into `out` at `at` without discontinuities: a 0.5 ms fade-in, a fade-out over its last
/// samples, and a raised-cosine choke when a mono voice is cut by the next
/// note (`choke_after` samples after its start).
pub fn add_declicked(out: &mut [f32], at: usize, buf: &[f32], choke_after: Option<usize>) {
    if at >= out.len() || buf.is_empty() {
        return;
    }
    let fade = (CHOKE_FADE_S * SR) as usize;
    let mut n = buf.len().min(out.len() - at);
    let mut choke_from = usize::MAX;
    if let Some(c) = choke_after {
        if c + fade < n {
            n = c + fade;
            choke_from = c;
        }
    }
    let end_fade = END_FADE.min(n / 8).max(1);
    let start_fade = START_FADE.min(n / 8);
    for (j, (o, s)) in out[at..at + n].iter_mut().zip(buf.iter()).enumerate() {
        let mut g = 1.0f32;
        if j < start_fade {
            g *= (j as f32 + 0.5) / start_fade as f32;
        }
        let left = n - j;
        if left <= end_fade {
            g *= (left as f32 - 0.5).max(0.0) / end_fade as f32;
        }
        if j >= choke_from {
            let x = (j - choke_from) as f32 / fade as f32;
            g *= 0.5 + 0.5 * (std::f32::consts::PI * x.min(1.0)).cos();
        }
        *o += *s * g;
    }
}

/// Pre-fader audio of every track (instrument + track effects), reusable
/// while nothing but faders and the master changes. Gain staging and
/// mastering re-render the song several times with only those moved; with a
/// cache the tracks are synthesised once and the output stays bit-identical.
#[derive(Default)]
pub struct TrackCache {
    key: u64,
    tracks: HashMap<usize, std::sync::Arc<(Vec<f32>, Vec<f32>)>>,
}

impl TrackCache {
    /// Everything that shapes a track before its fader: the project with
    /// track faders, master gain and master chain set aside, plus the options.
    fn key_of(p: &Project, opts: &RenderOptions) -> u64 {
        let mut q = p.clone();
        for t in q.tracks.iter_mut() {
            t.volume_db = 0.0;
        }
        q.master_volume_db = 0.0;
        q.master_effects.clear();
        let s = format!(
            "{}|{:?}",
            serde_json::to_string(&q).unwrap_or_default(),
            opts
        );
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }
    pub fn len(&self) -> usize {
        self.tracks.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}

pub fn render(p: &Project, bank: &SampleBank, opts: &RenderOptions) -> Result<Mix> {
    render_cached(p, bank, opts, None)
}

/// `render`, reusing (and filling) a pre-fader track cache.
pub fn render_cached(
    p: &Project,
    bank: &SampleBank,
    opts: &RenderOptions,
    mut cache: Option<&mut TrackCache>,
) -> Result<Mix> {
    if let Some(c) = cache.as_deref_mut() {
        let k = TrackCache::key_of(p, opts);
        if c.key != k {
            c.key = k;
            c.tracks.clear();
        }
    }
    let empty = HashMap::new();
    let cached = cache.as_deref().map(|c| &c.tracks).unwrap_or(&empty);
    let keep = cache.is_some();
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

    // Tracks are independent until they are summed: render each track's
    // chain in parallel (a bounded batch at a time to cap memory), then sum
    // in track order so the mix is bit-identical to a serial render.
    struct TrackOut {
        audible: bool,
        l: Vec<f32>,
        r: Vec<f32>,
        pre: Option<(Vec<f32>, Vec<f32>)>,
        info: Option<TrackInfo>,
        fresh: Option<std::sync::Arc<(Vec<f32>, Vec<f32>)>>,
    }
    let render_track = |ti: usize| -> Option<TrackOut> {
        let track = &p.tracks[ti];
        let audible = !track.mute && (!any_solo || track.solo);
        if !audible && !opts.keep_stems && !opts.track_stats {
            return None;
        }
        let lanes = owner_lanes(p, &track.name);
        let (mut l, mut r, fresh) = match cached.get(&ti) {
            Some(b) => (b.0.clone(), b.1.clone(), None),
            None => {
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
                let mono_voice = matches!(track.instrument, Instrument::Bass808(_));
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
                        let a = render_note_slide(
                            &inst, e.pitch, e.vel, e.gate, bank, seed, e.slide_to,
                        );
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
                    // mono voices (808s) choke the previous note at the next onset
                    let choke_at = if mono_voice {
                        events[ti][k + 1..]
                            .iter()
                            .map(|x| x.start)
                            .find(|&s| s > e.start)
                            .map(|s| s - e.start)
                    } else {
                        None
                    };
                    add_declicked(&mut mono, e.start, buf, choke_at);
                    if spread > 0.0 {
                        add_declicked(&mut mono2, e.start, buf2, choke_at);
                    }
                }
                // pan (equal power), optionally automated
                let pan_lane = lanes
                    .iter()
                    .find(|x| parse_target(&x.param) == Some(Target::Pan));
                let pan_curve =
                    pan_lane.map(|lane| lane_curve(lane, total, &tl, |v| v.clamp(-1.0, 1.0)));
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
                let fresh = keep.then(|| std::sync::Arc::new((l.clone(), r.clone())));
                (l, r, fresh)
            }
        };
        let pre: Option<(Vec<f32>, Vec<f32>)> =
            if audible && track.sends.iter().any(|s| s.pre_fader) {
                Some((l.clone(), r.clone()))
            } else {
                None
            };
        apply_fader(&mut l, &mut r, track.volume_db, &lanes, &tl, None);
        let info = (opts.keep_stems || opts.track_stats).then(|| track_info(&track.name, &l, &r));
        Some(TrackOut {
            audible,
            l,
            r,
            pre,
            info,
            fresh,
        })
    };
    let mut fresh_tracks = Vec::new();
    let t_tracks = std::time::Instant::now();
    let batch = rayon::current_num_threads().clamp(1, 4);
    let order: Vec<usize> = (0..p.tracks.len()).collect();
    for chunk in order.chunks(batch) {
        use rayon::prelude::*;
        let outs: Vec<Option<TrackOut>> = chunk.par_iter().map(|&ti| render_track(ti)).collect();
        for (&ti, out) in chunk.iter().zip(outs) {
            let Some(TrackOut {
                audible,
                l,
                r,
                pre,
                info,
                fresh,
            }) = out
            else {
                continue;
            };
            if let Some(f) = fresh {
                fresh_tracks.push((ti, f));
            }
            let track = &p.tracks[ti];
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
            if let Some(info) = info {
                track_infos.push(info);
            }
            if opts.keep_stems {
                stems.push(Stem {
                    name: track.name.clone(),
                    left: l,
                    right: r,
                });
            }
        }
    }

    if let Some(c) = cache {
        c.tracks.extend(fresh_tracks);
    }
    crate::producer::prof("render.tracks", t_tracks);
    let t_bus = std::time::Instant::now();
    // buses: own chain + fader + balance, then into their output (another
    // bus = mixer insert routing, or the master), feeders before receivers
    let mut bus_stems = Vec::new();
    let mut bus_in: Vec<Option<(Vec<f32>, Vec<f32>)>> = bus_in.into_iter().map(Some).collect();
    for bi in bus_order(p) {
        let bus = &p.buses[bi];
        let Some((mut l, mut r)) = bus_in[bi].take() else {
            continue;
        };
        let lanes = owner_lanes(p, &bus.name);
        process_chain(&bus.effects, &lanes, &mut l, &mut r, &ctx, &tl);
        apply_fader(&mut l, &mut r, bus.volume_db, &lanes, &tl, Some(bus.pan));
        if !bus.mute {
            let dest = bus
                .output
                .as_deref()
                .and_then(|o| p.bus_index(o).ok())
                .filter(|d| bus_in[*d].is_some());
            let (dl, dr) = match dest {
                Some(d) => {
                    let (a, b) = bus_in[d].as_mut().unwrap();
                    (a, b)
                }
                None => (&mut ml, &mut mr),
            };
            for i in 0..total {
                dl[i] += l[i];
                dr[i] += r[i];
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

    crate::producer::prof("render.buses", t_bus);
    let t_master = std::time::Instant::now();
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
    crate::producer::prof("render.master", t_master);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Note, Track};

    /// The pre-fader track cache must not change a single sample: a render
    /// with faders/master moved and tracks reused equals a fresh render.
    #[test]
    fn track_cache_and_parallel_tracks_are_bit_identical() {
        let d = std::env::temp_dir().join("beatbox_render_cache_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = crate::engine::Engine::new(d);
        e.call(
            "generate_beat",
            &serde_json::json!({"style": "trap", "seed": 9}),
        )
        .unwrap();
        e.bank.sync(&e.project.samples);
        let o = RenderOptions {
            track_stats: true,
            ..Default::default()
        };
        let mut cache = TrackCache::default();
        let a = render_cached(&e.project, &e.bank, &o, Some(&mut cache)).unwrap();
        assert!(!cache.is_empty());
        // move faders and the master the way balance_mix / master_assistant do
        let mut q = e.project.clone();
        for t in q.tracks.iter_mut() {
            t.volume_db -= 2.5;
        }
        q.master_volume_db += 1.5;
        q.master_effects.clear();
        let cached = render_cached(&q, &e.bank, &o, Some(&mut cache)).unwrap();
        let fresh = render(&q, &e.bank, &o).unwrap();
        assert_eq!(cached.left, fresh.left);
        assert_eq!(cached.right, fresh.right);
        assert_ne!(a.left, fresh.left);
        // serial (1 thread) == parallel
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let serial = pool.install(|| render(&q, &e.bank, &o)).unwrap();
        assert_eq!(serial.left, fresh.left);
        assert_eq!(serial.right, fresh.right);
        // a composition change invalidates the cache
        let mut c = q.clone();
        if let Some(pt) = c.patterns.first_mut() {
            pt.clips.clear();
        }
        let n = render_cached(&c, &e.bank, &o, Some(&mut cache)).unwrap();
        assert_eq!(n.left, render(&c, &e.bank, &o).unwrap().left);
    }

    #[test]
    fn declick_fades_truncated_and_offset_voices() {
        // a buffer that stops at full scale and starts at full scale
        let buf = vec![0.8f32; 4000];
        let mut out = vec![0.0f32; 10000];
        add_declicked(&mut out, 1000, &buf, None);
        let d: f32 = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(d < 0.05, "max step {d}");
        assert!(crate::ears::clicks(&out, 10).is_empty());
    }

    #[test]
    fn mono_808_chokes_without_clicks() {
        let mut p = Project::new("c", 140.0);
        let inst = crate::instruments::preset("808").unwrap();
        p.tracks.push(Track::new("bass", inst));
        // overlapping long 808s on different pitches: the old note must stop
        // (mono) and the cut must be smooth
        let notes = vec![
            Note::new(0.0, 12.0, 36, 1.0),
            Note::new(3.0, 12.0, 41, 1.0),
            Note::new(7.0, 12.0, 43, 1.0),
            Note::new(10.0, 6.0, 36, 1.0),
        ];
        p.patterns[0].clips.insert("bass".into(), notes);
        p.master_effects.clear();
        let bank = SampleBank::default();
        let m = render(&p, &bank, &RenderOptions::default()).unwrap();
        let c = crate::ears::clicks(&m.left, 50);
        assert!(c.is_empty(), "clicks at {c:?}");
        // after the choke point (step 3), the first note is gone: the signal is
        // a single sine, so its peak stays below two summed voices
        let step = p.step_secs();
        let a = ((3.5 * step) * SR) as usize;
        let b = ((6.5 * step) * SR) as usize;
        let pk = m.left[a..b].iter().fold(0.0f32, |x, y| x.max(y.abs()));
        assert!(pk < 1.2, "peak {pk}");
    }
}
