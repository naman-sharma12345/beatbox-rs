//! Autonomous mixing primitives as MCP actions:
//! - `analyze_loudness`: BS.1770-4 / EBU R128 integrated, short-term (3 s),
//!   momentary (400 ms), Tech 3342 loudness range and true peak
//!   (`dsp::loudness_meter`).
//! - `carve_frequencies`: sidechain spectral ducking, a key (a vocal)
//!   carving one band of a target only while it sounds there
//!   (`dsp::carve_eq`).
//! Opt-in: nothing else calls them, and the project is not changed.

use crate::dsp::carve_eq::{carve_coeffs, carve_focus_hz, group_delay_samples, CarveMode, CarveParams, Carver};
use crate::dsp::loudness_meter::measure_stereo;
use crate::dsp::SR;
use crate::render::write_wav;
use crate::samples;
use crate::tools::{f_opt, obj, s_opt, Tool};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

fn r2(v: f64) -> f64 {
    if v <= -199.0 {
        return v;
    }
    (v * 100.0).round() / 100.0
}

fn analyze(e: &mut crate::engine::Engine, a: &Value) -> Result<Value> {
    // a file is measured at its own sample rate; the mix at the engine's
    let (name, l, r, sr) = if let Some(p) = s_opt(a, "path") {
        let path = e.resolve(&p);
        let (l, r, rate) = samples::decode_stereo_native(&path)?;
        (path.display().to_string(), l, r, rate as f32)
    } else {
        let m = e.mix()?;
        ("mix".to_string(), m.left.clone(), m.right.clone(), SR)
    };
    if l.is_empty() {
        bail!("no audio to measure");
    }
    let (rep, meter) = measure_stereo(&l, &r, sr);
    let mut v = json!({
        "source": name,
        "sample_rate": sr,
        "integrated_lufs": r2(rep.integrated_lufs),
        "short_term_max_lufs": r2(rep.short_term_max_lufs),
        "momentary_max_lufs": r2(rep.momentary_max_lufs),
        "loudness_range_lu": r2(rep.loudness_range_lu),
        "lra_low_lufs": r2(rep.lra_low_lufs),
        "lra_high_lufs": r2(rep.lra_high_lufs),
        "true_peak_dbtp": r2(rep.true_peak_dbtp),
        "sample_peak_dbfs": r2(rep.sample_peak_dbfs),
        "plr_db": r2(rep.true_peak_dbtp - rep.integrated_lufs),
        "duration_s": r2(rep.duration_s),
        "method": "ITU-R BS.1770-4 K-weighting; momentary 400 ms and short-term 3 s windows every 100 ms; integrated gated -70 LUFS abs / -10 LU rel; LRA per EBU Tech 3342 (-70 abs / -20 LU rel, 10th-95th percentile); true peak 4x per BS.1770-4 Annex 2",
    });
    if let Some(t) = f_opt(a, "target_lufs") {
        v["gain_to_target_db"] = json!(r2(t as f64 - rep.integrated_lufs));
    }
    if a.get("series").and_then(|s| s.as_bool()).unwrap_or(false) {
        let every = |x: Vec<f64>| x.into_iter().step_by(10).map(r2).collect::<Vec<_>>();
        v["short_term_lufs_per_s"] = json!(every(meter.short_term()));
        v["momentary_lufs_per_s"] = json!(every(meter.momentary()));
        v["series_note"] = json!("one value per second; short-term starts at 3 s, momentary at 0.4 s");
    }
    Ok(v)
}

