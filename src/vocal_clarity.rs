//! De-muffle / voice restoration: rebuild the top band a phone mic lost
//! (spectral bandwidth extension), a harmonic exciter, a dynamic "clarity"
//! EQ (boxiness down, presence and air up) and match-EQ to a reference.
//! Each tool writes a new sample and reports the tone balance before/after.

use crate::dsp::{Biquad, BiquadKind, SR};
use crate::engine::Engine;
use crate::tools::{f_opt, f_or, obj, s_opt, s_req, Tool};
use crate::vocal_repair::{mags, spectral, N};
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// Long-term average magnitude spectrum (N/2+1 bins).
pub fn ltas(x: &[f32]) -> Vec<f32> {
    let ms = mags(x);
    if ms.is_empty() {
        return vec![1e-9; N / 2 + 1];
    }
    // loud frames only: silence would drag the voice's spectrum toward the noise
    let e: Vec<f32> = ms.iter().map(|m| m.iter().map(|v| v * v).sum::<f32>()).collect();
    let mut s = e.clone();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let thr = s[s.len() / 2];
    let keep: Vec<usize> = (0..ms.len()).filter(|&i| e[i] >= thr).collect();
    (0..=N / 2).map(|k| (keep.iter().map(|&i| ms[i][k] * ms[i][k]).sum::<f32>() / keep.len().max(1) as f32).sqrt().max(1e-9)).collect()
}

fn band_db(spec: &[f32], lo: f32, hi: f32) -> f32 {
    let hz = SR / N as f32;
    let (a, b) = (((lo / hz) as usize).max(1), ((hi / hz) as usize).min(spec.len() - 1));
    let m = spec[a..=b].iter().map(|v| v * v).sum::<f32>() / (b - a + 1).max(1) as f32;
    10.0 * m.max(1e-18).log10()
}

/// Tone balance relative to the 500 Hz-2 kHz core (dB): body 200-500,
/// presence 2-5k, brilliance 5-8k, air 8-16k; and the top frequency still
/// within 35 dB of the core (where a phone mic or codec cut it off).
pub fn tone(x: &[f32]) -> Value {
    let s = ltas(x);
    let core = band_db(&s, 500.0, 2000.0);
    let r = |v: f32| ((v - core) * 10.0).round() / 10.0;
    json!({
        "body_200_500": r(band_db(&s, 200.0, 500.0)),
        "presence_2k_5k": r(band_db(&s, 2000.0, 5000.0)),
        "brilliance_5k_8k": r(band_db(&s, 5000.0, 8000.0)),
        "air_8k_16k": r(band_db(&s, 8000.0, 16000.0)),
        "top_hz": cutoff(&s),
    })
}

/// The highest frequency whose 1/3-octave level is within 35 dB of the core.
pub fn cutoff(s: &[f32]) -> f32 {
    let core = band_db(s, 500.0, 2000.0);
    let mut f = 16000.0f32;
    while f > 1500.0 {
        if band_db(s, f / 1.12, f * 1.12) > core - 35.0 {
            return f.round();
        }
        f /= 1.06;
    }
    1500.0
}

/// Bandwidth extension: the octave under the cutoff is band-passed, run
/// through a soft rectifier (it grows harmonics an octave and more up), the
/// result is high-passed at the cutoff, shaped to fall about 6 dB/octave
/// and mixed back at `amount`, scaled to the band it continues.
pub fn extend_bandwidth(x: &[f32], cutoff_hz: f32, amount: f32) -> Vec<f32> {
    let fc = cutoff_hz.clamp(2000.0, 16000.0);
    let mut hp1 = Biquad::new(BiquadKind::LowCut, fc / 2.0, 0.707, 0.0);
    let mut lp1 = Biquad::new(BiquadKind::HighCut, fc, 0.707, 0.0);
    let src: Vec<f32> = x.iter().map(|v| lp1.process(hp1.process(*v))).collect();
    // full-wave rectify (even harmonics) + a little odd from tanh
    let mut env = 0.0f32;
    let gen: Vec<f32> = src.iter().map(|v| {
        env = env.max(v.abs()) * 0.9995 + v.abs() * 0.0005;
        let n = v / env.max(1e-4);
        (n.abs() - 0.63) * env + 0.3 * (2.0 * n).tanh() * env
    }).collect();
    let mut hp2 = Biquad::new(BiquadKind::LowCut, fc, 0.707, 0.0);
    let mut hp3 = Biquad::new(BiquadKind::LowCut, fc, 0.707, 0.0);
    let mut tilt = Biquad::new(BiquadKind::HighShelf, fc * 2.0, 0.707, -6.0);
    let mut top = Biquad::new(BiquadKind::HighCut, 17000.0, 0.707, 0.0);
    let hi: Vec<f32> = gen.iter().map(|v| top.process(tilt.process(hp3.process(hp2.process(*v))))).collect();
    // level: the new band starts ~6 dB under the octave it continues
    let rms = |v: &[f32]| (v.iter().map(|s| s * s).sum::<f32>() / v.len().max(1) as f32).sqrt();
    let k = rms(&src) / rms(&hi).max(1e-9) * 0.5 * amount.clamp(0.0, 1.5);
    x.iter().zip(hi.iter()).map(|(a, b)| a + k * b).collect()
}

