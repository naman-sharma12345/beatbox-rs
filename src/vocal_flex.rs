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
        .map(|(a, b)| Phrase {
            start_s: (a as f32 * HOP - 0.03).max(0.0),
            end_s: (b as f32 * HOP + 0.06).min(len_s),
        })
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
        let prev_min = db[i - back..i]
            .iter()
            .cloned()
            .fold(f32::INFINITY, f32::min);
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
    /// Every anchor's time after the warp (moved or riding along), in order.
    #[serde(skip)]
    pub after: Vec<f32>,
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
pub fn grid_stats(
    onsets: &[f32],
    bpm: f32,
    start_beat: f32,
    grid_beats: f32,
    tol_s: f32,
) -> (f32, f32) {
    if onsets.is_empty() {
        return (0.0, 0.0);
    }
    let offs: Vec<f32> = onsets
        .iter()
        .map(|&t| nearest_grid(t, bpm, start_beat, grid_beats).0.abs())
        .collect();
    let on = offs.iter().filter(|&&o| o <= tol_s).count() as f32 / offs.len() as f32;
    let mean = offs.iter().sum::<f32>() / offs.len() as f32 * 1000.0;
    ((on * 1000.0).round() / 10.0, (mean * 10.0).round() / 10.0)
}

/// Pull each anchor toward its grid line by `strength` (0..1), capped at
/// `max_shift_s`; anchors keep their order and every segment's stretch stays
/// within 0.5..2.0. Returns the warped take (same start; pitch kept).
pub fn flex(
    x: &[f32],
    anchors: &[f32],
    bpm: f32,
    start_beat: f32,
    grid_beats: f32,
    strength: f32,
    max_shift_s: f32,
) -> (Vec<f32>, FlexResult) {
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
        moves.push(Move {
            from_s: t,
            to_s: to,
            grid_beat: g,
        });
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
    let y = if moves.is_empty() {
        x.to_vec()
    } else {
        crate::vocal::warp_audio(x, SR, out_len, map)
    };
    let tol = (grid_beats * 60.0 / bpm * 0.25).min(0.03);
    let (ob, mb) = grid_stats(anchors, bpm, start_beat, grid_beats, tol);
    let after: Vec<f32> = anchors
        .iter()
        .map(|&t| {
            moves
                .iter()
                .find(|m| (m.from_s - t).abs() < 1e-4)
                .map(|m| m.to_s)
                .unwrap_or_else(|| {
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
                })
        })
        .collect();
    let (oa, ma) = grid_stats(&after, bpm, start_beat, grid_beats, tol);
    (
        y,
        FlexResult {
            moves,
            mean_offset_ms_before: mb,
            mean_offset_ms_after: ma,
            on_grid_before: ob,
            on_grid_after: oa,
            min_stretch: (mn * 1000.0).round() / 1000.0,
            max_stretch: (mx * 1000.0).round() / 1000.0,
            after,
        },
    )
}

