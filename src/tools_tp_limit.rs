//! `true_peak_limit`: the streaming true-peak lookahead limiter
//! (`dsp::tp_limiter`) as one MCP action, on a file or the current mix.
//! Opt-in: nothing else calls it, and the project is not changed.

use crate::analysis;
use crate::dsp::tp_limiter::{limit_offline, TruePeakLimiterParams};
use crate::dsp::true_peak::true_peak_dbtp;
use crate::dsp::SR;
use crate::render::write_wav;
use crate::tools::{f_opt, obj, s_opt, Tool};
use crate::tools_sound::audio_for;
use anyhow::{bail, Result};
use serde_json::{json, Value};

fn r2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

/// The values a 16-bit write stores (what the written file measures).
fn quantized(x: &[f32]) -> Vec<f32> {
    x.iter()
        .map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16) as f32 / 32767.0)
        .collect()
}

fn run(e: &mut crate::engine::Engine, a: &Value) -> Result<Value> {
    let d = TruePeakLimiterParams::default();
    let p = TruePeakLimiterParams {
        ceiling_dbtp: f_opt(a, "ceiling_dbtp").unwrap_or(d.ceiling_dbtp),
        lookahead_ms: f_opt(a, "lookahead_ms").unwrap_or(d.lookahead_ms),
        release_fast_ms: f_opt(a, "release_fast_ms").unwrap_or(d.release_fast_ms),
        release_slow_ms: f_opt(a, "release_slow_ms").unwrap_or(d.release_slow_ms),
    };
    if !(-24.0..=0.0).contains(&p.ceiling_dbtp) {
        bail!(
            "ceiling_dbtp must be between -24 and 0 dBTP (got {})",
            p.ceiling_dbtp
        );
    }
    if !(2.0..=5.0).contains(&p.lookahead_ms) {
        bail!(
            "lookahead_ms must be between 2 and 5 (got {})",
            p.lookahead_ms
        );
    }
    if !(1.0..=1000.0).contains(&p.release_fast_ms)
        || !(p.release_fast_ms..=5000.0).contains(&p.release_slow_ms)
    {
        bail!(
            "release_fast_ms must be 1..1000 and release_slow_ms between it and 5000 (got {} / {})",
            p.release_fast_ms,
            p.release_slow_ms
        );
    }
    let (name, l, r) = audio_for(e, a)?;
    let tp_in = true_peak_dbtp(&[&l, &r]);
    let lu_in = analysis::loudness_at(&l, &r, SR).integrated_lufs;
    let mut ch = vec![l, r];
    let rep = limit_offline(&mut ch, SR, p);
    let mut v = json!({
        "source": name,
        "ceiling_dbtp": p.ceiling_dbtp,
        "lookahead_ms": p.lookahead_ms,
        "release_fast_ms": p.release_fast_ms,
        "release_slow_ms": p.release_slow_ms,
        "latency_samples": rep.latency,
        "latency_note": "the streaming limiter delays by latency_samples; this offline pass compensates it, so the output lines up with the input",
        "true_peak_before_dbtp": r2(tp_in),
        "max_gain_reduction_db": r2(rep.max_gain_reduction_db),
        "integrated_lufs_before": r2(lu_in),
        "integrated_lufs_after": r2(analysis::loudness_at(&ch[0], &ch[1], SR).integrated_lufs),
        "meter": "true peak: ITU-R BS.1770-4 Annex 2 4x interpolator plus the sample peak",
    });
    match s_opt(a, "out") {
        Some(out) => {
            let path = e.resolve(&out);
            write_wav(&path, &ch[0], &ch[1])?;
            let (ql, qr) = (quantized(&ch[0]), quantized(&ch[1]));
            v["out"] = json!(path.display().to_string());
            v["true_peak_after_dbtp"] = json!(r2(true_peak_dbtp(&[&ql, &qr])));
            v["true_peak_after_note"] = json!("measured on the 16-bit samples written to out");
        }
        None => {
            v["true_peak_after_dbtp"] = json!(r2(true_peak_dbtp(&[&ch[0], &ch[1]])));
            v["note"] = json!("measured only; pass out to write the limited audio (16-bit wav)");
        }
    }
    Ok(v)
}

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "true_peak_limit",
        description: "True-peak lookahead limiter on a file (path) or the current mix: holds the output under ceiling_dbtp (default -0.1) on the BS.1770-4 4x true-peak meter, with lookahead_ms (2..5, default 3) and a program-dependent dual release (release_fast_ms default 40 for transients, release_slow_ms default 400 for sustained limiting). Writes a 16-bit wav to out if given. Reports true peak before/after, max gain reduction, LUFS before/after and the exact latency in samples. Does not change the project.",
        mutates: false,
        schema: || {
            obj(
                json!({
                    "path": {"type": "string", "description": "Audio file to limit (default: the current mix)"},
                    "sample": {"type": "string", "description": "A registered sample to limit instead"},
                    "out": {"type": "string", "description": "Where to write the limited 16-bit wav"},
                    "ceiling_dbtp": {"type": "number", "description": "Ceiling in dBTP, -24..0 (default -0.1)"},
                    "lookahead_ms": {"type": "number", "description": "Lookahead / attack ramp in ms, 2..5 (default 3)"},
                    "release_fast_ms": {"type": "number", "description": "Fast release in ms, after transients (default 40)"},
                    "release_slow_ms": {"type": "number", "description": "Slow release in ms, after sustained limiting (default 400)"}
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
    fn true_peak_limit_holds_a_hot_file_under_the_ceiling_and_writes_it() {
        let dir = std::env::temp_dir().join("bb_tp_limit_tool");
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("hot.wav");
        // a +2 dBTP-ish file: a near full-scale fs/4 sine at 45 degrees (16-bit clamps the samples at 0.999)
        let x: Vec<f32> = (0..22050)
            .map(|i| {
                0.999
                    * (std::f32::consts::FRAC_PI_2 * i as f32 + std::f32::consts::FRAC_PI_4)
                        .sin()
                        .signum()
            })
            .collect();
        crate::render::write_wav(&src, &x, &x).unwrap();
        let mut e = Engine::new(dir.clone());
        let out = dir.join("lim.wav");
        let v = e
            .call("true_peak_limit", &json!({"path": src.display().to_string(), "out": out.display().to_string(), "ceiling_dbtp": -1.0}))
            .unwrap();
        assert!(v["true_peak_before_dbtp"].as_f64().unwrap() > 0.5, "{v}");
        let after = v["true_peak_after_dbtp"].as_f64().unwrap();
        assert!(after <= -0.9 && after >= -1.15, "{v}");
        assert_eq!(v["latency_samples"].as_u64().unwrap(), 132 + 10, "{v}");
        let (l, _r) = crate::samples::decode_stereo(&out).unwrap();
        assert_eq!(l.len(), 22050);
        assert!(e
            .call(
                "true_peak_limit",
                &json!({"path": src.display().to_string(), "ceiling_dbtp": 3.0})
            )
            .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
