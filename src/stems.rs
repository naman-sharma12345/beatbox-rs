//! Stem separation (FL's stem separator): split a finished mix or a sample
//! into drums, bass, vocals and other, so an AI can remix, re-balance or
//! sample one part. Classic signal processing, no model: harmonic/percussive
//! separation by median filtering of the spectrogram (Fitzgerald 2010), the
//! harmonic part split by frequency (bass) and by centre-panned energy in the
//! voice band (vocals). Soft masks sum to one, so the four stems add back to
//! the input. Good for loops and simple mixes; dense mixes bleed.

use crate::analysis::fft;
use crate::dsp::SR;
use crate::engine::Engine;
use crate::tools::{obj, s_opt, s_req, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::f32::consts::PI;

const N: usize = 2048;
const HOP: usize = 512;
const MED: usize = 17;

pub const STEM_NAMES: [&str; 4] = ["drums", "bass", "vocals", "other"];

fn stft(x: &[f32], win: &[f32]) -> Vec<(Vec<f32>, Vec<f32>)> {
    let frames = x.len() / HOP + 1;
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut re = vec![0.0f32; N];
        let mut im = vec![0.0f32; N];
        for k in 0..N {
            let i = (f * HOP + k) as isize - (N / 2) as isize;
            if i >= 0 && (i as usize) < x.len() {
                re[k] = x[i as usize] * win[k];
            }
        }
        fft(&mut re, &mut im);
        re.truncate(N / 2 + 1);
        im.truncate(N / 2 + 1);
        out.push((re, im));
    }
    out
}

fn istft(spec: &[(Vec<f32>, Vec<f32>)], win: &[f32], len: usize) -> Vec<f32> {
    let mut y = vec![0.0f32; len + N];
    let mut norm = vec![0.0f32; len + N];
    for (f, (re0, im0)) in spec.iter().enumerate() {
        // rebuild the full conjugate-symmetric spectrum, inverse via conj trick
        let mut re = vec![0.0f32; N];
        let mut im = vec![0.0f32; N];
        for k in 0..=N / 2 {
            re[k] = re0[k];
            im[k] = -im0[k];
            if k > 0 && k < N / 2 {
                re[N - k] = re0[k];
                im[N - k] = im0[k];
            }
        }
        fft(&mut re, &mut im);
        for k in 0..N {
            let i = (f * HOP + k) as isize - (N / 2) as isize;
            if i >= 0 && (i as usize) < len {
                y[i as usize] += re[k] / N as f32 * win[k];
                norm[i as usize] += win[k] * win[k];
            }
        }
    }
    y.truncate(len);
    for (v, n) in y.iter_mut().zip(norm.iter()) {
        if *n > 1e-6 {
            *v /= n;
        }
    }
    y
}

fn median(v: &mut [f32]) -> f32 {
    let m = v.len() / 2;
    v.select_nth_unstable_by(m, |a, b| a.partial_cmp(b).unwrap());
    v[m]
}

/// Separate a stereo signal into [drums, bass, vocals, other], each (L, R).
pub fn separate(l: &[f32], r: &[f32]) -> Vec<(Vec<f32>, Vec<f32>)> {
    let len = l.len().min(r.len());
    let win: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / N as f32).cos()).collect();
    let sl = stft(&l[..len], &win);
    let sr = stft(&r[..len], &win);
    let frames = sl.len();
    let bins = N / 2 + 1;
    // magnitude of the mid signal
    let mag: Vec<Vec<f32>> = (0..frames)
        .map(|f| (0..bins).map(|k| {
            let (a, b) = (sl[f].0[k] + sr[f].0[k], sl[f].1[k] + sr[f].1[k]);
            0.5 * (a * a + b * b).sqrt()
        }).collect())
        .collect();
    let h = MED / 2;
    let mut buf = Vec::with_capacity(MED);
    let mut masks: Vec<Vec<[f32; 4]>> = vec![vec![[0.0; 4]; bins]; frames];
    let hz = |k: usize| k as f32 * SR / N as f32;
    for f in 0..frames {
        for k in 0..bins {
            buf.clear();
            buf.extend((f.saturating_sub(h)..(f + h + 1).min(frames)).map(|g| mag[g][k]));
            let harm = median(&mut buf);
            buf.clear();
            buf.extend((k.saturating_sub(h)..(k + h + 1).min(bins)).map(|j| mag[f][j]));
            let perc = median(&mut buf);
            let (h2, p2) = (harm * harm, perc * perc);
            let pm = p2 / (h2 + p2 + 1e-12);
            let hm = 1.0 - pm;
            let fr = hz(k);
            // bass: harmonic energy under ~150 Hz, fading out by 250 Hz
            let bass = ((250.0 - fr) / 100.0).clamp(0.0, 1.0);
            // vocals: centre-panned harmonic energy in the voice band
            let (lr, li, rr, ri) = (sl[f].0[k], sl[f].1[k], sr[f].0[k], sr[f].1[k]);
            let dl = ((lr - rr).powi(2) + (li - ri).powi(2)).sqrt();
            let sm = (lr * lr + li * li).sqrt() + (rr * rr + ri * ri).sqrt() + 1e-12;
            let centre = (1.0 - dl / sm).clamp(0.0, 1.0).powi(4);
            let band = if fr < 150.0 { 0.0 } else if fr < 250.0 { (fr - 150.0) / 100.0 } else if fr < 6000.0 { 1.0 } else { ((9000.0 - fr) / 3000.0).max(0.0) };
            let voc = (1.0 - bass) * centre * band;
            masks[f][k] = [pm, hm * bass, hm * voc, hm * (1.0 - bass - voc).max(0.0)];
        }
    }
    (0..4)
        .map(|s| {
            let apply = |spec: &Vec<(Vec<f32>, Vec<f32>)>| -> Vec<(Vec<f32>, Vec<f32>)> {
                spec.iter().enumerate().map(|(f, (re, im))| {
                    (re.iter().enumerate().map(|(k, v)| v * masks[f][k][s]).collect(), im.iter().enumerate().map(|(k, v)| v * masks[f][k][s]).collect())
                }).collect()
            };
            (istft(&apply(&sl), &win, len), istft(&apply(&sr), &win, len))
        })
        .collect()
}