/// Where each phrase goes when whole phrases are placed on the grid: each
/// starts on the grid line nearest its natural time (gaps kept), but never
/// before the previous phrase has ended. Returns song beats.
pub fn place_phrases(
    ph: &[Phrase],
    bpm: f32,
    start_beat: f32,
    grid_beats: f32,
    keep_gaps: bool,
) -> Vec<f32> {
    let bps = bpm / 60.0;
    let mut out = Vec::new();
    let mut free = f32::NEG_INFINITY;
    let mut drift = 0.0f32;
    for p in ph {
        let natural = start_beat + p.start_s * bps + drift;
        let want = if keep_gaps {
            natural
        } else {
            free.max(start_beat)
        };
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

/// How well a set of onsets sits on the grid: the median distance to the
/// nearest grid line (ms) and the share within 25 ms.
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
pub struct LockStats {
    pub median_ms: f32,
    pub within_25ms_pct: f32,
}

/// The lock tolerance: an onset within 25 ms of a grid line reads as on it.
pub const LOCK_TOL_S: f32 = 0.025;

fn lock_stats(errs: &[f32]) -> LockStats {
    if errs.is_empty() {
        return LockStats {
            median_ms: 0.0,
            within_25ms_pct: 0.0,
        };
    }
    let mut e: Vec<f32> = errs.iter().map(|x| x.abs()).collect();
    e.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = e[e.len() / 2] * 1000.0;
    let within = e.iter().filter(|&&x| x <= LOCK_TOL_S + 1e-6).count() as f32 / e.len() as f32;
    LockStats {
        median_ms: (med * 10.0).round() / 10.0,
        within_25ms_pct: (within * 1000.0).round() / 10.0,
    }
}

/// Distance from `t` to the nearest point of the comb `phase + k * period`.
fn comb_err(t: f32, phase: f32, period: f32) -> f32 {
    let d = (t - phase).rem_euclid(period);
    if d > period * 0.5 {
        d - period
    } else {
        d
    }
}

/// The comb phase (0..period) that puts the most onsets within the lock
/// tolerance (ties: the smallest total distance), and how many it puts there.
pub fn best_phase(r: &[f32], period: f32) -> (f32, usize) {
    let (ph, hits, _) = best_phase_d(r, period);
    (ph, hits)
}

/// `best_phase` plus the total |onset - grid| distance at that phase (s).
fn best_phase_d(r: &[f32], period: f32) -> (f32, usize, f32) {
    if r.is_empty() || period <= 0.0 {
        return (0.0, 0, 0.0);
    }
    let steps = 48;
    let mut best = (0.0f32, 0usize, f32::INFINITY);
    for k in 0..steps {
        let ph = k as f32 * period / steps as f32;
        let mut hits = 0;
        let mut dist = 0.0;
        for &t in r {
            let d = comb_err(t, ph, period).abs();
            if d <= LOCK_TOL_S {
                hits += 1;
            }
            dist += d;
        }
        if hits > best.1 || (hits == best.1 && dist < best.2) {
            best = (ph, hits, dist);
        }
    }
    best
}

/// Tempo candidates for phrases that are each free to start anywhere: for
/// every bpm in lo..=hi (0.25 steps) each phrase gets its own best grid phase
/// and the score is the share of onsets on the grid, minus what the same
/// search catches on random onsets of the same count and span (so a finer
/// grid, i.e. a faster tempo, is not a free win: the best-of-48-phases pick
/// alone lands far more random onsets than 2*tol/period).
/// Best first: (bpm, score 0..1).
pub fn lock_tempo(groups: &[Vec<f32>], lo: f32, hi: f32, grid_beats: f32) -> Vec<(f32, f32)> {
    let n: usize = groups.iter().map(|g| g.len()).sum();
    if n == 0 {
        return Vec::new();
    }
    let (lo, hi) = (lo.min(hi).max(40.0), hi.max(lo).min(240.0));
    // random onset sets matched to each phrase (count and span), fixed seed
    const SIMS: usize = 6;
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rnd = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 40) as f32) / ((1u64 << 24) as f32)
    };
    let nulls: Vec<Vec<Vec<f32>>> = groups
        .iter()
        .map(|g| {
            let span = g.iter().cloned().fold(0.0f32, f32::max) + 0.05;
            (0..SIMS)
                .map(|_| g.iter().map(|_| rnd() * span).collect())
                .collect()
        })
        .collect();
    let mut out = Vec::new();
    let mut b = lo;
    while b <= hi + 1e-3 {
        let period = grid_beats * 60.0 / b;
        let (mut hits, mut dist, mut null_hits) = (0usize, 0.0f32, 0usize);
        for (g, ns) in groups.iter().zip(nulls.iter()) {
            let (_, h, d) = best_phase_d(g, period);
            hits += h;
            dist += d;
            for s in ns {
                null_hits += best_phase_d(s, period).1;
            }
        }
        let null = (null_hits as f32 / (SIMS * n) as f32).min(0.99);
        let score = (hits as f32 / n as f32 - null) / (1.0 - null);
        out.push((b, score, dist / n as f32));
        b += 0.25;
    }
    // equal hit scores (a phrase-free lock saturates easily): the tempo
    // whose onsets sit closest to its grid wins
    out.sort_by(|a, b| {
        let (sa, sb) = ((a.1 * 1000.0).round(), (b.1 * 1000.0).round());
        sb.partial_cmp(&sa)
            .unwrap()
            .then(a.2.partial_cmp(&b.2).unwrap())
    });
    out.into_iter().map(|(b, s, _)| (b, s)).collect()
}

/// Half/double time: a vocal that locks at b also locks at b/2 and 2b (and
/// often 2b/3, 3b/2). Among the candidates related to the best one by those
/// ratios and scoring within `OCTAVE_SLACK` of it, the one nearest `pref`
/// (log scale) goes first. Returns the reordered list and, when it moved,
/// the bpm it replaced.
pub const OCTAVE_SLACK: f32 = 0.03;
pub fn fold_tempo(c: Vec<(f32, f32)>, pref: f32) -> (Vec<(f32, f32)>, Option<f32>) {
    let Some(&(b0, s0)) = c.first() else {
        return (c, None);
    };
    let mut best = (b0, s0, (b0 / pref).ln().abs());
    for &(b, s) in &c {
        if s < s0 - OCTAVE_SLACK {
            continue;
        }
        let r = b / b0;
        let related = [0.5f32, 2.0, 2.0 / 3.0, 1.5]
            .iter()
            .any(|&k| (r / k - 1.0).abs() < 0.02);
        let d = (b / pref).ln().abs();
        if related && d + 0.05 < best.2 {
            best = (b, s, d);
        }
    }
    if best.0 == b0 {
        return (c, None);
    }
    let mut c = c;
    let k = c.iter().position(|x| x.0 == best.0).unwrap();
    let x = c.remove(k);
    c.insert(0, x);
    (c, Some(b0))
}

/// The default tempo search for a vocal with no bpm given.
pub const TEMPO_RANGE: (f32, f32) = (70.0, 140.0);

/// Knobs for `lock_vocal`.
#[derive(Clone, Debug)]
pub struct LockOpts {
    /// The song tempo; None = pick it from the vocal within `range`.
    pub bpm: Option<f32>,
    pub range: (f32, f32),
    /// Half/double-time candidates fold toward this tempo (see `fold_tempo`).
    pub pref_bpm: f32,
    /// The song beat the first phrase is placed near.
    pub start_beat: f32,
    pub grid_beats: f32,
    pub min_pause_s: f32,
    pub keep_gaps: bool,
    /// After placing, pull each syllable toward the grid inside its phrase
    /// (0 = phrases untouched, 1 = hard quantize), capped at `max_shift_s`.
    pub flex: f32,
    pub max_shift_s: f32,
}

impl Default for LockOpts {
    fn default() -> Self {
        LockOpts {
            bpm: None,
            range: TEMPO_RANGE,
            pref_bpm: 96.0,
            start_beat: 0.0,
            grid_beats: 0.25,
            min_pause_s: 0.3,
            keep_gaps: true,
            flex: 0.5,
            max_shift_s: 0.045,
        }
    }
}

