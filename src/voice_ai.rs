//! Voice clean-up that has to earn its place: a neural noise remover
//! (DeepFilterNet 3, with RNNoise as the built-in fallback), corrective tone
//! matching against a clean-vocal curve (EQ only, nothing synthesised), and a
//! DNSMOS speech-quality score so every change can be checked against the
//! raw take.
//!
//! - `denoise_voice`: DeepFilterNet 3 via the project's own MIT/Apache-2.0
//!   `deep-filter` binary (BEATBOX_DEEPFILTER, `deep-filter` on PATH, or the
//!   v0.5.6 release fetched once into the cache and sha256-checked; not
//!   redistributed here), else RNNoise through the `nnnoiseless` crate
//!   (BSD-3-Clause). `atten_lim_db` caps how far the noise is pushed down
//!   (the remover's output is mixed with the take, as DeepFilterNet's own
//!   attenuation limit does) and `mix` blends with the dry take.
//! - `match_vocal_tone`: long-term spectrum of the voiced frames vs a target
//!   (a reference sample or a built-in clean close-mic vocal curve), turned
//!   into a smooth static EQ (1/3-octave bands, smoothed, at most +/-6 dB, no
//!   boost where the band is near the take's noise floor), plus a dynamic
//!   200-500 Hz cut that only acts on frames that turn boxy.
//! - `score_voice_quality`: DNSMOS P.835 (Microsoft, MIT) SIG / BAK / OVRL
//!   through `scripts/dnsmos.py` under BEATBOX_PYTHON (needs numpy and
//!   onnxruntime). "better" = SIG and OVRL both above the baseline.

use crate::dsp::SR;
use crate::tools::{b_or, f_opt, f_or, obj, s_opt, s_req, Tool};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const DFN_VERSION: &str = "0.5.6";
const DFN_URL: &str = "https://github.com/Rikorose/DeepFilterNet/releases/download/v0.5.6/deep-filter-0.5.6-x86_64-unknown-linux-musl";
const DFN_SHA256: &str = "70775e251eee44c0f2451a1e833326cf8bcbbe304d3e7cd12851e6fce72ef7da";
const DNSMOS_PY: &str = include_str!("../scripts/dnsmos.py");

fn r2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}

// ------------------------------------------------------------ sha256

