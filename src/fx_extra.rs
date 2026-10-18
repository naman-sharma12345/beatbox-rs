//! Studio effects added in sprint 4: parametric EQ, multiband compressor,
//! dynamic EQ, de-esser, soft clipper, gate, phaser, flanger, tempo-synced
//! stutter / gross-beat gate, pitch shifter, convolution reverb with
//! synthesized impulse responses, and auto-pan / tremolo.
//!
//! Every processor is a whole-buffer stereo function (like the rest of the
//! rack), deterministic and allocation-light.

use crate::dsp::tape_dc::{self, TapeDcGuard};
use crate::dsp::*;
use crate::fx::*;
use std::f32::consts::PI;

fn coef(ms: f32) -> f32 {
    (-1.0 / (ms.max(0.05) * 0.001 * SR)).exp()
}

// ---------------- parametric EQ ----------------

pub fn band_filter(b: &EqBand) -> Biquad {
    let kind = match b.kind {
        EqBandKind::Bell => BiquadKind::Bell,
        EqBandKind::LowShelf => BiquadKind::LowShelf,
        EqBandKind::HighShelf => BiquadKind::HighShelf,
        EqBandKind::LowCut => BiquadKind::LowCut,
        EqBandKind::HighCut => BiquadKind::HighCut,
        EqBandKind::Notch => BiquadKind::Notch,
    };
    Biquad::new(kind, b.freq, b.q, b.gain_db.clamp(-24.0, 24.0))
}

pub fn parametric_eq(p: &ParametricEqFx, l: &mut [f32], r: &mut [f32]) {
    let out = db_to_gain(p.output_db);
    for ch in [&mut *l, &mut *r] {
        for b in p.bands.iter().filter(|b| b.enabled) {
            // cuts are 12 dB/oct per stage; `slope` stacks stages (24, 48 dB/oct)
            let stages = if matches!(b.kind, EqBandKind::LowCut | EqBandKind::HighCut) {
                b.stages.clamp(1, 4)
            } else {
                1
            };
            for _ in 0..stages {
                let mut f = band_filter(b);
                for s in ch.iter_mut() {
                    *s = f.process(*s);
                }
            }
        }
        if out != 1.0 {
            for s in ch.iter_mut() {
                *s *= out;
            }
        }
    }
}

/// Combined magnitude response (dB) of an EQ at `freq` — for GUIs and describe.
pub fn eq_response_db(p: &ParametricEqFx, freq: f32) -> f32 {
    p.bands
        .iter()
        .filter(|b| b.enabled)
        .map(|b| {
            let stages = if matches!(b.kind, EqBandKind::LowCut | EqBandKind::HighCut) {
                b.stages.clamp(1, 4) as f32
            } else {
                1.0
            };
            band_filter(b).response_db(freq) * stages
        })
        .sum::<f32>()
        + p.output_db
}

// ---------------- multiband compressor ----------------

/// Split into low / mid / high with Linkwitz-Riley-style lowpasses and
/// complementary subtraction (bands always sum back to the input).
fn split3(x: &[f32], f_lo: f32, f_hi: f32) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let n = x.len();
    let (mut lo, mut mid, mut hi) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    let q = std::f32::consts::FRAC_1_SQRT_2;
    let (mut a1, mut a2) = (
        Biquad::new(BiquadKind::HighCut, f_lo, q, 0.0),
        Biquad::new(BiquadKind::HighCut, f_lo, q, 0.0),
    );
    let (mut b1, mut b2) = (
        Biquad::new(BiquadKind::HighCut, f_hi, q, 0.0),
        Biquad::new(BiquadKind::HighCut, f_hi, q, 0.0),
    );
    for i in 0..n {
        let l = a2.process(a1.process(x[i]));
        let rest = x[i] - l;
        let m = b2.process(b1.process(rest));
        lo[i] = l;
        mid[i] = m;
        hi[i] = rest - m;
    }
    (lo, mid, hi)
}

struct Comp {
    env: f32,
    att: f32,
    rel: f32,
    thr: f32,
    ratio: f32,
}

impl Comp {
    fn new(thr: f32, ratio: f32, att_ms: f32, rel_ms: f32) -> Self {
        Comp {
            env: 0.0,
            att: coef(att_ms),
            rel: coef(rel_ms),
            thr,
            ratio: ratio.max(1.0),
        }
    }
    /// Gain for a detector sample (soft 6 dB knee).
    fn gain(&mut self, x: f32) -> f32 {
        let c = if x > self.env { self.att } else { self.rel };
        self.env = c * self.env + (1.0 - c) * x;
        let over = gain_to_db(self.env) - self.thr;
        let knee = 6.0;
        let gr = if over <= -knee / 2.0 {
            0.0
        } else if over < knee / 2.0 {
            (1.0 - 1.0 / self.ratio) * (over + knee / 2.0).powi(2) / (2.0 * knee)
        } else {
            over * (1.0 - 1.0 / self.ratio)
        };
        db_to_gain(-gr)
    }
}

pub fn multiband(p: &MultibandFx, l: &mut [f32], r: &mut [f32]) {
    let (f_lo, f_hi) = (
        p.low_freq.clamp(40.0, 1000.0),
        p.high_freq
            .clamp(p.low_freq.clamp(40.0, 1000.0) * 1.5, 12000.0),
    );
    let (ll, lm, lh) = split3(l, f_lo, f_hi);
    let (rl, rm, rh) = split3(r, f_lo, f_hi);
    let bands = [
        (&ll, &rl, p.low_threshold_db, p.low_ratio, p.low_gain_db),
        (&lm, &rm, p.mid_threshold_db, p.mid_ratio, p.mid_gain_db),
        (&lh, &rh, p.high_threshold_db, p.high_ratio, p.high_gain_db),
    ];
    let n = l.len();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    for (bl, br, thr, ratio, gain_db) in bands {
        let mut c = Comp::new(thr, ratio, p.attack_ms, p.release_ms);
        let mk = db_to_gain(gain_db);
        for i in 0..n {
            let g = c.gain(bl[i].abs().max(br[i].abs())) * mk;
            out_l[i] += bl[i] * g;
            out_r[i] += br[i] * g;
        }
    }
    let mix = p.mix.clamp(0.0, 1.0);
    for i in 0..n {
        l[i] = l[i] * (1.0 - mix) + out_l[i] * mix;
        r[i] = r[i] * (1.0 - mix) + out_r[i] * mix;
    }
}

// ---------------- dynamic EQ / de-esser ----------------

pub fn dynamic_eq(p: &DynamicEqFx, l: &mut [f32], r: &mut [f32]) {
    let n = l.len();
    let mut bp = [
        Biquad::new(BiquadKind::Bandpass, p.freq, p.q, 0.0),
        Biquad::new(BiquadKind::Bandpass, p.freq, p.q, 0.0),
    ];
    let (att, rel) = (coef(p.attack_ms), coef(p.release_ms));
    let ratio = p.ratio.max(1.0);
    let range = p.range_db.clamp(-24.0, 12.0);
    let mut env = 0.0f32;
    for i in 0..n {
        let (bl, br) = (bp[0].process(l[i]), bp[1].process(r[i]));
        let x = bl.abs().max(br.abs());
        let c = if x > env { att } else { rel };
        env = c * env + (1.0 - c) * x;
        let over = (gain_to_db(env) - p.threshold_db).max(0.0);
        let amount = over * (1.0 - 1.0 / ratio);
        // cut (range < 0) or boost (range > 0) the band, at most |range| dB
        let db = if range < 0.0 {
            -amount.min(-range)
        } else {
            amount.min(range)
        };
        let g = db_to_gain(db) - 1.0;
        l[i] += bl * g;
        r[i] += br * g;
    }
}

/// Keyed dynamic EQ: the band dips by `range_db` (cut) or lifts (boost)
/// while the `source` track is playing. Each trigger (a note start or a
/// word onset in an audio clip) holds the dip for `release_ms` after it,
/// with an `attack_ms` ramp in and a smooth recovery, so a pad steps out
/// of the voice's band under the words and comes back between lines.
pub fn keyed_dynamic_eq(p: &DynamicEqFx, trig: &[usize], l: &mut [f32], r: &mut [f32]) {
    let n = l.len();
    let mut bp = [
        Biquad::new(BiquadKind::Bandpass, p.freq, p.q, 0.0),
        Biquad::new(BiquadKind::Bandpass, p.freq, p.q, 0.0),
    ];
    let range = p.range_db.clamp(-24.0, 12.0);
    let hold = (p.release_ms.max(5.0) * 0.001 * SR) as usize;
    let (att, rel) = (
        coef(p.attack_ms.max(1.0)),
        coef(p.release_ms.max(5.0) * 0.5),
    );
    let mut ti = 0usize;
    let mut until = 0usize;
    let mut env = 0.0f32;
    for i in 0..n {
        while ti < trig.len() && trig[ti] <= i {
            until = until.max(trig[ti] + hold);
            ti += 1;
        }
        let target = if i < until && ti > 0 { 1.0 } else { 0.0 };
        let c = if target > env { att } else { rel };
        env = c * env + (1.0 - c) * target;
        let g = db_to_gain(range * env) - 1.0;
        let (bl, br) = (bp[0].process(l[i]), bp[1].process(r[i]));
        l[i] += bl * g;
        r[i] += br * g;
    }
}

/// Where a level-keyed band starts and finishes dipping: the source's
/// level (a 5 ms power follower) from -45 dBFS (no dip, the same floor
/// `render::audio_onsets` uses) to -35 dBFS (the full `range_db`).
pub const LEVEL_KEY_FLOOR_DB: f32 = -45.0;
pub const LEVEL_KEY_KNEE_DB: f32 = 10.0;

/// The dip amount (0 = flat, 1 = the full `range_db`) of a level-keyed band
/// at each of `n` samples: the source's level through a 5 ms power follower,
/// mapped over the -45..-35 dBFS knee, then smoothed per sample with the
/// band's attack (rising) and release (falling) time constants, so the gain
/// glides and never steps.
pub fn level_duck_curve(p: &DynamicEqFx, key: &[f32], n: usize) -> Vec<f32> {
    let pc = coef(5.0);
    let (att, rel) = (coef(p.attack_ms.max(1.0)), coef(p.release_ms.max(5.0)));
    let (mut pw, mut env) = (0.0f32, 0.0f32);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let x = key.get(i).copied().unwrap_or(0.0);
        pw = pc * pw + (1.0 - pc) * x * x;
        let db = 10.0 * (pw + 1e-12).log10();
        let target = ((db - LEVEL_KEY_FLOOR_DB) / LEVEL_KEY_KNEE_DB).clamp(0.0, 1.0);
        let c = if target > env { att } else { rel };
        env = c * env + (1.0 - c) * target;
        out.push(env);
    }
    out
}

/// Level-keyed dynamic EQ: the band dips by up to `range_db` while `key`
/// (the source's mono audio, aligned with this buffer) sounds, following its
/// level (see `level_duck_curve`), so a held vowel keeps the dip down.
pub fn level_keyed_dynamic_eq(p: &DynamicEqFx, key: &[f32], l: &mut [f32], r: &mut [f32]) {
    let n = l.len();
    let mut bp = [
        Biquad::new(BiquadKind::Bandpass, p.freq, p.q, 0.0),
        Biquad::new(BiquadKind::Bandpass, p.freq, p.q, 0.0),
    ];
    let range = p.range_db.clamp(-24.0, 12.0);
    for (i, env) in level_duck_curve(p, key, n).into_iter().enumerate() {
        let g = db_to_gain(range * env) - 1.0;
        let (bl, br) = (bp[0].process(l[i]), bp[1].process(r[i]));
        l[i] += bl * g;
        r[i] += br * g;
    }
}