/// Harmonic exciter: the band over `freq` is saturated and blended back.
pub fn excite(x: &[f32], freq: f32, drive: f32, mix: f32) -> Vec<f32> {
    let mut hp = Biquad::new(BiquadKind::LowCut, freq, 0.707, 0.0);
    let mut hp2 = Biquad::new(BiquadKind::LowCut, freq, 0.707, 0.0);
    let d = 1.0 + drive.clamp(0.0, 1.0) * 9.0;
    x.iter().map(|v| {
        let h = hp.process(*v);
        let s = hp2.process((h * d).tanh() / d.sqrt());
        v + mix.clamp(0.0, 1.0) * s
    }).collect()
}

/// Clarity EQ: per frame, the 200-500 Hz body is cut by as much as it
/// stands over the voice's own average balance (up to `box_db`), then a
/// static presence bell (3-6 kHz) and an air shelf (10 kHz+) are added.
pub fn clarity(x: &[f32], box_db: f32, presence_db: f32, air_db: f32) -> Vec<f32> {
    let hz = SR / N as f32;
    let s = ltas(x);
    let target = band_db(&s, 200.0, 500.0) - band_db(&s, 500.0, 2000.0) - 2.0;
    let (b0, b1, c0, c1) = ((200.0 / hz) as usize, (500.0 / hz) as usize, (500.0 / hz) as usize, (2000.0 / hz) as usize);
    let stat: Vec<f32> = (0..=N / 2).map(|k| {
        let f = k as f32 * hz;
        // presence bell centred 4.2 kHz (~1.2 oct), air shelf from 10 kHz
        let p = presence_db * (-0.5 * ((f / 4200.0).max(1e-3).log2() / 0.6).powi(2)).exp();
        let a = air_db * (1.0 / (1.0 + (10000.0 / f.max(1.0)).powi(4)));
        10f32.powf((p + a) / 20.0)
    }).collect();
    let mut prev = 1.0f32;
    let bw = 0.7f32;
    spectral(x, |_, m, g| {
        let e = |a: usize, b: usize| m[a..=b].iter().map(|v| v * v).sum::<f32>() / (b - a + 1) as f32;
        let (eb, ec) = (e(b0, b1), e(c0, c1));
        let over = if eb > 1e-12 && ec > 1e-12 { 10.0 * (eb / ec).log10() - target } else { 0.0 };
        let cut = over.clamp(0.0, box_db.abs());
        let gb = 10f32.powf(-cut / 20.0);
        let gs = if gb < prev { 0.5 * prev + 0.5 * gb } else { 0.85 * prev + 0.15 * gb };
        prev = gs;
        for k in 0..=N / 2 {
            let f = k as f32 * hz;
            // the cut is a bell over 200-500 (centre 320 Hz)
            let w = (-0.5 * ((f / 320.0).max(1e-3).log2() / bw).powi(2)).exp();
            g[k] = stat[k] * (1.0 + w * (gs - 1.0));
        }
    })
}

/// Match EQ: a smooth curve (1/3-octave) that moves the take's long-term
/// spectrum toward the reference's, scaled by `amount`, capped at ±max_db.
pub fn match_curve(x: &[f32], reference: &[f32], amount: f32, max_db: f32) -> Vec<f32> {
    let (a, b) = (ltas(x), ltas(reference));
    let hz = SR / N as f32;
    // overall level does not count: align on 500 Hz-2 kHz
    let off = band_db(&b, 500.0, 2000.0) - band_db(&a, 500.0, 2000.0);
    (0..=N / 2).map(|k| {
        let f = (k as f32 * hz).max(20.0);
        let d = band_db(&b, f / 1.12, f * 1.12) - off - band_db(&a, f / 1.12, f * 1.12);
        let d = if f < 40.0 || f > 18000.0 { 0.0 } else { d };
        10f32.powf((d * amount).clamp(-max_db, max_db) / 20.0)
    }).collect()
}