/// Plain SHA-256 (FIPS 180-4), for checking downloaded helpers.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    let mut w = [0u32; 64];
    for chunk in msg.chunks(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

// ------------------------------------------------------------ helpers

fn cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("BEATBOX_CACHE") {
        return PathBuf::from(d);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cache").join("beatbox")
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// 16-bit mono WAV at any rate (helpers that want 48 kHz or 16 kHz).
pub fn write_wav_mono(path: &Path, x: &[f32], rate: u32) -> Result<()> {
    use std::io::Write;
    let data_len = (x.len() * 2) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in x {
        f.write_all(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())?;
    }
    f.flush()?;
    Ok(())
}

fn tmp_dir(tag: &str) -> Result<PathBuf> {
    let d = std::env::temp_dir().join(format!(
        "beatbox_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// Lag (in samples, 0..max) at which `y` best lines up with `x`.
pub fn best_lag(x: &[f32], y: &[f32], max: usize) -> usize {
    let n = x.len().min(y.len()).min((SR as usize) * 20);
    if n < max * 2 {
        return 0;
    }
    let step = 4;
    let mut best = (0usize, f64::MIN);
    for lag in (0..=max).step_by(step) {
        let mut s = 0.0f64;
        for i in (0..n - lag).step_by(2) {
            s += x[i] as f64 * y[i + lag] as f64;
        }
        if s > best.1 {
            best = (lag, s);
        }
    }
    let (lo, hi) = (best.0.saturating_sub(step), (best.0 + step).min(max));
    for lag in lo..=hi {
        let mut s = 0.0f64;
        for i in 0..n - lag {
            s += x[i] as f64 * y[i + lag] as f64;
        }
        if s > best.1 {
            best = (lag, s);
        }
    }
    best.0
}

// ------------------------------------------------------------ denoise

#[derive(Clone, Debug)]
pub struct DenoiseOpts {
    pub atten_lim_db: f32,
    pub mix: f32,
    pub backend: String,
}

/// Where the DeepFilterNet binary is, fetching the pinned release once on
/// x86_64 Linux. Err says why it is not available.
pub fn deepfilter_binary(allow_download: bool) -> Result<PathBuf> {
    if let Ok(p) = std::env::var("BEATBOX_DEEPFILTER") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
        bail!(
            "BEATBOX_DEEPFILTER points at {}, which is not a file",
            p.display()
        );
    }
    if let Some(p) = which("deep-filter") {
        return Ok(p);
    }
    let dest = cache_dir().join(format!("deep-filter-{DFN_VERSION}"));
    if dest.is_file() {
        return Ok(dest);
    }
    if !(cfg!(target_os = "linux") && cfg!(target_arch = "x86_64")) {
        bail!("no deep-filter binary: set BEATBOX_DEEPFILTER or put deep-filter on PATH (prebuilt fetch is x86_64 Linux only)");
    }
    if !allow_download {
        bail!("deep-filter is not cached yet and download is off");
    }
    let resp = ureq::get(DFN_URL)
        .timeout(std::time::Duration::from_secs(180))
        .call()
        .map_err(|e| anyhow!("fetching deep-filter: {e}"))?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf)?;
    let got = sha256_hex(&buf);
    if got != DFN_SHA256 {
        bail!("deep-filter download checksum mismatch ({got})");
    }
    std::fs::create_dir_all(cache_dir())?;
    let tmp = dest.with_extension("part");
    std::fs::write(&tmp, &buf)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, &dest)?;
    Ok(dest)
}

/// DeepFilterNet 3 at full strength (the attenuation limit is applied by the
/// caller, the same way for both backends). Output is at SR, aligned with x.
pub fn deepfilter_full(x: &[f32], bin: &Path) -> Result<Vec<f32>> {
    let d = tmp_dir("dfn")?;
    let pk = x.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    let g = 0.7 / pk;
    let x48: Vec<f32> = crate::resample::resample(x, SR as u32, 48_000)
        .iter()
        .map(|v| v * g)
        .collect();
    let inp = d.join("in.wav");
    write_wav_mono(&inp, &x48, 48_000)?;
    let out_dir = d.join("out");
    let res = std::process::Command::new(bin)
        .arg("-D")
        .arg("-o")
        .arg(&out_dir)
        .arg(&inp)
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("running {}", bin.display()))?;
    let outp = out_dir.join("in.wav");
    if !res.status.success() || !outp.is_file() {
        let _ = std::fs::remove_dir_all(&d);
        bail!(
            "deep-filter failed: {}",
            String::from_utf8_lossy(&res.stderr)
                .lines()
                .last()
                .unwrap_or("")
        );
    }
    let y = crate::samples::decode_file(&outp);
    let _ = std::fs::remove_dir_all(&d);
    let mut y: Vec<f32> = y?.iter().map(|v| v / g).collect();
    y.resize(x.len(), 0.0);
    Ok(y)
}

/// RNNoise (nnnoiseless) at full strength, delay-compensated, at SR.
pub fn rnnoise_full(x: &[f32]) -> Vec<f32> {
    use nnnoiseless::DenoiseState;
    let fs = DenoiseState::FRAME_SIZE;
    let x48 = crate::resample::resample(x, SR as u32, 48_000);
    let mut st = DenoiseState::new();
    let mut y48 = Vec::with_capacity(x48.len() + 2 * fs);
    let mut inb = vec![0.0f32; fs];
    let mut outb = vec![0.0f32; fs];
    let mut i = 0;
    while i < x48.len() + 2 * fs {
        for k in 0..fs {
            inb[k] = x48.get(i + k).copied().unwrap_or(0.0) * 32768.0;
        }
        st.process_frame(&mut outb, &inb);
        y48.extend(outb.iter().map(|v| v / 32768.0));
        i += fs;
    }
    let lag = best_lag(&x48, &y48, 2 * fs);
    let y48: Vec<f32> = y48[lag..].to_vec();
    let mut y = crate::resample::resample(&y48, 48_000, SR as u32);
    y.resize(x.len(), 0.0);
    y
}

/// Apply the attenuation limit and dry/wet mix: the removed part is let back
/// in at -atten_lim_db, then blended with the take by `mix`.
pub fn apply_limits(x: &[f32], full: &[f32], atten_lim_db: f32, mix: f32) -> Vec<f32> {
    let lim = if atten_lim_db >= 100.0 {
        0.0
    } else {
        10f32.powf(-atten_lim_db.max(0.0) / 20.0)
    };
    let m = mix.clamp(0.0, 1.0);
    x.iter()
        .zip(full.iter())
        .map(|(d, w)| {
            let y = w * (1.0 - lim) + d * lim;
            y * m + d * (1.0 - m)
        })
        .collect()
}

/// Run the chosen backend. Returns (output, backend used, note).
pub fn denoise(x: &[f32], o: &DenoiseOpts) -> Result<(Vec<f32>, String, Option<String>)> {
    let want = o.backend.to_lowercase();
    let mut note = None;
    if want == "auto" || want == "deepfilter" || want == "dfn" {
        match deepfilter_binary(true).and_then(|b| deepfilter_full(x, &b)) {
            Ok(full) => {
                return Ok((
                    apply_limits(x, &full, o.atten_lim_db, o.mix),
                    "deepfilternet3".into(),
                    None,
                ))
            }
            Err(e) if want == "auto" => {
                note = Some(format!("DeepFilterNet unavailable ({e}); used RNNoise"))
            }
            Err(e) => return Err(e),
        }
    } else if want != "rnnoise" {
        bail!("backend must be auto, deepfilter or rnnoise");
    }
    let full = rnnoise_full(x);
    Ok((
        apply_limits(x, &full, o.atten_lim_db, o.mix),
        "rnnoise".into(),
        note,
    ))
}

// ------------------------------------------------------------ DNSMOS

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Mos {
    pub sig: f32,
    pub bak: f32,
    pub ovrl: f32,
}

fn python() -> String {
    std::env::var("BEATBOX_PYTHON").unwrap_or_else(|_| "python3".into())
}

/// DNSMOS scores for each buffer (mono, at SR). Err when the helper or its
/// python deps are missing.
pub fn dnsmos(bufs: &[&[f32]]) -> Result<Vec<Mos>> {
    let d = tmp_dir("dnsmos")?;
    let script = d.join("dnsmos.py");
    std::fs::write(&script, DNSMOS_PY)?;
    let mut files = Vec::new();
    for (i, b) in bufs.iter().enumerate() {
        let p = d.join(format!("s{i}.wav"));
        let x16 = crate::resample::resample(b, SR as u32, 16_000);
        write_wav_mono(&p, &x16, 16_000)?;
        files.push(p);
    }
    let models = cache_dir().join("dnsmos");
    let out = std::process::Command::new(python())
        .arg(&script)
        .args(&files)
        .arg("--models")
        .arg(&models)
        .stdin(std::process::Stdio::null())
        .output();
    let _ = std::fs::remove_dir_all(&d);
    let out = out.map_err(|e| anyhow!("DNSMOS needs python (BEATBOX_PYTHON): {e}"))?;
    if !out.status.success() {
        bail!(
            "DNSMOS helper failed: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("no details")
        );
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        anyhow!(
            "DNSMOS helper said: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("nothing")
        )
    })?;
    if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
        bail!("{e}");
    }
    files
        .iter()
        .map(|f| {
            let s = &v["scores"][f.to_string_lossy().as_ref()];
            Ok(Mos {
                sig: finite_mos_score(s, "SIG")?,
                bak: finite_mos_score(s, "BAK")?,
                ovrl: finite_mos_score(s, "OVRL")?,
            })
        })
        .collect()
}

