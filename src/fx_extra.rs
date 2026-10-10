//! Studio effects added in sprint 4: parametric EQ, multiband compressor,
//! dynamic EQ, de-esser, soft clipper, gate, phaser, flanger, tempo-synced
//! stutter / gross-beat gate, pitch shifter, convolution reverb with
//! synthesized impulse responses, and auto-pan / tremolo.
//!
//! Every processor is a whole-buffer stereo function (like the rest of the
//! rack), deterministic and allocation-light.

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
    let (att, rel) = (coef(p.attack_ms.max(1.0)), coef(p.release_ms.max(5.0) * 0.5));
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

pub fn phaser(p: &PhaserFx, l: &mut [f32], r: &mut [f32], step_secs: f32) {
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
            if i % 16 == 0 {
                let lfo = 0.5 + 0.5 * (2.0 * PI * (rate * i as f32 / SR + phase0)).sin();
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

pub fn flanger(p: &FlangerFx, l: &mut [f32], r: &mut [f32], step_secs: f32) {
    let rate = lfo_rate(p.rate_hz, p.sync_steps, step_secs);
    let max = ((p.delay_ms + p.depth_ms).max(0.1) * 0.001 * SR) as usize + 8;
    let mix = p.mix.clamp(0.0, 1.0);
    let fb = p.feedback.clamp(-0.95, 0.95);
    for (ci, ch) in [&mut *l, &mut *r].into_iter().enumerate() {
        let mut buf = vec![0.0f32; max];
        let phase0 = ci as f32 * 0.25;
        for (i, s) in ch.iter_mut().enumerate() {
            let lfo = 0.5 + 0.5 * (2.0 * PI * (rate * i as f32 / SR + phase0)).sin();
            let d = ((p.delay_ms.max(0.05) + p.depth_ms.max(0.0) * lfo) * 0.001 * SR).max(1.0);
            let w = i % max;
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
    for i in 0..n {
        let (wl, wr) = match p.mode {
            StutterMode::Gate => {
                let k = (i as f32 / step) as usize;
                let lv = levels[k % levels.len()];
                let pos = i - (k as f32 * step) as usize;
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
                let c0 = (i / cycle) * cycle;
                let pos = (i - c0) % slice;
                let src = c0 + pos;
                let g = win(pos, slice);
                (
                    src_l.get(src).copied().unwrap_or(0.0) * g,
                    src_r.get(src).copied().unwrap_or(0.0) * g,
                )
            }
            StutterMode::HalfTime => {
                let c0 = (i / cycle) * cycle;
                let pos = i - c0;
                let srcf = c0 as f32 + pos as f32 * 0.5;
                let g = win(pos, cycle);
                (lerp_read(&src_l, srcf) * g, lerp_read(&src_r, srcf) * g)
            }
            StutterMode::Reverse => {
                let c0 = (i / cycle) * cycle;
                let pos = i - c0;
                let len = cycle.min(n - c0);
                let src = c0 + len - 1 - pos.min(len - 1);
                let g = win(pos, len);
                (src_l[src] * g, src_r[src] * g)
            }
            StutterMode::TapeStop => {
                // speed falls 1 -> 0 over each cycle: position = c0 + T*(u - u^2/2)
                let c0 = (i / cycle) * cycle;
                let u = (i - c0) as f32 / cycle as f32;
                let srcf = c0 as f32 + cycle as f32 * (u - 0.5 * u * u);
                let g = (1.0 - u).powf(0.3) * win(i - c0, cycle).max(0.0);
                (lerp_read(&src_l, srcf) * g, lerp_read(&src_r, srcf) * g)
            }
        };
        l[i] = src_l[i] * (1.0 - mix) + wl * mix;
        r[i] = src_r[i] * (1.0 - mix) + wr * mix;
    }
}

// ---------------- pitch shifter ----------------

pub fn pitch_shift(p: &PitchShiftFx, l: &mut [f32], r: &mut [f32]) {
    let ratio = 2f32.powf((p.semitones.clamp(-24.0, 24.0) + p.cents / 100.0) / 12.0);
    let w = ((p.window_ms.clamp(10.0, 200.0) * 0.001 * SR) as usize).max(64) as f32;
    let mix = p.mix.clamp(0.0, 1.0);
    let size = w as usize * 2 + 4;
    for ch in [&mut *l, &mut *r] {
        let mut buf = vec![0.0f32; size];
        let mut d = 0.0f32;
        for (i, s) in ch.iter_mut().enumerate() {
            let wi = i % size;
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
    let (irl, irr) = synth_ir(p);
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

pub fn autopan(p: &AutopanFx, l: &mut [f32], r: &mut [f32], step_secs: f32) {
    let period = (p.steps.max(0.25) * step_secs * SR).max(1.0);
    let depth = p.depth.clamp(0.0, 1.0);
    for i in 0..l.len() {
        let ph = (i as f32 / period).fract();
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
}