/// One phrase as placed: where it is in the source take, where it starts in
/// the song, and where it lives in the locked take (`out_*`, the clip's
/// offset/length).
#[derive(Clone, Debug, Serialize)]
pub struct LockedPhrase {
    pub src_s: f32,
    pub len_s: f32,
    pub beat: f32,
    pub onsets: usize,
    pub out_offset_s: f32,
    pub out_len_s: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct LockReport {
    pub bpm: f32,
    pub bpm_picked: bool,
    pub grid_beats: f32,
    /// Best tempi with their lock score (when the tempo was picked).
    pub candidates: Vec<(f32, f32)>,
    /// The top-scoring tempo when half/double-time folding moved the pick.
    pub folded_from: Option<f32>,
    pub onsets: usize,
    pub phrases: usize,
    /// The take laid whole at the best single phase (what a free-time lay gets).
    pub free_time: LockStats,
    /// Each phrase on its own best grid phase, phrases untouched.
    pub placed: LockStats,
    /// After the in-phrase syllable pull (= placed when flex is 0).
    pub locked: LockStats,
}

/// Beat follows the vocal: find the take's phrases and syllables, pick (or
/// take) a tempo, start EACH phrase where its syllables land on the grid
/// (its own phase, near its natural time, gaps kept), optionally pull the
/// syllables inside each phrase toward the grid, and report the lock error
/// before and after. Returns the locked take (phrases back to back with
/// short silences; equal to the input when flex is 0) and the placements.
pub fn lock_vocal(x: &[f32], o: &LockOpts) -> (Vec<f32>, Vec<LockedPhrase>, LockReport) {
    let on = syllable_onsets(x, 0.5, 0.09);
    let ph = phrases(x, o.min_pause_s, None);
    let groups: Vec<Vec<f32>> = ph
        .iter()
        .map(|p| {
            on.iter()
                .filter(|&&t| t >= p.start_s && t < p.end_s)
                .map(|&t| t - p.start_s)
                .collect()
        })
        .collect();
    let g = o.grid_beats.max(0.05);
    let mut folded_from = None;
    let (bpm, picked, candidates) = match o.bpm {
        Some(b) => (b, false, Vec::new()),
        None => {
            let (c, folded) = fold_tempo(lock_tempo(&groups, o.range.0, o.range.1, g), o.pref_bpm);
            folded_from = folded;
            let b = c
                .first()
                .map(|x| x.0)
                .unwrap_or((o.range.0 + o.range.1) * 0.5);
            (
                b,
                true,
                c.into_iter()
                    .take(5)
                    .map(|(b, s)| (b, (s * 1000.0).round() / 1000.0))
                    .collect(),
            )
        }
    };
    let bps = bpm / 60.0;
    let period = g / bps;
    // free time: the whole take at one phase
    let (gph, _) = best_phase(&on, period);
    let free_time = lock_stats(
        &on.iter()
            .map(|&t| comb_err(t, gph, period))
            .collect::<Vec<_>>(),
    );
    let mut placed_err = Vec::new();
    let mut locked_err = Vec::new();
    let mut out_audio: Vec<f32> = Vec::new();
    let gap = (0.05 * SR) as usize;
    let mut placed = Vec::new();
    let mut free = f32::NEG_INFINITY;
    let mut drift = 0.0f32;
    for (p, r) in ph.iter().zip(groups.iter()) {
        let len = p.end_s - p.start_s;
        // the phase in seconds that puts this phrase's syllables on the comb;
        // a phrase with no onset found starts its sound (30 ms in) on a line
        let phi = if r.is_empty() {
            0.03f32.rem_euclid(period)
        } else {
            best_phase(r, period).0
        };
        let natural = if o.keep_gaps {
            o.start_beat + p.start_s * bps + drift
        } else {
            free.max(o.start_beat)
        };
        // song beat b with b + phi*bps on a grid line, nearest the natural spot
        let k = ((natural + phi * bps) / g).round();
        let mut b = k * g - phi * bps;
        while b < free - 1e-4 || b < -1e-4 {
            b += g;
        }
        if o.keep_gaps {
            drift = b - (o.start_beat + p.start_s * bps);
        }
        free = b + len * bps;
        let line0 = b; // song beat of segment time 0
        for &t in r {
            let sb = line0 + t * bps;
            placed_err.push(((sb / g).round() * g - sb).abs() / bps);
        }
        let (s0, s1) = (
            (p.start_s * SR) as usize,
            ((p.end_s * SR) as usize).min(x.len()),
        );
        let seg = &x[s0.min(s1)..s1];
        let (y, after) = if o.flex > 0.0 && !r.is_empty() {
            let (y, res) = flex(seg, r, bpm, line0, g, o.flex, o.max_shift_s);
            (y, res.after)
        } else {
            (Vec::new(), r.clone())
        };
        for &t in &after {
            let sb = line0 + t * bps;
            locked_err.push(((sb / g).round() * g - sb).abs() / bps);
        }
        let (off, olen) = if o.flex > 0.0 {
            let off = out_audio.len();
            let body: &[f32] = if y.is_empty() { seg } else { &y };
            out_audio.extend_from_slice(body);
            out_audio.extend(std::iter::repeat_n(0.0, gap));
            (off as f32 / SR, body.len() as f32 / SR)
        } else {
            (p.start_s, len)
        };
        placed.push(LockedPhrase {
            src_s: r3(p.start_s),
            len_s: r3(len),
            beat: (b * 10000.0).round() / 10000.0,
            onsets: r.len(),
            out_offset_s: off,
            out_len_s: olen,
        });
    }
    let out = if o.flex > 0.0 { out_audio } else { x.to_vec() };
    let report = LockReport {
        bpm: (bpm * 100.0).round() / 100.0,
        bpm_picked: picked,
        grid_beats: g,
        candidates,
        folded_from,
        onsets: on.len(),
        phrases: ph.len(),
        free_time,
        placed: lock_stats(&placed_err),
        locked: lock_stats(&locked_err),
    };
    (out, placed, report)
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
    let info = crate::samples::SampleInfo {
        name,
        path: path.to_string_lossy().into(),
        source,
        license: "own recording".into(),
        author: String::new(),
        duration: 0.0,
    };
    crate::tools::register_sample(e, info)
}

fn split_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (name, x) = take(e, a)?;
    let ph = phrases(
        &x,
        f_or(a, "min_pause_s", 0.3).clamp(0.08, 3.0),
        f_opt(a, "threshold_db"),
    );
    if ph.is_empty() {
        bail!("no voiced phrase found (try threshold_db -45 or a shorter min_pause_s)");
    }
    let prefix = s_opt(a, "prefix").unwrap_or_else(|| format!("{name}_ph"));
    let save = b_or(a, "save", true);
    let mut out = Vec::new();
    for (i, p) in ph.iter().enumerate() {
        let (s0, s1) = (
            (p.start_s * SR) as usize,
            ((p.end_s * SR) as usize).min(x.len()),
        );
        let mut seg = x[s0..s1].to_vec();
        crate::audio_edit::fade(&mut seg, 5.0, 20.0);
        let mut row = json!({"index": i, "start_s": r3(p.start_s), "end_s": r3(p.end_s), "seconds": r3(p.end_s - p.start_s)});
        if save {
            let reg = register(
                e,
                &format!("{prefix}{:02}", i + 1),
                &seg,
                format!(
                    "phrase {} of {name} ({:.2}-{:.2} s)",
                    i + 1,
                    p.start_s,
                    p.end_s
                ),
            )?;
            row["sample"] = reg["sample"].clone();
        }
        out.push(row);
    }
    let pauses: Vec<Value> = ph
        .windows(2)
        .map(|w| json!({"from_s": r3(w[0].end_s), "to_s": r3(w[1].start_s)}))
        .collect();
    if save {
        e.revision += 1;
    }
    Ok(
        json!({"source": name, "phrases": out, "pauses": pauses, "count": ph.len(), "next": ["add_audio_clip each phrase where you want it", "place_vocal_phrases to put them all on the grid in one go"]}),
    )
}

