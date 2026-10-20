//! `multiband_dynamics`: the LR4 multiband compressor (`dsp::multiband`)
//! as one MCP action, on a file or the current mix. Opt-in: nothing else
//! calls it, and the project is not changed.

use crate::analysis;
use crate::dsp::multiband::{process_offline, BandParams};
use crate::dsp::SR;
use crate::render::write_wav;
use crate::tools::{obj, s_opt, Tool};
use crate::tools_sound::audio_for;
use anyhow::{bail, Result};
use serde_json::{json, Value};

fn r2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

fn band_from(v: &Value, d: BandParams) -> Result<BandParams> {
    let f = |k: &str, dv: f32| {
        v.get(k)
            .and_then(|x| x.as_f64())
            .map(|x| x as f32)
            .unwrap_or(dv)
    };
    let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    if let Some(m) = v.as_object() {
        for k in m.keys() {
            if ![
                "threshold_db",
                "ratio",
                "attack_ms",
                "release_ms",
                "knee_db",
                "makeup_db",
                "solo",
                "bypass",
            ]
            .contains(&k.as_str())
            {
                bail!("unknown band setting '{k}' (threshold_db, ratio, attack_ms, release_ms, knee_db, makeup_db, solo, bypass)");
            }
        }
    }
    let p = BandParams {
        threshold_db: f("threshold_db", d.threshold_db),
        ratio: f("ratio", d.ratio),
        attack_ms: f("attack_ms", d.attack_ms),
        release_ms: f("release_ms", d.release_ms),
        knee_db: f("knee_db", d.knee_db),
        makeup_db: f("makeup_db", d.makeup_db),
        solo: b("solo"),
        bypass: b("bypass"),
    };
    if !(-80.0..=0.0).contains(&p.threshold_db)
        || !(1.0..=100.0).contains(&p.ratio)
        || !(0.0..=24.0).contains(&p.knee_db)
    {
        bail!("band: threshold_db -80..0, ratio 1..100, knee_db 0..24");
    }
    if !(0.0..=500.0).contains(&p.attack_ms)
        || !(1.0..=5000.0).contains(&p.release_ms)
        || !(-24.0..=24.0).contains(&p.makeup_db)
    {
        bail!("band: attack_ms 0..500, release_ms 1..5000, makeup_db -24..24");
    }
    Ok(p)
}