pub fn deesser(p: &DeesserFx, l: &mut [f32], r: &mut [f32]) {
    let n = l.len();
    let q = std::f32::consts::FRAC_1_SQRT_2;
    // complementary split (low + high == input) so the cut is phase-safe
    let mut lp = [
        Biquad::new(BiquadKind::HighCut, p.freq, q, 0.0),
        Biquad::new(BiquadKind::HighCut, p.freq, q, 0.0),
    ];
    let (att, rel) = (coef(0.5), coef(p.release_ms));
    let mut env = 0.0f32;
    let range = p.range_db.clamp(-24.0, 0.0);
    for i in 0..n {
        let (hl, hr) = (l[i] - lp[0].process(l[i]), r[i] - lp[1].process(r[i]));
        let x = hl.abs().max(hr.abs());
        let c = if x > env { att } else { rel };
        env = c * env + (1.0 - c) * x;
        let over = (gain_to_db(env) - p.threshold_db).max(0.0);
        let db = (-over * 0.75).max(range);
        let g = db_to_gain(db) - 1.0;
        l[i] += hl * g;
        r[i] += hr * g;
    }
}

// ---------------- soft clipper ----------------

fn soft_clip_sample(x: f32, t: f32, c: f32) -> f32 {
    let a = x.abs();
    if a <= t {
        x
    } else {
        let room = (c - t).max(1e-4);
        x.signum() * (t + room * ((a - t) / room).tanh())
    }
}

pub fn soft_clipper(p: &SoftClipFx, l: &mut [f32], r: &mut [f32]) {
    let c = db_to_gain(p.ceiling_db.min(0.0));
    let t = db_to_gain(p.threshold_db.min(p.ceiling_db - 0.1)).min(c * 0.999);
    let drive = db_to_gain(p.drive_db.clamp(-12.0, 24.0));
    let mix = p.mix.clamp(0.0, 1.0);
    for ch in [&mut *l, &mut *r] {
        let mut prev = 0.0f32;
        let mut prev_mid_out = 0.0f32;
        for s in ch.iter_mut() {
            let x = *s * drive;
            let wet = if p.oversample {
                // 2x: clip the midpoint and the sample, then average back down
                let mid = 0.5 * (prev + x);
                let ym = soft_clip_sample(mid, t, c);
                let y = soft_clip_sample(x, t, c);
                let out = 0.25 * prev_mid_out + 0.5 * ym + 0.25 * y;
                prev_mid_out = y;
                out.clamp(-c, c)
            } else {
                soft_clip_sample(x, t, c)
            };
            prev = x;
            *s = *s * (1.0 - mix) + wet * mix;
        }
    }
}

// ---------------- gate ----------------

pub fn gate(p: &GateFx, l: &mut [f32], r: &mut [f32]) {
    let thr = db_to_gain(p.threshold_db);
    let floor = db_to_gain(p.range_db.min(0.0));
    let (att, rel) = (coef(p.attack_ms), coef(p.release_ms));
    let det = coef(1.0);
    let hold = (p.hold_ms.max(0.0) * 0.001 * SR) as usize;
    let (mut env, mut g, mut held) = (0.0f32, floor, 0usize);
    for i in 0..l.len() {
        let x = l[i].abs().max(r[i].abs());
        env = if x > env {
            x
        } else {
            det * env + (1.0 - det) * x
        };
        let target = if env >= thr {
            held = hold;
            1.0
        } else if held > 0 {
            held -= 1;
            1.0
        } else {
            floor
        };
        let c = if target > g { att } else { rel };
        g = c * g + (1.0 - c) * target;
        l[i] *= g;
        r[i] *= g;
    }
}

// ---------------- phaser / flanger ----------------

fn lfo_rate(rate_hz: f32, sync_steps: f32, step_secs: f32) -> f32 {
    if sync_steps > 0.0 {
        1.0 / (sync_steps * step_secs).max(1e-3)
    } else {
        rate_hz.max(0.0)
    }
}

/// `offset`: the buffer's first sample's place in the song (samples), so
/// a window's LFO is where a whole-song render's is.
pub fn phaser(p: &PhaserFx, l: &mut [f32], r: &mut [f32], step_secs: f32, offset: usize) {
    let rate = lfo_rate(p.rate_hz, p.sync_steps, step_secs);
    let stages = p.stages.clamp(2, 12) as usize;
    let mix = p.mix.clamp(0.0, 1.0);
    let fb = p.feedback.clamp(-0.95, 0.95);
    for (ci, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
        let mut x1 = vec![0.0f32; stages];
        let mut y1 = vec![0.0f32; stages];
        let mut last = 0.0f32;
        let mut a = 0.0f32;
        let phase0 = ci as f32 * 0.25;
        for (i, s) in ch.iter_mut().enumerate() {
            // coefficients update on the song's 16-sample grid; a window
            // that starts between grid points computes its first set at the
            // grid point before it, as a whole-song render did
            let k = i + offset;
            if k % 16 == 0 || i == 0 {
                let k16 = k - k % 16;
                let lfo = 0.5 + 0.5 * (2.0 * PI * (rate * k16 as f32 / SR + phase0)).sin();
                let f = (p.center_hz * 2f32.powf((lfo - 0.5) * 4.0 * p.depth.clamp(0.0, 1.0)))
                    .clamp(40.0, 16000.0);
                let t = (PI * f / SR).tan();
                a = (t - 1.0) / (t + 1.0);
            }
            let mut v = *s + last * fb;
            for k in 0..stages {
                let y = a * v + x1[k] - a * y1[k];
                x1[k] = v;
                y1[k] = y;
                v = y;
            }
            last = v;
            *s = *s * (1.0 - mix * 0.5) + v * mix * 0.5;
            if !s.is_finite() {
                *s = 0.0;
            }
        }
    }
}

/// `offset`: the buffer's place in the song (samples), see `phaser`.
pub fn flanger(p: &FlangerFx, l: &mut [f32], r: &mut [f32], step_secs: f32, offset: usize) {
    let rate = lfo_rate(p.rate_hz, p.sync_steps, step_secs);
    let max = ((p.delay_ms + p.depth_ms).max(0.1) * 0.001 * SR) as usize + 8;
    let mix = p.mix.clamp(0.0, 1.0);
    let fb = p.feedback.clamp(-0.95, 0.95);
    for (ci, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
        let mut buf = vec![0.0f32; max];
        let phase0 = ci as f32 * 0.25;
        for (i, s) in ch.iter_mut().enumerate() {
            let k = i + offset;
            let lfo = 0.5 + 0.5 * (2.0 * PI * (rate * k as f32 / SR + phase0)).sin();
            let d = ((p.delay_ms.max(0.05) + p.depth_ms.max(0.0) * lfo) * 0.001 * SR).max(1.0);
            let w = k % max;
            let pos = (w as f32 - d).rem_euclid(max as f32);
            let i0 = pos as usize % max;
            let i1 = (i0 + 1) % max;
            let f = pos - pos.floor();
            let delayed = buf[i0] * (1.0 - f) + buf[i1] * f;
            buf[w] = *s + delayed * fb;
            *s = *s * (1.0 - mix * 0.5) + delayed * mix * 0.7;
        }
    }
}

// ---------------- stutter / gross-beat style ----------------

/// Gate pattern chars: x/X/1 = open, o = half, ./0/- = closed.
pub fn gate_levels(pattern: &str) -> Vec<f32> {
    let v: Vec<f32> = pattern
        .chars()
        .filter_map(|c| match c {
            'x' | 'X' | '1' => Some(1.0),
            'o' | 'O' => Some(0.5),
            '.' | '0' | '-' | '_' => Some(0.0),
            _ => None,
        })
        .collect();
    if v.is_empty() {
        vec![1.0]
    } else {
        v
    }
}

pub fn stutter(p: &StutterFx, l: &mut [f32], r: &mut [f32], step_secs: f32) {
    stutter_at(p, l, r, step_secs, 0)
}

/// `stutter` for a buffer that starts `offset` samples into the song: the
/// step grid and the cycles count from the song's start, so a window
/// matches the same samples of a whole-song pass (source audio from before
/// the buffer reads as silence).
pub fn stutter_at(p: &StutterFx, l: &mut [f32], r: &mut [f32], step_secs: f32, offset: usize) {
    stutter_guarded(p, l, r, step_secs, offset, true)
}

/// `stutter_at` with the tape-stop DC guard switchable (`dc_guard`; the
/// rack always runs it on). The guard (`crate::dsp::tape_dc`, a 20 Hz
/// 2-pole high-pass) touches only `tape_stop` mode: the other modes never
/// read it, so their output is the same bit for bit either way.
fn stutter_guarded(
    p: &StutterFx,
    l: &mut [f32],
    r: &mut [f32],
    step_secs: f32,
    offset: usize,
    dc_guard: bool,
) {
    let n = l.len();
    let step = (step_secs * SR).max(1.0);
    let mix = p.mix.clamp(0.0, 1.0);
    if mix <= 0.0 {
        return;
    }
    let fade = ((p.smooth_ms.max(0.5) * 0.001 * SR) as usize).max(1);
    let (src_l, src_r) = (l.to_vec(), r.to_vec());
    let cycle = (p.cycle_steps.max(0.25) * step) as usize;
    let cycle = cycle.max(1);
    let slice = ((p.slice_steps.max(0.0625) * step) as usize).clamp(1, cycle);
    // window that ramps at both ends of a segment of length `len`
    let win = |pos: usize, len: usize| -> f32 {
        let a = (pos as f32 / fade as f32).min(1.0);
        let b = ((len.saturating_sub(pos)) as f32 / fade as f32).min(1.0);
        a.min(b)
    };
    let levels = gate_levels(&p.pattern);
    // source sample at song position `a` (silence before the buffer)
    let at = |src: &[f32], a: usize| {
        if a >= offset {
            src.get(a - offset).copied().unwrap_or(0.0)
        } else {
            0.0
        }
    };
    let lerp_at = |src: &[f32], f: f32| lerp_read(src, f - offset as f32);
    let end = offset + n;
    // tape stop: source position of song sample `j` (speed falls 1 -> 0 over
    // each cycle: position = c0 + T*(u - u^2/2))
    let tape_pos = |j: usize| -> f32 {
        let c0 = (j / cycle) * cycle;
        let u = (j - c0) as f32 / cycle as f32;
        c0 as f32 + cycle as f32 * (u - 0.5 * u * u)
    };
    // the DC guard on the tape-stop read, one per channel, and the cycle it
    // was warmed for
    let mut dc = (TapeDcGuard::default(), TapeDcGuard::default());
    let mut dc_cycle: Option<usize> = None;
    for i in 0..n {
        let j = i + offset;
        let (wl, wr) = match p.mode {
            StutterMode::Gate => {
                let k = (j as f32 / step) as usize;
                let lv = levels[k % levels.len()];
                let pos = j - (k as f32 * step) as usize;
                let seg = step as usize;
                // smooth edges only where the level changes
                let next = levels[(k + 1) % levels.len()];
                let prev = levels[(k + levels.len() - 1) % levels.len()];
                let mut g = lv;
                if pos < fade && prev != lv {
                    let t = pos as f32 / fade as f32;
                    g = prev + (lv - prev) * t;
                }
                if seg.saturating_sub(pos) < fade && next != lv {
                    let t = (seg.saturating_sub(pos)) as f32 / fade as f32;
                    g = next + (lv - next) * t;
                }
                (src_l[i] * g, src_r[i] * g)
            }
            StutterMode::Stutter => {
                let c0 = (j / cycle) * cycle;
                let pos = (j - c0) % slice;
                let src = c0 + pos;
                let g = win(pos, slice);
                (at(&src_l, src) * g, at(&src_r, src) * g)
            }
            StutterMode::HalfTime => {
                let c0 = (j / cycle) * cycle;
                let pos = j - c0;
                let srcf = c0 as f32 + pos as f32 * 0.5;
                let g = win(pos, cycle);
                (lerp_at(&src_l, srcf) * g, lerp_at(&src_r, srcf) * g)
            }
            StutterMode::Reverse => {
                let c0 = (j / cycle) * cycle;
                let pos = j - c0;
                let len = cycle.min(end - c0);
                let src = c0 + len - 1 - pos.min(len - 1);
                let g = win(pos, len);
                (at(&src_l, src) * g, at(&src_r, src) * g)
            }
            StutterMode::TapeStop => {
                let c0 = (j / cycle) * cycle;
                let u = (j - c0) as f32 / cycle as f32;
                let srcf = tape_pos(j);
                let g = (1.0 - u).powf(0.3) * win(j - c0, cycle).max(0.0);
                let (mut rl, mut rr) = (lerp_at(&src_l, srcf), lerp_at(&src_r, srcf));
                if dc_guard {
                    if dc_cycle != Some(c0) {
                        // a new ramp: warm the guard on the source playing at
                        // unity speed into it, then on this cycle's reads up to
                        // here (a buffer that starts mid-ramp)
                        dc_cycle = Some(c0);
                        dc.0.reset();
                        dc.1.reset();
                        for a in c0.saturating_sub(tape_dc::preroll())..c0 {
                            dc.0.process(at(&src_l, a));
                            dc.1.process(at(&src_r, a));
                        }
                        for jj in c0..j {
                            let f = tape_pos(jj);
                            dc.0.process(lerp_at(&src_l, f));
                            dc.1.process(lerp_at(&src_r, f));
                        }
                    }
                    rl = dc.0.process(rl);
                    rr = dc.1.process(rr);
                }
                (rl * g, rr * g)
            }
        };
        l[i] = src_l[i] * (1.0 - mix) + wl * mix;
        r[i] = src_r[i] * (1.0 - mix) + wr * mix;
    }
}