fn onsets_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (name, x) = take(e, a)?;
    let on = syllable_onsets(
        &x,
        f_or(a, "sensitivity", 0.5),
        f_or(a, "min_gap_ms", 90.0) / 1000.0,
    );
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
        "phrase" => phrases(&x, f_or(a, "min_pause_s", 0.3), None)
            .iter()
            .map(|p| p.start_s + 0.03)
            .collect(),
        "syllable" | "word" => syllable_onsets(&x, f_or(a, "sensitivity", 0.5), 0.09),
        u => bail!("unit '{u}': use syllable or phrase"),
    };
    if anchors.is_empty() {
        bail!("no onsets found in '{}'", info.name);
    }
    let (y, res) = flex(&x, &anchors, bpm, sb, gb, strength, max_shift);
    let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_flex", info.name));
    let reg = register(
        e,
        &nn,
        &y,
        format!(
            "{} (flex: {unit} onsets to {gb} beats, strength {strength})",
            info.source
        ),
    )?;
    let new = reg["sample"].as_str().unwrap_or(&nn).to_string();
    // clips that played the old take now play the aligned one
    let mut switched = 0;
    if b_or(a, "replace_clips", true) {
        for c in e.project.audio_clips.iter_mut() {
            if c.sample.eq_ignore_ascii_case(&info.name)
                && (c.start_beat - sb).abs() < 1e-3
                && c.offset_s.abs() < 1e-6
            {
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
        e.project
            .audio_clips
            .retain(|c| !(c.track == track && c.sample.eq_ignore_ascii_case(&info.name)));
    }
    let gain = f_or(a, "gain_db", 0.0);
    let mut placed = Vec::new();
    for (p, &beat) in ph.iter().zip(at.iter()) {
        e.project.audio_clips.push(AudioClip {
            track: track.clone(),
            sample: info.name.clone(),
            start_beat: beat,
            offset_s: p.start_s,
            length_s: Some(p.end_s - p.start_s),
            gain_db: gain,
            pitch_st: 0.0,
            fade_in_ms: None,
            fade_out_ms: None,
            id: String::new(),
        });
        let natural = sb + p.start_s * bpm / 60.0;
        placed.push(json!({"from_s": r3(p.start_s), "to_s": r3(p.end_s), "beat": r3(beat), "bar": (beat / 4.0).floor() as i64 + 1, "moved_ms": r3((beat - natural) * 60.0 / bpm * 1000.0)}));
    }
    e.revision += 1;
    Ok(
        json!({"track": track, "sample": info.name, "grid_beats": gb, "keep_gaps": keep, "phrases": placed.len(), "placed": placed, "clips": e.project.audio_clips.len(), "next": ["align_vocal_to_grid for word-level tightening", "measure_vocal_vs_beat to check level and timing"]}),
    )
}

fn lock_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (name, x) = take(e, a)?;
    let track = match s_opt(a, "track") {
        Some(t) => {
            let ti = e.project.track_index(&t)?;
            Some(e.project.tracks[ti].name.clone())
        }
        None => None,
    };
    let auto = matches!(a.get("bpm"), Some(Value::String(s)) if s == "auto")
        || (a.get("bpm").is_none() && track.is_none());
    let bpm = if auto {
        None
    } else {
        Some(f_opt(a, "bpm").unwrap_or(e.project.bpm))
    };
    let range = match a.get("bpm_range").and_then(|v| v.as_array()) {
        Some(r) if r.len() == 2 => (
            r[0].as_f64().unwrap_or(TEMPO_RANGE.0 as f64) as f32,
            r[1].as_f64().unwrap_or(TEMPO_RANGE.1 as f64) as f32,
        ),
        Some(_) => bail!("bpm_range is [low, high], e.g. [70, 140]"),
        None => TEMPO_RANGE,
    };
    let o = LockOpts {
        bpm,
        range,
        pref_bpm: f_or(a, "pref_bpm", 96.0).clamp(40.0, 240.0),
        start_beat: f_or(a, "start_beat", 0.0),
        grid_beats: grid_of(a, "1/16")?,
        min_pause_s: f_or(a, "min_pause_s", 0.3).clamp(0.08, 3.0),
        keep_gaps: b_or(a, "keep_gaps", true),
        flex: f_or(a, "flex", 0.5).clamp(0.0, 1.0),
        max_shift_s: f_or(a, "max_shift_ms", 45.0).clamp(5.0, 200.0) / 1000.0,
    };
    let (y, placed, rep) = lock_vocal(&x, &o);
    if placed.is_empty() {
        bail!("no voiced phrase found in '{name}'");
    }
    let mut out = json!({
        "source": name,
        "bpm": rep.bpm,
        "lock": rep,
        "placed": placed.iter().take(u_or(a, "max_rows", 16) as usize).map(|p| json!({"from_s": p.src_s, "len_s": p.len_s, "beat": r3(p.beat), "bar": (p.beat / 4.0).floor() as i64 + 1, "onsets": p.onsets})).collect::<Vec<_>>(),
        "note": "lock = onset-to-grid error: median ms and the share within 25 ms; free_time = the take laid whole at one tempo, placed = each phrase on its own best spot, locked = after the syllable pull (flex)",
    });
    if b_or(a, "set_bpm", false) && rep.bpm_picked {
        e.project.bpm = rep.bpm;
        out["bpm_set"] = json!(true);
    }
    let Some(track) = track else {
        out["next"] = json!(["give track (and the sample) to place it; set_bpm:true makes the project play at the picked tempo"]);
        return Ok(out);
    };
    let sample = s_opt(a, "sample").ok_or_else(|| {
        anyhow::anyhow!("placing needs a registered sample (import_sample first), not a path")
    })?;
    let sample = crate::tools_sound::sample_data(e, &sample)?.0.name;
    let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{sample}_locked"));
    if b_or(a, "replace", true) {
        // a re-lock replaces the last locked take instead of piling up copies
        e.project.audio_clips.retain(|c| {
            !(c.track == track
                && (c.sample.eq_ignore_ascii_case(&sample) || c.sample.eq_ignore_ascii_case(&nn)))
        });
        if !e
            .project
            .audio_clips
            .iter()
            .any(|c| c.sample.eq_ignore_ascii_case(&nn))
        {
            e.project
                .samples
                .retain(|s| !s.name.eq_ignore_ascii_case(&nn));
        }
    }
    let clip_sample = if o.flex > 0.0 {
        let reg = register(
            e,
            &nn,
            &y,
            format!(
                "{sample} locked to {} bpm (phrases placed, syllables pulled {:.0}%)",
                rep.bpm,
                o.flex * 100.0
            ),
        )?;
        reg["sample"].as_str().unwrap_or(&nn).to_string()
    } else {
        sample.clone()
    };
    let gain = f_or(a, "gain_db", 0.0);
    for p in &placed {
        e.project.audio_clips.push(AudioClip {
            track: track.clone(),
            sample: clip_sample.clone(),
            start_beat: p.beat,
            offset_s: p.out_offset_s,
            length_s: Some(p.out_len_s),
            gain_db: gain,
            pitch_st: 0.0,
            fade_in_ms: Some(8.0),
            fade_out_ms: Some(40.0),
            id: String::new(),
        });
    }
    e.revision += 1;
    out["track"] = json!(track);
    out["sample"] = json!(clip_sample);
    out["phrases"] = json!(placed.len());
    out["next"] = json!(["measure_vocal_vs_beat to check level and timing on the render"]);
    Ok(out)
}