fn finite_mos_score(score: &Value, key: &str) -> Result<f32> {
    score[key]
        .as_f64()
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
        .ok_or_else(|| anyhow!("missing or non-finite DNSMOS {key} score"))
}

/// Missing requested evidence is not a pass. Explicit opt-outs stay explicit.
fn denoise_switch_decision(args: &Value, quality: &Value) -> (&'static str, bool) {
    if b_or(args, "force", false) {
        return ("forced", true);
    }
    if !b_or(args, "score", true) {
        return ("scoring_disabled", true);
    }
    match quality["better_than_raw"].as_bool() {
        Some(true) => ("score_improved", true),
        Some(false) => ("score_not_improved", false),
        None => ("score_unavailable", false),
    }
}

fn finish_denoise(
    e: &mut crate::Engine,
    args: &Value,
    info: &crate::samples::SampleInfo,
    x: &[f32],
    y: &[f32],
    mut extra: Value,
) -> Result<Value> {
    let (reason, allowed) = denoise_switch_decision(args, &extra["quality"]);
    let mut guarded = args.clone();
    // A candidate must never overwrite the source or a previous candidate file.
    let base = crate::samples::sample_name(
        &s_opt(args, "new_name").unwrap_or_else(|| format!("{}_dn", info.name)),
    );
    let mut candidate = base.clone();
    let mut suffix = 2;
    while e
        .project
        .samples
        .iter()
        .any(|s| s.name.eq_ignore_ascii_case(&candidate))
        || e.samples_dir().join(format!("{candidate}.wav")).exists()
    {
        candidate = format!("{base}_{suffix}");
        suffix += 1;
    }
    guarded["new_name"] = json!(candidate);

    if !allowed {
        if let Value::Object(m) = &mut guarded {
            m.remove("track");
            m.remove("replace_clips");
        }
    }
    extra["clip_switch_guard"] = json!({"status":reason,"replacement_allowed":allowed,"requested":b_or(args,"replace_clips",false)||s_opt(args,"track").is_some(),"note":"DNSMOS is a speech-quality estimate, not a singing-quality or taste verdict."});
    let mut out = crate::sc_strip::finish(e, &guarded, info, x, y, "dn", "denoise_voice", extra)?;
    if !allowed {
        out["warning"] = json!(if reason == "score_unavailable" {
            "Quality scoring unavailable; processed sample saved, original clips kept. Listen before choosing it. force:true or score:false explicitly bypasses this guard."
        } else {
            "Result did not improve both DNSMOS SIG and OVRL; processed sample saved, original clips kept. Listen before choosing it. force:true explicitly bypasses this guard."
        });
    }
    Ok(out)
}

fn mos_json(m: &Mos) -> Value {
    json!({"SIG": r2(m.sig), "BAK": r2(m.bak), "OVRL": r2(m.ovrl)})
}

fn better(m: &Mos, base: &Mos) -> bool {
    m.sig > base.sig && m.ovrl > base.ovrl
}

// ------------------------------------------------------------ tone matching

/// 1/3-octave band centres used for tone matching (63 Hz .. 16 kHz).
pub fn bands() -> Vec<f32> {
    (0..25).map(|i| 63.0 * 2f32.powf(i as f32 / 3.0)).collect()
}

/// Built-in target: the long-term spectrum shape of a clean close-mic vocal,
/// relative dB per band. A gentle low roll-off below ~120 Hz, a broad body
/// 150-500 Hz, then about -4.5 dB/oct above 1 kHz (close-miked sung vocals
/// sit brighter than the ~-6 dB/oct of average speech) and a softer top
/// above 10 kHz.
pub fn clean_vocal_target(f: f32) -> f32 {
    let oct = |a: f32, b: f32| (b / a).log2();
    let mut db = if f < 140.0 {
        -12.0 * oct(f, 140.0)
    } else if f < 500.0 {
        0.0
    } else if f < 1000.0 {
        -2.0 * oct(500.0, f)
    } else {
        -2.0 - 4.5 * oct(1000.0, f)
    };
    if f > 10_000.0 {
        db -= 6.0 * oct(10_000.0, f);
    }
    db
}

struct Spec {
    voiced: Vec<f32>,
    noise: Vec<f32>,
}

const N: usize = 4096;
const HOP: usize = 1024;

fn hann_sqrt() -> Vec<f32> {
    (0..N)
        .map(|i| (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos()).sqrt())
        .collect()
}

fn band_of_bins(fc: &[f32]) -> Vec<(usize, usize)> {
    let bin = |f: f32| ((f / SR * N as f32).round() as usize).clamp(1, N / 2);
    fc.iter()
        .map(|&c| {
            (
                bin(c / 2f32.powf(1.0 / 6.0)),
                bin(c * 2f32.powf(1.0 / 6.0)).max(bin(c / 2f32.powf(1.0 / 6.0)) + 1),
            )
        })
        .collect()
}

fn frame_power(x: &[f32], start: usize, win: &[f32]) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let mut re = vec![0.0f32; N];
    let mut im = vec![0.0f32; N];
    for i in 0..N {
        re[i] = x.get(start + i).copied().unwrap_or(0.0) * win[i];
    }
    crate::analysis::fft(&mut re, &mut im);
    let p = (0..=N / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect();
    (p, re, im)
}