fn save(e: &mut Engine, base: &crate::samples::SampleInfo, nn: &str, y: &[f32], what: &str) -> Result<String> {
    let mut y = y.to_vec();
    // never hand back a clipped file
    let pk = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if pk > 0.98 {
        let g = 0.98 / pk;
        y.iter_mut().for_each(|v| *v *= g);
    }
    let name = crate::samples::sample_name(nn);
    std::fs::create_dir_all(e.samples_dir())?;
    let path = e.samples_dir().join(format!("{name}.wav"));
    crate::render::write_wav(&path, &y, &y)?;
    let info = crate::samples::SampleInfo { name, path: path.to_string_lossy().into(), source: format!("{} ({what})", base.source), license: base.license.clone(), author: base.author.clone(), duration: 0.0 };
    let r = crate::tools::register_sample(e, info)?;
    e.revision += 1;
    Ok(r["sample"].as_str().unwrap_or(nn).to_string())
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "analyze_vocal_tone",
            description: "How muffled or bright a vocal is: body (200-500 Hz), presence (2-5k), brilliance (5-8k) and air (8-16k) relative to the 500 Hz-2 kHz core, and top_hz, the highest frequency the recording still carries (a phone take often stops near 4-8 kHz). Read-only; sample or path.",
            mutates: false,
            schema: || obj(json!({"sample": {"type": "string"}, "path": {"type": "string"}}), &[]),
            run: |e, a| {
                let (name, l, r) = crate::tools_sound::audio_for(e, a)?;
                let m: Vec<f32> = l.iter().zip(r.iter()).map(|(x, y)| 0.5 * (x + y)).collect();
                let t = tone(&m);
                let mut find = Vec::new();
                if t["top_hz"].as_f64().unwrap_or(16000.0) < 9000.0 {
                    find.push("band-limited (phone/codec): extend_bandwidth rebuilds the top");
                }
                if t["presence_2k_5k"].as_f64().unwrap_or(0.0) < -12.0 {
                    find.push("dull: little presence; clarity_eq or an exciter");
                }
                if t["body_200_500"].as_f64().unwrap_or(0.0) > 2.0 {
                    find.push("boxy: too much 200-500 Hz; clarity_eq cuts it");
                }
                Ok(json!({"source": name, "tone_db": t, "findings": find}))
            },
        },
        Tool {
            name: "extend_bandwidth",
            description: "De-muffle a band-limited vocal (phone mic, voice note, old codec): rebuilds the missing top band from harmonics of the octave below the cutoff (spectral bandwidth extension). cutoff_hz default = detected top_hz; amount 0..1.5 (default 0.7). Writes <sample>_bright and reports tone before/after.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "cutoff_hz": {"type": "number"}, "amount": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let before = tone(&x);
                let fc = f_opt(a, "cutoff_hz").unwrap_or_else(|| before["top_hz"].as_f64().unwrap_or(8000.0) as f32);
                if fc >= 15000.0 && f_opt(a, "cutoff_hz").is_none() {
                    bail!("the take already reaches {fc} Hz: nothing to extend (try clarity_eq or exciter)");
                }
                let y = extend_bandwidth(&x, fc, f_or(a, "amount", 0.7));
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_bright", info.name));
                let s = save(e, &info, &nn, &y, &format!("bandwidth extension from {fc:.0} Hz"))?;
                Ok(json!({"sample": s, "cutoff_hz": fc, "tone_db": {"before": before, "after": tone(&y)}}))
            },
        },
        Tool {
            name: "exciter",
            description: "Harmonic exciter for a dull vocal or instrument: the band above freq (default 3000 Hz) is saturated (drive 0..1, default 0.4) and blended in (mix 0..1, default 0.25), adding upper harmonics that read as clarity. Writes <sample>_excited.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "freq": {"type": "number"}, "drive": {"type": "number"}, "mix": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let y = excite(&x, f_or(a, "freq", 3000.0).clamp(800.0, 12000.0), f_or(a, "drive", 0.4), f_or(a, "mix", 0.25));
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_excited", info.name));
                let s = save(e, &info, &nn, &y, "exciter")?;
                Ok(json!({"sample": s, "tone_db": {"before": tone(&x), "after": tone(&y)}}))
            },
        },
        Tool {
            name: "clarity_eq",
            description: "Dynamic clarity EQ for a vocal: cuts 200-500 Hz boxiness only when and as much as it builds up (up to box_db, default 4), adds presence around 3-6 kHz (presence_db, default 3) and air from 10 kHz (air_db, default 3). Writes <sample>_clear and reports tone before/after.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "box_db": {"type": "number"}, "presence_db": {"type": "number"}, "air_db": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let y = clarity(&x, f_or(a, "box_db", 4.0).clamp(0.0, 12.0), f_or(a, "presence_db", 3.0).clamp(-6.0, 9.0), f_or(a, "air_db", 3.0).clamp(-6.0, 9.0));
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_clear", info.name));
                let s = save(e, &info, &nn, &y, "clarity EQ")?;
                Ok(json!({"sample": s, "tone_db": {"before": tone(&x), "after": tone(&y)}}))
            },
        },
        Tool {
            name: "match_eq",
            description: "Match EQ: make a take's tone like a reference (reference = a sample name, or reference_path = a wav/mp3 of a vocal or song you like). A smooth 1/3-octave curve, amount 0..1 (default 0.6), capped at max_db (default 8). Writes <sample>_matched and reports the tone of both and the result.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "reference": {"type": "string"}, "reference_path": {"type": "string"}, "amount": {"type": "number"}, "max_db": {"type": "number"}, "new_name": {"type": "string"}}), &["sample"]),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let reference: Vec<f32> = if let Some(r) = s_opt(a, "reference") {
                    crate::tools_sound::sample_data(e, &r)?.1
                } else if let Some(p) = s_opt(a, "reference_path") {
                    let (l, r) = crate::samples::decode_stereo(&e.resolve(&p))?;
                    l.iter().zip(r.iter()).map(|(a, b)| 0.5 * (a + b)).collect()
                } else {
                    bail!("give reference (a sample) or reference_path (a file)");
                };
                let curve = match_curve(&x, &reference, f_or(a, "amount", 0.6).clamp(0.0, 1.0), f_or(a, "max_db", 8.0).clamp(1.0, 18.0));
                let y = spectral(&x, |_, _, g| g.copy_from_slice(&curve));
                let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_matched", info.name));
                let s = save(e, &info, &nn, &y, "match EQ")?;
                Ok(json!({"sample": s, "tone_db": {"take": tone(&x), "reference": tone(&reference), "result": tone(&y)}}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phone_voice() -> Vec<f32> {
        // a buzzy voice-like tone, then a 3.5 kHz brickwall (a phone)
        let n = (2.0 * SR) as usize;
        let mut x: Vec<f32> = (0..n).map(|i| {
            let t = i as f32 / SR;
            (1..40).map(|h| (t * 150.0 * h as f32 * std::f32::consts::TAU).sin() / h as f32).sum::<f32>() * 0.1
        }).collect();
        for _ in 0..4 {
            let mut lp = Biquad::new(BiquadKind::HighCut, 3500.0, 0.707, 0.0);
            x.iter_mut().for_each(|v| *v = lp.process(*v));
        }
        x
    }

    #[test]
    fn bandwidth_extension_adds_air_to_a_phone_take() {
        let x = phone_voice();
        let t0 = tone(&x);
        assert!(t0["top_hz"].as_f64().unwrap() < 9000.0, "{t0}");
        let y = extend_bandwidth(&x, 3500.0, 0.8);
        let t1 = tone(&y);
        assert!(t1["air_8k_16k"].as_f64().unwrap() > t0["air_8k_16k"].as_f64().unwrap() + 10.0, "{t0} -> {t1}");
        assert!(t1["top_hz"].as_f64().unwrap() > t0["top_hz"].as_f64().unwrap());
    }

    #[test]
    fn clarity_and_match_move_the_tone() {
        let x = phone_voice();
        let y = clarity(&x, 6.0, 4.0, 3.0);
        let (t0, t1) = (tone(&x), tone(&y));
        assert!(t1["presence_2k_5k"].as_f64().unwrap() > t0["presence_2k_5k"].as_f64().unwrap() + 1.5, "{t0} -> {t1}");
        let ex = excite(&x, 2000.0, 0.6, 0.4);
        assert!(tone(&ex)["brilliance_5k_8k"].as_f64().unwrap() > t0["brilliance_5k_8k"].as_f64().unwrap());
        // match a dark take to a brighter reference
        let bright = extend_bandwidth(&x, 3500.0, 1.0);
        let c = match_curve(&x, &bright, 1.0, 12.0);
        let hz = SR / N as f32;
        assert!(c[(10000.0 / hz) as usize] > 1.5, "boost at 10k: {}", c[(10000.0 / hz) as usize]);
    }
}
