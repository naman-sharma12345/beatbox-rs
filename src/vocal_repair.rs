//! Vocal repair, RX-style: spectral noise reduction from a learned noise
//! profile, hum removal, click repair, a breath/noise gate with hysteresis
//! and late-reverb reduction. Every tool writes a new sample and reports
//! what it measured before and after.

use crate::dsp::{Biquad, BiquadKind, Fft, SR};
use crate::engine::Engine;
use crate::tools::{f_opt, f_or, obj, s_opt, s_req, u_or, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};

pub const N: usize = 2048;
pub const H: usize = 512;

fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect()
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
        out.push((0..=N / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect());
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
    xp.extend(std::iter::repeat(0.0).take(pad + N));
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
            let (s0, s1) = (((a * SR) as usize).min(x.len()), ((b * SR) as usize).min(x.len()));
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
        let e: Vec<f32> = ms.iter().map(|m| m.iter().skip(4).map(|v| v * v).sum::<f32>()).collect();
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
    let floor = 10f32.powf(-reduction_db.abs() / 20.0);
    let alpha = sensitivity.clamp(0.5, 4.0);
    let mut prev = vec![1.0f32; N / 2 + 1];
    let mut raw = vec![1.0f32; N / 2 + 1];
    spectral(x, |_, m, g| {
        for k in 0..=N / 2 {
            // mean magnitude -> noise power (Rayleigh: E|N|^2 = 4/pi (E|N|)^2)
            let n2 = 1.27 * (alpha * profile[k]).powi(2);
            let s2 = (m[k] * m[k] - n2).max(0.0);
            raw[k] = (s2 / (s2 + n2).max(1e-12)).sqrt().max(floor);
        }
        for k in 0..=N / 2 {
            let a = raw[k.saturating_sub(2)];
            let b = raw[k.saturating_sub(1)];
            let c = raw[(k + 1).min(N / 2)];
            let d = raw[(k + 2).min(N / 2)];
            let s = 0.1 * a + 0.2 * b + 0.4 * raw[k] + 0.2 * c + 0.1 * d;
            // fast open, slower close
            let v = if s > prev[k] { 0.3 * prev[k] + 0.7 * s } else { 0.7 * prev[k] + 0.3 * s };
            prev[k] = v;
            g[k] = v.max(floor);
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
    let frames = if x.len() >= M { ((x.len() - M) / (M / 2) + 1).min(24) } else { 0 };
    if frames == 0 {
        return (50.0, 0.0);
    }
    let step = if frames > 1 { (x.len() - M) / (frames - 1) } else { 0 };
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
    if a >= b { (50.0, a) } else { (60.0, b) }
}

pub fn remove_hum(x: &[f32], f0: f32, harmonics: usize, q: f32) -> Vec<f32> {
    let mut filters: Vec<Biquad> = (1..=harmonics).map(|h| f0 * h as f32).filter(|f| *f < SR * 0.45).map(|f| Biquad::new(BiquadKind::Notch, f, q, 0.0)).collect();
    // the fundamental's rumble below it goes too
    filters.push(Biquad::new(BiquadKind::LowCut, (f0 * 0.6).max(25.0), 0.707, 0.0));
    x.iter().map(|v| filters.iter_mut().fold(*v, |s, f| f.process(s))).collect()
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
        let local = lvl[i.saturating_sub(win / 2).max(0)].max(lvl[(i + win / 2).min(d.len() - 1)]).max(1e-5);
        if d[i] > k * local && d[i] > 0.02 {
            let mut j = i;
            while j + 1 < d.len() && j - i < maxlen && d[j + 1] > 0.5 * k * local {
                j += 1;
            }
            if j - i < maxlen {
                let (a, b) = (i.saturating_sub(3), (j + 3).min(y.len() - 1));
                let (ya, yb) = (y[a], y[b]);
                let (da, db) = (y[a] - y[a.saturating_sub(1)], y[(b + 1).min(y.len() - 1)] - y[b]);
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
pub fn gate(x: &[f32], open_db: f32, hyst_db: f32, attack_ms: f32, hold_ms: f32, release_ms: f32, range_db: f32) -> (Vec<f32>, f32) {
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
        let t = if target.get(fi).copied().unwrap_or(false) { 1.0 } else { lo };
        g = if t > g { t + (g - t) * a } else { t + (g - t) * r };
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
                let gk = (1.0 - amount * late / p[k].max(1e-12)).max(floor * floor).sqrt();
                let v = 0.5 * prev[k] + 0.5 * gk;
                prev[k] = v;
                g[k] = v;
            }
        }
        hist.push(p);
    })
}

/// RMS (dB) of the quietest 10% and loudest 10% of 20 ms windows.
pub fn floor_and_peak_db(x: &[f32]) -> (f32, f32) {
    let w = (0.02 * SR) as usize;
    let mut v: Vec<f32> = x.chunks(w).filter(|c| c.len() == w).map(|c| 10.0 * (c.iter().map(|s| s * s).sum::<f32>() / w as f32).max(1e-12).log10()).collect();
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

fn save(e: &mut Engine, base: &crate::samples::SampleInfo, nn: &str, y: &[f32], what: &str) -> Result<String> {
    let name = crate::samples::sample_name(nn);
    std::fs::create_dir_all(e.samples_dir())?;
    let path = e.samples_dir().join(format!("{name}.wav"));
    crate::render::write_wav(&path, y, y)?;
    let info = crate::samples::SampleInfo { name, path: path.to_string_lossy().into(), source: format!("{} ({what})", base.source), license: base.license.clone(), author: base.author.clone(), duration: 0.0 };
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
            description: "Spectral noise reduction (RX-style): subtracts a learned noise print per frequency with smoothed gains (no warbling). profile = a learn_noise_profile name (default: learned from the take's quietest parts now). reduction_db = deepest cut (default 18; 10 gentle, 30 strong), sensitivity 0.5-4 (default 1.5; higher removes more, risks the voice). Writes <sample>_nr and reports noise floor and SNR before/after.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "profile": {"type": "string"}, "reduction_db": {"type": "number"}, "sensitivity": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
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
                let rd = f_or(a, "reduction_db", 18.0).clamp(3.0, 40.0);
                let sens = f_or(a, "sensitivity", 1.5);
                let y = reduce_noise(&x, &p, rd, sens);
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
        let clean: Vec<f32> = (0..n).map(|i| {
            let t = i as f32 / SR;
            let on = (t > 0.6 && t < 1.4) as i32 as f32;
            on * 0.3 * ((t * 220.0 * std::f32::consts::TAU).sin() + 0.4 * (t * 660.0 * std::f32::consts::TAU).sin())
        }).collect();
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
    fn hum_is_found_and_notched() {
        let x: Vec<f32> = (0..(2.0 * SR) as usize).map(|i| { let t = i as f32 / SR; 0.1 * (t * 60.0 * std::f32::consts::TAU).sin() + 0.05 * (t * 180.0 * std::f32::consts::TAU).sin() }).collect();
        let (f, s) = detect_hum(&x);
        assert_eq!(f, 60.0);
        assert!(s > 6.0);
        let y = remove_hum(&x, 60.0, 8, 30.0);
        let e = |v: &[f32]| v[(0.5 * SR) as usize..].iter().map(|s| s * s).sum::<f32>();
        assert!(e(&y) < e(&x) * 0.05);
    }

    #[test]
    fn clicks_are_repaired_and_gate_ducks_gaps() {
        let mut x: Vec<f32> = (0..SR as usize).map(|i| 0.2 * (i as f32 / SR * 200.0 * std::f32::consts::TAU).sin()).collect();
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
        let e = |v: &[f32]| v[(0.45 * SR) as usize..(0.9 * SR) as usize].iter().map(|s| s * s).sum::<f32>();
        assert!(e(&y) < e(&x) * 0.6, "{} vs {}", e(&y), e(&x));
    }
}