/// Long-term band levels of the voiced frames and of the quiet frames.
fn spectrum(x: &[f32], fc: &[f32]) -> Spec {
    let win = hann_sqrt();
    let bb = band_of_bins(fc);
    let mut frames = Vec::new();
    let mut e = Vec::new();
    let mut s = 0;
    while s + N <= x.len().max(N) {
        let (p, _, _) = frame_power(x, s, &win);
        let tot: f32 = p.iter().sum();
        e.push(10.0 * tot.max(1e-20).log10());
        frames.push(
            bb.iter()
                .map(|&(a, b)| p[a..b].iter().sum::<f32>() / (b - a) as f32)
                .collect::<Vec<f32>>(),
        );
        s += HOP;
        if x.len() < N {
            break;
        }
    }
    let mut sorted = e.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let top = sorted.last().copied().unwrap_or(0.0);
    let floor = sorted.get(sorted.len() / 10).copied().unwrap_or(-200.0);
    let thr = top - 30.0f32.min((top - floor) * 0.6);
    let avg = |sel: &dyn Fn(f32) -> bool| -> Vec<f32> {
        let mut acc = vec![0.0f64; fc.len()];
        let mut n = 0;
        for (fr, &en) in frames.iter().zip(e.iter()) {
            if sel(en) {
                for (a, v) in acc.iter_mut().zip(fr) {
                    *a += *v as f64;
                }
                n += 1;
            }
        }
        acc.iter()
            .map(|a| 10.0 * ((a / n.max(1) as f64) as f32).max(1e-20).log10())
            .collect()
    };
    let voiced = avg(&|en| en >= thr);
    let noise = avg(&|en| en <= floor + 1.0);
    Spec { voiced, noise }
}

#[derive(Clone, Debug)]
pub struct ToneOpts {
    pub amount: f32,
    pub max_db: f32,
    pub dyn_max_db: f32,
    pub dyn_threshold_db: f32,
}

pub struct ToneResult {
    pub y: Vec<f32>,
    pub curve: Vec<f32>,
    pub dev_before: Vec<f32>,
    pub dyn_avg_db: f32,
    pub dyn_active: f32,
}

fn smooth3(v: &[f32]) -> Vec<f32> {
    (0..v.len())
        .map(|i| {
            let a = v[i.saturating_sub(1)];
            let c = v[(i + 1).min(v.len() - 1)];
            0.25 * a + 0.5 * v[i] + 0.25 * c
        })
        .collect()
}

/// Deviation of the voiced long-term spectrum from the target, per band,
/// level-aligned on 300 Hz - 3 kHz (positive = the take has too much).
fn deviation(meas: &[f32], target: &[f32], fc: &[f32]) -> Vec<f32> {
    let mut d: Vec<f32> = meas.iter().zip(target).map(|(m, t)| m - t).collect();
    let mut mid: Vec<f32> = d
        .iter()
        .zip(fc)
        .filter(|(_, f)| **f >= 300.0 && **f <= 3000.0)
        .map(|(v, _)| *v)
        .collect();
    mid.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let off = mid.get(mid.len() / 2).copied().unwrap_or(0.0);
    d.iter_mut().for_each(|v| *v -= off);
    d
}