fn mono(l: &[f32], r: &[f32]) -> Vec<f32> {
    l.iter().zip(r.iter()).map(|(x, y)| 0.5 * (x + y)).collect()
}

pub(crate) fn band_rms_db(x: &[f32], lo: f32, hi: f32) -> f32 {
    let mut h = Biquad::new(BiquadKind::LowCut, lo, 0.707, 0.0);
    let mut l = Biquad::new(BiquadKind::HighCut, hi, 0.707, 0.0);
    let e: f64 = x
        .iter()
        .map(|v| {
            let y = l.process(h.process(*v));
            (y * y) as f64
        })
        .sum::<f64>()
        / x.len().max(1) as f64;
    (10.0 * e.max(1e-12).log10()) as f32
}

/// The track's audio clips summed dry (gain only, no FX) on the song
/// timeline, `n` samples long; None with no clips or a tempo curve.
fn dry_clips(e: &mut Engine, track: &str, n: usize) -> Option<Vec<f32>> {
    if !e.project.tempo_points.is_empty() {
        return None;
    }
    e.bank.sync(&e.project.samples);
    let bs = 60.0 / e.project.bpm;
    let mut out = vec![0.0f32; n];
    let mut any = false;
    for c in e
        .project
        .audio_clips
        .iter()
        .filter(|c| c.track.eq_ignore_ascii_case(track))
    {
        let Some(d) = e.bank.get(&c.sample) else {
            continue;
        };
        let off = ((c.offset_s.max(0.0) * SR) as usize).min(d.len());
        let len = c
            .length_s
            .map(|l| ((l.max(0.0) * SR) as usize).min(d.len() - off))
            .unwrap_or(d.len() - off);
        let at = (c.start_beat * bs * SR).round() as i64;
        let g = crate::dsp::db_to_gain(c.gain_db);
        for i in 0..len {
            let k = at + i as i64;
            if k >= 0 && (k as usize) < n {
                out[k as usize] += d[off + i] * g;
                any = true;
            }
        }
    }
    any.then_some(out)
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
    let s1 = f_opt(a, "to_beat")
        .map(|b| ((b * bs * SR) as usize).min(n))
        .unwrap_or(n);
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
    // no beat under the vocal (an a cappella window): there is nothing to be over
    let no_beat = band_rms_db(&bm, 40.0, 16000.0) < -90.0;
    let full_db = band_rms_db(&vm, 40.0, 16000.0) - band_rms_db(&bm, 40.0, 16000.0);
    let pres_db = band_rms_db(&vm, 1000.0, 4000.0) - band_rms_db(&bm, 1000.0, 4000.0);
    let body_db = band_rms_db(&vm, 200.0, 800.0) - band_rms_db(&bm, 200.0, 800.0);
    let gb = grid_of(a, "1/16")?;
    let on: Vec<f32> = syllable_onsets(&v, 0.5, 0.09);
    let start_beat = s0 as f32 / SR / bs;
    // the same timing measured on the dry clips (what align_vocal_to_grid and
    // detect_vocal_onsets see): no inserts, no reverb/delay returns, no
    // doubles. When the two disagree, the track's FX are adding onsets.
    let dry = dry_clips(e, &tname, n);
    let dry_timing = dry.as_ref().map(|d| {
        let seg = &d[s0..s1.min(d.len())];
        let on_d = syllable_onsets(seg, 0.5, 0.09);
        let tol_d = (gb * bs * 0.25).min(0.03);
        let (g, m) = grid_stats(&on_d, bpm, start_beat, gb, tol_d);
        let mut sg: Vec<f32> = on_d.iter().map(|&t| -nearest_grid(t, bpm, start_beat, gb).0 * 1000.0).collect();
        sg.sort_by(|x, y| x.partial_cmp(y).unwrap());
        json!({"syllables": on_d.len(), "on_grid_pct": g, "mean_offset_ms": m, "median_signed_ms": (sg.get(sg.len() / 2).copied().unwrap_or(0.0) * 10.0).round() / 10.0})
    });
    let tol = (gb * bs * 0.25).min(0.03);
    let (on_grid, mean_ms) = grid_stats(&on, bpm, start_beat, gb, tol);
    // signed offsets: early (-) or late (+) against the nearest grid line
    let mut signed: Vec<f32> = on
        .iter()
        .map(|&t| -nearest_grid(t, bpm, start_beat, gb).0 * 1000.0)
        .collect();
    signed.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let median = signed.get(signed.len() / 2).copied().unwrap_or(0.0);
    let mut verdict = Vec::new();
    if no_beat {
        verdict.push("no beat plays under the vocal in this window, so its level over the beat is not measured".to_string());
    } else if pres_db < 3.0 {
        verdict.push(format!("the vocal is only {pres_db:+.1} dB over the beat at 1-4 kHz: words will be hard to hear (aim for +6 or more)"));
    }
    if full_db < -2.0 && !no_beat {
        verdict.push(format!(
            "the vocal is {full_db:+.1} dB under the beat overall"
        ));
    }
    if on_grid < 50.0 {
        verdict.push(format!("only {on_grid}% of syllables land within {:.0} ms of the grid: try align_vocal_to_grid or place_vocal_phrases", tol * 1000.0));
    }
    if median.abs() > 15.0 {
        verdict.push(format!(
            "the vocal sits {} by about {:.0} ms (median)",
            if median > 0.0 { "late" } else { "early" },
            median.abs()
        ));
    }
    if let Some(d) = &dry_timing {
        let g = d["on_grid_pct"].as_f64().unwrap_or(0.0) as f32;
        if g - on_grid > 15.0 {
            verdict.push(format!("the dry clips are {g}% on the grid but the rendered track only {on_grid}%: its effects (delay/reverb returns, doubles, saturation) add onsets that land off the grid; the timing itself is fine"));
        }
    }
    if verdict.is_empty() {
        verdict.push("the vocal sits over the beat and on the grid".into());
    }
    Ok(json!({
        "track": tname,
        "window_s": [r3(s0 as f32 / SR), r3(s1 as f32 / SR)],
        "vocal_over_beat_db": if no_beat { Value::Null } else { json!({"full_band": (full_db * 10.0).round() / 10.0, "presence_1_4k": (pres_db * 10.0).round() / 10.0, "body_200_800": (body_db * 10.0).round() / 10.0}) },
        "timing": {"grid_beats": gb, "syllables": on.len(), "on_grid_pct": on_grid, "mean_offset_ms": mean_ms, "median_signed_ms": (median * 10.0).round() / 10.0, "tolerance_ms": r3(tol * 1000.0), "measured_on": "the rendered track (its inserts, sends' returns and every clip on it)"},
        "timing_dry_clips": dry_timing,
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
            name: "lock_vocal_to_beat",
            description: "Beat follows the vocal: for a take with no steady pulse (a free-time a cappella), find its phrases and syllables, pick the tempo that locks it best (bpm 'auto', within bpm_range, default [70,140]; half/double-time ties fold toward pref_bpm, default 96) or use the song's, start EACH phrase where its syllables land on the 1/16 grid (its own offset, near its natural time, gaps kept), pull the syllables inside each phrase toward the grid (flex 0..1, default 0.5, max_shift_ms 45; 0 keeps phrases untouched), and report the lock: median onset-to-grid ms and % within 25 ms for the take laid free-time, placed, and locked. Without track it only analyses; with track + sample it places the clips (playing <sample>_locked when flex > 0). set_bpm:true sets the project tempo to the picked one.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "path": {"type": "string"}, "track": {"type": "string"}, "bpm": {"anyOf": [{"type": "number"}, {"type": "string", "enum": ["auto"]}], "description": "number, or 'auto' to pick from the vocal (default: auto without track, the song's with track)"}, "bpm_range": {"type": "array", "items": {"type": "number"}}, "pref_bpm": {"type": "number"}, "set_bpm": {"type": "boolean"}, "start_beat": {"type": "number"}, "grid": {"description": "default '1/16'"}, "flex": {"type": "number"}, "max_shift_ms": {"type": "number"}, "min_pause_s": {"type": "number"}, "keep_gaps": {"type": "boolean"}, "gain_db": {"type": "number"}, "replace": {"type": "boolean"}, "new_name": {"type": "string"}, "max_rows": {"type": "integer"}}), &[]),
            run: lock_tool,
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
                    x[s + i] += 0.4
                        * env
                        * ((ph * 300.0 * std::f32::consts::TAU).sin()
                            + 0.5 * (ph * 900.0 * std::f32::consts::TAU).sin());
                }
            }
        }
        x
    }

    /// Phrases whose syllables sit on a 96 bpm 16th grid inside each phrase,
    /// but each phrase starts at a free (off-grid) moment: a free-time take.
    fn free_time_take() -> (Vec<f32>, f32) {
        let s16 = 60.0 / 96.0 / 4.0;
        let pos = [0.0, 2.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 13.0, 14.0];
        // each pause lands its phrase 39 ms further round the 16th grid
        // (k * 1/16 + 0.039 - 0.12 s), so one free-time grid cannot hold
        // the whole take while every phrase on its own spot can
        let gaps = [0.388, 1.169, 0.857, 1.326, 0.700, 1.013, 0.544, 0.700];
        let mut times = Vec::new();
        let mut t0 = 0.4f32;
        let mut rng = crate::dsp::Rng::new(11);
        for g in gaps {
            for p in pos {
                times.push(t0 + p * s16 + 0.004 * rng.bipolar());
            }
            t0 += 14.0 * s16 + 0.12 + g;
        }
        (take_at(&times, t0 + 0.5), s16)
    }

    #[test]
    fn lock_picks_the_tempo_and_locks_each_phrase() {
        let (x, _) = free_time_take();
        let (y, placed, rep) = lock_vocal(
            &x,
            &LockOpts {
                flex: 0.0,
                ..Default::default()
            },
        );
        assert!((rep.bpm - 96.0).abs() <= 1.0, "{rep:?}");
        assert!(rep.bpm_picked && !rep.candidates.is_empty());
        assert_eq!(rep.phrases, 8, "{rep:?}");
        assert_eq!(placed.len(), 8);
        // free time cannot lock the whole take; each phrase on its own spot can
        assert!(rep.free_time.within_25ms_pct < 75.0, "{rep:?}");
        assert!(
            rep.placed.within_25ms_pct >= 90.0 && rep.placed.median_ms <= 12.0,
            "{rep:?}"
        );
        // flex 0 plays the take itself; phrases never overlap and keep order
        assert_eq!(y.len(), x.len());
        for w in placed.windows(2) {
            assert!(
                w[1].beat >= w[0].beat + w[0].len_s * 96.0 / 60.0 - 1e-3,
                "{placed:?}"
            );
        }
        // the syllable pull only tightens
        let (_, _, r2) = lock_vocal(
            &x,
            &LockOpts {
                bpm: Some(rep.bpm),
                flex: 0.6,
                ..Default::default()
            },
        );
        assert!(
            r2.locked.within_25ms_pct >= r2.placed.within_25ms_pct,
            "{r2:?}"
        );
        assert!(r2.locked.median_ms <= r2.placed.median_ms + 0.5, "{r2:?}");
    }

    #[test]
    fn lock_tool_analyses_and_places_clips() {
        let dir = std::env::temp_dir().join("beatbox_vocal_lock_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("new_project", &json!({"name": "lock", "bpm": 90}))
            .unwrap();
        let (x, _) = free_time_take();
        let wav = dir.join("take.wav");
        crate::render::write_wav(&wav, &x, &x).unwrap();
        e.call(
            "import_sample",
            &json!({"path": wav.to_string_lossy(), "name": "take"}),
        )
        .unwrap();
        let a = e
            .call(
                "lock_vocal_to_beat",
                &json!({"sample": "take", "set_bpm": true}),
            )
            .unwrap();
        assert!((a["bpm"].as_f64().unwrap() - 96.0).abs() <= 1.0, "{a}");
        assert!(
            (e.project.bpm - 96.0).abs() <= 1.0,
            "set_bpm moves the project tempo"
        );
        e.call("add_track", &json!({"name": "vocal", "preset": "pad"}))
            .unwrap();
        let p = e
            .call(
                "lock_vocal_to_beat",
                &json!({"sample": "take", "track": "vocal", "start_beat": 4}),
            )
            .unwrap();
        assert_eq!(p["phrases"], 8, "{p}");
        assert_eq!(p["sample"], "take_locked", "{p}");
        let clips: Vec<_> = e
            .project
            .audio_clips
            .iter()
            .filter(|c| c.track == "vocal")
            .collect();
        assert_eq!(clips.len(), 8);
        assert!(clips
            .iter()
            .all(|c| c.sample == "take_locked" && c.start_beat >= 3.0));
        assert!(
            p["lock"]["locked"]["within_25ms_pct"].as_f64().unwrap() >= 85.0,
            "{p}"
        );
        // a second lock replaces the first (no duplicate clips or samples)
        e.call(
            "lock_vocal_to_beat",
            &json!({"sample": "take", "track": "vocal", "start_beat": 4}),
        )
        .unwrap();
        assert_eq!(
            e.project
                .audio_clips
                .iter()
                .filter(|c| c.track == "vocal")
                .count(),
            8
        );
        assert_eq!(
            e.project
                .samples
                .iter()
                .filter(|s| s.name.starts_with("take_locked"))
                .count(),
            1
        );
    }

    #[test]
    fn tempo_search_over_70_140_does_not_drift_to_the_fast_edge() {
        // 12 phrases of syllables on 96 bpm 16ths (+-8 ms), each phrase at
        // its own offset: the 70-140 search must find 96, not the top edge
        // where a finer grid catches more by chance.
        let mut seed: u32 = 7;
        let mut r = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1u32 << 24) as f32
        };
        let six = 60.0 / 96.0 / 4.0;
        let groups: Vec<Vec<f32>> = (0..12)
            .map(|_| {
                let mut slots: Vec<u32> = (0..32).filter(|_| r() < 0.35).collect();
                slots.dedup();
                slots
                    .iter()
                    .map(|&k| k as f32 * six + (r() - 0.5) * 0.016)
                    .collect()
            })
            .collect();
        let c = lock_tempo(&groups, TEMPO_RANGE.0, TEMPO_RANGE.1, 0.25);
        let (c, _) = fold_tempo(c, 96.0);
        assert!((c[0].0 - 96.0).abs() <= 1.0, "picked {:?}", &c[..5]);
    }

    #[test]
    fn half_and_double_time_fold_toward_the_preferred_tempo() {
        let c = vec![(140.0, 0.50), (70.0, 0.49), (100.0, 0.20)];
        let (f, from) = fold_tempo(c.clone(), 96.0);
        assert_eq!((f[0].0, from), (70.0, Some(140.0)));
        let (f, from) = fold_tempo(c.clone(), 130.0);
        assert_eq!((f[0].0, from), (140.0, None));
        // a related tempo that scores clearly worse does not win
        let (f, _) = fold_tempo(vec![(140.0, 0.50), (70.0, 0.40)], 96.0);
        assert_eq!(f[0].0, 140.0);
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
        let ph = vec![
            Phrase {
                start_s: 0.13,
                end_s: 1.2,
            },
            Phrase {
                start_s: 1.9,
                end_s: 2.6,
            },
        ];
        let at = place_phrases(&ph, 120.0, 4.0, 1.0, true);
        assert_eq!(at, vec![4.0, 8.0]);
        let packed = place_phrases(&ph, 120.0, 4.0, 1.0, false);
        assert_eq!(packed[0], 4.0);
        assert!(
            packed[1] >= 4.0 + 1.07 * 2.0 && packed[1] <= 7.0,
            "{packed:?}"
        );
    }

    #[test]
    fn tools_place_and_measure_a_take() {
        let dir = std::env::temp_dir().join("beatbox_vocal_flex_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("new_project", &json!({"name": "flex", "bpm": 120}))
            .unwrap();
        let x = take_at(&[0.53, 0.83, 1.13, 2.53, 2.83], 3.5);
        let wav = dir.join("take.wav");
        crate::render::write_wav(&wav, &x, &x).unwrap();
        e.call(
            "import_sample",
            &json!({"path": wav.to_string_lossy(), "name": "take"}),
        )
        .unwrap();
        let s = e
            .call("split_vocal_at_pauses", &json!({"sample": "take"}))
            .unwrap();
        assert_eq!(s["count"], 2, "{s}");
        e.call("add_track", &json!({"name": "vocal", "preset": "pad"}))
            .unwrap();
        let p = e
            .call(
                "place_vocal_phrases",
                &json!({"sample": "take", "track": "vocal", "start_beat": 0}),
            )
            .unwrap();
        assert_eq!(p["phrases"], 2, "{p}");
        let a = e
            .call(
                "align_vocal_to_grid",
                &json!({"sample": "take", "strength": 1.0}),
            )
            .unwrap();
        assert!(
            a["mean_offset_ms"]["after"].as_f64().unwrap()
                <= a["mean_offset_ms"]["before"].as_f64().unwrap(),
            "{a}"
        );
        let m = e
            .call("measure_vocal_vs_beat", &json!({"track": "vocal"}))
            .unwrap();
        assert!(m["timing"]["syllables"].as_u64().unwrap() >= 3, "{m}");
        // no beat in this project: the level over the beat is not invented
        assert!(m["vocal_over_beat_db"].is_null(), "{m}");
        // dry clips and the rendered track agree when the track has no FX
        let d = &m["timing_dry_clips"];
        assert!(d.is_object(), "{m}");
        let (g_dry, g_wet) = (
            d["on_grid_pct"].as_f64().unwrap(),
            m["timing"]["on_grid_pct"].as_f64().unwrap(),
        );
        assert!((g_dry - g_wet).abs() <= 20.0, "{m}");
        // a 3-step echo on the track adds onsets to the render only
        e.call("add_effect", &json!({"track": "vocal", "type": "delay", "params": {"steps": 1.3, "mix": 0.9, "feedback": 0.6}})).unwrap();
        let m2 = e
            .call("measure_vocal_vs_beat", &json!({"track": "vocal"}))
            .unwrap();
        assert_eq!(
            m2["timing_dry_clips"]["on_grid_pct"], m["timing_dry_clips"]["on_grid_pct"],
            "dry timing ignores FX: {m2}"
        );
    }
}