fn run(e: &mut crate::engine::Engine, a: &Value) -> Result<Value> {
    let xo: Vec<f32> = match a.get("crossovers") {
        Some(Value::Array(v)) => v
            .iter()
            .filter_map(|x| x.as_f64())
            .map(|x| x as f32)
            .collect(),
        Some(_) => bail!("crossovers must be an array of frequencies in Hz"),
        None => vec![150.0, 2500.0],
    };
    if xo.is_empty()
        || xo.len() > 4
        || xo.windows(2).any(|w| w[1] <= w[0])
        || xo.iter().any(|f| !(20.0..=18000.0).contains(f))
    {
        bail!("crossovers: 1 to 4 ascending frequencies between 20 and 18000 Hz (default [150, 2500])");
    }
    let nb = xo.len() + 1;
    let mut bands = vec![BandParams::default(); nb];
    if let Some(v) = a.get("bands") {
        let arr = v.as_array().ok_or_else(|| {
            anyhow::anyhow!("bands must be an array of band settings, low to high")
        })?;
        if arr.len() > nb {
            bail!(
                "{} bands given for {} crossovers ({} bands)",
                arr.len(),
                xo.len(),
                nb
            );
        }
        for (i, bv) in arr.iter().enumerate() {
            bands[i] = band_from(bv, BandParams::default())?;
        }
    }
    let (name, l, r) = audio_for(e, a)?;
    let lu_in = analysis::loudness_at(&l, &r, SR).integrated_lufs;
    let mut ch = vec![l, r];
    let gr = process_offline(&mut ch, SR, &xo, &bands);
    let mut v = json!({
        "source": name,
        "crossovers_hz": xo,
        "crossover": "Linkwitz-Riley 24 dB/oct, allpass-compensated: with no compression the bands sum to unity magnitude",
        "latency_samples": 0,
        "bands": bands.iter().zip(&gr).map(|(p, g)| json!({
            "threshold_db": p.threshold_db, "ratio": p.ratio, "attack_ms": p.attack_ms, "release_ms": p.release_ms,
            "knee_db": p.knee_db, "makeup_db": p.makeup_db, "solo": p.solo, "bypass": p.bypass,
            "max_gain_reduction_db": r2(*g),
        })).collect::<Vec<_>>(),
        "integrated_lufs_before": r2(lu_in),
        "integrated_lufs_after": r2(analysis::loudness_at(&ch[0], &ch[1], SR).integrated_lufs),
    });
    match s_opt(a, "out") {
        Some(out) => {
            let path = e.resolve(&out);
            write_wav(&path, &ch[0], &ch[1])?;
            v["out"] = json!(path.display().to_string());
        }
        None => {
            v["note"] = json!("measured only; pass out to write the processed audio (16-bit wav)")
        }
    }
    Ok(v)
}

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "multiband_dynamics",
        description: "Multiband compressor on a file (path) or the current mix: Linkwitz-Riley 24 dB/oct crossovers (default [150, 2500] Hz, up to 4) and a soft-knee compressor per band (threshold_db, ratio, attack_ms, release_ms, knee_db, makeup_db, solo, bypass; bands low to high). Writes a 16-bit wav to out if given; reports per-band max gain reduction and LUFS before/after. Does not change the project.",
        mutates: false,
        schema: || {
            let band = json!({"type": "object", "properties": {
                "threshold_db": {"type": "number", "description": "-80..0 (default -18)"},
                "ratio": {"type": "number", "description": "1..100 (default 2)"},
                "attack_ms": {"type": "number", "description": "default 10"},
                "release_ms": {"type": "number", "description": "default 150"},
                "knee_db": {"type": "number", "description": "soft-knee width, default 6"},
                "makeup_db": {"type": "number", "description": "default 0"},
                "solo": {"type": "boolean"},
                "bypass": {"type": "boolean"}
            }, "additionalProperties": false});
            obj(
                json!({
                    "path": {"type": "string", "description": "Audio file (default: the current mix)"},
                    "sample": {"type": "string", "description": "A registered sample instead"},
                    "out": {"type": "string", "description": "Where to write the processed 16-bit wav"},
                    "crossovers": {"type": "array", "items": {"type": "number"}, "description": "Ascending crossover frequencies in Hz, 1..4 (default [150, 2500])"},
                    "bands": {"type": "array", "items": band, "description": "Per-band settings, low to high (missing bands use the defaults)"}
                }),
                &[],
            )
        },
        run,
    }]
}

#[cfg(test)]
mod tests {
    use crate::engine::Engine;
    use serde_json::json;

    #[test]
    fn multiband_dynamics_compresses_a_file_band_by_band() {
        let dir = std::env::temp_dir().join("bb_multiband_tool");
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("in.wav");
        let x: Vec<f32> = (0..22050)
            .map(|i| {
                let t = i as f32 / 44100.0;
                0.7 * (2.0 * std::f32::consts::PI * 60.0 * t).sin()
                    + 0.02 * (2.0 * std::f32::consts::PI * 5000.0 * t).sin()
            })
            .collect();
        crate::render::write_wav(&src, &x, &x).unwrap();
        let mut e = Engine::new(dir.clone());
        let out = dir.join("mb.wav");
        let v = e
            .call(
                "multiband_dynamics",
                &json!({"path": src.display().to_string(), "out": out.display().to_string(),
                        "bands": [{"threshold_db": -20.0, "ratio": 4.0, "knee_db": 0.0}, {"threshold_db": -20.0}, {"threshold_db": -20.0}]}),
            )
            .unwrap();
        let b = v["bands"].as_array().unwrap();
        assert_eq!(b.len(), 3);
        assert!(
            b[0]["max_gain_reduction_db"].as_f64().unwrap() > 10.0,
            "{v}"
        );
        assert!(b[2]["max_gain_reduction_db"].as_f64().unwrap() < 0.5, "{v}");
        assert!(out.exists());
        assert!(e
            .call(
                "multiband_dynamics",
                &json!({"path": src.display().to_string(), "crossovers": [2500, 150]})
            )
            .is_err());
        assert!(e
            .call(
                "multiband_dynamics",
                &json!({"path": src.display().to_string(), "bands": [{"thresh": -3}]})
            )
            .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