/// Static corrective curve + dynamic low-mid cut, applied with an STFT
/// (sqrt-Hann, 75% overlap; the static part is a zero-phase EQ).
pub fn match_tone(x: &[f32], target: &[f32], o: &ToneOpts) -> ToneResult {
    let fc = bands();
    let sp = spectrum(x, &fc);
    let dev = deviation(&sp.voiced, target, &fc);
    let mut corr: Vec<f32> = dev.iter().map(|d| -d * o.amount).collect();
    corr = smooth3(&corr);
    for (i, c) in corr.iter_mut().enumerate() {
        let f = fc[i];
        let snr = sp.voiced[i] - sp.noise[i];
        let mut hi = o.max_db;
        if snr < 10.0 {
            hi = 0.0; // never lift a band that is mostly noise
        } else if snr < 20.0 {
            hi = hi.min((snr - 10.0) / 10.0 * o.max_db);
        }
        if f < 100.0 || f > 12_000.0 {
            hi = hi.min(1.5);
        }
        *c = c.clamp(-o.max_db, hi);
    }
    // per-bin gain, interpolated in log frequency between band centres
    let gain_db: Vec<f32> = (0..=N / 2)
        .map(|k| {
            let f = (k as f32 * SR / N as f32).max(fc[0]);
            let pos = (f / fc[0]).log2() * 3.0;
            let i = (pos.floor() as usize).min(fc.len() - 1);
            let j = (i + 1).min(fc.len() - 1);
            let t = (pos - i as f32).clamp(0.0, 1.0);
            corr[i] * (1.0 - t) + corr[j] * t
        })
        .collect();
    // dynamic cut: when the 200-500 Hz band rises above its usual relation to
    // 500-2000 Hz (the target's), it is cut by up to dyn_max_db
    let bin = |f: f32| ((f / SR * N as f32).round() as usize).clamp(1, N / 2);
    let (b200, b500, b2k) = (bin(200.0), bin(500.0), bin(2000.0));
    let taper: Vec<f32> = (0..=N / 2)
        .map(|k| {
            let f = k as f32 * SR / N as f32;
            if f < 141.0 || f > 707.0 {
                0.0
            } else if f < 200.0 {
                (f / 141.0).log2() * 2.0
            } else if f <= 500.0 {
                1.0
            } else {
                1.0 - (f / 500.0).log2() * 2.0
            }
        })
        .collect();
    // the take's own long-term relation on voiced frames (per-bin power);
    // the static EQ moves frame and average alike, so it cancels out
    let lt_ratio = {
        let lin = |lo: f32, hi: f32| {
            let v: Vec<f32> = fc
                .iter()
                .zip(&sp.voiced)
                .filter(|(f, _)| **f >= lo && **f <= hi)
                .map(|(_, d)| 10f32.powf(d / 10.0))
                .collect();
            v.iter().sum::<f32>() / v.len().max(1) as f32
        };
        10.0 * (lin(200.0, 500.0).max(1e-20) / lin(501.0, 2000.0).max(1e-20)).log10()
    };
    let win = hann_sqrt();
    let mut y = vec![0.0f32; x.len() + N];
    let mut cut = 0.0f32;
    let (mut cut_sum, mut active, mut nfr) = (0.0f32, 0usize, 0usize);
    let peak_e = {
        let mut m = 0.0f32;
        for c in x.chunks(HOP) {
            m = m.max(c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32);
        }
        m
    };
    let g_static: Vec<f32> = gain_db.iter().map(|d| 10f32.powf(d / 20.0)).collect();
    // start one frame early so the first samples get full overlap
    let mut start: isize = -((N - HOP) as isize);
    while (start as i64) < x.len() as i64 {
        let mut re = vec![0.0f32; N];
        let mut im = vec![0.0f32; N];
        let mut en = 0.0f32;
        for i in 0..N {
            let idx = start + i as isize;
            let v = if idx >= 0 {
                x.get(idx as usize).copied().unwrap_or(0.0)
            } else {
                0.0
            };
            en += v * v;
            re[i] = v * win[i];
        }
        crate::analysis::fft(&mut re, &mut im);
        let pw = |a: usize, b: usize| {
            (a..b).map(|k| re[k] * re[k] + im[k] * im[k]).sum::<f32>() / (b - a) as f32
        };
        let voiced = en / N as f32 > peak_e * 1e-3;
        let ratio = 10.0 * (pw(b200, b500).max(1e-20) / pw(b500, b2k).max(1e-20)).log10();
        let excess = ratio - lt_ratio - o.dyn_threshold_db;
        let want = if voiced {
            (excess * 0.5).clamp(0.0, o.dyn_max_db)
        } else {
            0.0
        };
        cut = if want > cut {
            cut + (want - cut) * 0.6
        } else {
            cut + (want - cut) * 0.15
        };
        nfr += 1;
        cut_sum += cut;
        if cut > 0.5 {
            active += 1;
        }
        for k in 0..=N / 2 {
            let g = g_static[k] * 10f32.powf(-cut * taper[k] / 20.0);
            re[k] *= g;
            im[k] *= g;
            if k > 0 && k < N / 2 {
                re[N - k] *= g;
                im[N - k] *= g;
            }
        }
        // inverse FFT via conjugate trick
        for v in im.iter_mut() {
            *v = -*v;
        }
        crate::analysis::fft(&mut re, &mut im);
        for i in 0..N {
            let idx = start + i as isize;
            if idx >= 0 && (idx as usize) < y.len() {
                y[idx as usize] += re[i] / N as f32 * win[i] * 0.5;
            }
        }
        start += HOP as isize;
    }
    y.truncate(x.len());
    ToneResult {
        y,
        curve: corr,
        dev_before: dev,
        dyn_avg_db: if nfr > 0 { cut_sum / nfr as f32 } else { 0.0 },
        dyn_active: active as f32 / nfr.max(1) as f32,
    }
}

fn rms_dev(dev: &[f32], fc: &[f32]) -> f32 {
    let v: Vec<f32> = dev
        .iter()
        .zip(fc)
        .filter(|(_, f)| **f >= 150.0 && **f <= 10_000.0)
        .map(|(d, _)| *d)
        .collect();
    (v.iter().map(|d| d * d).sum::<f32>() / v.len().max(1) as f32).sqrt()
}

fn band_map(fc: &[f32], v: &[f32]) -> Value {
    Value::Object(
        fc.iter()
            .zip(v)
            .map(|(f, d)| {
                (
                    if *f >= 1000.0 {
                        format!("{:.1}k", f / 1000.0)
                    } else {
                        format!("{f:.0}")
                    },
                    json!(crate::sc_strip::r1(*d)),
                )
            })
            .collect(),
    )
}

// ------------------------------------------------------------ tools