// ---------------- pitch shifter ----------------

pub fn pitch_shift(p: &PitchShiftFx, l: &mut [f32], r: &mut [f32]) {
    pitch_shift_at(p, l, r, 0)
}

/// `pitch_shift` for a buffer that starts `offset` samples into the song:
/// the ring-buffer slot and the grain clock run from the song's start (the
/// clock is stepped through the first `offset` samples), so a window matches
/// the same samples of a whole-song pass once its first window has filled.
pub fn pitch_shift_at(p: &PitchShiftFx, l: &mut [f32], r: &mut [f32], offset: usize) {
    let ratio = 2f32.powf((p.semitones.clamp(-24.0, 24.0) + p.cents / 100.0) / 12.0);
    let w = ((p.window_ms.clamp(10.0, 200.0) * 0.001 * SR) as usize).max(64) as f32;
    let mix = p.mix.clamp(0.0, 1.0);
    let size = w as usize * 2 + 4;
    // the grain clock at the buffer's first sample
    let mut d0 = 0.0f32;
    for _ in 0..offset {
        d0 = (d0 + (1.0 - ratio)).rem_euclid(w);
    }
    for ch in [&mut *l, &mut *r] {
        let mut buf = vec![0.0f32; size];
        let mut d = d0;
        for (i, s) in ch.iter_mut().enumerate() {
            let wi = (i + offset) % size;
            buf[wi] = *s;
            d = (d + (1.0 - ratio)).rem_euclid(w);
            let tap = |delay: f32| {
                let pos = (wi as f32 - delay - 1.0).rem_euclid(size as f32);
                let i0 = pos as usize % size;
                let f = pos - pos.floor();
                buf[i0] * (1.0 - f) + buf[(i0 + 1) % size] * f
            };
            let d2 = (d + w * 0.5).rem_euclid(w);
            let g1 = (PI * d / w).sin();
            let g2 = (PI * d2 / w).sin();
            let wet = tap(d) * g1 + tap(d2) * g2;
            *s = *s * (1.0 - mix) + wet * mix;
        }
    }
}

// ---------------- convolution reverb ----------------

/// Synthesize a stereo impulse response for a space.
pub fn synth_ir(p: &ConvolutionFx) -> (Vec<f32>, Vec<f32>) {
    let decay = p.decay_s.clamp(0.1, 8.0);
    let pre = (p.predelay_ms.clamp(0.0, 250.0) * 0.001 * SR) as usize;
    let len = pre + (decay * 1.1 * SR) as usize;
    let (bright, density_ms, er): (f32, f32, &[(f32, f32)]) = match p.space {
        IrSpace::Room => (
            0.55,
            1.5,
            &[
                (4.1, 0.7),
                (7.3, 0.55),
                (11.2, 0.5),
                (16.7, 0.35),
                (23.0, 0.3),
            ],
        ),
        IrSpace::Hall => (
            0.45,
            2.5,
            &[
                (13.0, 0.5),
                (21.0, 0.45),
                (34.0, 0.35),
                (47.0, 0.3),
                (61.0, 0.2),
            ],
        ),
        IrSpace::Plate => (0.85, 0.3, &[(1.0, 0.4), (2.3, 0.35), (3.1, 0.3)]),
        IrSpace::Chamber => (
            0.6,
            1.0,
            &[(6.0, 0.6), (9.5, 0.5), (14.0, 0.45), (19.0, 0.35)],
        ),
        IrSpace::Spring => (0.7, 0.2, &[(29.0, 0.6), (58.0, 0.45), (87.0, 0.3)]),
        IrSpace::Cathedral => (
            0.35,
            4.0,
            &[
                (25.0, 0.45),
                (41.0, 0.4),
                (67.0, 0.35),
                (93.0, 0.3),
                (130.0, 0.2),
            ],
        ),
    };
    let damping = p.damping.clamp(0.0, 1.0);
    let make = |seed: u64| -> Vec<f32> {
        let mut rng = Rng::new(seed);
        let mut ir = vec![0.0f32; len.max(2)];
        let mut lp = 0.0f32;
        let build = (density_ms * 0.001 * SR).max(1.0);
        for (i, v) in ir.iter_mut().enumerate().skip(pre) {
            let t = (i - pre) as f32;
            let ts = t / SR;
            let env = (-6.91 * ts / decay).exp() * (1.0 - (-t / build).exp());
            // the tail gets darker over time (air absorption)
            let c = (bright * (1.0 - damping * 0.8) * (-ts * damping * 2.5).exp()).clamp(0.02, 1.0);
            lp += c * (rng.bipolar() - lp);
            *v = lp * env;
        }
        for &(ms, g) in er {
            let k = pre + (ms * 0.001 * SR * (0.9 + 0.2 * rng.f32())) as usize;
            if k < ir.len() {
                ir[k] += g * p.early.clamp(0.0, 1.0) * if rng.chance(0.5) { 1.0 } else { -1.0 };
            }
        }
        // spring: dispersive repeats of the first 30 ms
        if p.space == IrSpace::Spring {
            let rep = (0.029 * SR) as usize;
            for k in (pre + rep..ir.len()).rev() {
                let prev = ir[k - rep];
                ir[k] += prev * 0.5;
            }
        }
        // normalize energy
        let e: f32 = ir.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        for x in ir.iter_mut() {
            *x /= e;
        }
        ir
    };
    let a = make(0xC0FFEE);
    let b = make(0xBEEF);
    // width: blend decorrelated channels
    let w = p.width.clamp(0.0, 1.0);
    let l: Vec<f32> = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| x * (0.5 + 0.5 * w) + y * (0.5 - 0.5 * w))
        .collect();
    let r: Vec<f32> = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| y * (0.5 + 0.5 * w) + x * (0.5 - 0.5 * w))
        .collect();
    (l, r)
}

/// Windowed-sinc lowpass FIR, odd length, unity DC gain, linear phase
/// (group delay (taps-1)/2). Deterministic: a pure function of the
/// cutoff and tap count. Blackman window.
pub fn lowpass_fir(cutoff_hz: f32, taps: usize) -> Vec<f32> {
    let n = taps.max(3) | 1;
    let m = (n - 1) / 2;
    let fc = (cutoff_hz / SR).clamp(1e-4, 0.4999);
    let mut h = vec![0.0f32; n];
    let mut sum = 0.0f32;
    for (j, v) in h.iter_mut().enumerate() {
        let k = j as f32 - m as f32;
        let sinc = if j == m {
            2.0 * fc
        } else {
            (2.0 * std::f32::consts::PI * fc * k).sin() / (std::f32::consts::PI * k)
        };
        let w = 0.42 - 0.5 * (2.0 * std::f32::consts::PI * j as f32 / (n - 1) as f32).cos()
            + 0.08 * (4.0 * std::f32::consts::PI * j as f32 / (n - 1) as f32).cos();
        *v = sinc * w;
        sum += *v;
    }
    for v in h.iter_mut() {
        *v /= sum;
    }
    h
}

/// Linear-phase 3-way frequency split (Frequency Splitter class):
/// windowed-sinc FIR crossovers, complementary by construction. With
/// h1 = lowpass(low_hz), h2 = lowpass(high_hz) and x_d = the input
/// delayed by the FIR group delay m = (taps-1)/2: low = x*h1,
/// mid = x*h2 - x*h1, high = x_d - x*h2, so the three bands share one
/// linear phase and sum back to the delayed input. The insert solos
/// ONE band (`p.band`) - band sends are one insert per lane. Latency
/// is m samples; the rt arm reports it like cab_ir reports its own.
pub fn freq_splitter(p: &crate::fx::FreqSplitterFx, l: &mut [f32], r: &mut [f32]) {
    let taps = (p.taps as usize).max(3) | 1;
    let m = (taps - 1) / 2;
    let lo = p.low_hz.min(p.high_hz);
    let hi = p.high_hz.max(p.low_hz);
    let h1 = lowpass_fir(lo, taps);
    let h2 = lowpass_fir(hi, taps);
    let g = db_to_gain(p.gain_db);
    let mix = p.mix.clamp(0.0, 1.0);
    for chan in [l, r] {
        let x = chan.to_vec();
        let s1 = if p.band == crate::fx::FreqSplitBand::High {
            Vec::new()
        } else {
            convolve(&x, &h1)
        };
        let s2 = if p.band == crate::fx::FreqSplitBand::Low {
            Vec::new()
        } else {
            convolve(&x, &h2)
        };
        for i in 0..chan.len() {
            let wet = match p.band {
                crate::fx::FreqSplitBand::Low => s1[i],
                crate::fx::FreqSplitBand::Mid => s2[i] - s1[i],
                crate::fx::FreqSplitBand::High => (if i >= m { x[i - m] } else { 0.0 }) - s2[i],
            };
            chan[i] = (wet * g) * mix + x[i] * (1.0 - mix);
        }
    }
}

/// Rejects only configs that are nonsense today: out-of-range or
/// non-finite crossover/gain/mix, equal crossovers (silent mid band), and
/// tap counts that are degenerate (< 33) or unbounded cost (> 65537).
/// Inverted low/high stays valid (it is swapped, as before).
pub fn check_freq_splitter(p: &crate::fx::FreqSplitterFx) -> Result<(), String> {
    let rng = |name: &str, v: f32, lo: f32, hi: f32| {
        if v.is_finite() && v >= lo && v <= hi {
            Ok(())
        } else {
            Err(format!(
                "freq_splitter: {name} must be {lo}..{hi} (got {v})"
            ))
        }
    };
    if !(33..=65_537).contains(&p.taps) {
        return Err(format!(
            "freq_splitter: taps must be 33..65537 (got {})",
            p.taps
        ));
    }
    rng("low_hz", p.low_hz, 20.0, 20_000.0)?;
    rng("high_hz", p.high_hz, 20.0, 20_000.0)?;
    if p.low_hz == p.high_hz {
        return Err(
            "freq_splitter: low_hz and high_hz must differ (equal gives a silent mid band)".into(),
        );
    }
    rng("gain_db", p.gain_db, -60.0, 24.0)?;
    rng("mix", p.mix, 0.0, 1.0)
}