fn carve(e: &mut crate::engine::Engine, a: &Value) -> Result<Value> {
    let d = CarveParams::default();
    let (lo, hi) = match s_opt(a, "band").as_deref() {
        Some("mud") => (150.0, 400.0),
        Some("presence") => (2000.0, 4000.0),
        Some(other) => bail!("band '{other}': use mud (150-400 Hz), presence (2-4 kHz), or low_hz/high_hz"),
        None => (d.low_hz, d.high_hz),
    };
    let mode = match s_opt(a, "mode").as_deref() {
        None | Some("bell") => CarveMode::Bell,
        Some("low_shelf") => CarveMode::LowShelf,
        Some("high_shelf") => CarveMode::HighShelf,
        Some(other) => bail!("mode '{other}': bell, low_shelf or high_shelf"),
    };
    let p = CarveParams {
        mode,
        low_hz: f_opt(a, "low_hz").unwrap_or(lo),
        high_hz: f_opt(a, "high_hz").unwrap_or(hi),
        key_low_hz: f_opt(a, "key_low_hz"),
        key_high_hz: f_opt(a, "key_high_hz"),
        threshold_db: f_opt(a, "threshold_db").unwrap_or(d.threshold_db),
        ratio: f_opt(a, "ratio").unwrap_or(d.ratio),
        max_cut_db: f_opt(a, "max_cut_db").unwrap_or(d.max_cut_db),
        attack_ms: f_opt(a, "attack_ms").unwrap_or(d.attack_ms),
        release_ms: f_opt(a, "release_ms").unwrap_or(d.release_ms),
    };
    if !(20.0..=20000.0).contains(&p.low_hz) || p.high_hz <= p.low_hz * 1.05 || p.high_hz > 20000.0 {
        bail!("low_hz/high_hz: 20..20000 Hz with high_hz above low_hz (got {}..{})", p.low_hz, p.high_hz);
    }
    if !(0.0..=24.0).contains(&p.max_cut_db) || !(1.0..=100.0).contains(&p.ratio) || !(-80.0..=0.0).contains(&p.threshold_db) {
        bail!("max_cut_db 0..24, ratio 1..100, threshold_db -80..0");
    }
    let tpath = e.resolve(&s_opt(a, "target").ok_or_else(|| anyhow!("target: the audio file to carve"))?);
    let kpath = e.resolve(&s_opt(a, "key").ok_or_else(|| anyhow!("key: the audio file that drives the carve (the vocal)"))?);
    let (mut l, mut r) = samples::decode_stereo(&tpath)?;
    let (kl, kr) = samples::decode_stereo(&kpath)?;
    let key: Vec<f32> = kl.iter().zip(&kr).map(|(a, b)| 0.5 * (a + b)).collect();
    let mut c = Carver::new(SR, p);
    c.process(&key, &mut [l.as_mut_slice(), r.as_mut_slice()]);
    let (max_cut, ducked) = c.stats();
    let mut v = json!({
        "target": tpath.display().to_string(),
        "key": kpath.display().to_string(),
        "band_hz": [p.low_hz, p.high_hz],
        "key_band_hz": [p.key_low_hz.unwrap_or(p.low_hz), p.key_high_hz.unwrap_or(p.high_hz)],
        "threshold_db": p.threshold_db, "ratio": p.ratio, "max_cut_db": p.max_cut_db,
        "attack_ms": p.attack_ms, "release_ms": p.release_ms,
        "deepest_cut_db": (max_cut * 100.0).round() / 100.0,
        "carved_percent": if l.is_empty() { 0.0 } else { ((ducked as f64 / l.len() as f64) * 1000.0).round() / 10.0 },
        "latency_samples": 0,
    });
    // phase: report the filter's group delay where it acts, at the deepest cut used (or max_cut_db)
    let cut = if max_cut > 0.0 { max_cut as f64 } else { p.max_cut_db as f64 };
    let (b, aa) = carve_coeffs(&p, cut, SR as f64);
    let focus = carve_focus_hz(&p, SR as f64);
    let gd = group_delay_samples(b, aa, focus, SR as f64);
    v["mode"] = json!(match mode { CarveMode::Bell => "bell", CarveMode::LowShelf => "low_shelf", CarveMode::HighShelf => "high_shelf" });
    v["phase"] = json!({
        "type": "minimum-phase IIR (biquad); no latency, so the carved target stays aligned with the key",
        "at_hz": focus.round(),
        "cut_db": (cut * 100.0).round() / 100.0,
        "group_delay_samples": (gd * 100.0).round() / 100.0,
        "group_delay_ms": (gd / SR as f64 * 1e5).round() / 100.0,
    });
    match s_opt(a, "out") {
        Some(out) => {
            let path = e.resolve(&out);
            write_wav(&path, &l, &r)?;
            v["out"] = json!(path.display().to_string());
        }
        None => v["note"] = json!("measured only; pass out to write the carved target (16-bit wav)"),
    }
    Ok(v)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "analyze_loudness",
            description: "Loudness per ITU-R BS.1770-4 / EBU R128 of a file (path, at its own sample rate) or the current mix: integrated LUFS, short-term (3 s) and momentary (400 ms) max, EBU Tech 3342 loudness range, true peak (4x) and sample peak, PLR. series: true adds per-second short-term and momentary curves; target_lufs adds the gain to reach it.",
            mutates: false,
            schema: || {
                obj(
                    json!({
                        "path": {"type": "string", "description": "Audio file (default: the current mix)"},
                        "series": {"type": "boolean", "description": "Add per-second short-term and momentary values"},
                        "target_lufs": {"type": "number", "description": "Report the gain needed to reach this integrated loudness"}
                    }),
                    &[],
                )
            },
            run: analyze,
        },
        Tool {
            name: "carve_frequencies",
            description: "Sidechain spectral ducking: a key file (a vocal) carves one band of a target file only while the key has energy there (dynamic EQ). mode: bell (default), low_shelf (corner high_hz) or high_shelf (corner low_hz). band: mud (150-400 Hz) or presence (2-4 kHz), or low_hz/high_hz; threshold_db (-40), ratio (4), max_cut_db (6), attack_ms (10), release_ms (150), optional key_low_hz/key_high_hz. Writes the carved target to out (16-bit wav); reports the deepest cut, time carved and the filter's phase (minimum-phase, group delay at the band). Does not change the project.",
            mutates: false,
            schema: || {
                obj(
                    json!({
                        "target": {"type": "string", "description": "Audio file to carve (the pad, guitars, synth bus)"},
                        "key": {"type": "string", "description": "Audio file that drives the carve (the vocal)"},
                        "out": {"type": "string", "description": "Where to write the carved target"},
                        "band": {"type": "string", "enum": ["mud", "presence"], "description": "mud = 150-400 Hz, presence = 2-4 kHz"},
                        "mode": {"type": "string", "enum": ["bell", "low_shelf", "high_shelf"], "description": "Cut shape (default bell)"},
                        "low_hz": {"type": "number"},
                        "high_hz": {"type": "number"},
                        "key_low_hz": {"type": "number", "description": "Key detector band (default: the carve band)"},
                        "key_high_hz": {"type": "number"},
                        "threshold_db": {"type": "number", "description": "Key band level where carving starts, dBFS (default -40)"},
                        "ratio": {"type": "number", "description": "default 4"},
                        "max_cut_db": {"type": "number", "description": "Deepest cut, 0..24 dB (default 6)"},
                        "attack_ms": {"type": "number", "description": "default 10"},
                        "release_ms": {"type": "number", "description": "default 150"}
                    }),
                    &["target", "key"],
                )
            },
            run: carve,
        },
    ]
}