fn separate_stems(e: &mut Engine, a: &Value) -> Result<Value> {
    let path = e.resolve(&s_req(a, "path")?);
    if !path.exists() {
        bail!("no file at {}", path.display());
    }
    // method: auto (neural when its model is installed or fetch:true), neural, classic
    let method = s_opt(a, "method").unwrap_or_else(|| "auto".into());
    let fetch = a.get("fetch").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut fell_back: Option<String> = None;
    if method == "neural" || (method == "auto" && (fetch || model_file().exists())) {
        let dir = match s_opt(a, "out_dir") {
            Some(d) => e.resolve(&d),
            None => e.renders_dir().join(format!("{}_stems", path.file_stem().and_then(|s| s.to_str()).unwrap_or("audio"))),
        };
        std::fs::create_dir_all(&dir)?;
        let threads = a.get("threads").and_then(|v| v.as_u64()).unwrap_or(2);
        match separate_neural(e, &path, &dir, fetch, threads) {
            Ok(v) => return Ok(v),
            Err(err) if method == "neural" => return Err(err),
            Err(err) => fell_back = Some(err.to_string()),
        }
    }
    let (l, r) = crate::samples::decode_stereo(&path)?;
    if l.len() < (SR * 0.5) as usize {
        bail!("less than half a second of audio");
    }
    if l.len() > (SR * 600.0) as usize {
        bail!("over 10 minutes; trim it first (edit_sample)");
    }
    let dir = match s_opt(a, "out_dir") {
        Some(d) => e.resolve(&d),
        None => e.renders_dir().join(format!("{}_stems", path.file_stem().and_then(|s| s.to_str()).unwrap_or("audio"))),
    };
    std::fs::create_dir_all(&dir)?;
    let stems = separate(&l, &r);
    let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
    let total = rms(&l) + rms(&r) + 1e-9;
    let mut out = Vec::new();
    for (name, (sl, sr)) in STEM_NAMES.iter().zip(stems.iter()) {
        let p = dir.join(format!("{name}.wav"));
        crate::render::write_wav(&p, sl, sr)?;
        let share = (rms(sl) + rms(sr)) / total;
        out.push(json!({"stem": name, "path": p.to_string_lossy(), "level_share_pct": (share * 100.0).round()}));
    }
    Ok(json!({"stems": out, "seconds": (l.len() as f32 / SR * 100.0).round() / 100.0, "neural_unavailable": fell_back,
        "method": "harmonic/percussive median filtering + bass band + centre-panned voice band (no model); the four stems sum back to the input",
        "next": ["import_sample", "add_audio_clip"]}))
}

/// The neural separator's helper (htdemucs, ONNX, CPU): scripts/separate_stems.py.
pub const STEMS_PY: &str = include_str!("../scripts/separate_stems.py");

/// Where the separator model lives once fetched (~/.beatbox/models or $BEATBOX_MODELS).
pub fn model_file() -> std::path::PathBuf {
    let dir = std::env::var("BEATBOX_MODELS").map(std::path::PathBuf::from).unwrap_or_else(|_| {
        std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".beatbox").join("models")
    });
    dir.join("htdemucs_fp16weights.onnx")
}