pub fn check_granular(p: &crate::fx::GranularFx) -> Result<(), String> {
    let rng = |name: &str, v: f32, lo: f32, hi: f32| {
        if v.is_finite() && v >= lo && v <= hi {
            Ok(())
        } else {
            Err(format!("granular: {name} must be {lo}..{hi} (got {v})"))
        }
    };
    rng("grain_ms", p.grain_ms, 5.0, 500.0)?;
    rng("density_hz", p.density_hz, 0.5, 200.0)?;
    rng("pitch_semitones", p.pitch_semitones, -24.0, 24.0)?;
    rng("position_jitter_ms", p.position_jitter_ms, 0.0, 500.0)?;
    rng("spread", p.spread, 0.0, 1.0)?;
    rng("mix", p.mix, 0.0, 1.0)?;
    if !p.freeze_at_ms.is_finite() {
        return Err("granular: freeze_at_ms must be finite (negative = off)".into());
    }
    Ok(())
}

struct Grain {
    start: usize,
    src0: f32,
    gl: f32,
    gr: f32,
    /// Frozen region first index; `flen` 0 = read the live input.
    a: usize,
    flen: usize,
}

/// Most grains that can sound at once. The densest legal setting
/// (200 grains/s, 500 ms, scheduler gaps >= 0.75 of the interval) needs
/// ~140, so this cap is never the thing that shapes the sound.
pub const GRANULAR_MAX_GRAINS: usize = 512;
/// Input history kept for grains (2^17 samples): the furthest a grain can
/// read behind the write head is jitter + grain * (rate - 1) <= 88 202.
const GR_RING: usize = 1 << 17;
/// Largest frozen slice: grain (<= 22 050) + jitter (<= 22 050) samples.
const GR_FREEZE_MAX: usize = 44_101;

#[inline]
fn gr_clean(x: f32) -> f32 {
    if x.is_finite() && x.abs() >= f32::MIN_POSITIVE {
        x
    } else {
        0.0
    }
}

/// The granular scheduler as a per-sample state machine. The offline pass
/// and the streamed arm both run exactly this code, so they agree to the
/// bit at any block size. Every buffer is sized in `new`; `step` never
/// allocates.
pub struct GranularCore {
    ring: [Vec<f32>; 2],
    frozen: [Vec<f32>; 2],
    grains: Vec<Grain>,
    glen: usize,
    rate: f32,
    interval: f64,
    jit: f32,
    pub(crate) mix: f32,
    norm: f32,
    spread: f32,
    freeze: Option<usize>,
    fa: usize,
    flen: usize,
    state: u32,
    next: f64,
    i: usize,
    /// Most grains alive at once so far (for the cap test).
    pub peak_active: usize,
}

impl GranularCore {
    /// `total` = clip length in samples (the freeze point is clamped to it,
    /// as in the offline pass).
    pub fn new(p: &crate::fx::GranularFx, total: usize) -> Self {
        let glen = ((p.grain_ms * 0.001 * SR) as usize).max(16);
        GranularCore {
            ring: [vec![0.0; GR_RING], vec![0.0; GR_RING]],
            frozen: [vec![0.0; GR_FREEZE_MAX], vec![0.0; GR_FREEZE_MAX]],
            grains: Vec::with_capacity(GRANULAR_MAX_GRAINS),
            glen,
            rate: 2.0f32.powf(p.pitch_semitones / 12.0),
            interval: (SR / p.density_hz.max(0.5)).max(1.0) as f64,
            jit: p.position_jitter_ms.max(0.0) * 0.001 * SR,
            mix: p.mix.clamp(0.0, 1.0),
            norm: 1.0 / (p.density_hz * p.grain_ms * 0.001 * 0.5).max(1.0).sqrt(),
            spread: p.spread.clamp(0.0, 1.0),
            freeze: if p.freeze_at_ms >= 0.0 && total > 0 {
                Some(((p.freeze_at_ms * 0.001 * SR) as usize).min(total - 1))
            } else {
                None
            },
            fa: 0,
            flen: 0,
            state: if p.seed == 0 { 0x9E37_79B9 } else { p.seed },
            next: 0.0,
            i: 0,
            peak_active: 0,
        }
    }

    #[inline]
    fn rand01(&mut self) -> f32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 17;
        self.state ^= self.state << 5;
        (self.state >> 8) as f32 / 16_777_216.0
    }

    #[inline]
    fn read_live(&self, c: usize, pos: f32, hi: usize) -> f32 {
        if pos < 0.0 {
            return 0.0;
        }
        let i0 = (pos as usize).min(hi);
        let fr = pos - i0 as f32;
        let a = self.ring[c][i0 & (GR_RING - 1)];
        let b = self.ring[c][(i0 + 1).min(hi) & (GR_RING - 1)];
        a + (b - a) * fr
    }

    #[inline]
    fn read_frozen(&self, c: usize, pos: f32, a0: usize, flen: usize) -> f32 {
        let hi = a0 + flen - 1;
        let i0 = (pos.max(0.0) as usize).clamp(a0, hi);
        let fr = pos - i0 as f32;
        let a = self.frozen[c][i0 - a0];
        let b = self.frozen[c][(i0 + 1).min(hi) - a0];
        a + (b - a) * fr
    }

    /// One stereo sample in, one out (wet/dry mixed).
    pub fn step(&mut self, xl: f32, xr: f32) -> (f32, f32) {
        let i = self.i;
        let (xl, xr) = (gr_clean(xl), gr_clean(xr));
        self.ring[0][i & (GR_RING - 1)] = xl;
        self.ring[1][i & (GR_RING - 1)] = xr;
        if self.freeze == Some(i) {
            let flen = ((self.glen as f32 + self.jit) as usize)
                .min(i + 1)
                .clamp(1, GR_FREEZE_MAX);
            self.fa = i + 1 - flen;
            self.flen = flen;
            for k in 0..flen {
                let src = (self.fa + k) & (GR_RING - 1);
                self.frozen[0][k] = self.ring[0][src];
                self.frozen[1][k] = self.ring[1][src];
            }
        }
        while self.next <= i as f64 {
            let u = (self.rand01() * 2.0 - 1.0) * self.spread;
            let ang = (u + 1.0) * std::f32::consts::FRAC_PI_4;
            let (gl, gr) = (
                ang.cos() * std::f32::consts::SQRT_2,
                ang.sin() * std::f32::consts::SQRT_2,
            );
            let pos_r = self.rand01();
            let mut g = Grain {
                start: i,
                src0: 0.0,
                gl,
                gr,
                a: 0,
                flen: 0,
            };
            match self.freeze {
                Some(f) if i >= f => {
                    g.a = self.fa;
                    g.flen = self.flen;
                    g.src0 = self.fa as f32 + pos_r * self.flen as f32;
                }
                _ => {
                    let lead = if self.rate > 1.0 {
                        self.glen as f32 * (self.rate - 1.0)
                    } else {
                        0.0
                    };
                    g.src0 = i as f32 - lead - pos_r * self.jit;
                }
            }
            if self.grains.len() < GRANULAR_MAX_GRAINS {
                self.grains.push(g);
            }
            let jitter = self.rand01() as f64;
            self.next += self.interval * (0.75 + 0.5 * jitter);
        }
        self.peak_active = self.peak_active.max(self.grains.len());
        let (mut wl, mut wr) = (0.0f32, 0.0f32);
        for g in &self.grains {
            let t = i - g.start;
            let w = 0.5 - 0.5 * (2.0 * PI * (t as f32 + 0.5) / self.glen as f32).cos();
            let mut pos = g.src0 + self.rate * t as f32;
            if g.flen > 0 {
                let rel = (pos - g.a as f32).rem_euclid(g.flen as f32);
                pos = g.a as f32 + rel;
                wl += self.read_frozen(0, pos, g.a, g.flen) * w * g.gl;
                wr += self.read_frozen(1, pos, g.a, g.flen) * w * g.gr;
            } else {
                wl += self.read_live(0, pos, i) * w * g.gl;
                wr += self.read_live(1, pos, i) * w * g.gr;
            }
        }
        let glen = self.glen;
        self.grains.retain(|g| i + 1 - g.start < glen);
        self.i = i + 1;
        let (wl, wr) = (
            if wl.is_finite() { wl } else { 0.0 },
            if wr.is_finite() { wr } else { 0.0 },
        );
        (
            (wl * self.norm) * self.mix + xl * (1.0 - self.mix),
            (wr * self.norm) * self.mix + xr * (1.0 - self.mix),
        )
    }
}

/// Live-input granular insert, offline. Causal: a grain only reads input
/// at or before the sample being produced. `mix` 0 leaves the buffers
/// untouched (bit-exact).
pub fn granular(p: &crate::fx::GranularFx, l: &mut [f32], r: &mut [f32]) {
    let n = l.len();
    if p.mix <= 0.0 || n == 0 {
        return;
    }
    let mut core = GranularCore::new(p, n);
    for i in 0..n {
        let (a, b) = core.step(l[i], r[i]);
        l[i] = a;
        r[i] = b;
    }
}

/// Uniformly partitioned FFT convolution; output has `x.len()` samples.
pub fn convolve(x: &[f32], ir: &[f32]) -> Vec<f32> {
    const B: usize = 2048;
    let n = x.len();
    let mut out = vec![0.0f32; n];
    if n == 0 || ir.is_empty() {
        return out;
    }
    let fft = Fft::new(2 * B);
    let parts: Vec<(Vec<f32>, Vec<f32>)> = ir
        .chunks(B)
        .map(|c| {
            let mut re = vec![0.0f32; 2 * B];
            let mut im = vec![0.0f32; 2 * B];
            re[..c.len()].copy_from_slice(c);
            fft.forward(&mut re, &mut im);
            (re, im)
        })
        .collect();
    let k = parts.len();
    // frequency-domain delay line of input blocks
    let mut fdl: Vec<(Vec<f32>, Vec<f32>)> = vec![(vec![0.0; 2 * B], vec![0.0; 2 * B]); k];
    let mut head = 0usize;
    let blocks = n.div_ceil(B);
    let mut overlap = vec![0.0f32; B];
    let mut acc_re = vec![0.0f32; 2 * B];
    let mut acc_im = vec![0.0f32; 2 * B];
    for bi in 0..blocks {
        let s = bi * B;
        let e = (s + B).min(n);
        let (re, im) = &mut fdl[head];
        re.iter_mut().for_each(|v| *v = 0.0);
        im.iter_mut().for_each(|v| *v = 0.0);
        re[..e - s].copy_from_slice(&x[s..e]);
        fft.forward(re, im);
        acc_re.iter_mut().for_each(|v| *v = 0.0);
        acc_im.iter_mut().for_each(|v| *v = 0.0);
        for (j, (hr, hi)) in parts.iter().enumerate() {
            let (xr, xi) = &fdl[(head + k - j) % k];
            for t in 0..2 * B {
                acc_re[t] += xr[t] * hr[t] - xi[t] * hi[t];
                acc_im[t] += xr[t] * hi[t] + xi[t] * hr[t];
            }
        }
        fft.inverse(&mut acc_re, &mut acc_im);
        for t in 0..B {
            if s + t < n {
                out[s + t] = acc_re[t] + overlap[t];
            }
        }
        overlap.copy_from_slice(&acc_re[B..]);
        head = (head + 1) % k;
    }
    out
}