fn score_pair(x: &[f32], y: &[f32]) -> Value {
    match dnsmos(&[x, y]) {
        Ok(m) => {
            json!({"raw": mos_json(&m[0]), "processed": mos_json(&m[1]), "better_than_raw": better(&m[1], &m[0]), "rule": "better = SIG and OVRL both above the raw take (DNSMOS P.835)"})
        }
        Err(e) => json!({"unavailable": e.to_string()}),
    }
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "denoise_voice",
            description: "Neural noise removal for a vocal sample (writes a new one): DeepFilterNet 3 (falls back to RNNoise when it is not available; backend auto|deepfilter|rnnoise). atten_lim_db caps how far noise is pushed down (default 15; 100 = no cap; 12-20 keeps a phone take natural), mix blends with the dry take (default 1). Nothing is synthesised and no EQ is added. score (default true) rates raw vs result with DNSMOS (SIG voice, BAK background, OVRL overall) and only switches clips (track / replace_clips) when the result beats the raw take on SIG and OVRL, unless force or score:false. If requested scoring is unavailable or invalid, the new sample is kept but original clips are not switched. DNSMOS is a speech estimate, not a singing-quality verdict. reduce_noise is the older spectral-subtraction tool.",
            mutates: true,
            schema: || crate::sc_strip::common(json!({"atten_lim_db": {"type": "number"}, "mix": {"type": "number"}, "backend": {"type": "string", "enum": ["auto", "deepfilter", "rnnoise"]}, "score": {"type": "boolean"}, "force": {"type": "boolean"}})),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let o = DenoiseOpts { atten_lim_db: f_or(a, "atten_lim_db", 15.0).clamp(0.0, 100.0), mix: f_or(a, "mix", 1.0).clamp(0.0, 1.0), backend: s_opt(a, "backend").unwrap_or_else(|| "auto".into()) };
                let (y, used, note) = denoise(&x, &o)?;
                let sc = if b_or(a, "score", true) { score_pair(&x, &y) } else { Value::Null };
                let mut out = finish_denoise(e,a,&info,&x,&y,json!({"backend":used,"atten_lim_db":o.atten_lim_db,"mix":o.mix,"quality":sc}))?;
                if let Some(n) = note {
                    out["note"] = json!(n);
                }
                Ok(out)
            },
        },
        Tool {
            name: "match_vocal_tone",
            description: "De-muffle by correction, not synthesis (writes a new sample): measures the voice's long-term spectrum (voiced frames, 1/3-octave bands) against a clean close-mic vocal curve, or against reference (another sample, e.g. a clean vocal you like), and applies a smooth static EQ: amount (default 0.7) of the difference, smoothed, at most max_db (default 6) either way, and never lifting a band that sits near the take's noise floor (so hiss is not boosted). Plus a dynamic 200-500 Hz cut (up to dyn_max_db, default 4) that only acts on frames that turn boxy. Reports the curve, the deviation before/after and DNSMOS raw vs result (score, default true).",
            mutates: true,
            schema: || crate::sc_strip::common(json!({"reference": {"type": "string", "description": "sample whose voiced spectrum is the target (default: built-in clean vocal curve)"}, "amount": {"type": "number"}, "max_db": {"type": "number"}, "dyn_max_db": {"type": "number"}, "dyn_threshold_db": {"type": "number"}, "score": {"type": "boolean"}})),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let fc = bands();
                let (target, tname) = match s_opt(a, "reference") {
                    Some(r) => {
                        let (_, rx) = crate::tools_sound::sample_data(e, &r)?;
                        (spectrum(&rx, &fc).voiced, format!("reference sample '{r}'"))
                    }
                    None => (fc.iter().map(|f| clean_vocal_target(*f)).collect(), "built-in clean close-mic vocal curve".to_string()),
                };
                let o = ToneOpts { amount: f_or(a, "amount", 0.7).clamp(0.0, 1.0), max_db: f_or(a, "max_db", 6.0).clamp(0.0, 12.0), dyn_max_db: f_or(a, "dyn_max_db", 4.0).clamp(0.0, 12.0), dyn_threshold_db: f_or(a, "dyn_threshold_db", 3.0).clamp(0.0, 20.0) };
                let r = match_tone(&x, &target, &o);
                let after = deviation(&spectrum(&r.y, &fc).voiced, &target, &fc);
                let sc = if b_or(a, "score", true) { score_pair(&x, &r.y) } else { Value::Null };
                crate::sc_strip::finish(e, a, &info, &x, &r.y, "tone", "match_vocal_tone", json!({
                    "target": tname,
                    "eq_curve_db": band_map(&fc, &r.curve),
                    "deviation_from_target_db": {"before": band_map(&fc, &r.dev_before), "after": band_map(&fc, &after), "rms_before": crate::sc_strip::r1(rms_dev(&r.dev_before, &fc)), "rms_after": crate::sc_strip::r1(rms_dev(&after, &fc))},
                    "dynamic_low_mid_cut": {"avg_db": crate::sc_strip::r1(r.dyn_avg_db), "active_pct": crate::sc_strip::r1(r.dyn_active * 100.0)},
                    "quality": sc,
                }))
            },
        },
        Tool {
            name: "score_voice_quality",
            description: "Rate vocal samples with DNSMOS P.835 (Microsoft's MIT speech-quality model): SIG = the voice itself, BAK = background noise, OVRL = overall, each about 1-5. baseline (default: the first sample) is the reference; a sample counts as better only when SIG and OVRL both beat it. Needs python with numpy + onnxruntime (BEATBOX_PYTHON); the model is fetched once and checksum-checked. start_s / length_s score a window.",
            mutates: false,
            schema: || obj(json!({"samples": {"type": "array", "items": {"type": "string"}}, "baseline": {"type": "string"}, "start_s": {"type": "number"}, "length_s": {"type": "number"}}), &["samples"]),
            run: |e, a| {
                let names: Vec<String> = a["samples"].as_array().map(|v| v.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default();
                if names.is_empty() {
                    bail!("samples: give at least one sample name");
                }
                let base = s_opt(a, "baseline").unwrap_or_else(|| names[0].clone());
                let mut all = vec![base.clone()];
                all.extend(names.iter().filter(|n| !n.eq_ignore_ascii_case(&base)).cloned());
                let s0 = (f_or(a, "start_s", 0.0).max(0.0) * SR) as usize;
                let len = f_opt(a, "length_s").map(|l| (l.max(0.5) * SR) as usize);
                let mut bufs = Vec::new();
                for n in &all {
                    let (_, x) = crate::tools_sound::sample_data(e, n)?;
                    let a0 = s0.min(x.len());
                    let b0 = len.map(|l| (a0 + l).min(x.len())).unwrap_or(x.len());
                    bufs.push(x[a0..b0].to_vec());
                }
                let refs: Vec<&[f32]> = bufs.iter().map(|b| b.as_slice()).collect();
                let m = dnsmos(&refs)?;
                let rows: Vec<Value> = all.iter().zip(&m).map(|(n, s)| json!({"sample": n, "scores": mos_json(s), "better_than_baseline": if n == &base { Value::Null } else { json!(better(s, &m[0])) }})).collect();
                Ok(json!({"baseline": base, "results": rows, "rule": "better = SIG and OVRL both above the baseline"}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    fn voice_and_noise() -> (Vec<f32>, Vec<f32>) {
        // a "voice": harmonic bursts with gaps; plus steady broadband noise
        let n = (SR * 4.0) as usize;
        let mut seed = 12345u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        let mut v = vec![0.0f32; n];
        let mut z = vec![0.0f32; n];
        for i in 0..n {
            let t = i as f32 / SR;
            let on = (t * 2.0).fract() < 0.6;
            if on {
                let f0 = 180.0 + 20.0 * (t * 3.0).sin();
                v[i] = (1..12)
                    .map(|h| (2.0 * std::f32::consts::PI * f0 * h as f32 * t).sin() / h as f32)
                    .sum::<f32>()
                    * 0.15;
            }
            z[i] = rnd() * 0.02;
        }
        (v, z)
    }

    #[test]
    fn rnnoise_lowers_the_noise_floor_and_keeps_the_voice() {
        let (v, z) = voice_and_noise();
        let x: Vec<f32> = v.iter().zip(&z).map(|(a, b)| a + b).collect();
        let full = rnnoise_full(&x);
        let y = apply_limits(&x, &full, 15.0, 1.0);
        let (f0, p0) = crate::vocal_repair::floor_and_peak_db(&x);
        let (f1, p1) = crate::vocal_repair::floor_and_peak_db(&y);
        assert!(f1 < f0 - 6.0, "floor {f0} -> {f1}");
        assert!((p1 - p0).abs() < 4.0, "voice {p0} -> {p1}");
        // the attenuation limit holds: no deeper than ~15 dB below the input floor
        assert!(f1 > f0 - 17.0, "floor {f0} -> {f1} beyond the limit");
    }

    #[test]
    fn tone_match_lifts_a_muffled_take_and_spares_the_noise() {
        let (v, z) = voice_and_noise();
        // muffle: one-pole lowpass at ~1.5 kHz, plus boxy 300 Hz boost
        let mut m = vec![0.0f32; v.len()];
        let a = (-2.0 * std::f32::consts::PI * 1500.0 / SR).exp();
        let mut s = 0.0;
        for i in 0..v.len() {
            s = (1.0 - a) * v[i] + a * s;
            m[i] = s + z[i] * 0.05;
        }
        let fc = bands();
        let target: Vec<f32> = fc.iter().map(|f| clean_vocal_target(*f)).collect();
        let o = ToneOpts {
            amount: 0.7,
            max_db: 6.0,
            dyn_max_db: 4.0,
            dyn_threshold_db: 3.0,
        };
        let r = match_tone(&m, &target, &o);
        let before = rms_dev(&r.dev_before, &fc);
        let after = rms_dev(&deviation(&spectrum(&r.y, &fc).voiced, &target, &fc), &fc);
        assert!(after < before, "deviation {before} -> {after}");
        assert!(r.curve.iter().all(|c| c.abs() <= 6.0 + 1e-3));
        // flat target on a flat signal: unity (reconstruction check)
        let flat = vec![0.0f32; fc.len()];
        let o0 = ToneOpts {
            amount: 0.0,
            max_db: 6.0,
            dyn_max_db: 0.0,
            dyn_threshold_db: 3.0,
        };
        let r0 = match_tone(&v, &flat, &o0);
        let err: f32 = v
            .iter()
            .zip(&r0.y)
            .skip(N)
            .take(v.len() - 2 * N)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(err < 1e-3, "reconstruction error {err}");
    }
}

#[cfg(test)]
mod denoise_guard_tests {
    use super::*;
    #[test]
    fn absent_requested_score_is_not_permission_to_replace() {
        for quality in [
            Value::Null,
            json!({"unavailable":"python missing"}),
            json!({"better_than_raw":null}),
        ] {
            assert_eq!(
                denoise_switch_decision(&json!({}), &quality),
                ("score_unavailable", false)
            );
            assert_eq!(
                denoise_switch_decision(&json!({"force":true}), &quality),
                ("forced", true)
            );
            assert_eq!(
                denoise_switch_decision(&json!({"score":false}), &quality),
                ("scoring_disabled", true)
            );
        }
        assert_eq!(
            denoise_switch_decision(&json!({}), &json!({"better_than_raw":false})),
            ("score_not_improved", false)
        );
        assert_eq!(
            denoise_switch_decision(&json!({}), &json!({"better_than_raw":true})),
            ("score_improved", true)
        );
    }
    #[test]
    fn missing_null_or_float_overflow_score_is_unavailable() {
        for v in [
            json!({}),
            json!({"SIG":null}),
            json!({"SIG":1e100}),
            json!({"SIG":"4.2"}),
        ] {
            assert!(finite_mos_score(&v, "SIG").is_err());
        }
        assert!(finite_mos_score(&json!({"SIG":4.}), "OVRL").is_err());
        assert_eq!(finite_mos_score(&json!({"SIG":4.25}), "SIG").unwrap(), 4.25);
    }
    #[test]
    fn finish_guard_saves_candidate_but_keeps_original_clips_until_explicit_opt_out() {
        let mut e = crate::Engine::new(
            std::env::temp_dir().join(format!("bb233_{}", crate::creative::fresh_seed())),
        );
        e.call("new_project", &json!({"name":"guard","bpm":120}))
            .unwrap();
        e.call("add_track", &json!({"name":"voice","preset":"pad"}))
            .unwrap();
        let x = vec![0.1; 4410];
        let path = e.workdir.join("source.wav");
        crate::render::write_wav(&path, &x, &x).unwrap();
        let info = crate::samples::SampleInfo {
            name: "take".into(),
            path: path.to_string_lossy().into(),
            source: "fixture".into(),
            license: "CC0".into(),
            author: "test".into(),
            duration: 0.1,
        };
        crate::tools::register_sample(&mut e, info.clone()).unwrap();
        e.call(
            "add_audio_clip",
            &json!({"sample":"take","track":"voice","start_beat":0}),
        )
        .unwrap();
        let original = e.project.audio_clips.clone();
        let bytes = std::fs::read(&path).unwrap();
        let reserved = e.samples_dir().join("candidate.wav");
        std::fs::create_dir_all(e.samples_dir()).unwrap();
        std::fs::write(&reserved, b"preserve existing file").unwrap();

        let a = json!({"sample":"take","replace_clips":true,"new_name":"candidate"});
        let out = finish_denoise(
            &mut e,
            &a,
            &info,
            &x,
            &x,
            json!({"quality":{"unavailable":"offline"}}),
        )
        .unwrap();
        assert_eq!(out["clips_switched"], 0);
        assert_eq!(out["clip_switch_guard"]["status"], "score_unavailable");
        assert!(out["warning"]
            .as_str()
            .unwrap()
            .contains("original clips kept"));
        assert_eq!(e.project.audio_clips, original);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read(&reserved).unwrap(), b"preserve existing file");
        assert!(e.project.samples.iter().any(|s| s.name == "candidate_2"));
        let out = finish_denoise(
            &mut e,
            &json!({"sample":"take","track":"voice","new_name":"forced","force":true}),
            &info,
            &x,
            &x,
            json!({"quality":{"unavailable":"offline"}}),
        )
        .unwrap();
        assert_eq!(out["clips_switched"], 1);
        assert_eq!(out["clip_switch_guard"]["status"], "forced");
        assert_eq!(e.project.audio_clips[0].sample, "forced");
    }
    #[test]
    fn actual_denoise_dispatch_preserves_source_and_unique_candidates() {
        if std::env::var_os("BEATBOX233_CHILD").is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "actual_denoise_dispatch_preserves_source_and_unique_candidates",
                    "--nocapture",
                ])
                .env("BEATBOX233_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let root = std::path::PathBuf::from(&format!("/tmp/v233_dispatch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // This is an isolated process: score failure cannot race other tests.
        std::env::set_var("BEATBOX_PYTHON", "/nonexistent-bb233-python");
        for (label, extra, status, switched) in [
            ("guard", json!({}), "score_unavailable", 0),
            ("force", json!({"force":true}), "forced", 1),
            ("no_score", json!({"score":false}), "scoring_disabled", 1),
        ] {
            let mut e = crate::Engine::new(root.join(label));
            e.call("new_project", &json!({"name":"guard"})).unwrap();
            e.call("add_track", &json!({"name":"voice","preset":"pad"}))
                .unwrap();
            let path = e.samples_dir().join("take.wav");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let x: Vec<f32> = (0..4410).map(|i| 0.1 * (i as f32 * 0.1).sin()).collect();
            crate::render::write_wav(&path, &x, &x).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            e.call("import_sample", &json!({"path":path,"name":"take"}))
                .unwrap();
            e.call(
                "add_audio_clip",
                &json!({"sample":"take","track":"voice","start_beat":0}),
            )
            .unwrap();
            let original = e.project.audio_clips.clone();
            let mut args =
                json!({"sample":"take","new_name":"take","track":"voice","backend":"rnnoise"});
            for (k, v) in extra.as_object().unwrap() {
                args[k] = v.clone();
            }
            let r = e.call("denoise_voice", &args).unwrap();
            assert_eq!(r["clips_switched"], switched);
            assert_eq!(r["clip_switch_guard"]["status"], status);
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(r["sample"], "take_2");
            if switched == 0 {
                assert_eq!(e.project.audio_clips, original);
            } else {
                assert_eq!(e.project.audio_clips[0].sample, "take_2");
            }
            println!("{} {}", label, serde_json::to_string_pretty(&r).unwrap());
            // A second candidate must preserve the first too.
            let p = e.samples_dir().join("take_2.wav");
            let first = std::fs::read(&p).unwrap();
            let r2 = e.call("denoise_voice", &args).unwrap();
            assert_eq!(r2["sample"], "take_3");
            assert_eq!(std::fs::read(&p).unwrap(), first);
        }
    }
    #[cfg(unix)]
    #[test]
    fn scoring_helper_errors_and_invalid_payloads_fail_closed() {
        use std::os::unix::fs::PermissionsExt;
        if std::env::var_os("BEATBOX233_SCORE_CHILD").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "scoring_helper_errors_and_invalid_payloads_fail_closed",
                    "--nocapture",
                ])
                .env("BEATBOX233_SCORE_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let d = std::env::temp_dir().join(format!("bb233_score_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let helper = d.join("fake-python");
        std::env::set_var("BEATBOX_PYTHON", &helper);
        for (payload, code) in [
            (r#"{"scores":{}}"#, 0),
            (r#"{"scores":{}}"#, 1),
            ("not JSON", 0),
        ] {
            std::fs::write(
                &helper,
                format!("#!/bin/sh\nprintf '%s' '{}'\nexit {}\n", payload, code),
            )
            .unwrap();
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
            let q = score_pair(&[0.1; 441], &[0.05; 441]);
            assert!(q["unavailable"].is_string());
            assert_eq!(
                denoise_switch_decision(&json!({}), &q),
                ("score_unavailable", false)
            );
        }
        std::fs::remove_dir_all(d).unwrap();
    }
}