/// Neural separation through the helper script; Err carries the helper's words.
fn separate_neural(e: &Engine, path: &std::path::Path, dir: &std::path::Path, fetch: bool, threads: u64) -> Result<Value> {
    let script = e.workdir.join(".beatbox_separate_stems.py");
    std::fs::write(&script, STEMS_PY)?;
    let py = std::env::var("BEATBOX_PYTHON").unwrap_or_else(|_| "python3".into());
    let mut cmd = std::process::Command::new(&py);
    cmd.arg(&script).arg(path).arg(dir).arg("--threads").arg(threads.max(1).to_string());
    if fetch {
        cmd.arg("--fetch");
    }
    let o = cmd.output().map_err(|err| anyhow::anyhow!("running {py}: {err} (set BEATBOX_PYTHON to a Python with onnxruntime)"))?;
    let txt = String::from_utf8_lossy(&o.stdout);
    let v: Value = serde_json::from_str(txt.trim()).map_err(|_| {
        anyhow::anyhow!("separator failed: {}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("no output"))
    })?;
    if let Some(err) = v.get("error").and_then(|x| x.as_str()) {
        bail!("{err}");
    }
    let mut out = Vec::new();
    for name in STEM_NAMES.iter() {
        if let Some(p) = v["stems"].get(*name).and_then(|x| x.as_str()) {
            out.push(json!({"stem": name, "path": p}));
        }
    }
    Ok(json!({"stems": out, "seconds": v["duration_s"], "took_s": v["seconds"],
        "method": "htdemucs neural separator (MIT, ONNX on CPU, 7.8 s segments)",
        "next": ["import_sample", "add_audio_clip", "transcribe"]}))
}

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "separate_stems",
        description: "Stem separator (FL): split an audio file (a mix, a loop, a sample) into drums, bass, vocals and other WAVs in out_dir (default renders/<name>_stems). Signal processing, no model: drums = percussive part, bass = harmonic below ~200 Hz, vocals = centre-panned harmonic voice band, other = the rest; the stems sum back to the input. Then import_sample / add_audio_clip a stem to remix it. method: auto (default) uses the htdemucs neural separator when its model is installed (fetch:true downloads it once, 166 MB, MIT; CPU, a few minutes per song) and the signal-processing split otherwise (dense mixes bleed); neural / classic force one.",
        mutates: false,
        schema: || obj(json!({
            "path": {"type": "string", "description": "audio file (wav, mp3, flac, ogg)"},
            "out_dir": {"type": "string"},
            "method": {"type": "string", "enum": ["auto", "neural", "classic"]},
            "fetch": {"type": "boolean", "description": "download the neural separator model if missing"},
            "threads": {"type": "integer"}
        }), &["path"]),
        run: separate_stems,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_sum_back_and_split_kick_from_tone() {
        let n = (SR * 2.0) as usize;
        // a centred 440 Hz tone (vocal-ish), a 60 Hz bass, clicks every 0.25 s (drums)
        let mut l = vec![0.0f32; n];
        for i in 0..n {
            let t = i as f32 / SR;
            l[i] = 0.2 * (2.0 * PI * 440.0 * t).sin() + 0.2 * (2.0 * PI * 60.0 * t).sin();
            if i % (SR as usize / 4) < 30 {
                l[i] += 0.6 * (1.0 - (i % (SR as usize / 4)) as f32 / 30.0);
            }
        }
        let r = l.clone();
        let s = separate(&l, &r);
        let mid = n / 4..3 * n / 4;
        // sum of stems ~ input
        let err: f32 = mid.clone().map(|i| (s.iter().map(|x| x.0[i]).sum::<f32>() - l[i]).abs()).fold(0.0, f32::max);
        assert!(err < 0.02, "stems don't sum back: {err}");
        let e = |x: &[f32]| mid.clone().map(|i| x[i] * x[i]).sum::<f32>();
        let (dr, ba, vo) = (e(&s[0].0), e(&s[1].0), e(&s[2].0));
        assert!(ba > 0.0 && vo > 0.0 && dr > 0.0);
        // the bass stem is mostly the 60 Hz tone, the vocal stem the 440 Hz tone
        let tone = |x: &[f32], f: f32| {
            let (mut c, mut si) = (0.0f32, 0.0f32);
            for i in mid.clone() {
                let t = i as f32 / SR;
                c += x[i] * (2.0 * PI * f * t).cos();
                si += x[i] * (2.0 * PI * f * t).sin();
            }
            (c * c + si * si).sqrt()
        };
        assert!(tone(&s[1].0, 60.0) > 5.0 * tone(&s[1].0, 440.0), "bass stem");
        assert!(tone(&s[2].0, 440.0) > 5.0 * tone(&s[2].0, 60.0), "vocal stem");
    }
}