#[cfg(test)]
mod tests {
    use crate::engine::Engine;
    use serde_json::json;
    use std::f32::consts::PI;

    #[test]
    fn analyze_loudness_reads_a_minus_23_lufs_file_and_carve_frequencies_ducks_mud() {
        let dir = std::env::temp_dir().join("bb_mix_analysis_tool");
        let _ = std::fs::create_dir_all(&dir);
        // 10 s of 1 kHz at -23 dBFS per channel: -23 LUFS, 0 LU range, -23 dBTP
        let a = 10f32.powf(-23.0 / 20.0);
        let x: Vec<f32> = (0..441000).map(|i| a * (2.0 * PI * 1000.0 * i as f32 / 44100.0).sin()).collect();
        let f = dir.join("m23.wav");
        crate::render::write_wav(&f, &x, &x).unwrap();
        let mut e = Engine::new(dir.clone());
        let v = e.call("analyze_loudness", &json!({"path": f.display().to_string(), "series": true, "target_lufs": -14.0})).unwrap();
        assert!((v["integrated_lufs"].as_f64().unwrap() + 23.0).abs() < 0.15, "{v}");
        assert!((v["momentary_max_lufs"].as_f64().unwrap() + 23.0).abs() < 0.15, "{v}");
        assert!(v["loudness_range_lu"].as_f64().unwrap() < 0.2, "{v}");
        assert!((v["true_peak_dbtp"].as_f64().unwrap() + 23.0).abs() < 0.2, "{v}");
        assert!((v["gain_to_target_db"].as_f64().unwrap() - 9.0).abs() < 0.15, "{v}");
        assert!(v["short_term_lufs_per_s"].as_array().unwrap().len() >= 7);
        // carve: a 250 Hz pad against a 220 Hz "vocal"
        let n = 44100;
        let pad: Vec<f32> = (0..n).map(|i| 0.25 * (2.0 * PI * 250.0 * i as f32 / 44100.0).sin()).collect();
        let voc: Vec<f32> = (0..n).map(|i| 0.25 * (2.0 * PI * 220.0 * i as f32 / 44100.0).sin()).collect();
        let (pf, vf, of) = (dir.join("pad.wav"), dir.join("voc.wav"), dir.join("carved.wav"));
        crate::render::write_wav(&pf, &pad, &pad).unwrap();
        crate::render::write_wav(&vf, &voc, &voc).unwrap();
        let v = e
            .call("carve_frequencies", &json!({"target": pf.display().to_string(), "key": vf.display().to_string(), "out": of.display().to_string(), "band": "mud"}))
            .unwrap();
        assert!((v["deepest_cut_db"].as_f64().unwrap() - 6.0).abs() < 0.05, "{v}");
        assert!(v["carved_percent"].as_f64().unwrap() > 90.0, "{v}");
        assert!(of.exists());
        assert!(v["phase"]["group_delay_samples"].as_f64().unwrap().is_finite(), "{v}");
        let v = e
            .call("carve_frequencies", &json!({"target": pf.display().to_string(), "key": vf.display().to_string(), "band": "mud", "mode": "low_shelf"}))
            .unwrap();
        assert_eq!(v["mode"], "low_shelf");
        assert!((v["deepest_cut_db"].as_f64().unwrap() - 6.0).abs() < 0.05, "{v}");
        assert!(e.call("carve_frequencies", &json!({"target": pf.display().to_string(), "key": vf.display().to_string(), "band": "air"})).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
