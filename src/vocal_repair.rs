//! Vocal repair, RX-style: spectral noise reduction from a learned noise
//! profile, hum removal, click repair, de-clip, de-plosive, a breath/noise
//! gate with hysteresis and late-reverb reduction. Every tool writes a new
//! sample and reports what it measured before and after.

use crate::dsp::{db_to_gain, Biquad, BiquadKind, Fft, SR};
use crate::engine::Engine;
use crate::tools::{f_opt, f_or, obj, s_opt, s_req, u_or, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};

pub const N: usize = 2048;
pub const H: usize = 512;

fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
        .collect()
}

/// Magnitude spectra (N/2+1 bins) of every frame.
pub fn mags(x: &[f32]) -> Vec<Vec<f32>> {
    let fft = Fft::new(N);
    let w = hann(N);
    let mut out = Vec::new();
    let mut off = 0usize;
    let mut re = vec![0.0f32; N];
    let mut im = vec![0.0f32; N];
    while off + N <= x.len() {
        for i in 0..N {
            re[i] = x[off + i] * w[i];
            im[i] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        out.push(
            (0..=N / 2)
                .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt())
                .collect(),
        );
        off += H;
    }
    out
}

/// STFT -> per-frame gains from `gain(frame, mags, out_gains)` -> overlap-add.
/// The signal is padded so the edges are processed too.
pub fn spectral<F: FnMut(usize, &[f32], &mut [f32])>(x: &[f32], mut gain: F) -> Vec<f32> {
    let pad = N;
    let mut xp = vec![0.0f32; pad];
    xp.extend_from_slice(x);
    xp.extend(std::iter::repeat_n(0.0, pad + N));
    let fft = Fft::new(N);
    let w = hann(N);
    let mut out = vec![0.0f32; xp.len()];
    let mut ws = vec![0.0f32; xp.len()];
    let mut re = vec![0.0f32; N];
    let mut im = vec![0.0f32; N];
    let mut m = vec![0.0f32; N / 2 + 1];
    let mut g = vec![1.0f32; N / 2 + 1];
    let mut off = 0usize;
    let mut f = 0usize;
    while off + N <= xp.len() {
        for i in 0..N {
            re[i] = xp[off + i] * w[i];
            im[i] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        for k in 0..=N / 2 {
            m[k] = (re[k] * re[k] + im[k] * im[k]).sqrt();
            g[k] = 1.0;
        }
        gain(f, &m, &mut g);
        for k in 0..=N / 2 {
            re[k] *= g[k];
            im[k] *= g[k];
            if k > 0 && k < N / 2 {
                re[N - k] *= g[k];
                im[N - k] *= g[k];
            }
        }
        fft.inverse(&mut re, &mut im);
        for i in 0..N {
            out[off + i] += re[i] * w[i];
            ws[off + i] += w[i] * w[i];
        }
        off += H;
        f += 1;
    }
    for (o, s) in out.iter_mut().zip(ws.iter()) {
        if *s > 1e-4 {
            *o /= *s;
        }
    }
    out[pad..pad + x.len()].to_vec()
}

/// A noise profile: mean magnitude per bin of the chosen frames (the
/// quietest 10% when no region is given).
pub fn learn_profile(x: &[f32], region: Option<(f32, f32)>) -> Vec<f32> {
    let src: &[f32] = match region {
        Some((a, b)) => {
            let (s0, s1) = (
                ((a * SR) as usize).min(x.len()),
                ((b * SR) as usize).min(x.len()),
            );
            &x[s0..s1]
        }
        None => x,
    };
    let ms = mags(src);
    if ms.is_empty() {
        return vec![0.0; N / 2 + 1];
    }
    let mut idx: Vec<usize> = (0..ms.len()).collect();
    if region.is_none() {
        let e: Vec<f32> = ms
            .iter()
            .map(|m| m.iter().skip(4).map(|v| v * v).sum::<f32>())
            .collect();
        idx.sort_by(|a, b| e[*a].partial_cmp(&e[*b]).unwrap());
        idx.truncate((ms.len() / 10).max(1));
    }
    let mut p = vec![0.0f32; N / 2 + 1];
    for &i in &idx {
        for k in 0..=N / 2 {
            p[k] += ms[i][k] / idx.len() as f32;
        }
    }
    p
}

/// Spectral noise reduction (Wiener-style with over-subtraction, gains
/// smoothed over time and frequency so it does not warble).
pub fn reduce_noise(x: &[f32], profile: &[f32], reduction_db: f32, sensitivity: f32) -> Vec<f32> {
    reduce_noise_ex(x, profile, reduction_db, sensitivity, None)
}

/// The noise reducer behind `reduce_noise`. The clean-speech estimate is
/// decision-directed (Ephraim-Malah style: 96 % from the previous frame's
/// cleaned power, 4 % from this frame's excess), so a noise bin that pokes
/// over the print for one frame no longer opens on its own: that one-frame
/// flicker is the "musical noise" / warble of plain spectral subtraction.
/// Gains are Wiener (xi / (1 + xi)), then smoothed over frequency and time.
/// `keep_air_hz`: above it the deepest cut is halved (in dB), so weak
/// consonants and air are not stripped from a dark take.
pub fn reduce_noise_ex(
    x: &[f32],
    profile: &[f32],
    reduction_db: f32,
    sensitivity: f32,
    keep_air_hz: Option<f32>,
) -> Vec<f32> {
    let floor = 10f32.powf(-reduction_db.abs() / 20.0);
    let alpha = sensitivity.clamp(0.5, 4.0);
    let hz = SR / N as f32;
    let floors: Vec<f32> = (0..=N / 2)
        .map(|k| match keep_air_hz {
            Some(f) if k as f32 * hz >= f => floor.sqrt(),
            _ => floor,
        })
        .collect();
    let mut prev = vec![1.0f32; N / 2 + 1];
    let mut raw = vec![1.0f32; N / 2 + 1];
    let mut clean_prev = vec![0.0f32; N / 2 + 1];
    spectral(x, |_, m, g| {
        for k in 0..=N / 2 {
            // mean magnitude -> noise power (Rayleigh: E|N|^2 = 4/pi (E|N|)^2)
            let n2 = (1.27 * (alpha * profile[k]).powi(2)).max(1e-14);
            let gamma = m[k] * m[k] / n2;
            let xi = 0.96 * clean_prev[k] / n2 + 0.04 * (gamma - 1.0).max(0.0);
            let w = (xi / (1.0 + xi)).max(floors[k]);
            raw[k] = w;
            clean_prev[k] = (w * m[k]).powi(2);
        }
        for k in 0..=N / 2 {
            let a = raw[k.saturating_sub(2)];
            let b = raw[k.saturating_sub(1)];
            let c = raw[(k + 1).min(N / 2)];
            let d = raw[(k + 2).min(N / 2)];
            let s = 0.1 * a + 0.2 * b + 0.4 * raw[k] + 0.2 * c + 0.1 * d;
            // fast open, slower close
            let v = if s > prev[k] {
                0.3 * prev[k] + 0.7 * s
            } else {
                0.7 * prev[k] + 0.3 * s
            };
            prev[k] = v;
            g[k] = v.max(floors[k]);
        }
    })
}

/// 50 or 60 Hz: whichever mains series carries more energy in its first
/// four harmonics relative to the neighbouring bins.
pub fn detect_hum(x: &[f32]) -> (f32, f32) {
    // fine resolution (2.7 Hz bins): a 16k FFT averaged over up to 24 frames
    const M: usize = 16384;
    let fft = Fft::new(M);
    let w = hann(M);
    let mut avg = vec![0.0f32; M / 2 + 1];
    let frames = if x.len() >= M {
        ((x.len() - M) / (M / 2) + 1).min(24)
    } else {
        0
    };
    if frames == 0 {
        return (50.0, 0.0);
    }
    let step = if frames > 1 {
        (x.len() - M) / (frames - 1)
    } else {
        0
    };
    let (mut re, mut im) = (vec![0.0f32; M], vec![0.0f32; M]);
    for f in 0..frames {
        let off = f * step;
        for i in 0..M {
            re[i] = x[off + i] * w[i];
            im[i] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        for k in 0..=M / 2 {
            avg[k] += (re[k] * re[k] + im[k] * im[k]).sqrt() / frames as f32;
        }
    }
    let hz = SR / M as f32;
    let score = |f0: f32| -> f32 {
        let mut s = 0.0;
        for h in 1..=4 {
            let k = (f0 * h as f32 / hz).round() as usize;
            if k + 4 >= avg.len() || k < 4 {
                continue;
            }
            let peak = avg[k].max(avg[k - 1]).max(avg[k + 1]);
            let side = (0.5 * (avg[k - 4] + avg[k + 4])).max(1e-9);
            s += 20.0 * (peak / side).log10();
        }
        s / 4.0
    };
    let (a, b) = (score(50.0), score(60.0));
    if a >= b {
        (50.0, a)
    } else {
        (60.0, b)
    }
}

pub fn remove_hum(x: &[f32], f0: f32, harmonics: usize, q: f32) -> Vec<f32> {
    let mut filters: Vec<Biquad> = (1..=harmonics)
        .map(|h| f0 * h as f32)
        .filter(|f| *f < SR * 0.45)
        .map(|f| Biquad::new(BiquadKind::Notch, f, q, 0.0))
        .collect();
    // the fundamental's rumble below it goes too
    filters.push(Biquad::new(
        BiquadKind::LowCut,
        (f0 * 0.6).max(25.0),
        0.707,
        0.0,
    ));
    x.iter()
        .map(|v| filters.iter_mut().fold(*v, |s, f| f.process(s)))
        .collect()
}

/// Clicks: samples whose high-passed value jumps over `k` x the local
/// median level; each burst (up to max_ms) is redrawn by a cubic bridge.
pub fn remove_clicks(x: &[f32], k: f32, max_ms: f32) -> (Vec<f32>, usize) {
    let mut hp = Biquad::new(BiquadKind::LowCut, 3000.0, 0.707, 0.0);
    let d: Vec<f32> = x.iter().map(|v| hp.process(*v).abs()).collect();
    let win = (0.01 * SR) as usize;
    // running mean of |hp| as the local level (cheap stand-in for a median)
    let mut lvl = vec![0.0f32; d.len()];
    let mut acc = 0.0f32;
    for i in 0..d.len() {
        acc += d[i];
        if i >= win {
            acc -= d[i - win];
        }
        lvl[i] = acc / win.min(i + 1) as f32;
    }
    let mut y = x.to_vec();
    let maxlen = ((max_ms / 1000.0) * SR) as usize;
    let mut n = 0usize;
    let mut i = 8usize;
    while i + 8 < d.len() {
        let local = lvl[i.saturating_sub(win / 2)]
            .max(lvl[(i + win / 2).min(d.len() - 1)])
            .max(1e-5);
        if d[i] > k * local && d[i] > 0.02 {
            let mut j = i;
            while j + 1 < d.len() && j - i < maxlen && d[j + 1] > 0.5 * k * local {
                j += 1;
            }
            if j - i < maxlen {
                let (a, b) = (i.saturating_sub(3), (j + 3).min(y.len() - 1));
                let (ya, yb) = (y[a], y[b]);
                let (da, db) = (
                    y[a] - y[a.saturating_sub(1)],
                    y[(b + 1).min(y.len() - 1)] - y[b],
                );
                let len = (b - a) as f32;
                for t in a + 1..b {
                    let s = (t - a) as f32 / len;
                    let h00 = 2.0 * s * s * s - 3.0 * s * s + 1.0;
                    let h10 = s * s * s - 2.0 * s * s + s;
                    let h01 = -2.0 * s * s * s + 3.0 * s * s;
                    let h11 = s * s * s - s * s;
                    y[t] = h00 * ya + h10 * da * len + h01 * yb + h11 * db * len;
                }
                n += 1;
            }
            i = j + 8;
            continue;
        }
        i += 1;
    }
    (y, n)
}

/// Gate with hysteresis: opens when the voice band rises over `open_db`,
/// closes only when it falls `hyst_db` under that and stays there for
/// `hold_ms`; the closed level is `range_db` (not silence: breaths duck).
pub fn gate(
    x: &[f32],
    open_db: f32,
    hyst_db: f32,
    attack_ms: f32,
    hold_ms: f32,
    release_ms: f32,
    range_db: f32,
) -> (Vec<f32>, f32) {
    let db = crate::vocal_flex::band_db(x);
    let hop = (crate::vocal_flex::HOP * SR) as usize;
    let close = open_db - hyst_db.abs();
    let hold = (hold_ms / 1000.0 / crate::vocal_flex::HOP).round() as usize;
    let mut open = false;
    let mut since = 0usize;
    let mut target = Vec::with_capacity(db.len());
    for &d in &db {
        if d >= open_db {
            open = true;
            since = 0;
        } else if open && d < close {
            since += 1;
            if since > hold {
                open = false;
            }
        } else if open {
            since = 0;
        }
        target.push(open);
    }
    let lo = 10f32.powf(-range_db.abs() / 20.0);
    let a = (-1.0 / (attack_ms.max(0.1) / 1000.0 * SR)).exp();
    let r = (-1.0 / (release_ms.max(1.0) / 1000.0 * SR)).exp();
    // look ahead by the attack so the first consonant is not chopped
    let la = (attack_ms / 1000.0 * SR) as usize + hop;
    let mut g = lo;
    let mut y = vec![0.0f32; x.len()];
    let mut closed = 0usize;
    for i in 0..x.len() {
        let fi = ((i + la) / hop).min(target.len().saturating_sub(1));
        let t = if target.get(fi).copied().unwrap_or(false) {
            1.0
        } else {
            lo
        };
        g = if t > g {
            t + (g - t) * a
        } else {
            t + (g - t) * r
        };
        if t < 1.0 {
            closed += 1;
        }
        y[i] = x[i] * g;
    }
    (y, closed as f32 / x.len().max(1) as f32)
}

/// Late-reverb suppression: the tail is predicted from the signal
/// `delay_ms` earlier decaying at `rt60_s`, and that much is taken out.
pub fn dereverb(x: &[f32], amount: f32, rt60_s: f32, delay_ms: f32) -> Vec<f32> {
    let hop_s = H as f32 / SR;
    let d = ((delay_ms / 1000.0) / hop_s).round().max(1.0) as usize;
    let decay = (-6.9 * 2.0 * (d as f32 * hop_s) / rt60_s.max(0.05)).exp();
    let mut hist: Vec<Vec<f32>> = Vec::new();
    let floor = 0.18f32;
    let mut prev = vec![1.0f32; N / 2 + 1];
    spectral(x, |f, m, g| {
        let p: Vec<f32> = m.iter().map(|v| v * v).collect();
        if f >= d {
            let old = &hist[f - d];
            for k in 0..=N / 2 {
                let late = decay * old[k];
                let gk = (1.0 - amount * late / p[k].max(1e-12))
                    .max(floor * floor)
                    .sqrt();
                let v = 0.5 * prev[k] + 0.5 * gk;
                prev[k] = v;
                g[k] = v;
            }
        }
        hist.push(p);
    })
}

/// RMS (dB) of the quietest 10% and loudest 10% of 20 ms windows.
/// De-clip a digitally clipped take: samples pinned at the rail (within 0.1%
/// of the take's full scale, same sign, zero slope for 3+ samples - a clean
/// peak never stays exactly level that long) are redrawn as a cubic Hermite
/// curve through the clean samples and slopes on both sides. Returns (audio,
/// regions as (start_s, end_s)). Repaired peaks can exceed the old rail -
/// normalise afterwards if 0 dBFS was the ceiling. Analog soft-clipping has
/// no flat tops and is out of scope.
pub fn declip(x: &[f32]) -> (Vec<f32>, Vec<(f32, f32)>) {
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if peak < 0.05 {
        return (x.to_vec(), Vec::new());
    }
    let rail = peak * 0.001;
    let eps_d = 1e-6f32;
    let pinned = |v: f32| (v.abs() - peak).abs() <= rail;
    let mut y = x.to_vec();
    let mut regions = Vec::new();
    let mut i = 0usize;
    while i < x.len() {
        if pinned(x[i]) {
            let sign = x[i].signum();
            let mut j = i;
            while j + 1 < x.len()
                && pinned(x[j + 1])
                && x[j + 1].signum() == sign
                && (x[j + 1] - x[j]).abs() <= eps_d
            {
                j += 1;
            }
            // a real flat top is 3+ pinned samples; a clean peak is 1-2
            if j - i + 1 >= 3 && i >= 3 && j + 4 < x.len() {
                let p0 = x[i - 1] as f64;
                let p1 = x[j + 1] as f64;
                let m0 = (x[i - 1] - x[i - 3]) as f64 / 2.0;
                let m1 = (x[j + 3] - x[j + 1]) as f64 / 2.0;
                let n = (j + 2 - i) as f64;
                for (k, v) in y.iter_mut().enumerate().take(j + 1).skip(i) {
                    let t = (k - i + 1) as f64 / (n + 1.0);
                    let t2 = t * t;
                    let t3 = t2 * t;
                    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
                    let h10 = t3 - 2.0 * t2 + t;
                    let h01 = -2.0 * t3 + 3.0 * t2;
                    let h11 = t3 - t2;
                    *v = (h00 * p0 + h10 * m0 * n + h01 * p1 + h11 * m1 * n) as f32;
                }
                regions.push((i as f32 / SR, (j + 1) as f32 / SR));
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    (y, regions)
}

/// Zero-phase low-pass (forward + backward biquad, 4th order): the split
/// band lines up exactly with the input, so `x + band * (g - 1)` is
/// bit-identical to `x` wherever g stays 1.
fn lp_zp(x: &[f32], freq: f32) -> Vec<f32> {
    let mut b = Biquad::new(
        BiquadKind::HighCut,
        freq,
        std::f32::consts::FRAC_1_SQRT_2,
        0.0,
    );
    let mut y: Vec<f32> = x.iter().map(|&v| b.process(v)).collect();
    y.reverse();
    let mut b2 = Biquad::new(
        BiquadKind::HighCut,
        freq,
        std::f32::consts::FRAC_1_SQRT_2,
        0.0,
    );
    for v in y.iter_mut() {
        *v = b2.process(*v);
    }
    y.reverse();
    y
}

/// One-pole envelope of |x|: fast attack, slow release, per-sample.
fn env_follow(x: &[f32], attack_ms: f32, release_ms: f32) -> Vec<f32> {
    let aa = (-1.0 / (attack_ms * 0.001 * SR)).exp();
    let ar = (-1.0 / (release_ms * 0.001 * SR)).exp();
    let mut out = vec![0.0f32; x.len()];
    let mut e = 0.0f32;
    for (i, v) in x.iter().enumerate() {
        let a = v.abs();
        e = if a > e {
            aa * e + (1.0 - aa) * a
        } else {
            ar * e + (1.0 - ar) * a
        };
        out[i] = e;
    }
    out
}

/// Plosive pops (p/b bursts straight into the mic): a sudden surge of
/// sub-90 Hz energy that dominates the low band and dies within a few
/// hundred milliseconds. Detection compares a fast sub-band envelope
/// against a slow context follower (1 s rise, 150 ms fall) so steady hum,
/// bass lines and sung low notes are absorbed by the context and not
/// flagged, and the sub band must dominate the low band so a note onset
/// above 90 Hz is not a pop either. Each pop is repaired by pulling just
/// the sub-90 Hz band down toward its context, never by more than
/// `strength_db`; with the zero-phase split, audio outside a pop is
/// bit-identical. Pops inside the first 250 ms are missed - the context
/// needs a moment to learn the take. Returns the repaired signal and the
/// pops as (start_s, end_s).
pub fn deplosive(x: &[f32], surge_db: f32, strength_db: f32) -> (Vec<f32>, Vec<(f32, f32)>) {
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if peak < 0.05 {
        return (x.to_vec(), Vec::new());
    }
    let sub = lp_zp(x, 90.0);
    let low = lp_zp(x, 250.0);
    let sub_env = env_follow(&sub, 3.0, 50.0);
    let low_env = env_follow(&low, 3.0, 50.0);
    let ac = (-1.0 / SR).exp();
    let rc = (-1.0 / (0.150 * SR)).exp();
    let mut ctx = vec![0.0f32; x.len()];
    let mut c = 0.0f32;
    for (i, &v) in sub_env.iter().enumerate() {
        c = if v > c {
            ac * c + (1.0 - ac) * v
        } else {
            rc * c + (1.0 - rc) * v
        };
        ctx[i] = c;
    }
    let surge = db_to_gain(surge_db);
    let floor = 0.01f32;
    let warmup = (0.25 * SR) as usize;
    let mut flag = vec![false; x.len()];
    for i in warmup..x.len() {
        flag[i] =
            sub_env[i] > ctx[i] * surge && sub_env[i] > floor && sub_env[i] > 0.45 * low_env[i];
    }
    let merge_gap = (0.040 * SR) as usize;
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < x.len() {
        if flag[i] {
            let mut j = i;
            while j + 1 < x.len() && flag[j + 1] {
                j += 1;
            }
            runs.push((i, j));
            i = j + 1;
        } else {
            i += 1;
        }
    }
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for r in runs {
        if let Some(last) = merged.last_mut() {
            if r.0.saturating_sub(last.1) <= merge_gap {
                last.1 = r.1;
                continue;
            }
        }
        merged.push(r);
    }
    // a pop is a burst: 15-300 ms. Anything sustained is hum or bass.
    let min_d = (0.015 * SR) as usize;
    let max_d = (0.300 * SR) as usize;
    let kept: Vec<(usize, usize)> = merged
        .into_iter()
        .filter(|r| {
            let d = r.1 - r.0 + 1;
            (min_d..=max_d).contains(&d)
        })
        .collect();
    if kept.is_empty() {
        return (x.to_vec(), Vec::new());
    }
    let min_g = db_to_gain(-strength_db);
    let mut g = vec![1.0f32; x.len()];
    for &(s, en) in &kept {
        let s2 = s.saturating_sub((0.010 * SR) as usize);
        let e2 = (en + (0.060 * SR) as usize).min(x.len() - 1);
        for (i, gv) in g.iter_mut().enumerate().take(e2 + 1).skip(s2) {
            let t = (ctx[i] * surge / sub_env[i].max(1e-9)).clamp(min_g, 1.0);
            if t < *gv {
                *gv = t;
            }
        }
    }
    // smooth the gain: 2 ms pull-down, 30 ms recover, so no zipper noise
    let ga = (-1.0 / (0.002 * SR)).exp();
    let gr = (-1.0 / (0.030 * SR)).exp();
    let mut gs = 1.0f32;
    for gv in g.iter_mut() {
        let t = *gv;
        gs = if t < gs {
            ga * gs + (1.0 - ga) * t
        } else {
            gr * gs + (1.0 - gr) * t
        };
        *gv = gs;
    }
    let mut y = x.to_vec();
    for i in 0..x.len() {
        y[i] = x[i] + sub[i] * (g[i] - 1.0);
    }
    let regions = kept
        .iter()
        .map(|&(s, en)| (s as f32 / SR, en as f32 / SR))
        .collect();
    (y, regions)
}

/// Breath events between phrases: frames that are noisy (high spectral
/// flatness between 1 and 8 kHz) and louder than the take's floor, grouped
/// into regions. The take comes back with each region pulled down by
/// `amount_db` (raised-cosine shoulders, so no clicks) and the regions as
/// (start_s, end_s). Breaths are found by their noisy, unpitched character,
/// not their level, so a loud breath is caught and a soft sung note (flat
/// spectrum peak, low flatness) is left alone.
pub fn reduce_breaths(x: &[f32], amount_db: f32, sensitivity: f32) -> (Vec<f32>, Vec<(f32, f32)>) {
    let m = mags(x);
    if m.len() < 8 {
        return (x.to_vec(), Vec::new());
    }
    let hz = SR / N as f32;
    let lo = (1000.0 / hz) as usize;
    let hi = ((8000.0 / hz) as usize).min(N / 2);
    let mut db = Vec::with_capacity(m.len());
    let mut flat = Vec::with_capacity(m.len());
    for f in &m {
        let power: f32 = f.iter().map(|v| v * v).sum();
        db.push(10.0 * (power + 1e-12).log10());
        let band = &f[lo..=hi];
        let gm = (band.iter().map(|v| (v + 1e-9).ln()).sum::<f32>() / band.len() as f32).exp();
        let am = band.iter().sum::<f32>() / band.len() as f32;
        flat.push(gm / (am + 1e-12));
    }
    let mut sorted = db.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = sorted[sorted.len() / 10];
    let flat_min = 0.55 - sensitivity.clamp(0.0, 1.0) * 0.3;
    let cand: Vec<bool> = (0..m.len())
        .map(|i| flat[i] >= flat_min && db[i] >= floor + 6.0)
        .collect();
    // group, bridging wobbles of a frame or two
    let mut regions: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < m.len() {
        if cand[i] {
            let mut j = i;
            while j + 1 < m.len() && (cand[j + 1] || (j + 2 < m.len() && cand[j + 2])) {
                j += if cand[j + 1] { 1 } else { 2 };
            }
            if j - i + 1 >= 5 {
                regions.push((i, j));
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    if regions.is_empty() {
        return (x.to_vec(), Vec::new());
    }
    let g = db_to_gain(amount_db.min(0.0));
    let fade = (0.015 * SR) as usize;
    let mut gain = vec![1.0f32; x.len()];
    for &(a, b) in &regions {
        let s = a * H;
        let e = ((b + 1) * H).min(x.len());
        for k in s..e {
            gain[k] = g;
        }
        for k in 0..fade {
            let w = 0.5 * (1.0 - (std::f32::consts::PI * k as f32 / fade as f32).cos());
            if s + k < e {
                gain[s + k] = (1.0 - w) + g * w;
            }
            if e > fade + 1 && e - 1 - k >= s {
                let p = e - 1 - k;
                gain[p] = (1.0 - w) + g * w;
            }
        }
    }
    let out: Vec<f32> = x.iter().zip(gain.iter()).map(|(v, g)| v * g).collect();
    let secs = regions
        .iter()
        .map(|&(a, b)| {
            (
                a as f32 * H as f32 / SR,
                ((b + 1) * H).min(x.len()) as f32 / SR,
            )
        })
        .collect();
    (out, secs)
}

pub fn floor_and_peak_db(x: &[f32]) -> (f32, f32) {
    let w = (0.02 * SR) as usize;
    let mut v: Vec<f32> = x
        .chunks(w)
        .filter(|c| c.len() == w)
        .map(|c| {
            10.0 * (c.iter().map(|s| s * s).sum::<f32>() / w as f32)
                .max(1e-12)
                .log10()
        })
        .collect();
    if v.is_empty() {
        return (-120.0, -120.0);
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = (v.len() / 10).max(1);
    let lo = v[..n].iter().sum::<f32>() / n as f32;
    let hi = v[v.len() - n..].iter().sum::<f32>() / n as f32;
    (lo, hi)
}

fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

fn save(
    e: &mut Engine,
    base: &crate::samples::SampleInfo,
    nn: &str,
    y: &[f32],
    what: &str,
) -> Result<String> {
    let name = crate::samples::sample_name(nn);
    std::fs::create_dir_all(e.samples_dir())?;
    let path = e.samples_dir().join(format!("{name}.wav"));
    crate::render::write_wav(&path, y, y)?;
    let info = crate::samples::SampleInfo {
        name,
        path: path.to_string_lossy().into(),
        source: format!("{} ({what})", base.source),
        license: base.license.clone(),
        author: base.author.clone(),
        duration: 0.0,
    };
    let r = crate::tools::register_sample(e, info)?;
    e.revision += 1;
    Ok(r["sample"].as_str().unwrap_or(nn).to_string())
}

fn profiles_dir(e: &Engine) -> std::path::PathBuf {
    e.workdir.join("noise_profiles")
}

fn report(before: &[f32], after: &[f32]) -> Value {
    let (f0, p0) = floor_and_peak_db(before);
    let (f1, p1) = floor_and_peak_db(after);
    json!({"noise_floor_db": {"before": r1(f0), "after": r1(f1)}, "loud_parts_db": {"before": r1(p0), "after": r1(p1)}, "signal_to_noise_db": {"before": r1(p0 - f0), "after": r1(p1 - f1)}})
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "learn_noise_profile",
            description: "Learn a noise print (RX-style) from a sample: from_s/to_s mark a stretch of pure noise (room tone, hiss before the vocal); without them the quietest 10% of the take is used. Saved under name (default <sample>) for reduce_noise.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "from_s": {"type": "number"}, "to_s": {"type": "number"}, "name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let region = match (f_opt(a, "from_s"), f_opt(a, "to_s")) {
                    (Some(f), Some(t)) if t > f + 0.1 => Some((f, t)),
                    (Some(_), Some(_)) => bail!("the noise stretch must be at least 0.1 s"),
                    _ => None,
                };
                let p = learn_profile(&x, region);
                let name = crate::samples::sample_name(&s_opt(a, "name").unwrap_or_else(|| info.name.clone()));
                std::fs::create_dir_all(profiles_dir(e))?;
                std::fs::write(profiles_dir(e).join(format!("{name}.json")), serde_json::to_string(&p)?)?;
                let hz = SR / N as f32;
                let band = |lo: f32, hi: f32| -> f32 {
                    let (a, b) = ((lo / hz) as usize, ((hi / hz) as usize).min(p.len() - 1));
                    let m = p[a..=b].iter().map(|v| v * v).sum::<f32>() / (b - a + 1) as f32;
                    r1(10.0 * m.max(1e-14).log10())
                };
                Ok(json!({"profile": name, "from": if region.is_some() { "the stretch you marked" } else { "the quietest 10% of the take" }, "bands_db": {"low_20_250": band(20.0, 250.0), "mid_250_2k": band(250.0, 2000.0), "high_2k_8k": band(2000.0, 8000.0), "air_8k_16k": band(8000.0, 16000.0)}, "next": "reduce_noise {sample, profile}"}))
            },
        },
        Tool {
            name: "reduce_noise",
            description: "Spectral noise reduction (RX-style): subtracts a learned noise print per frequency with smoothed gains (no warbling). profile = a learn_noise_profile name (default: learned from the take's quietest parts now). reduction_db = deepest cut (default 12 = gentle; 18 medium, 30 strong), sensitivity 0.5-4 (default 1.0; higher removes more, risks the voice), keep_air (default true: above 4 kHz the cut is halved so consonants and air stay). Decision-directed gains, so the leftover noise stays smooth instead of warbling. Writes <sample>_nr and reports noise floor and SNR before/after.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "profile": {"type": "string"}, "reduction_db": {"type": "number"}, "sensitivity": {"type": "number"}, "keep_air": {"type": "boolean"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                if x.len() < N * 2 {
                    bail!("the sample is too short to denoise");
                }
                let p: Vec<f32> = match s_opt(a, "profile") {
                    Some(n) => {
                        let f = profiles_dir(e).join(format!("{}.json", crate::samples::sample_name(&n)));
                        let s = std::fs::read_to_string(&f).map_err(|_| anyhow::anyhow!("no noise profile '{n}' (learn_noise_profile first)"))?;
                        serde_json::from_str(&s)?
                    }
                    None => learn_profile(&x, None),
                };
                let rd = f_or(a, "reduction_db", 12.0).clamp(3.0, 40.0);
                let sens = f_or(a, "sensitivity", 1.0);
                let keep_air = a.get("keep_air").and_then(|v| v.as_bool()).unwrap_or(true);
                let y = reduce_noise_ex(&x, &p, rd, sens, if keep_air { Some(4000.0) } else { None });
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_nr", info.name));
                let s = save(e, &info, &nn, &y, &format!("noise reduction {rd} dB, sensitivity {sens}"))?;
                Ok(json!({"sample": s, "measured": report(&x, &y)}))
            },
        },
        Tool {
            name: "remove_hum",
            description: "De-hum: notch out mains hum and its harmonics (freq 50 or 60 Hz, default auto-detected; harmonics default 8, q default 30 = narrow) plus the rumble under it. Writes <sample>_dehum and reports the hum found.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "freq": {"type": "number"}, "harmonics": {"type": "integer"}, "q": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let (det, strength) = detect_hum(&x);
                let f0 = f_opt(a, "freq").unwrap_or(det);
                let h = u_or(a, "harmonics", 8).clamp(1, 24) as usize;
                let y = remove_hum(&x, f0, h, f_or(a, "q", 30.0).clamp(2.0, 120.0));
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_dehum", info.name));
                let s = save(e, &info, &nn, &y, &format!("de-hum {f0} Hz x{h}"))?;
                let (_, after) = detect_hum(&y);
                Ok(json!({"sample": s, "hum_hz": f0, "detected_hz": det, "hum_peak_db": {"before": r1(strength), "after": r1(after)}, "note": if strength < 3.0 { "little hum was found; the notches are harmless but may not be needed" } else { "hum removed" }}))
            },
        },
        Tool {
            name: "remove_clicks",
            description: "De-click: finds clicks, pops and edit ticks (sharp high-frequency jumps over the local level) and redraws each one (up to max_ms, default 2) with a smooth bridge. sensitivity 4-20 (default 8; lower finds more). Writes <sample>_declick and reports how many were repaired.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "sensitivity": {"type": "number"}, "max_ms": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let (y, n) = remove_clicks(&x, f_or(a, "sensitivity", 8.0).clamp(2.0, 40.0), f_or(a, "max_ms", 2.0).clamp(0.2, 10.0));
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_declick", info.name));
                let s = save(e, &info, &nn, &y, &format!("de-click, {n} repaired"))?;
                Ok(json!({"sample": s, "clicks_repaired": n}))
            },
        },
        Tool {
            name: "gate_vocal",
            description: "Breath and noise gate with hysteresis for a vocal: opens at threshold_db (voice-band level; default from the take's noise floor), closes hysteresis_db lower (default 6) after hold_ms (default 90), attack_ms 2 with look-ahead so first consonants stay, release_ms 120. range_db (default 24) is how far gaps drop: breaths duck, they don't vanish. Writes <sample>_gated.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "threshold_db": {"type": "number"}, "hysteresis_db": {"type": "number"}, "attack_ms": {"type": "number"}, "hold_ms": {"type": "number"}, "release_ms": {"type": "number"}, "range_db": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let db = crate::vocal_flex::band_db(&x);
                let thr = f_opt(a, "threshold_db").unwrap_or_else(|| crate::vocal_flex::voice_threshold(&db, None));
                let (y, closed) = gate(&x, thr, f_or(a, "hysteresis_db", 6.0), f_or(a, "attack_ms", 2.0), f_or(a, "hold_ms", 90.0), f_or(a, "release_ms", 120.0), f_or(a, "range_db", 24.0));
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_gated", info.name));
                let s = save(e, &info, &nn, &y, &format!("gate {thr:.1} dB"))?;
                Ok(json!({"sample": s, "threshold_db": r1(thr), "closed_pct": r1(closed * 100.0), "measured": report(&x, &y)}))
            },
        },
        Tool {
            name: "declip_sample",
            description: "De-clip a take recorded too hot (RX De-clip style): finds the squared-off tops - samples pinned at full scale with zero slope - and redraws them as smooth curves through the clean audio on both sides. Works on digital clipping (an interface or DAW rail); analog soft-clipping has no flat tops and can't be detected this way. Repaired peaks can exceed the old rail, so normalise afterwards if you need 0 dBFS headroom. Writes <sample>_declip and reports the repaired regions. No regions found = the take wasn't clipped, returned unchanged.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                if x.len() < N {
                    bail!("the sample is too short to de-clip");
                }
                let (y, regions) = declip(&x);
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_declip", info.name));
                let s = save(e, &info, &nn, &y, "de-clip")?;
                Ok(json!({"sample": s, "regions_repaired": regions.len(), "measured": report(&x, &y)}))
            },
        },
        Tool {
            name: "deplosive_sample",
            description: "De-plosive a take (RX De-plosive style): finds mic pops from p/b consonants - sudden sub-90 Hz surges that dominate the low band and die fast - and pulls just that sub band down toward the take's own low-end level. surge_db sets how far above its surroundings a surge must be to count (default 7), strength_db the deepest pull-down allowed (default 12 dB). Steady hum, bass lines and sung low notes are left alone, and audio outside a pop is bit-identical. Pops in the first 250 ms of the take are missed. Writes <sample>_depop and reports the repaired regions. No regions found = no pops, returned unchanged.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "surge_db": {"type": "number"}, "strength_db": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                if x.len() < N {
                    bail!("the sample is too short to de-plosive");
                }
                let surge = f_or(a, "surge_db", 7.0).clamp(3.0, 20.0);
                let strength = f_or(a, "strength_db", 12.0).clamp(3.0, 30.0);
                let (y, regions) = deplosive(&x, surge, strength);
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_depop", info.name));
                let s = save(e, &info, &nn, &y, "de-plosive")?;
                Ok(json!({"sample": s, "regions_repaired": regions.len(), "measured": report(&x, &y)}))
            },
        },
        Tool {
            name: "reduce_breaths",
            description: "De-breath a vocal take (RX-style): finds breaths by their noisy, unpitched character between phrases and turns just those moments down, amount_db at a time (default -12; -6 gentle, -20 nearly silent). Unlike gate_vocal, detection is by breath character, not level, so a loud breath is caught and quiet singing is left alone. sensitivity 0..1 (default 0.5; higher catches softer, less obvious breaths and can nip at whispered or breathy singing). Writes <sample>_db and reports the breath regions found. Empty regions = no breaths found, take returned unchanged.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "amount_db": {"type": "number"}, "sensitivity": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                if x.len() < N * 4 {
                    bail!("the sample is too short to de-breath");
                }
                let amount = f_or(a, "amount_db", -12.0).clamp(-40.0, 0.0);
                let sens = f_or(a, "sensitivity", 0.5).clamp(0.0, 1.0);
                let (y, regions) = reduce_breaths(&x, amount, sens);
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_db", info.name));
                let s = save(e, &info, &nn, &y, &format!("de-breath {amount:.0} dB"))?;
                Ok(json!({"sample": s, "breaths": regions.len(), "regions": regions.iter().map(|&(a, b)| json!({"start_s": r1(a), "end_s": r1(b)})).collect::<Vec<_>>(), "measured": report(&x, &y)}))
            },
        },
        Tool {
            name: "reduce_reverb",
            description: "De-reverb: takes the room's late tail out of a vocal recorded in a reflective room (spectral late-reverb suppression). amount 0..1 (default 0.6), rt60_s = the room's decay (default 0.5), delay_ms = where the late tail starts (default 50). Strong settings thin the voice. Writes <sample>_dry.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "amount": {"type": "number"}, "rt60_s": {"type": "number"}, "delay_ms": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let amt = f_or(a, "amount", 0.6).clamp(0.0, 1.0);
                let y = dereverb(&x, amt, f_or(a, "rt60_s", 0.5).clamp(0.1, 3.0), f_or(a, "delay_ms", 50.0).clamp(20.0, 200.0));
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_dry", info.name));
                let s = save(e, &info, &nn, &y, &format!("de-reverb {amt}"))?;
                Ok(json!({"sample": s, "measured": report(&x, &y)}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice_with_noise(seed: u64) -> (Vec<f32>, Vec<f32>) {
        let mut rng = crate::dsp::Rng::new(seed);
        let n = (2.0 * SR) as usize;
        let clean: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / SR;
                let on = (t > 0.6 && t < 1.4) as i32 as f32;
                on * 0.3
                    * ((t * 220.0 * std::f32::consts::TAU).sin()
                        + 0.4 * (t * 660.0 * std::f32::consts::TAU).sin())
            })
            .collect();
        let noisy = clean.iter().map(|v| v + 0.02 * rng.bipolar()).collect();
        (clean, noisy)
    }

    #[test]
    fn noise_reduction_lowers_the_floor_and_keeps_the_voice() {
        let (clean, noisy) = voice_with_noise(5);
        let p = learn_profile(&noisy, Some((0.0, 0.5)));
        let y = reduce_noise(&noisy, &p, 24.0, 1.5);
        let (f0, _) = floor_and_peak_db(&noisy);
        let (f1, _) = floor_and_peak_db(&y);
        assert!(f1 < f0 - 10.0, "floor {f0} -> {f1}");
        let rms = |v: &[f32]| (v.iter().map(|s| s * s).sum::<f32>() / v.len() as f32).sqrt();
        let a = (0.8 * SR) as usize;
        let b = (1.2 * SR) as usize;
        let ratio = rms(&y[a..b]) / rms(&clean[a..b]);
        assert!(ratio > 0.85 && ratio < 1.15, "voice kept: {ratio}");
    }

    #[test]
    fn leftover_noise_does_not_flicker() {
        // plain spectral subtraction leaves isolated one-frame tones in noise
        // gaps ("musical noise"); the decision-directed gains must not
        let (_, noisy) = voice_with_noise(11);
        let p = learn_profile(&noisy, Some((0.0, 0.5)));
        let y = reduce_noise(&noisy, &p, 18.0, 1.0);
        let gap = &y[(1.6 * SR) as usize..(1.95 * SR) as usize];
        let ms = mags(gap);
        // per-bin frame-to-frame jumps over 12 dB, as a share of all bins
        let mut jumps = 0usize;
        let mut total = 0usize;
        for w in ms.windows(2) {
            for k in 20..N / 2 {
                let (a, b) = (w[0][k].max(1e-9), w[1][k].max(1e-9));
                if (20.0 * (b / a).log10()).abs() > 12.0 {
                    jumps += 1;
                }
                total += 1;
            }
        }
        let share = jumps as f32 / total.max(1) as f32;
        assert!(share < 0.2, "flicker share {share}");
        let y2 = reduce_noise_ex(&noisy, &p, 18.0, 1.0, Some(4000.0));
        let (f_full, _) = floor_and_peak_db(&y);
        let (f_air, _) = floor_and_peak_db(&y2);
        assert!(
            f_air >= f_full - 0.5,
            "keep_air never cuts more: {f_full} vs {f_air}"
        );
    }

    #[test]
    fn hum_is_found_and_notched() {
        let x: Vec<f32> = (0..(2.0 * SR) as usize)
            .map(|i| {
                let t = i as f32 / SR;
                0.1 * (t * 60.0 * std::f32::consts::TAU).sin()
                    + 0.05 * (t * 180.0 * std::f32::consts::TAU).sin()
            })
            .collect();
        let (f, s) = detect_hum(&x);
        assert_eq!(f, 60.0);
        assert!(s > 6.0);
        let y = remove_hum(&x, 60.0, 8, 30.0);
        let e = |v: &[f32]| v[(0.5 * SR) as usize..].iter().map(|s| s * s).sum::<f32>();
        assert!(e(&y) < e(&x) * 0.05);
    }

    #[test]
    fn clicks_are_repaired_and_gate_ducks_gaps() {
        let mut x: Vec<f32> = (0..SR as usize)
            .map(|i| 0.2 * (i as f32 / SR * 200.0 * std::f32::consts::TAU).sin())
            .collect();
        for &p in &[10000usize, 25000, 33000] {
            x[p] += 0.8;
            x[p + 1] -= 0.6;
        }
        let (y, n) = remove_clicks(&x, 8.0, 2.0);
        assert_eq!(n, 3);
        assert!(y[10000].abs() < 0.3);
        let (_, noisy) = voice_with_noise(9);
        let db = crate::vocal_flex::band_db(&noisy);
        let thr = crate::vocal_flex::voice_threshold(&db, None);
        let (g, closed) = gate(&noisy, thr, 6.0, 2.0, 90.0, 120.0, 24.0);
        assert!(closed > 0.3 && closed < 0.8, "closed {closed}");
        assert!(g[(0.2 * SR) as usize].abs() < 0.01);
        assert!((g[(1.0 * SR) as usize] - noisy[(1.0 * SR) as usize]).abs() < 0.02);
    }

    #[test]
    fn dereverb_shortens_a_tail() {
        let mut x = vec![0.0f32; (1.5 * SR) as usize];
        let mut rng = crate::dsp::Rng::new(2);
        for i in 0..(0.2 * SR) as usize {
            x[i] = 0.5 * rng.bipolar();
        }
        // an exponential tail (RT60 0.6 s)
        for i in (0.2 * SR) as usize..x.len() {
            let t = (i as f32 / SR) - 0.2;
            x[i] = 0.5 * rng.bipolar() * (-6.9 * t / 0.6).exp();
        }
        let y = dereverb(&x, 0.9, 0.6, 50.0);
        let e = |v: &[f32]| {
            v[(0.45 * SR) as usize..(0.9 * SR) as usize]
                .iter()
                .map(|s| s * s)
                .sum::<f32>()
        };
        assert!(e(&y) < e(&x) * 0.6, "{} vs {}", e(&y), e(&x));
    }
    fn sung_phrase(len_s: f32, peak: f32) -> Vec<f32> {
        // voice-like: 220 Hz with 1/k harmonics up to ~8 kHz
        let n = (len_s * SR) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / SR;
                let env = (std::f32::consts::PI * i as f32 / n as f32).sin().powf(0.5);
                let mut v = 0.0f32;
                let mut k = 1usize;
                while 220.0 * k as f32 <= 8000.0 {
                    v += (std::f32::consts::TAU * 220.0 * k as f32 * t).sin() / k as f32;
                    k += 1;
                }
                peak * env * v * 0.5
            })
            .collect()
    }

    fn breath_noise(len_s: f32, peak: f32, seed: u64) -> Vec<f32> {
        let n = (len_s * SR) as usize;
        let mut rng = crate::dsp::Rng::new(seed);
        let mut hp_prev = 0.0f32;
        let mut in_prev = 0.0f32;
        (0..n)
            .map(|i| {
                let w = rng.bipolar();
                let a = 0.82f32; // one-pole highpass pushes energy up-band
                let hp = a * (hp_prev + w - in_prev);
                hp_prev = hp;
                in_prev = w;
                let swell = (std::f32::consts::PI * i as f32 / n as f32).sin();
                peak * swell * hp * 2.2
            })
            .collect()
    }

    #[test]
    fn breaths_are_found_and_ducked_phrases_untouched() {
        // phrase, gap, breath, gap, phrase, breath, phrase
        let mut x = Vec::new();
        x.extend(sung_phrase(0.6, 0.4));
        x.extend(vec![0.0f32; (0.1 * SR) as usize]);
        let b1_at = x.len();
        x.extend(breath_noise(0.35, 0.06, 11));
        let b1_end = x.len();
        x.extend(vec![0.0f32; (0.05 * SR) as usize]);
        x.extend(sung_phrase(0.6, 0.4));
        let b2_at = x.len();
        x.extend(breath_noise(0.3, 0.05, 12));
        let b2_end = x.len();
        x.extend(sung_phrase(0.6, 0.4));
        let (y, regions) = reduce_breaths(&x, -12.0, 0.5);
        assert_eq!(regions.len(), 2, "regions {regions:?}");
        let tol = 0.1;
        assert!(
            (regions[0].0 - b1_at as f32 / SR).abs() < tol
                && (regions[0].1 - b1_end as f32 / SR).abs() < tol,
            "region 0 {:?} vs {}-{}",
            regions[0],
            b1_at as f32 / SR,
            b1_end as f32 / SR
        );
        assert!(
            (regions[1].0 - b2_at as f32 / SR).abs() < tol
                && (regions[1].1 - b2_end as f32 / SR).abs() < tol,
            "region 1 {:?} vs {}-{}",
            regions[1],
            b2_at as f32 / SR,
            b2_end as f32 / SR
        );
        let rms = |v: &[f32], a: f32, b: f32| {
            let s = (a * SR) as usize;
            let e = (b * SR) as usize;
            (v[s..e].iter().map(|v| v * v).sum::<f32>() / (e - s) as f32).sqrt()
        };
        let drop_db = 20.0 * (rms(&y, 0.75, 1.00) / rms(&x, 0.75, 1.00)).log10();
        assert!(drop_db < -7.0, "breath ducked {drop_db:.1} dB");
        let phrase_db = 20.0 * (rms(&y, 0.15, 0.55) / rms(&x, 0.15, 0.55)).log10();
        assert!(phrase_db.abs() < 0.5, "phrase moved {phrase_db:.2} dB");
        // a take that is all singing has no breaths, and silence stays silent
        let (_, r2) = reduce_breaths(&sung_phrase(2.0, 0.4), -12.0, 0.5);
        assert!(r2.is_empty(), "tone-only regions {r2:?}");
        let (_, r3) = reduce_breaths(&vec![0.0f32; (2.0 * SR) as usize], -12.0, 0.5);
        assert!(r3.is_empty(), "silence regions {r3:?}");
    }
    #[test]
    fn declip_redraws_clipped_tops_and_spares_clean_audio() {
        let n = (2.0 * SR) as usize;
        let orig: Vec<f32> = (0..n)
            .map(|i| 1.6 * (i as f32 * 440.0 / SR * std::f32::consts::TAU).sin())
            .collect();
        let clipped: Vec<f32> = orig.iter().map(|v| v.clamp(-0.7, 0.7)).collect();
        let (y, regions) = declip(&clipped);
        assert!(regions.len() >= 800, "regions {}", regions.len());
        let max_err = y
            .iter()
            .zip(orig.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        let clipped_err = clipped
            .iter()
            .zip(orig.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max_err < clipped_err * 0.2,
            "err {max_err} vs clipped {clipped_err}"
        );
        // no pinned runs left (3+ samples on the old rail, same sign)
        let mut runs = 0;
        let mut i = 0;
        while i < y.len() {
            if (y[i].abs() - 0.7).abs() <= 0.7 * 0.001 {
                let s = y[i].signum();
                let mut j = i;
                while j + 1 < y.len()
                    && (y[j + 1].abs() - 0.7).abs() <= 0.7 * 0.001
                    && y[j + 1].signum() == s
                {
                    j += 1;
                }
                if j - i + 1 >= 3 {
                    runs += 1;
                }
                i = j + 1;
            } else {
                i += 1;
            }
        }
        assert_eq!(runs, 0, "pinned runs left");
        // clean peaks are untouched, even deep bass
        for hz in [50.0f32, 220.0, 1000.0] {
            let clean: Vec<f32> = (0..n)
                .map(|i| 0.5 * (i as f32 * hz / SR * std::f32::consts::TAU).sin())
                .collect();
            let (y2, r2) = declip(&clean);
            assert!(r2.is_empty(), "{hz} Hz regions {r2:?}");
            assert_eq!(y2, clean, "{hz} Hz changed");
        }
        // silence and quiet takes are no-ops
        let (y3, r3) = declip(&vec![0.01f32; 1000]);
        assert!(r3.is_empty());
        assert_eq!(y3, vec![0.01f32; 1000]);
    }
    #[test]
    fn deplosive_pulls_pops_down_and_spares_everything_else() {
        let n = (2.0 * SR) as usize;
        let vocal: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / SR;
                0.30 * (t * 220.0 * std::f32::consts::TAU).sin()
                    + 0.12 * (t * 440.0 * std::f32::consts::TAU).sin()
                    + 0.06 * (t * 660.0 * std::f32::consts::TAU).sin()
            })
            .collect();
        // a pop at 0.5 s: 70 Hz burst decaying over ~150 ms
        let mut popped = vocal.clone();
        let b0 = (0.5 * SR) as usize;
        for i in 0..(0.15 * SR) as usize {
            let t = i as f32 / SR;
            popped[b0 + i] += 0.9 * (t * 70.0 * std::f32::consts::TAU).sin() * (-t / 0.05).exp();
        }
        let (y, regions) = deplosive(&popped, 7.0, 12.0);
        assert!(!regions.is_empty(), "pop not found");
        assert!(
            regions[0].0 > 0.4 && regions[0].0 < 0.6,
            "pop start {:?}",
            regions
        );
        let band_rms = |x: &[f32], from: usize, to: usize| {
            let b = lp_zp(&x[from..to], 90.0);
            (b.iter().map(|v| v * v).sum::<f32>() / b.len() as f32).sqrt()
        };
        let w0 = (0.50 * SR) as usize;
        let w1 = (0.62 * SR) as usize;
        let before = band_rms(&popped, w0, w1);
        let after = band_rms(&y, w0, w1);
        assert!(
            20.0 * (before / after).log10() >= 5.0,
            "pop only pulled {before:.4} -> {after:.4}"
        );
        // outside the pop the take is untouched
        let pre = (0..(0.3 * SR) as usize)
            .map(|i| (y[i] - popped[i]).abs())
            .fold(0.0f32, f32::max);
        let post = ((1.5 * SR) as usize..n)
            .map(|i| (y[i] - popped[i]).abs())
            .fold(0.0f32, f32::max);
        assert!(pre < 1e-4, "pre changed {pre}");
        assert!(post < 1e-4, "post changed {post}");
        // the clean take passes through untouched
        let (y2, r2) = deplosive(&vocal, 7.0, 12.0);
        assert!(r2.is_empty(), "clean regions {r2:?}");
        assert_eq!(y2, vocal, "clean take changed");
        // steady hum, a bass line and a sudden low note are not pops
        for hz in [60.0f32, 110.0] {
            let tone: Vec<f32> = (0..n)
                .map(|i| 0.25 * (i as f32 * hz / SR * std::f32::consts::TAU).sin())
                .collect();
            let (_, r3) = deplosive(&tone, 7.0, 12.0);
            assert!(r3.is_empty(), "{hz} Hz flagged {r3:?}");
        }
        let mut note = vec![0.0f32; n];
        for i in (0.3 * SR) as usize..n {
            let t = i as f32 / SR;
            note[i] = 0.4 * (t * 150.0 * std::f32::consts::TAU).sin();
        }
        let (_, r4) = deplosive(&note, 7.0, 12.0);
        assert!(r4.is_empty(), "note onset flagged {r4:?}");
        // quiet takes are a no-op
        let (y5, r5) = deplosive(&vec![0.01f32; 1000], 7.0, 12.0);
        assert!(r5.is_empty());
        assert_eq!(y5, vec![0.01f32; 1000]);
    }
}