pub fn convolution(p: &ConvolutionFx, l: &mut [f32], r: &mut [f32]) {
    let file = if p.ir_path.is_empty() {
        None
    } else {
        crate::fx_amp::load_ir(&p.ir_path, 10.0)
    };
    let (irl, irr) = match &file {
        Some(a) => (
            a.0.clone(),
            if a.1.is_empty() {
                a.0.clone()
            } else {
                a.1.clone()
            },
        ),
        None => synth_ir(p),
    };
    let mono: Vec<f32> = l.iter().zip(r.iter()).map(|(a, b)| 0.5 * (a + b)).collect();
    let wl = convolve(&mono, &irl);
    let wr = convolve(&mono, &irr);
    let mix = p.mix.clamp(0.0, 1.0);
    let dry = 1.0 - mix * 0.5;
    let wet = mix * 0.6;
    for i in 0..l.len() {
        l[i] = l[i] * dry + wl[i] * wet;
        r[i] = r[i] * dry + wr[i] * wet;
    }
}

// ---------------- auto-pan / tremolo ----------------

/// `offset`: the buffer's place in the song (samples), see `phaser`.
pub fn autopan(p: &AutopanFx, l: &mut [f32], r: &mut [f32], step_secs: f32, offset: usize) {
    let period = (p.steps.max(0.25) * step_secs * SR).max(1.0);
    let depth = p.depth.clamp(0.0, 1.0);
    for i in 0..l.len() {
        let ph = ((i + offset) as f32 / period).fract();
        let lfo = (2.0 * PI * ph).sin();
        if p.tremolo {
            let g = 1.0 - depth * (0.5 - 0.5 * lfo);
            l[i] *= g;
            r[i] *= g;
        } else {
            let pan = lfo * depth;
            let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
            let (gl, gr) = (
                angle.cos() * std::f32::consts::SQRT_2,
                angle.sin() * std::f32::consts::SQRT_2,
            );
            let m = 0.5 * (l[i] + r[i]);
            let s = 0.5 * (l[i] - r[i]);
            l[i] = (m + s) * gl;
            r[i] = (m - s) * gr;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f32, n: usize, a: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI * f * i as f32 / SR).sin() * a)
            .collect()
    }
    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn multiband_bands_reconstruct_when_flat() {
        let x: Vec<f32> = sine(100.0, 20000, 0.3)
            .iter()
            .zip(sine(5000.0, 20000, 0.3))
            .map(|(a, b)| a + b)
            .collect();
        let (a, b, c) = split3(&x, 200.0, 3000.0);
        for i in 0..x.len() {
            assert!((a[i] + b[i] + c[i] - x[i]).abs() < 1e-4);
        }
    }

    #[test]
    fn soft_clipper_holds_ceiling() {
        let mut l = sine(200.0, 10000, 2.0);
        let mut r = l.clone();
        soft_clipper(&SoftClipFx::default(), &mut l, &mut r);
        let c = db_to_gain(SoftClipFx::default().ceiling_db) + 1e-4;
        assert!(l.iter().all(|x| x.abs() <= c));
    }

    #[test]
    fn deesser_cuts_highs_not_lows() {
        let mut hl = sine(8000.0, 20000, 0.5);
        let mut hr = hl.clone();
        let before = rms(&hl);
        deesser(&DeesserFx::default(), &mut hl, &mut hr);
        assert!(rms(&hl) < before * 0.8, "{} vs {}", rms(&hl), before);
        let mut ll = sine(200.0, 20000, 0.5);
        let mut lr = ll.clone();
        let b2 = rms(&ll);
        deesser(&DeesserFx::default(), &mut ll, &mut lr);
        assert!((rms(&ll) - b2).abs() < b2 * 0.05);
    }

    #[test]
    fn gate_closes_on_quiet_tail() {
        let mut l: Vec<f32> = sine(300.0, 20000, 0.5);
        for v in l[10000..].iter_mut() {
            *v *= 0.001;
        }
        let mut r = l.clone();
        gate(&GateFx::default(), &mut l, &mut r);
        assert!(rms(&l[2000..9000]) > 0.3);
        assert!(rms(&l[16000..]) < 1e-4);
    }

    #[test]
    fn pitch_shift_octave_up_moves_energy() {
        let mut l = sine(220.0, 44100, 0.5);
        let mut r = l.clone();
        pitch_shift(&PitchShiftFx::default(), &mut l, &mut r);
        // zero crossings roughly double
        let zc = |x: &[f32]| x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let z = zc(&l[4410..]);
        assert!((350..530).contains(&z), "{z}");
    }

    #[test]
    fn convolution_matches_direct() {
        let x: Vec<f32> = (0..5000)
            .map(|i| ((i * 7919) % 13) as f32 / 13.0 - 0.5)
            .collect();
        let h: Vec<f32> = (0..3000)
            .map(|i| (-(i as f32) / 500.0).exp() * ((i % 5) as f32 - 2.0))
            .collect();
        let y = convolve(&x, &h);
        for &i in &[0usize, 10, 2047, 2048, 3500, 4999] {
            let mut d = 0.0f32;
            for k in 0..=i.min(h.len() - 1) {
                d += x[i - k] * h[k];
            }
            assert!((d - y[i]).abs() < 1e-2, "{i}: {d} vs {}", y[i]);
        }
        let p = ConvolutionFx::default();
        let (a, b) = synth_ir(&p);
        assert!(a.len() > 10000 && a.iter().chain(b.iter()).all(|v| v.is_finite()));
    }

    /// Worst |mean| of the last `tail` samples of each `cycle`-long ramp: the
    /// DC a tape stop leaves where the tape has all but stopped (speed under
    /// 5% for 100 ms of a 2 s ramp, so every tone here is below 11 Hz).
    fn stop_tail_dc(x: &[f32], cycle: usize, tail: usize) -> f32 {
        (1..=x.len() / cycle)
            .map(|k| {
                let s = &x[k * cycle - tail..k * cycle];
                (s.iter().map(|v| *v as f64).sum::<f64>() / tail as f64).abs() as f32
            })
            .fold(0.0, f32::max)
    }

    /// B75: the tape-stop DC guard. A 0.5 FS sine under a 2 s tape-stop ramp,
    /// at 55 / 110 / 220 Hz and 8 start phases; the DC is the worst mean of
    /// the ramp's last 100 ms. Measured: unguarded 0.142 FS (producers saw
    /// 0.12 FS on a real render), guarded 0.0100 FS.
    #[test]
    fn tape_stop_dc_guard_removes_the_stop_drift() {
        let step = 0.125;
        let cycle = (16.0 * step * SR) as usize;
        let tail = (0.1 * SR) as usize;
        let p = StutterFx {
            mode: StutterMode::TapeStop,
            cycle_steps: 16.0,
            ..Default::default()
        };
        let (mut off, mut on) = (0.0f32, 0.0f32);
        for f in [55.0f32, 110.0, 220.0] {
            for k in 0..8 {
                let ph = k as f32 * PI / 4.0;
                let x: Vec<f32> = (0..2 * cycle)
                    .map(|i| (2.0 * PI * f * i as f32 / SR + ph).sin() * 0.5)
                    .collect();
                let (mut l0, mut r0) = (x.clone(), x.clone());
                stutter_guarded(&p, &mut l0, &mut r0, step, 0, false);
                let (mut l1, mut r1) = (x.clone(), x.clone());
                stutter_guarded(&p, &mut l1, &mut r1, step, 0, true);
                assert!(l1.iter().all(|v| v.is_finite()) && l1 == r1);
                off = off.max(stop_tail_dc(&l0, cycle, tail));
                on = on.max(stop_tail_dc(&l1, cycle, tail));
            }
        }
        eprintln!("tape-stop DC (worst mean of the last 100 ms): guard off {off:.4} FS, on {on:.4} FS ({:.1} dB)", 20.0 * (on / off).log10());
        assert!(off > 0.10, "the unguarded stop drifts: {off}");
        assert!(on < 0.015, "the guarded stop: {on}");
        // the rack's entry point runs the guard
        let x: Vec<f32> = (0..cycle)
            .map(|i| (2.0 * PI * 110.0 * i as f32 / SR).sin() * 0.5)
            .collect();
        let (mut a, mut b) = (x.clone(), x.clone());
        stutter(&p, &mut a, &mut b, step);
        let (mut c, mut d) = (x.clone(), x.clone());
        stutter_guarded(&p, &mut c, &mut d, step, 0, true);
        assert_eq!(a, c);
    }

    /// Re-trigger: the ramp starts again at every cycle. Warm-started, the
    /// guard adds no blip there: the mean of the output over a whole number
    /// of source periods just after the fade-in at each re-trigger (where
    /// the speed is still ~1) stays under the same 0.015 FS bound. A guard
    /// started cold at the re-trigger (computed here for comparison) blips.
    #[test]
    fn tape_stop_dc_guard_retrigger_has_no_blip() {
        let step = 0.125;
        let cycle = (16.0 * step * SR) as usize;
        let p = StutterFx {
            mode: StutterMode::TapeStop,
            cycle_steps: 16.0,
            ..Default::default()
        };
        let fade = ((p.smooth_ms.max(0.5) * 0.001 * SR) as usize).max(1);
        let (mut off, mut on, mut cold) = (0.0f32, 0.0f32, 0.0f32);
        for (f, periods) in [(55.0f32, 2.0f32), (110.0, 3.0), (220.0, 6.0)] {
            let w = (periods / f * SR).round() as usize;
            for k in 0..16 {
                let ph = k as f32 * PI / 8.0;
                let x: Vec<f32> = (0..3 * cycle)
                    .map(|i| (2.0 * PI * f * i as f32 / SR + ph).sin() * 0.5)
                    .collect();
                let (mut l0, mut r0) = (x.clone(), x.clone());
                stutter_guarded(&p, &mut l0, &mut r0, step, 0, false);
                let (mut l1, mut r1) = (x.clone(), x.clone());
                stutter_guarded(&p, &mut l1, &mut r1, step, 0, true);
                // the same ramp with the guard started from zero at the re-trigger
                let mut c = vec![0.0f32; 3 * cycle];
                for c0 in [cycle, 2 * cycle] {
                    let mut g = TapeDcGuard::default();
                    for j in c0..c0 + cycle {
                        let u = (j - c0) as f32 / cycle as f32;
                        let srcf = c0 as f32 + cycle as f32 * (u - 0.5 * u * u);
                        let a = ((j - c0) as f32 / fade as f32)
                            .min(1.0)
                            .min(((c0 + cycle - j) as f32 / fade as f32).min(1.0));
                        c[j] = g.process(lerp_read(&x, srcf)) * (1.0 - u).powf(0.3) * a;
                    }
                }
                let dc = |y: &[f32], c0: usize| {
                    (y[c0 + fade..c0 + fade + w]
                        .iter()
                        .map(|v| *v as f64)
                        .sum::<f64>()
                        / w as f64)
                        .abs() as f32
                };
                for c0 in [cycle, 2 * cycle] {
                    off = off.max(dc(&l0, c0));
                    on = on.max(dc(&l1, c0));
                    cold = cold.max(dc(&c, c0));
                }
            }
        }
        eprintln!("tape-stop re-trigger DC (whole periods after the fade-in): guard off {off:.4} FS, warm guard {on:.4} FS, cold-started guard {cold:.4} FS");
        assert!(on < 0.015, "no blip at the re-trigger: {on}");
        assert!(cold > 0.015, "a cold start would blip: {cold}");
    }

    /// Rate to zero: one 16 s ramp, the last seconds crawling toward rate 0.
    /// Finite, bounded, and decaying to silence; bypassed, the effect leaves
    /// the audio untouched and keeps no state (the next pass is a fresh one).
    #[test]
    fn tape_stop_dc_guard_long_ramp_and_bypass() {
        let step = 0.125;
        let p = StutterFx {
            mode: StutterMode::TapeStop,
            cycle_steps: 128.0,
            ..Default::default()
        };
        let cycle = (128.0 * step * SR) as usize;
        let x: Vec<f32> = (0..cycle)
            .map(|i| (2.0 * PI * 41.2 * i as f32 / SR).sin() * 0.5)
            .collect();
        let (mut l, mut r) = (x.clone(), x.clone());
        stutter(&p, &mut l, &mut r, step);
        assert!(l.iter().all(|v| v.is_finite() && v.abs() <= 0.75));
        let tail = l[cycle - (0.05 * SR) as usize..]
            .iter()
            .fold(0.0f32, |a, v| a.max(v.abs()));
        let early = rms(&l[..SR as usize]);
        eprintln!("16 s tape stop: rms first second {early:.3}, max |y| last 50 ms {tail:.2e}");
        assert!(tail < 0.01, "decays toward the stop: {tail}");
        // bypass: untouched, and un-bypassed it is the same as a fresh pass
        let mut b = Effect::Stutter(StutterFx {
            bypass: true,
            ..p.clone()
        });
        let ctx_tr = std::collections::HashMap::new();
        let ctx = FxContext {
            step_secs: step,
            triggers: &ctx_tr,
            offset: 0,
            levels: None,
        };
        let (mut bl, mut br) = (x.clone(), x.clone());
        b.process(&mut bl, &mut br, &ctx);
        assert!(bl == x && br == x);
        b.set_bypass(false);
        let (mut ul, mut ur) = (x.clone(), x.clone());
        b.process(&mut ul, &mut ur, &ctx);
        assert!(ul == l && ur == r);
    }

    /// Rate to exactly 0, held, then back up to 1: a tape transport whose
    /// read position integrates a rate envelope (1 -> 0 over 1 s, exactly 0.0
    /// for 2 s, 0 -> 1 over 1 s, then 1 for 0.5 s) reads a 110 + 41.2 Hz
    /// source through the guard. (The effects' own ramps only run 1 -> 0 and
    /// restart; this drives the guard through the full stop-and-start.)
    /// Every sample is finite, the state is never denormal (outputs under
    /// 1e-20 flush to 0), the output stays bounded, settles to exactly 0
    /// during the hold, comes back with the source, and the restart has no
    /// sample-to-sample jump larger than the source's own.
    #[test]
    fn tape_stop_dc_guard_rate_to_zero_and_back() {
        let sr = SR as usize;
        let x: Vec<f32> = (0..6 * sr)
            .map(|i| {
                let t = i as f32 / SR;
                (2.0 * PI * 110.0 * t).sin() * 0.5 + (2.0 * PI * 41.2 * t).sin() * 0.25
            })
            .collect();
        let src_jump = x
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        let rate = |i: usize| -> f64 {
            let t = i as f64 / SR as f64;
            if t < 1.0 {
                1.0 - t
            } else if t < 3.0 {
                0.0
            } else if t < 4.0 {
                t - 3.0
            } else {
                1.0
            }
        };
        let (mut gl, mut gr) = (TapeDcGuard::default(), TapeDcGuard::default());
        let mut pos = 0.0f64;
        let n = (4.5 * SR) as usize;
        let mut y = Vec::with_capacity(n);
        for i in 0..n {
            let rd = lerp_read(&x, pos as f32);
            let (a, b) = (gl.process(rd), gr.process(-rd));
            assert!(a.is_finite() && b.is_finite(), "sample {i}");
            for v in gl.state().into_iter().chain(gr.state()) {
                assert!(
                    v == 0.0 || v.is_normal(),
                    "denormal state {v:e} at sample {i}"
                );
            }
            y.push(a);
            pos += rate(i);
        }
        let peak = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let hold_end = y[3 * sr - sr / 2..3 * sr]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()));
        let back = rms(&y[4 * sr..]);
        let restart_jump = y[3 * sr..4 * sr]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        eprintln!("rate 1 -> 0 -> hold -> 1: peak {peak:.3}, max |y| last 0.5 s of the hold {hold_end:e}, rms back at rate 1 {back:.3}, max jump on the restart {restart_jump:.5} (source's own {src_jump:.5})");
        assert!(peak <= 1.0, "bounded: {peak}");
        assert_eq!(hold_end, 0.0, "the hold settles to exactly 0");
        assert!(back > 0.3, "back with the source: {back}");
        assert!(
            restart_jump <= src_jump,
            "the restart: {restart_jump} vs the source's {src_jump}"
        );
    }

    /// Rapid trigger -> interrupt -> retrigger, the way a transition plays a
    /// tape stop: the effect's mix automated (fx.0.mix, step points) through
    /// the rack's own automation path, ramps restarting every 1/8 note, the
    /// wet interrupted and re-triggered three times, each re-trigger
    /// mid-ramp (60%, 20%, 70% in). The guard is warm at every onset (it
    /// runs on the wet whether or not the mix lets it through, and each ramp
    /// starts from its warm-up): the largest sample-to-sample jump from 64
    /// samples before to 512 after each onset stays within the source's own
    /// largest jump (tolerance 0%). The wet alone, guarded and unguarded, is
    /// printed beside it.
    #[test]
    fn tape_stop_dc_guard_rapid_retrigger_no_onset_click() {
        use crate::automation::{AutoPoint, AutomationLane, Curve};
        let mut p = crate::project::Project::new("t", 120.0);
        for pt in p.patterns.iter_mut() {
            pt.bars = 16;
        }
        let tl = crate::render::Timeline::new(&p, &crate::render::RenderOptions::default());
        let sr = SR as usize;
        let n = 3 * sr;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / SR;
                (2.0 * PI * 110.0 * t).sin() * 0.5 + (2.0 * PI * 41.2 * t + 0.7).sin() * 0.25
            })
            .collect();
        let src_jump = x
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        // trigger at 0; interrupt / re-trigger (beats; a ramp restarts every 0.5 beat)
        let edges = [
            (1.3f32, 0.0f32),
            (1.8, 1.0),
            (2.6, 0.0),
            (3.1, 1.0),
            (3.9, 0.0),
            (4.35, 1.0),
        ];
        let mut lane = AutomationLane::new("x", "fx.0.mix");
        lane.points.push(AutoPoint {
            beat: 0.0,
            value: 1.0,
            curve: Curve::Step,
        });
        for (b, v) in edges {
            lane.points.push(AutoPoint {
                beat: b,
                value: v,
                curve: Curve::Step,
            });
        }
        let onsets: Vec<usize> = edges
            .iter()
            .filter(|e| e.1 > 0.0)
            .map(|e| (e.0 * 0.5 * SR) as usize)
            .collect();
        let tr = std::collections::HashMap::new();
        let ctx = FxContext {
            step_secs: 0.125,
            triggers: &tr,
            offset: 0,
            levels: None,
        };
        let effects = [
            (
                "stutter tape_stop",
                Effect::Stutter(StutterFx {
                    mode: StutterMode::TapeStop,
                    cycle_steps: 2.0,
                    ..Default::default()
                }),
            ),
            (
                "gross_beat tape_stop",
                Effect::GrossBeat(crate::fx::GrossBeatFx {
                    time: "tape_stop".into(),
                    cycle_steps: 2.0,
                    ..Default::default()
                }),
            ),
            (
                "gross_beat tape_stop_end",
                Effect::GrossBeat(crate::fx::GrossBeatFx {
                    time: "tape_stop_end".into(),
                    cycle_steps: 2.0,
                    ..Default::default()
                }),
            ),
        ];
        for (name, fx) in effects {
            let (mut l, mut r) = (x.clone(), x.clone());
            crate::render::process_chain(
                std::slice::from_ref(&fx),
                &[&lane],
                &mut l,
                &mut r,
                &ctx,
                &tl,
            );
            assert!(l.iter().all(|v| v.is_finite()));
            // the same automation over the effect without the guard
            let unguarded = |l: &mut [f32], r: &mut [f32]| match &fx {
                Effect::Stutter(s) => stutter_guarded(s, l, r, 0.125, 0, false),
                Effect::GrossBeat(g) => crate::fx_time::gross_beat_unguarded(g, l, r, 0.125, 0),
                _ => unreachable!(),
            };
            let (mut wl, mut wr) = (x.clone(), x.clone());
            unguarded(&mut wl, &mut wr);
            let (mut gl, mut gr) = (x.clone(), x.clone());
            fx.process(&mut gl, &mut gr, &ctx);
            for &o in &onsets {
                let w = o - 64..o + 512;
                let jump = |y: &[f32]| {
                    y[w.clone()]
                        .windows(2)
                        .map(|v| (v[1] - v[0]).abs())
                        .fold(0.0f32, f32::max)
                };
                let j = jump(&l);
                // the guarded vs unguarded wet at the onset (the automation
                // blends these with the dry)
                let (jg, ju) = (jump(&gl), jump(&wl));
                eprintln!("{name} re-trigger at {:.3} s: max jump {j:.5} (source's own {src_jump:.5}); wet alone guarded {jg:.5} / unguarded {ju:.5}", o as f32 / SR);
                assert!(
                    j <= src_jump,
                    "{name}: onset click at {o}: {j} > {src_jump}"
                );
            }
        }
    }

    /// The guard touches nothing but tape_stop mode: every other mode is the
    /// same bit for bit with it on or off.
    #[test]
    fn tape_stop_dc_guard_leaves_other_modes_bit_identical() {
        let x: Vec<f32> = (0..40_000)
            .map(|i| (2.0 * PI * 97.0 * i as f32 / SR).sin() * 0.4 + 0.1 * (i as f32 * 0.37).sin())
            .collect();
        for mode in [
            StutterMode::Gate,
            StutterMode::Stutter,
            StutterMode::HalfTime,
            StutterMode::Reverse,
        ] {
            let p = StutterFx {
                mode,
                mix: 0.8,
                ..Default::default()
            };
            let (mut a, mut b) = (x.clone(), x.clone());
            stutter_guarded(&p, &mut a, &mut b, 0.125, 0, false);
            let (mut c, mut d) = (x.clone(), x.clone());
            stutter_guarded(&p, &mut c, &mut d, 0.125, 0, true);
            assert!(a == c && b == d, "{mode:?}");
        }
    }

    /// With the guard warmed per ramp from the song position, a buffer that
    /// starts `offset` into the song (silent before it, as the windowed
    /// renders read it, and for 0.5 s after: the live-splice convention) matches the same samples of a whole-song pass exactly,
    /// including a buffer that starts mid-ramp.
    #[test]
    fn tape_stop_dc_guard_follows_the_song_position() {
        let p = StutterFx {
            mode: StutterMode::TapeStop,
            cycle_steps: 4.0,
            ..Default::default()
        };
        let n = 200_000;
        for off in [44_100usize, 50_000] {
            let x: Vec<f32> = (0..n)
                .map(|i| {
                    if i >= off + 22_050 {
                        (2.0 * PI * 61.0 * i as f32 / SR).sin() * 0.5
                    } else {
                        0.0
                    }
                })
                .collect();
            let (mut fl, mut fr) = (x.clone(), x.clone());
            stutter_at(&p, &mut fl, &mut fr, 0.125, 0);
            let (mut wl, mut wr) = (x[off..].to_vec(), x[off..].to_vec());
            stutter_at(&p, &mut wl, &mut wr, 0.125, off);
            assert!(fl[off..] == wl[..] && fr[off..] == wr[..], "offset {off}");
        }
    }

    #[test]
    fn stutter_modes_are_finite_and_gate_mutes() {
        for mode in [
            StutterMode::Gate,
            StutterMode::Stutter,
            StutterMode::HalfTime,
            StutterMode::Reverse,
            StutterMode::TapeStop,
        ] {
            let mut l = sine(300.0, 30000, 0.5);
            let mut r = l.clone();
            let p = StutterFx {
                mode,
                pattern: "x.".into(),
                ..Default::default()
            };
            stutter(&p, &mut l, &mut r, 0.125);
            assert!(l.iter().all(|v| v.is_finite()));
            if mode == StutterMode::Gate {
                let step = (0.125 * SR) as usize;
                assert!(rms(&l[step + 500..2 * step - 500]) < 1e-3);
                assert!(rms(&l[500..step - 500]) > 0.3);
            }
        }
    }

    #[test]
    fn parametric_eq_response_matches_bands() {
        let p = ParametricEqFx {
            bands: vec![
                EqBand {
                    kind: EqBandKind::LowCut,
                    freq: 100.0,
                    ..Default::default()
                },
                EqBand {
                    kind: EqBandKind::Bell,
                    freq: 2000.0,
                    gain_db: 4.0,
                    q: 1.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(eq_response_db(&p, 20.0) < -20.0);
        assert!((eq_response_db(&p, 2000.0) - 4.0).abs() < 0.3);
        let mut l = sine(30.0, 20000, 0.5);
        let mut r = l.clone();
        parametric_eq(&p, &mut l, &mut r);
        assert!(rms(&l[5000..]) < 0.1);
    }

    #[test]
    fn level_keyed_duck_holds_through_a_held_vowel_and_glides_without_steps() {
        let p = DynamicEqFx {
            freq: 500.0,
            q: 2.0,
            range_db: -4.0,
            attack_ms: 12.0,
            release_ms: 100.0,
            source: "vocal".into(),
            key: "level".into(),
            ..Default::default()
        };
        let sec = SR as usize;
        // a 1 s held vowel (no new onsets), 1 s of silence, then a hard entry
        let mut key = sine(220.0, sec, 0.3);
        key.extend(vec![0.0; sec]);
        key.extend(sine(220.0, sec, 0.3));
        let env = level_duck_curve(&p, &key, key.len());
        let held = &env[(0.1 * SR) as usize..sec];
        assert!(
            held.iter().all(|&e| e > 0.95),
            "the dip sags inside a held vowel: min {}",
            held.iter().cloned().fold(1.0, f32::min)
        );
        assert!(
            env[sec + (0.7 * SR) as usize..2 * sec]
                .iter()
                .all(|&e| e < 0.01),
            "the dip lingers in the silence"
        );
        // the gain curve glides: no per-sample step over 1e-3 (a -4 dB dip is a 0.37 swing)
        let g: Vec<f32> = env.iter().map(|&e| db_to_gain(-4.0 * e)).collect();
        let step = g
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(step < 1e-3, "gain step {step}");
        // silent key: the band is untouched, sample for sample
        let mut l = sine(500.0, sec, 0.5);
        let mut r = l.clone();
        let dry = l.clone();
        level_keyed_dynamic_eq(&p, &vec![0.0; sec], &mut l, &mut r);
        assert!(l == dry && r == dry);
        // sounding key: the band comes down by about the range
        let mut l2 = sine(500.0, sec, 0.5);
        let mut r2 = l2.clone();
        level_keyed_dynamic_eq(&p, &sine(220.0, sec, 0.3), &mut l2, &mut r2);
        let drop = 20.0 * (rms(&l2[sec / 2..]) / rms(&dry[sec / 2..])).log10();
        assert!(drop < -3.0 && drop > -4.5, "drop {drop}");
    }

    #[test]
    fn freq_splitter_bands_sum_back_to_the_delayed_input() {
        let n = 8000usize;
        let sig: Vec<f32> = (0..n)
            .map(|i| {
                ((i as f32) * 0.05).sin() * 0.4
                    + ((i as f32) * 0.4).sin() * 0.2
                    + ((i as f32) * 1.7).sin() * 0.1
            })
            .collect();
        let taps = 513u32;
        let m = ((taps - 1) / 2) as usize;
        let band = |b: FreqSplitBand, x: &[f32]| {
            let mut l = x.to_vec();
            let mut r = x.to_vec();
            freq_splitter(
                &FreqSplitterFx {
                    band: b,
                    taps,
                    ..Default::default()
                },
                &mut l,
                &mut r,
            );
            l
        };
        let lo = band(FreqSplitBand::Low, &sig);
        let mid = band(FreqSplitBand::Mid, &sig);
        let hi = band(FreqSplitBand::High, &sig);
        // the bands share one linear phase, so their sum tracks the
        // delayed input to convolution round-off (not bit-exact: f32
        // addition does not reassociate)
        let mut worst = 0.0f32;
        for i in m..n {
            let xd = sig[i - m];
            let sum = lo[i] + mid[i] + hi[i];
            worst = worst.max((sum - xd).abs());
        }
        assert!(worst < 1e-4, "band-sum error {worst}");
        // crossover placement: a 100 Hz sine lands in low, a 6 kHz sine
        // in high, each down > 40 dB in the other band
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let lo_sig: Vec<f32> = (0..n).map(|i| (i as f32 * 0.014).sin() * 0.5).collect();
        let hi_sig: Vec<f32> = (0..n).map(|i| (i as f32 * 0.85).sin() * 0.5).collect();
        let l_in_lo = rms(&band(FreqSplitBand::Low, &lo_sig)[m..]);
        let l_in_hi = rms(&band(FreqSplitBand::High, &lo_sig)[m..]);
        let h_in_hi = rms(&band(FreqSplitBand::High, &hi_sig)[m..]);
        let h_in_lo = rms(&band(FreqSplitBand::Low, &hi_sig)[m..]);
        assert!(
            l_in_lo > 20.0 * l_in_hi,
            "low sine leak {l_in_lo}/{l_in_hi}"
        );
        assert!(
            h_in_hi > 20.0 * h_in_lo,
            "high sine leak {h_in_hi}/{h_in_lo}"
        );
    }

    #[test]
    fn freq_splitter_is_deterministic_and_forces_odd_taps() {
        let n = 2000usize;
        let sig: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.11).sin() * 0.5).collect();
        let run = |taps: u32| {
            let mut l = sig.clone();
            let mut r = sig.clone();
            freq_splitter(
                &FreqSplitterFx {
                    taps,
                    band: FreqSplitBand::Mid,
                    ..Default::default()
                },
                &mut l,
                &mut r,
            );
            l
        };
        let a = run(257);
        let b = run(257);
        assert!(a
            .iter()
            .zip(b.iter())
            .all(|(x, y)| x.to_bits() == y.to_bits()));
        // even taps are forced odd: 256 == 257 bit for bit
        let c = run(256);
        assert!(a
            .iter()
            .zip(c.iter())
            .all(|(x, y)| x.to_bits() == y.to_bits()));
        // unity DC through the low band: DC in, same DC out (post-delay)
        let dc = vec![0.5f32; n];
        let mut l = dc.clone();
        let mut r = dc.clone();
        freq_splitter(
            &FreqSplitterFx {
                band: FreqSplitBand::Low,
                ..Default::default()
            },
            &mut l,
            &mut r,
        );
        let m = 256usize;
        for i in m + 512..n {
            assert!((l[i] - 0.5).abs() < 1e-5, "dc at {i}: {}", l[i]);
        }
    }

    fn gr_sig(n: usize) -> (Vec<f32>, Vec<f32>) {
        let l: Vec<f32> = (0..n)
            .map(|i| (i as f32 * 0.05).sin() * 0.5 + (i as f32 * 0.013).sin() * 0.3)
            .collect();
        let r: Vec<f32> = l.iter().rev().copied().collect();
        (l, r)
    }

    #[test]
    fn granular_mix_zero_is_bit_exact_bypass() {
        let (l0, r0) = gr_sig(20_000);
        let (mut l, mut r) = (l0.clone(), r0.clone());
        let p = crate::fx::GranularFx {
            mix: 0.0,
            ..Default::default()
        };
        granular(&p, &mut l, &mut r);
        assert!(l.iter().zip(&l0).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(r.iter().zip(&r0).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn granular_is_deterministic_per_seed_and_seeds_differ() {
        let (l0, r0) = gr_sig(30_000);
        let run = |seed: u32| {
            let (mut l, mut r) = (l0.clone(), r0.clone());
            let p = crate::fx::GranularFx {
                mix: 1.0,
                seed,
                ..Default::default()
            };
            granular(&p, &mut l, &mut r);
            (l, r)
        };
        let (a, b) = (run(7), run(7));
        assert!(a
            .0
            .iter()
            .zip(&b.0)
            .all(|(x, y)| x.to_bits() == y.to_bits()));
        assert!(a
            .1
            .iter()
            .zip(&b.1)
            .all(|(x, y)| x.to_bits() == y.to_bits()));
        let c = run(8);
        assert!(a
            .0
            .iter()
            .zip(&c.0)
            .any(|(x, y)| x.to_bits() != y.to_bits()));
        assert!(a.0.iter().chain(&a.1).all(|v| v.is_finite()));
        assert!(a.0.iter().any(|v| v.abs() > 1e-3), "wet path is silent");
    }

    #[test]
    fn granular_is_causal_and_pitch_up_does_not_peek_ahead() {
        let n = 30_000;
        let mut l = vec![0.0f32; n];
        l[20_000] = 1.0;
        let r = l.clone();
        for semis in [0.0f32, 12.0, 24.0] {
            let (mut a, mut b) = (l.clone(), r.clone());
            let p = crate::fx::GranularFx {
                mix: 1.0,
                pitch_semitones: semis,
                ..Default::default()
            };
            granular(&p, &mut a, &mut b);
            assert!(
                a[..20_000].iter().chain(&b[..20_000]).all(|v| *v == 0.0),
                "output before the impulse at {semis} st"
            );
        }
    }

    #[test]
    fn granular_freeze_keeps_sounding_after_the_input_stops() {
        let n = 60_000;
        let (mut l, mut r) = gr_sig(n);
        for i in 20_000..n {
            l[i] = 0.0;
            r[i] = 0.0;
        }
        let tail_energy = |freeze_at_ms: f32| {
            let (mut a, mut b) = (l.clone(), r.clone());
            let p = crate::fx::GranularFx {
                mix: 1.0,
                freeze_at_ms,
                ..Default::default()
            };
            granular(&p, &mut a, &mut b);
            a[40_000..].iter().map(|v| v * v).sum::<f32>()
        };
        let off = tail_energy(-1.0);
        let held = tail_energy(400.0);
        assert!(off < 1e-9, "unfrozen tail should be silent, got {off}");
        assert!(held > 1e-3, "frozen tail should keep sounding, got {held}");
    }

    #[test]
    fn granular_validation_rejects_nonsense() {
        use crate::fx::GranularFx;
        assert!(check_granular(&GranularFx::default()).is_ok());
        for bad in [
            GranularFx {
                grain_ms: 1.0,
                ..Default::default()
            },
            GranularFx {
                density_hz: 0.0,
                ..Default::default()
            },
            GranularFx {
                pitch_semitones: 30.0,
                ..Default::default()
            },
            GranularFx {
                mix: 1.5,
                ..Default::default()
            },
            GranularFx {
                spread: f32::NAN,
                ..Default::default()
            },
        ] {
            assert!(check_granular(&bad).is_err());
        }
    }

    #[test]
    fn granular_density_cap_bounds_active_grains() {
        let p = crate::fx::GranularFx {
            density_hz: 200.0,
            grain_ms: 500.0,
            mix: 1.0,
            ..Default::default()
        };
        let mut core = GranularCore::new(&p, 200_000);
        for i in 0..200_000usize {
            let x = (i as f32 * 0.02).sin() * 0.3;
            core.step(x, x);
        }
        assert!(core.peak_active > 50, "peak {}", core.peak_active);
        assert!(
            core.peak_active <= GRANULAR_MAX_GRAINS,
            "peak {} over the cap",
            core.peak_active
        );
    }

    #[test]
    fn granular_survives_nan_inf_and_denormal_input() {
        let n = 20_000usize;
        let mut l: Vec<f32> = (0..n).map(|i| (i as f32 * 0.03).sin() * 0.5).collect();
        let mut r = l.clone();
        for i in (0..n).step_by(997) {
            l[i] = f32::NAN;
            r[i] = f32::INFINITY;
        }
        for i in (3..n).step_by(1013) {
            l[i] = f32::NEG_INFINITY;
            r[i] = 1.0e-42;
        }
        for freeze_at_ms in [-1.0f32, 100.0] {
            let (mut a, mut b) = (l.clone(), r.clone());
            let p = crate::fx::GranularFx {
                mix: 0.7,
                freeze_at_ms,
                pitch_semitones: 9.0,
                ..Default::default()
            };
            granular(&p, &mut a, &mut b);
            assert!(a.iter().chain(&b).all(|v| v.is_finite()));
        }
    }

    #[test]
    fn granular_freeze_on_an_empty_buffer_is_silent_and_does_not_panic() {
        let p = crate::fx::GranularFx {
            mix: 1.0,
            freeze_at_ms: 0.0,
            ..Default::default()
        };
        let (mut l, mut r) = (vec![0.0f32; 10_000], vec![0.0f32; 10_000]);
        granular(&p, &mut l, &mut r);
        assert!(l.iter().chain(&r).all(|v| *v == 0.0));
        let (mut el, mut er): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
        granular(&p, &mut el, &mut er);
        let mut core = GranularCore::new(&p, 0);
        assert_eq!(core.step(0.0, 0.0), (0.0, 0.0));
    }

    #[test]
    fn granular_two_instances_same_seed_agree() {
        let p = crate::fx::GranularFx {
            mix: 1.0,
            seed: 99,
            ..Default::default()
        };
        let (mut a, mut b) = (GranularCore::new(&p, 40_000), GranularCore::new(&p, 40_000));
        for i in 0..40_000usize {
            let x = (i as f32 * 0.011).sin() * 0.4;
            let (ya, yb) = (a.step(x, -x), b.step(x, -x));
            assert_eq!(ya.0.to_bits(), yb.0.to_bits());
            assert_eq!(ya.1.to_bits(), yb.1.to_bits());
        }
    }

    #[test]
    fn freq_splitter_validation_rejects_only_nonsense() {
        use crate::fx::FreqSplitterFx as F;
        assert!(check_freq_splitter(&F::default()).is_ok());
        // inverted stays valid (swapped as before)
        assert!(check_freq_splitter(&F {
            low_hz: 2500.0,
            high_hz: 250.0,
            ..Default::default()
        })
        .is_ok());
        for taps in [33u32, 513, 65_537] {
            assert!(check_freq_splitter(&F {
                taps,
                ..Default::default()
            })
            .is_ok());
        }
        for bad in [
            F {
                taps: 1,
                ..Default::default()
            },
            F {
                taps: 31,
                ..Default::default()
            },
            F {
                taps: 1_048_577,
                ..Default::default()
            },
            F {
                taps: u32::MAX,
                ..Default::default()
            },
            F {
                low_hz: 1000.0,
                high_hz: 1000.0,
                ..Default::default()
            },
            F {
                low_hz: f32::NAN,
                ..Default::default()
            },
            F {
                high_hz: f32::INFINITY,
                ..Default::default()
            },
            F {
                low_hz: -100.0,
                ..Default::default()
            },
            F {
                high_hz: 30_000.0,
                ..Default::default()
            },
            F {
                gain_db: 80.0,
                ..Default::default()
            },
            F {
                mix: 1.5,
                ..Default::default()
            },
        ] {
            assert!(check_freq_splitter(&bad).is_err(), "{bad:?}");
        }
    }

    fn fs_sig(n: usize) -> (Vec<f32>, Vec<f32>) {
        let l: Vec<f32> = (0..n)
            .map(|i| (i as f32 * 0.05).sin() * 0.5 + (i as f32 * 0.013).sin() * 0.3)
            .collect();
        let r: Vec<f32> = l.iter().rev().copied().collect();
        (l, r)
    }

    fn fs_hash(l: &[f32], r: &[f32]) -> u64 {
        l.iter().chain(r).fold(0u64, |h, v| {
            h.rotate_left(5) ^ (v.to_bits() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        })
    }

    #[test]
    fn freq_splitter_validation_edges_and_non_finite() {
        use crate::fx::FreqSplitterFx as F;
        let ok = |p: F| check_freq_splitter(&p).is_ok();
        // each bound: edge accepted, one past rejected
        assert!(ok(F {
            taps: 33,
            ..Default::default()
        }));
        assert!(!ok(F {
            taps: 32,
            ..Default::default()
        }));
        assert!(ok(F {
            taps: 65_537,
            ..Default::default()
        }));
        assert!(!ok(F {
            taps: 65_538,
            ..Default::default()
        }));
        assert!(ok(F {
            low_hz: 20.0,
            ..Default::default()
        }));
        assert!(!ok(F {
            low_hz: 19.99,
            ..Default::default()
        }));
        assert!(ok(F {
            high_hz: 20_000.0,
            ..Default::default()
        }));
        assert!(!ok(F {
            high_hz: 20_000.1,
            ..Default::default()
        }));
        assert!(ok(F {
            gain_db: -60.0,
            ..Default::default()
        }));
        assert!(!ok(F {
            gain_db: -60.1,
            ..Default::default()
        }));
        assert!(ok(F {
            gain_db: 24.0,
            ..Default::default()
        }));
        assert!(!ok(F {
            gain_db: 24.1,
            ..Default::default()
        }));
        assert!(ok(F {
            mix: 0.0,
            ..Default::default()
        }));
        assert!(!ok(F {
            mix: -0.01,
            ..Default::default()
        }));
        assert!(ok(F {
            mix: 1.0,
            ..Default::default()
        }));
        assert!(!ok(F {
            mix: 1.01,
            ..Default::default()
        }));
        // inverted accepted, equal rejected
        assert!(ok(F {
            low_hz: 900.0,
            high_hz: 300.0,
            ..Default::default()
        }));
        assert!(!ok(F {
            low_hz: 300.0,
            high_hz: 300.0,
            ..Default::default()
        }));
        // NaN and infinities per float param
        for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(!ok(F {
                low_hz: v,
                ..Default::default()
            }));
            assert!(!ok(F {
                high_hz: v,
                ..Default::default()
            }));
            assert!(!ok(F {
                gain_db: v,
                ..Default::default()
            }));
            assert!(!ok(F {
                mix: v,
                ..Default::default()
            }));
        }
        // the error is a plain message naming the field, raised before
        // any buffer exists (taps is checked first, no Vec is sized by it)
        let e = check_freq_splitter(&F {
            taps: u32::MAX,
            ..Default::default()
        })
        .unwrap_err();
        assert!(e.contains("taps"), "{e}");
    }

    #[test]
    fn freq_splitter_mix_zero_is_bit_exact_and_even_taps_round_up() {
        use crate::fx::{FreqSplitBand, FreqSplitterFx as F};
        let (l0, r0) = fs_sig(8192);
        let (mut l, mut r) = (l0.clone(), r0.clone());
        freq_splitter(
            &F {
                mix: 0.0,
                ..Default::default()
            },
            &mut l,
            &mut r,
        );
        assert!(l.iter().zip(&l0).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(r.iter().zip(&r0).all(|(a, b)| a.to_bits() == b.to_bits()));
        // even taps round UP to the next odd (513 and 514 -> 513 and 515)
        let run = |taps: u32| {
            let (mut a, mut b) = (l0.clone(), r0.clone());
            freq_splitter(
                &F {
                    taps,
                    band: FreqSplitBand::Mid,
                    ..Default::default()
                },
                &mut a,
                &mut b,
            );
            fs_hash(&a, &b)
        };
        assert_eq!(run(514), run(515));
        assert_ne!(run(513), run(515));
    }

    /// Frozen copy of freq_splitter as it was when validation landed. The
    /// live fn must stay bit-identical to it on the same build (no libm
    /// dependence: both run here).
    fn freq_splitter_reference(p: &crate::fx::FreqSplitterFx, l: &mut [f32], r: &mut [f32]) {
        let taps = (p.taps as usize).max(3) | 1;
        let m = (taps - 1) / 2;
        let lo = p.low_hz.min(p.high_hz);
        let hi = p.high_hz.max(p.low_hz);
        let h1 = lowpass_fir(lo, taps);
        let h2 = lowpass_fir(hi, taps);
        let g = db_to_gain(p.gain_db);
        let mix = p.mix.clamp(0.0, 1.0);
        for chan in [l, r] {
            let x = chan.to_vec();
            let s1 = if p.band == crate::fx::FreqSplitBand::High {
                Vec::new()
            } else {
                convolve(&x, &h1)
            };
            let s2 = if p.band == crate::fx::FreqSplitBand::Low {
                Vec::new()
            } else {
                convolve(&x, &h2)
            };
            for i in 0..chan.len() {
                let wet = match p.band {
                    crate::fx::FreqSplitBand::Low => s1[i],
                    crate::fx::FreqSplitBand::Mid => s2[i] - s1[i],
                    crate::fx::FreqSplitBand::High => (if i >= m { x[i - m] } else { 0.0 }) - s2[i],
                };
                chan[i] = (wet * g) * mix + x[i] * (1.0 - mix);
            }
        }
    }

    #[test]
    fn freq_splitter_matches_its_frozen_reference_bit_for_bit() {
        use crate::fx::{FreqSplitBand, FreqSplitterFx as F};
        let (l0, r0) = fs_sig(6000);
        for band in [FreqSplitBand::Low, FreqSplitBand::Mid, FreqSplitBand::High] {
            for (taps, low_hz, high_hz, gain_db, mix) in [
                (513u32, 250.0f32, 2500.0f32, 0.0f32, 1.0f32),
                (34, 250.0, 2500.0, 0.0, 1.0),
                (64, 900.0, 300.0, 3.0, 0.6),
                (257, 120.0, 8000.0, -6.0, 0.0),
            ] {
                let p = F {
                    taps,
                    low_hz,
                    high_hz,
                    gain_db,
                    mix,
                    band,
                    ..Default::default()
                };
                let (mut a, mut b) = (l0.clone(), r0.clone());
                let (mut c, mut d) = (l0.clone(), r0.clone());
                freq_splitter(&p, &mut a, &mut b);
                freq_splitter_reference(&p, &mut c, &mut d);
                let same = a.iter().zip(&c).all(|(x, y)| x.to_bits() == y.to_bits())
                    && b.iter().zip(&d).all(|(x, y)| x.to_bits() == y.to_bits());
                assert!(same, "band {band:?} taps {taps}");
            }
        }
    }

    /// REPRO (expected red on current code): the windowed-sinc low-pass IR
    /// is symmetric in exact arithmetic but each tap is computed
    /// independently, so mirrored taps can differ in the last float bit.
    /// A linear-phase FIR should be exactly symmetric.
    #[test]
    fn lowpass_fir_is_bit_exact_symmetric() {
        let mut worst = (0usize, 0usize, 0usize, 0.0f32);
        let mut asym = 0usize;
        for taps in [33usize, 34, 64, 65, 129, 513, 1025, 4097] {
            for cutoff in [20.0f32, 250.0, 1000.0, 2500.0, 12000.0, 20000.0] {
                let h = lowpass_fir(cutoff, taps);
                let n = h.len();
                for i in 0..n / 2 {
                    if h[i].to_bits() != h[n - 1 - i].to_bits() {
                        asym += 1;
                        let d = (h[i] - h[n - 1 - i]).abs();
                        if d > worst.3 {
                            worst = (taps, cutoff as usize, i, d);
                        }
                    }
                }
            }
        }
        assert_eq!(
            asym, 0,
            "{asym} mirrored tap pairs differ in bits; worst (taps, cutoff, index, diff) = {worst:?}"
        );
    }
}
