//! Delivery tools: codec-aware export (wav/flac/mp3), stems, the master
//! assistant and reference analysis/comparison.

use crate::analysis;
use crate::audio_edit;
use crate::dsp::{db_to_gain, SR};
use crate::export;
use crate::fx::{Effect, FxContext, GainFx, LimiterFx};
use crate::render::{self, RenderOptions};
use crate::samples;
use crate::tools::{b_or, f_opt, obj, s_opt, s_req, u_or, Tool};
use crate::tools_ears::{region, region_props};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct DeliverOpts {
    pub format: String,
    pub bits: u32,
    pub dither: bool,
    pub noise_shaping: bool,
    pub target_lufs: Option<f32>,
    pub ceiling_dbtp: f32,
    pub limit: bool,
    /// Most dB the soft clipper may take off the peaks before the limiter
    /// (0 = limiter only). Short peaks clipped softly cost far less
    /// loudness than the limiter's release dips, so loud sections keep
    /// their lift over quiet ones.
    pub soft_clip_db: f32,
    /// 0..1: how much of the song's own loud/quiet contrast survives the
    /// limiter (1 = all of it, 0 = whatever the limiter leaves). Quieter
    /// sections are turned down by the gain the limiter took from the loud
    /// ones, after the limiter, so no peak can rise.
    pub preserve_dynamics: f32,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    /// A fixed gain in place of the codec trim (dB steps, applied in order,
    /// one encode, no trim loop). None = today's behaviour: encode, measure
    /// the decoded true peak and trim until it meets the ceiling. A file
    /// written with a fixed gain can sit over the ceiling; the report says so.
    /// Used by export_stems' stem_gain "common" and stem_gain_db (pinned).
    pub fixed_gain_db: Option<Vec<f32>>,
    /// Opt-in exact codec trim. false (default, unchanged): each pass trims by
    /// the DECODED true peak, at most 4 trims. An integer file that clips
    /// hard decodes with its peaks cut off, so the decoded peak under-reads the
    /// signal, each trim falls short and the loop can stop over the ceiling
    /// (IWBY v5's vocal stem: +0.85 dBTP, 382 samples at full scale after 4).
    /// true: each pass trims by the higher of the decoded true peak and the
    /// true peak of the signal about to be encoded, for up to 12 passes, so
    /// the file meets the ceiling unclipped (normally one trim).
    pub exact_trim: bool,
}

impl Default for DeliverOpts {
    fn default() -> Self {
        DeliverOpts {
            format: "wav".into(),
            bits: 24,
            dither: true,
            noise_shaping: false,
            target_lufs: None,
            ceiling_dbtp: -1.0,
            limit: true,
            soft_clip_db: 3.0,
            preserve_dynamics: 0.8,
            bitrate_kbps: 320,
            sample_rate: 44_100,
            fixed_gain_db: None,
            exact_trim: false,
        }
    }
}

pub fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn scale(l: &mut [f32], r: &mut [f32], db: f32) {
    let g = db_to_gain(db);
    for v in l.iter_mut().chain(r.iter_mut()) {
        *v *= g;
    }
}

fn tp_db(l: &[f32], r: &[f32]) -> f32 {
    crate::dsp::gain_to_db(analysis::true_peak(l).max(analysis::true_peak(r)))
}

/// The delivered file decoded back at its own sample rate: exactly the
/// frames a listener gets, at the rate the file declares.
pub struct DecodedOutput {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub sample_rate: u32,
}

impl DecodedOutput {
    pub fn read(path: &Path) -> Result<Self> {
        let (left, right, sample_rate) = samples::decode_stereo_native(path)?;
        Ok(DecodedOutput {
            left,
            right,
            sample_rate,
        })
    }
    /// Duration from the decoded frame count and the decoded rate.
    pub fn seconds(&self) -> f32 {
        self.left.len().min(self.right.len()) as f32 / self.sample_rate.max(1) as f32
    }
    /// True peak of the decoded file, measured at its own rate (the 4x
    /// oversampling is relative to the file's rate).
    pub fn true_peak_db(&self) -> f32 {
        tp_db(&self.left, &self.right)
    }
    /// BS.1770 loudness with K-weighting and gating windows for the file's rate.
    pub fn loudness(&self) -> analysis::Loudness {
        analysis::loudness_at(&self.left, &self.right, self.sample_rate as f32)
    }
}

/// Encode once to `path` and return the decoded file (what a listener gets).
fn encode_to(path: &Path, l: &[f32], r: &[f32], o: &DeliverOpts) -> Result<DecodedOutput> {
    if let Some(d) = path.parent() {
        if !d.as_os_str().is_empty() {
            std::fs::create_dir_all(d)?;
        }
    }
    let dither = o.dither && o.bits < 32;
    match o.format.as_str() {
        "wav" => std::fs::write(
            path,
            export::wav(l, r, o.sample_rate, o.bits, dither, o.noise_shaping)?,
        )?,
        "flac" => std::fs::write(
            path,
            export::flac(l, r, o.sample_rate, o.bits.min(24), dither, o.noise_shaping)?,
        )?,
        "mp3" => {
            if !ffmpeg_available() {
                bail!("mp3 export needs ffmpeg on PATH; export wav or flac instead");
            }
            let tmp = path.with_extension("tmp.wav");
            std::fs::write(&tmp, export::wav(l, r, o.sample_rate, 32, false, false)?)?;
            let out = std::process::Command::new("ffmpeg")
                .args(["-y", "-loglevel", "error", "-i"])
                .arg(&tmp)
                .args([
                    "-codec:a",
                    "libmp3lame",
                    "-b:a",
                    &format!("{}k", o.bitrate_kbps),
                ])
                .arg(path)
                .output()
                .context("running ffmpeg")?;
            let _ = std::fs::remove_file(&tmp);
            if !out.status.success() {
                bail!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr));
            }
        }
        f => bail!("unknown format '{f}' (wav, flac, mp3)"),
    }
    // always decode the real file, whatever its rate: codec overshoot (MP3)
    // and any rate the encoder chose are what QC has to see
    DecodedOutput::read(path)
}

/// Soft clipper: linear below 1/2 of `ceiling` (-6 dB), a smooth tanh knee
/// above, never past `ceiling`. Applied per sample to both channels.
pub fn soft_clip(l: &mut [f32], r: &mut [f32], ceiling: f32) {
    let c = ceiling.max(1e-6);
    let k = 0.5 * c;
    let span = c - k;
    for v in l.iter_mut().chain(r.iter_mut()) {
        let a = v.abs();
        if a > k {
            *v = v.signum() * (k + span * ((a - k) / span).tanh());
        }
    }
}

/// After the limiter: turn the quieter stretches down by the extra gain the
/// limiter took from the loudest ones, so the song keeps `amount` (0..1) of
/// its own contrast. Only ever lowers gain, so peaks stay under the ceiling.
/// `l`/`r` is the input before `gain_db`; `wl`/`wr` the limited output.
pub fn restore_contour(
    l: &[f32],
    r: &[f32],
    gain_db: f32,
    wl: &mut [f32],
    wr: &mut [f32],
    amount: f32,
) {
    let win = (2.0 * SR) as usize;
    let hop = (0.5 * SR) as usize;
    let n = wl.len().min(l.len());
    if n < 2 * win {
        return;
    }
    let g2 = crate::dsp::db_to_gain(gain_db).powi(2);
    let energy = |a: &[f32], b: &[f32]| {
        a.iter().zip(b).map(|(x, y)| x * x + y * y).sum::<f32>() / (2 * a.len()).max(1) as f32
    };
    // limiter loss per window (dB), only where there is music
    let mut centers = Vec::new();
    let mut loss = Vec::new();
    let mut start = 0;
    while start + win <= n {
        let ein = energy(&l[start..start + win], &r[start..start + win]) * g2;
        let eout = energy(&wl[start..start + win], &wr[start..start + win]);
        centers.push(start + win / 2);
        loss.push(if ein > 1e-7 && eout > 1e-9 {
            Some(10.0 * (ein / eout).log10())
        } else {
            None
        });
        start += hop;
    }
    let mut known: Vec<f32> = loss.iter().flatten().copied().collect();
    if known.len() < 4 {
        return;
    }
    known.sort_by(|a, b| a.partial_cmp(b).unwrap());
    // the loud sections' loss (90th percentile, robust to one spike)
    let top = known[(known.len() * 9 / 10).min(known.len() - 1)];
    let corr_db: Vec<f32> = loss
        .iter()
        .map(|x| x.map(|d| -(amount * (top - d)).max(0.0)).unwrap_or(0.0))
        .collect();
    // smooth the window curve (3 taps either side), then per-sample linear interpolation
    let sm: Vec<f32> = (0..corr_db.len())
        .map(|i| {
            let lo = i.saturating_sub(2);
            let hi = (i + 3).min(corr_db.len());
            corr_db[lo..hi].iter().sum::<f32>() / (hi - lo) as f32
        })
        .collect();
    let mut k = 0;
    for i in 0..n {
        while k + 1 < centers.len() && centers[k + 1] <= i {
            k += 1;
        }
        let db = if i <= centers[0] {
            sm[0]
        } else if k + 1 >= centers.len() {
            sm[centers.len() - 1]
        } else {
            let t = (i - centers[k]) as f32 / (centers[k + 1] - centers[k]) as f32;
            sm[k] + (sm[k + 1] - sm[k]) * t
        };
        if db < -0.01 {
            let g = crate::dsp::db_to_gain(db);
            wl[i] *= g;
            wr[i] *= g;
        }
    }
}

/// Short-term loudness lift of the loudest quarter over the quietest
/// quarter of the 3 s windows (dB): how much louder the drops are than the
/// verses. Used to show whether delivery kept the song's dynamics.
pub fn section_lift_db(l: &[f32], r: &[f32]) -> f32 {
    let w = (3.0 * SR) as usize;
    if l.len() < 4 * w {
        return 0.0;
    }
    let mut lv: Vec<f32> = l
        .chunks(w)
        .zip(r.chunks(w))
        .filter(|(a, _)| a.len() == w)
        .map(|(a, b)| {
            let e: f32 = a.iter().zip(b).map(|(x, y)| x * x + y * y).sum::<f32>() / (2 * w) as f32;
            10.0 * e.max(1e-12).log10()
        })
        .filter(|d| *d > -60.0)
        .collect();
    if lv.len() < 4 {
        return 0.0;
    }
    lv.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = lv.len() / 4;
    let lo: f32 = lv[..q.max(1)].iter().sum::<f32>() / q.max(1) as f32;
    let hi: f32 = lv[lv.len() - q.max(1)..].iter().sum::<f32>() / q.max(1) as f32;
    ((hi - lo) * 100.0).round() / 100.0
}

/// Codec-aware delivery: loudness-normalise to a target, true-peak limit,
/// encode, re-measure the decoded file and trim gain until the decoded true
/// peak meets the ceiling.
/// The limiter's ceiling under the delivery ceiling (dB). The encode's own
/// overshoot is caught by the codec trim after it, so only a small margin
/// is kept here: at 0.5 a 128k mp3 decoded 0.8 dB under the ceiling (B24:
/// -2.06 dBTP against -1.2), loudness left on the table.
pub const LIMITER_UNDER_CEILING_DB: f32 = 0.25;
/// Most trims exact_trim takes before it gives up (and says so).
pub const EXACT_TRIM_PASSES: usize = 12;
/// Most common trims export_stems' tp_trim takes before it gives up (and says so).
pub const TP_TRIM_PASSES: usize = 12;

/// What export_stems' opt-in tp_trim "common" did.
#[derive(Clone, Debug)]
pub struct TpTrim {
    /// The common trim steps (dB), appended to every stem's own gain.
    pub steps: Vec<f32>,
    /// Each stem's true peak (dBTP) read back from its file before any trim.
    pub before: Vec<f32>,
    /// Each stem's true peak read back from its file after the last trim.
    pub after: Vec<f32>,
    /// How many times every stem was written and read back.
    pub measures: usize,
    /// false: it gave up after `TP_TRIM_PASSES` trims with the hottest stem
    /// still over the ceiling.
    pub converged: bool,
}

/// One trim for every stem, set by the hottest one. `measure(steps)` writes
/// every stem with `steps` appended to its own gain and returns each stem's
/// true peak read back from the written file. Each trim aims 0.1 dB under
/// `ceiling` (the meter is 4x oversampled and can under-read a peak close to
/// Nyquist); it stops once the hottest stem is at or under `ceiling`, and
/// gives up after `max_trims` trims (converged false, never an endless loop).
/// Every stem gets the same steps, so the balance between stems is unchanged.
pub fn common_tp_trim<F: FnMut(&[f32]) -> Result<Vec<f32>>>(
    ceiling: f32,
    max_trims: usize,
    mut measure: F,
) -> Result<TpTrim> {
    let mut steps: Vec<f32> = Vec::new();
    let before = measure(&steps)?;
    let mut now = before.clone();
    let mut measures = 1;
    loop {
        let hottest = now.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        if hottest <= ceiling {
            return Ok(TpTrim {
                steps,
                before,
                after: now,
                measures,
                converged: true,
            });
        }
        if steps.len() >= max_trims {
            return Ok(TpTrim {
                steps,
                before,
                after: now,
                measures,
                converged: false,
            });
        }
        steps.push(-(hottest - ceiling + 0.1));
        now = measure(&steps)?;
        measures += 1;
    }
}

/// Samples of an integer file at its full-scale code (hard clipped), per
/// channel sample; None for float and lossy files, which do not clip there.
fn clipped_samples(d: &DecodedOutput, o: &DeliverOpts) -> Option<u64> {
    if o.format == "mp3" || o.format == "wav" && o.bits == 32 {
        return None;
    }
    let bits = if o.format == "flac" {
        o.bits.min(24)
    } else {
        o.bits
    };
    // the largest positive code is 1 LSB under 1.0; count both rails
    let top = 1.0 - 1.5 / (1u64 << (bits - 1)) as f32;
    Some(
        d.left
            .iter()
            .chain(d.right.iter())
            .filter(|x| x.abs() >= top)
            .count() as u64,
    )
}

pub fn deliver(l: &[f32], r: &[f32], path: &Path, o: &DeliverOpts) -> Result<Value> {
    let before = analysis::loudness(l, r);
    let lift_before = section_lift_db(l, r);
    let mut steps = Vec::new();
    let trig = HashMap::new();
    let ctx = FxContext {
        step_secs: 0.125,
        triggers: &trig,
        offset: 0,
        levels: None,
    };
    let limiter = Effect::Limiter(LimiterFx {
        ceiling_db: o.ceiling_dbtp - LIMITER_UNDER_CEILING_DB,
        release_ms: 80.0,
        true_peak: true,
        ..Default::default()
    });
    // gain -> (limit) -> measure, repeated so the limiter's loudness loss is made up
    let mut gain = o
        .target_lufs
        .map(|t| t - before.integrated_lufs)
        .unwrap_or(0.0);
    let (mut l, mut r) = (l.to_vec(), r.to_vec());
    // keeping the contrast turns the quiet parts down, so the loudness
    // target takes more passes and bigger steps to reach
    let preserving = o.preserve_dynamics > 0.0 && o.limit;
    let passes = if preserving { 10 } else { 4 };
    for pass in 0..passes {
        let (mut wl, mut wr) = (l.clone(), r.clone());
        scale(&mut wl, &mut wr, gain);
        let peak = tp_db(&wl, &wr);
        let limited = o.limit && peak > o.ceiling_dbtp - 0.3;
        if limited {
            // soft clip the top (up to soft_clip_db) first, then the limiter
            // catches the rest and the inter-sample peaks
            let over = peak - (o.ceiling_dbtp - LIMITER_UNDER_CEILING_DB);
            let take = over.min(o.soft_clip_db.max(0.0));
            if take > 0.05 {
                soft_clip(&mut wl, &mut wr, crate::dsp::db_to_gain(peak - take));
            }
            limiter.process(&mut wl, &mut wr, &ctx);
            if o.preserve_dynamics > 0.0 {
                restore_contour(&l, &r, gain, &mut wl, &mut wr, o.preserve_dynamics);
            }
        }
        let now = analysis::loudness(&wl, &wr).integrated_lufs;
        steps.push(json!({"step": "gain_stage", "pass": pass + 1, "gain_db": (gain * 100.0).round() / 100.0, "limited": limited, "lufs": now}));
        let err = o.target_lufs.map(|t| t - now).unwrap_or(0.0);
        if err.abs() <= 0.3 || pass == passes - 1 {
            l = wl;
            r = wr;
            break;
        }
        gain += err
            * if limited && preserving {
                1.8
            } else if limited {
                1.2
            } else {
                1.0
            };
    }
    if o.sample_rate != SR as u32 {
        l = crate::resample::resample(&l, SR as u32, o.sample_rate);
        r = crate::resample::resample(&r, SR as u32, o.sample_rate);
        steps.push(json!({"step": "resample", "to_hz": o.sample_rate}));
    }
    let mut trim_total = 0.0f32;
    // every gain step applied after the gain stage, exactly as applied (f32):
    // fed back as a fixed gain they rebuild this file bit for bit
    let mut gain_steps: Vec<f32> = Vec::new();
    if let Some(fixed) = &o.fixed_gain_db {
        // a fixed gain: the level does not depend on this signal's own peak
        for &g in fixed {
            if g != 0.0 {
                scale(&mut l, &mut r, g);
            }
            gain_steps.push(g);
        }
        let decoded_tp = encode_to(path, &l, &r, o)?.true_peak_db();
        let total: f32 = fixed.iter().sum();
        steps.push(json!({"step": "fixed_gain", "gain_db": (total * 100.0).round() / 100.0, "decoded_true_peak": decoded_tp}));
    } else {
        // the file on disk is always the last encode: never trim without
        // re-encoding (the final pass only measures)
        let last = if o.exact_trim { EXACT_TRIM_PASSES } else { 4 };
        for pass in 0..=last {
            let decoded_tp = encode_to(path, &l, &r, o)?.true_peak_db();
            // exact: the signal's own true peak as well, which a clipped
            // integer decode under-reads (the default reads the decode only)
            let level = if o.exact_trim {
                decoded_tp.max(tp_db(&l, &r))
            } else {
                decoded_tp
            };
            if level <= o.ceiling_dbtp
                || o.format == "wav" && o.bits == 32 && pass > 0
                || pass == last
            {
                break;
            }
            let trim = -(level - o.ceiling_dbtp + 0.1);
            scale(&mut l, &mut r, trim);
            trim_total += trim;
            gain_steps.push(trim);
            steps.push(json!({"step": "codec_trim", "pass": pass + 1, "decoded_true_peak": decoded_tp, "trim_db": (trim * 100.0).round() / 100.0}));
        }
    }
    // final QC on the file as written: decoded at its own rate, never the
    // pre-encode buffer and never assuming the engine rate
    let dec = DecodedOutput::read(path)?;
    let decoded_tp = dec.true_peak_db();
    let clipped = clipped_samples(&dec, o);
    let after = dec.loudness();
    let over = decoded_tp > o.ceiling_dbtp + 0.05;
    let trims = gain_steps.len();
    let mut warnings: Vec<String> = Vec::new();
    if over && o.fixed_gain_db.is_none() {
        warnings.push(format!(
            "codec trim gave up after {trims} trim(s): the file is at {decoded_tp:+.2} dBTP, over the {:+.1} dBTP ceiling{}",
            o.ceiling_dbtp,
            if o.exact_trim { "" } else { " (exact_trim:true trims by the signal's own peak until it meets it)" }
        ));
    }
    if let Some(c) = clipped.filter(|c| *c > 0) {
        warnings.push(format!(
            "{c} samples sit at {}-bit full scale (hard clipped){}",
            if o.format == "flac" {
                o.bits.min(24)
            } else {
                o.bits
            },
            if o.fixed_gain_db.is_some() {
                ": the fixed gain is too hot for an integer file (32-bit float wav cannot clip)"
            } else {
                ""
            }
        ));
    }
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let lufs_ok = o
        .target_lufs
        .map(|t| (after.integrated_lufs - t).abs() <= 1.0)
        .unwrap_or(true);
    Ok(json!({
        "path": path, "format": o.format, "bits": if o.format == "mp3" { Value::Null } else { json!(o.bits) },
        "bitrate_kbps": if o.format == "mp3" { json!(o.bitrate_kbps) } else { Value::Null },
        "sample_rate": o.sample_rate, "dither": o.dither && o.bits < 32 && o.format != "mp3",
        "bytes": bytes, "seconds": (dec.seconds() * 100.0).round() / 100.0,
        "decoded": {"sample_rate": dec.sample_rate, "frames": dec.left.len().min(dec.right.len())},
        "before": {"integrated_lufs": before.integrated_lufs, "true_peak_dbtp": before.true_peak_dbtp},
        "after": {"integrated_lufs": after.integrated_lufs, "true_peak_dbtp": (decoded_tp * 100.0).round() / 100.0, "short_term_max_lufs": after.short_term_max_lufs, "loudness_range_lu": after.loudness_range_lu},
        "codec_trim_db": (trim_total * 100.0).round() / 100.0,
        "fixed_gain": o.fixed_gain_db.is_some(),
        "gain_steps_db": gain_steps.iter().map(|g| *g as f64).collect::<Vec<f64>>(),
        // the same steps as exact f32 text (answers round numbers to 4 decimals)
        "gain_steps_exact": gain_steps.iter().map(|g| g.to_string()).collect::<Vec<String>>(),
        "over_ceiling": over,
        "exact_trim": o.exact_trim,
        "clipped_samples": clipped,
        "warnings": warnings,
        "section_lift_db": {"before": lift_before, "after": if o.sample_rate == SR as u32 { section_lift_db(&l, &r) } else { lift_before }, "note": "loudest vs quietest quarter of 3 s windows"},
        "dynamics_note": match o.target_lufs {
            Some(t) if preserving && after.integrated_lufs < t - 0.5 => Value::from(format!(
                "Stopped {:.1} dB under the target to keep the loud sections louder than the quiet ones (preserve_dynamics {:.1}); preserve_dynamics 0 gets closer to the target but flattens them.",
                t - after.integrated_lufs, o.preserve_dynamics)),
            _ => Value::Null,
        },
        "steps": steps,
        "passed": decoded_tp <= o.ceiling_dbtp + 0.05 && lufs_ok,
    }))
}

fn opts_from(a: &Value, default_fmt: &str) -> Result<DeliverOpts> {
    let d = DeliverOpts::default();
    let format = s_opt(a, "format")
        .unwrap_or_else(|| default_fmt.to_string())
        .to_lowercase();
    let bits = u_or(a, "bits", if format == "flac" { 24 } else { d.bits as u64 }) as u32;
    let sr = u_or(a, "sample_rate", 44_100) as u32;
    if !(8_000..=192_000).contains(&sr) {
        bail!("sample_rate must be 8000..192000");
    }
    Ok(DeliverOpts {
        format,
        bits,
        dither: b_or(a, "dither", bits <= 24),
        noise_shaping: b_or(a, "noise_shaping", false),
        target_lufs: f_opt(a, "target_lufs"),
        ceiling_dbtp: f_opt(a, "true_peak_ceiling").unwrap_or(d.ceiling_dbtp),
        limit: b_or(a, "limit", true),
        soft_clip_db: f_opt(a, "soft_clip_db")
            .unwrap_or(d.soft_clip_db)
            .clamp(0.0, 6.0),
        preserve_dynamics: f_opt(a, "preserve_dynamics")
            .unwrap_or(d.preserve_dynamics)
            .clamp(0.0, 1.0),
        bitrate_kbps: (u_or(a, "bitrate_kbps", 320) as u32).clamp(64, 320),
        sample_rate: sr,
        fixed_gain_db: None,
        exact_trim: b_or(a, "exact_trim", false),
    })
}

/// export_stems' stem_gain_db: stem name (lowercased) -> the dB steps to
/// apply in order (one number is one step). A step is a number or a numeric
/// string: tool answers round numbers to 4 decimals, so the report's
/// stem_gain_db_pin carries each step as the exact f32 in text, and fed back
/// it rebuilds the same files bit for bit.
fn stem_gain_pins(v: Option<&Value>) -> Result<HashMap<String, Vec<f32>>> {
    let mut m = HashMap::new();
    let Some(v) = v else { return Ok(m) };
    if v.is_null() {
        return Ok(m);
    }
    let obj = v.as_object().ok_or_else(|| {
        anyhow!("stem_gain_db must be an object: stem name -> dB (or a list of dB steps)")
    })?;
    let step = |k: &str, x: &Value| -> Result<f32> {
        match x {
            Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0) as f32),
            Value::String(t) => t
                .trim()
                .parse::<f32>()
                .map_err(|_| anyhow!("stem_gain_db[{k:?}]: {t:?} is not a number")),
            _ => bail!("stem_gain_db[{k:?}]: steps must be numbers"),
        }
    };
    for (k, g) in obj {
        let steps: Vec<f32> = match g {
            Value::Array(xs) => xs.iter().map(|x| step(k, x)).collect::<Result<_>>()?,
            x => vec![step(k, x)?],
        };
        if steps.iter().any(|s| !s.is_finite() || s.abs() > 60.0) {
            bail!("stem_gain_db[{k:?}]: each step must be within -60..60 dB");
        }
        m.insert(k.to_lowercase(), steps);
    }
    Ok(m)
}

fn ext(fmt: &str) -> &str {
    match fmt {
        "flac" => "flac",
        "mp3" => "mp3",
        _ => "wav",
    }
}

fn delivery_props() -> Value {
    json!({
        "format": {"type": "string", "enum": ["wav", "flac", "mp3"], "description": "default wav"},
        "bits": {"type": "integer", "enum": [16, 24, 32], "description": "wav 16/24/32-float, flac 16/24 (default 24)"},
        "dither": {"type": "boolean", "description": "TPDF dither when reducing to 16/24 bit (default true)"},
        "noise_shaping": {"type": "boolean", "description": "first-order noise-shaped dither (default false)"},
        "sample_rate": {"type": "integer", "description": "44100 (default) or e.g. 48000 (windowed-sinc SRC)"},
        "bitrate_kbps": {"type": "integer", "description": "mp3 bitrate (default 320)"},
        "exact_trim": {"type": "boolean", "description": "Opt-in (default false, unchanged): trim by the higher of the decoded true peak and the signal's own true peak, up to 12 passes, so a hot signal written to an integer file meets the ceiling instead of clipping (the default trims by the decoded peak only, which a hard-clipped decode under-reads, and gives up after 4 trims). A trim that gives up is named in the report's warnings either way"},
    })
}

fn merge_props(mut a: Value, b: Value) -> Value {
    if let (Value::Object(x), Value::Object(y)) = (&mut a, b) {
        x.extend(y);
    }
    a
}

/// Index of the last limiter in a chain.
fn limiter_pos(fx: &[Effect]) -> Option<usize> {
    fx.iter().rposition(|e| matches!(e, Effect::Limiter(_)))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "export_audio",
            description: "Deliver the song (or a section/window): wav (16/24-bit int or 32-bit float), flac (16/24, built-in encoder) or mp3 (ffmpeg/libmp3lame). Optional target_lufs loudness normalisation and a true-peak limiter; the file is then ENCODED, DECODED and RE-MEASURED, and the gain is trimmed until the decoded true peak meets the ceiling (MP3 overshoot included). TPDF dither on bit-depth reduction. Returns before/after LUFS and dBTP, codec trim and pass/fail.",
            mutates: false,
            schema: || obj(merge_props(merge_props(delivery_props(), region_props()), json!({
                "path": {"type": "string", "description": "Output path (default renders/<project>.<ext>)"},
                "target_lufs": {"type": "number", "description": "Normalise integrated loudness to this (e.g. -14 streaming, -9 club). Omit to keep the mix level"},
                "true_peak_ceiling": {"type": "number", "description": "dBTP ceiling after encoding (default -1)"},
                "limit": {"type": "boolean", "description": "Apply a true-peak limiter when the gain would exceed the ceiling (default true)"},
                "preserve_dynamics": {"type": "number", "minimum": 0, "maximum": 1, "description": "How much of the loud/quiet section contrast survives limiting (default 0.8; 0 = limiter only)"},
                "soft_clip_db": {"type": "number", "description": "Most dB a soft clipper takes off the peaks before the limiter (default 3, 0 = limiter only); keeps drops louder than verses at loud targets"},
                "tail_s": {"type": "number"},
            })), &[]),
            run: |e, a| {
                let o = opts_from(a, "wav")?;
                let m = e.mix()?;
                let p = e.project.clone();
                let (s0, s1, label) = region(&p, a, m.left.len().min(m.right.len()))?;
                let path = match s_opt(a, "path") {
                    Some(x) => e.resolve(&x),
                    None => e.renders_dir().join(format!("{}.{}", samples::sample_name(&p.name), ext(&o.format))),
                };
                let mut v = deliver(&m.left[s0..s1], &m.right[s0..s1], &path, &o)?;
                v["window"] = json!(label);
                Ok(v)
            },
        },
        Tool {
            name: "export_stems",
            description: "Export every track AND every bus/return (reverb, delay, drum group) as its own file, all the same length and aligned at 0. mode pre_master (default): post-fader, before the master chain; the track stems routed straight to master plus the bus stems SUM BACK to the pre-master mix, and the residual is reported. mode post_master: each stem runs through the render's own master section (fader + automation, DC blocker, master chain, sidechain keys); a linear master nulls within -60 dB, a compressor/limiter cannot (it hears each stem alone; reported). Formats wav/flac/mp3 with dither; no loudness normalisation. Stem level (stem_gain): trim (default) = each stem on its own is trimmed until its decoded true peak meets 0 dBTP, so a stem's level depends on its own peak (lowering a stem's peaks, e.g. by de-essing, makes it louder in the file); common = ONE gain for every stem (the trim the hottest stem needs), so the balance between stems is exactly the mix's; stem_gain_db pins a stem's gain (dB, or a list of dB steps), so processing never moves it. Every stem reports gain_steps_db, and the report's stem_gain_db_pin (each step as exact text) is the map that rebuilds these exact files.",
            mutates: false,
            schema: || obj(merge_props(delivery_props(), json!({
                "dir": {"type": "string", "description": "Output folder (default renders/<project>_stems)"},
                "mode": {"type": "string", "enum": ["pre_master", "post_master"], "description": "default pre_master"},
                "include_buses": {"type": "boolean", "description": "Also export bus/return outputs (default true)"},
                "tracks": {"type": "array", "items": {"type": "string"}, "description": "Only these tracks/buses"},
                "stem_gain": {"type": "string", "enum": ["trim", "common"], "description": "trim (default, as before): each stem trimmed to 0 dBTP on its own, so its level follows its own true peak; common: one gain for all stems (the hottest stem's trim), balance exactly as mixed"},
                "stem_format": {"type": "string", "enum": ["int", "f32"], "description": "int (default, unchanged): wav/flac at bits (default 24-bit, dithered); f32: 32-bit float wav, no dither, so a stem above 0 dBFS (a pinned gain, a hot bus) is carried exactly and never clipped. Each stem reports clipped_samples (integer files) and warnings"},
                "stem_gain_db": {"type": "object", "description": "Pinned gain per stem name (track or bus name as in the project; dB as a number or numeric string, or a list of dB steps applied in order). A pinned stem is not trimmed: its level no longer depends on its own peak and the file may exceed 0 dBTP (reported as over_ceiling). Feed back a report's stem_gain_db_pin to rebuild the same files bit for bit. Overrides stem_gain for the stems it names"},
                "tp_trim": {"type": "string", "enum": ["common"], "description": "Opt-in, stem_format f32 only (absent: unchanged). common: after each stem's own gain (pinned, common or as mixed), ONE extra trim for every stem, set by the hottest stem's true peak read back from its written file, aiming 0.1 dB under the 0 dBTP ceiling; repeated until the hottest stem meets 0 dBTP, giving up with a warning after 12 trims. The balance between stems stays exactly as it was (every stem gets the same steps). The report's tp_trim block has each trim, the passes and the hottest stem before/after; each stem reports true_peak_before_dbtp, and stem_gain_db_pin rebuilds the trimmed files bit for bit"},
            })), &[]),
            run: |e, a| {
                let mut o = opts_from(a, "wav")?;
                o.limit = false;
                o.target_lufs = None;
                o.ceiling_dbtp = 0.0;
                let mode = s_opt(a, "mode").unwrap_or_else(|| "pre_master".into());
                if mode != "pre_master" && mode != "post_master" { bail!("mode must be pre_master or post_master"); }
                // stem_format f32: 32-bit float wav, so a stem over 0 dBFS (a pinned
                // gain, or one the trim leaves hot) is carried, never clipped
                let stem_format = s_opt(a, "stem_format").unwrap_or_else(|| "int".into()).to_lowercase();
                match stem_format.as_str() {
                    "int" => {}
                    "f32" => {
                        if o.format != "wav" { bail!("stem_format f32 writes 32-bit float wav; format {:?} cannot carry it", o.format); }
                        if a.get("bits").and_then(|b| b.as_u64()).is_some_and(|b| b != 32) { bail!("stem_format f32 is 32-bit: drop bits or set it to 32"); }
                        o.bits = 32;
                        o.dither = false;
                    }
                    _ => bail!("stem_format must be int or f32"),
                }
                let stem_gain = s_opt(a, "stem_gain").unwrap_or_else(|| "trim".into()).to_lowercase();
                if stem_gain != "trim" && stem_gain != "common" { bail!("stem_gain must be trim or common"); }
                let pinned = stem_gain_pins(a.get("stem_gain_db"))?;
                // tp_trim common (opt-in, float stems only): after each stem's
                // own gain, one more trim for every stem, set by the hottest
                // stem's read-back true peak, until it meets the ceiling. Absent:
                // nothing changes.
                let tp_trim = match s_opt(a, "tp_trim").map(|m| m.to_lowercase()) {
                    None => false,
                    Some(m) if m == "common" => {
                        if stem_format != "f32" { bail!("tp_trim common needs stem_format f32 (float stems are measured and trimmed unclipped)"); }
                        true
                    }
                    Some(m) => bail!("tp_trim must be common (got {m:?})"),
                };
                let p = e.project.clone();
                let dir = match s_opt(a, "dir") {
                    Some(d) => e.resolve(&d),
                    None => e.renders_dir().join(format!("{}_stems", samples::sample_name(&p.name))),
                };
                e.bank.sync(&p.samples);
                let ropts = RenderOptions { keep_stems: true, ..Default::default() };
                let mix = render::render(&p, &e.bank, &ropts)?;
                // the pre-master reference: same song with an empty master chain at 0 dB
                let mut pm = p.clone();
                pm.master_effects.clear();
                pm.master_volume_db = 0.0;
                pm.automation.retain(|l| !l.target.eq_ignore_ascii_case("master"));
                let pre = render::render(&pm, &e.bank, &RenderOptions::default())?;
                let any_solo = p.tracks.iter().any(|t| t.solo);
                let n = mix.left.len();
                let (mut sl, mut sr) = (vec![0.0f32; n], vec![0.0f32; n]);
                let mut add = |s: &render::Stem| { for i in 0..n.min(s.left.len()) { sl[i] += s.left[i]; sr[i] += s.right[i]; } };
                for (t, s) in p.tracks.iter().zip(mix.stems.iter()) {
                    let audible = !t.mute && (!any_solo || t.solo);
                    let direct = t.output.as_deref().map(|b| p.bus_index(b).is_err()).unwrap_or(true);
                    if audible && direct { add(s); }
                }
                for (b, s) in p.buses.iter().zip(mix.bus_stems.iter()) {
                    if !b.mute { add(s); }
                }
                let resid = |rl: &[f32], rr: &[f32], al: &[f32], ar: &[f32]| -> f32 {
                    let (mut e1, mut e0) = (0.0f64, 0.0f64);
                    for i in 0..rl.len().min(al.len()) {
                        e0 += (rl[i] as f64).powi(2) + (rr[i] as f64).powi(2);
                        e1 += ((rl[i] - al[i]) as f64).powi(2) + ((rr[i] - ar[i]) as f64).powi(2);
                    }
                    if e0 < 1e-12 { -120.0 } else { ((10.0 * (e1.max(1e-20) / e0).log10()) * 10.0).round() as f32 / 10.0 }
                };
                // render() runs the master sum through a DC blocker (one-pole,
                // R 0.995, -3 dB near 35 Hz) before the master chain, so `pre`
                // has it and the stem taps do not: give the stem sum the same
                // filter (linear, so a correct routing nulls to float rounding).
                // Only this check's copy of the sum; stems and mix are untouched.
                let (mut dcl, mut dcr) = (crate::dsp::DcBlock::default(), crate::dsp::DcBlock::default());
                for (a, b) in sl.iter_mut().zip(sr.iter_mut()) {
                    *a = dcl.process(*a);
                    *b = dcr.process(*b);
                }
                let pre_resid = resid(&pre.left, &pre.right, &sl, &sr);
                // post_master: each stem goes through render()'s own master
                // section (render::master_section), in render order: master
                // fader with its volume lane, the DC blocker (R 0.995), the
                // master chain with its automation lanes and the render's
                // sidechain context (triggers, level keys, offset, timeline),
                // then the non-finite scrub. Every stage is fresh per stem, as
                // it is fresh for the master sum, so a LINEAR master (EQ,
                // filters, gain, automation, trigger-keyed ducking) applied to
                // the stems sums back to the real master to float rounding.
                // A compressor, limiter, clipper or saturator on the master
                // reacts to the full mix in render() and to each stem alone
                // here, so its stems cannot null; that is inherent to per-stem
                // export, and the report's sum_check says how far off it is.
                let mi = if mode == "post_master" { Some(render::master_inputs(&p, &e.bank, &ropts)) } else { None };
                if let Some(mi) = &mi {
                    debug_assert_eq!(mi.total, n, "post_master: master inputs and the mix disagree on length");
                }
                let master_fx = |l: &mut Vec<f32>, r: &mut Vec<f32>| {
                    let Some(mi) = &mi else { return };
                    let ctx = FxContext { step_secs: p.step_secs(), triggers: &mi.triggers, offset: mi.offset, levels: Some(&mi.levels) };
                    render::master_section(&p, l, r, &ctx, &mi.tl);
                };
                let only: Option<Vec<String>> = a.get("tracks").and_then(|v| v.as_array()).map(|x| x.iter().filter_map(|s| s.as_str().map(|s| s.to_lowercase())).collect());
                let mut stems: Vec<(&str, &render::Stem, bool)> = p.tracks.iter().zip(mix.stems.iter()).map(|(t, s)| ("track", s, !t.mute && (!any_solo || t.solo))).collect();
                if b_or(a, "include_buses", true) {
                    stems.extend(p.buses.iter().zip(mix.bus_stems.iter()).map(|(b, s)| ("bus", s, !b.mute)));
                }
                if let Some(o) = &only {
                    stems.retain(|(_, s, _)| o.contains(&s.name.to_lowercase()));
                }
                for k in pinned.keys() {
                    if !stems.iter().any(|(_, s, _)| s.name.to_lowercase() == *k) { bail!("stem_gain_db names {k:?}, which is not a stem of this export"); }
                }
                // common: the gain the hottest exported stem needs to meet the
                // ceiling (0.1 dB under it, as the trim aims), the same for all
                let common_db = if stem_gain == "common" {
                    let mut hottest = f32::NEG_INFINITY;
                    for (_, s, _) in &stems {
                        if pinned.contains_key(&s.name.to_lowercase()) { continue; }
                        let (mut l, mut r) = (s.left.clone(), s.right.clone());
                        if mode == "post_master" { master_fx(&mut l, &mut r); }
                        hottest = hottest.max(tp_db(&l, &r));
                    }
                    Some(if hottest > o.ceiling_dbtp { -(hottest - o.ceiling_dbtp + 0.1) } else { 0.0 })
                } else {
                    None
                };
                let base_gain = |name: &str| -> (Option<Vec<f32>>, &'static str) {
                    match (pinned.get(&name.to_lowercase()), common_db) {
                        (Some(steps), _) => (Some(steps.clone()), "pinned"),
                        (None, Some(c)) => (Some(if c != 0.0 { vec![c] } else { vec![] }), "common"),
                        (None, None) => (None, "trim"),
                    }
                };
                let stem_path = |kind: &str, s: &render::Stem| -> PathBuf {
                    dir.join(format!("{}{}.{}", if kind == "bus" { "bus_" } else { "" }, samples::sample_name(&s.name), ext(&o.format)))
                };
                // tp_trim: write every stem at its own gain plus the common
                // steps so far, read each file back, trim by the hottest
                let tp = if tp_trim {
                    Some(common_tp_trim(o.ceiling_dbtp, TP_TRIM_PASSES, |extra| {
                        let mut tps = Vec::with_capacity(stems.len());
                        for &(kind, s, _) in stems.iter() {
                            let (mut l, mut r) = (s.left.clone(), s.right.clone());
                            if mode == "post_master" {
                                master_fx(&mut l, &mut r);
                            }
                            let mut fixed = base_gain(&s.name).0.unwrap_or_default();
                            fixed.extend_from_slice(extra);
                            let path = stem_path(kind, s);
                            deliver(&l, &r, &path, &DeliverOpts { fixed_gain_db: Some(fixed), ..o.clone() })?;
                            tps.push(DecodedOutput::read(&path)?.true_peak_db());
                        }
                        Ok(tps)
                    })?)
                } else {
                    None
                };
                let names: Vec<String> = stems.iter().map(|(_, s, _)| s.name.clone()).collect();
                let mut out = Vec::new();
                let mut pin_map = serde_json::Map::new();
                let mut warns: Vec<Value> = Vec::new();
                let (mut ql, mut qr) = (vec![0.0f32; n], vec![0.0f32; n]);
                for (si, (kind, s, audible)) in stems.into_iter().enumerate() {
                    let (mut l, mut r) = (s.left.clone(), s.right.clone());
                    if mode == "post_master" {
                        master_fx(&mut l, &mut r);
                    }
                    let (mut fixed, mut source) = base_gain(&s.name);
                    if let Some(t) = &tp {
                        let mut f = fixed.unwrap_or_default();
                        f.extend_from_slice(&t.steps);
                        fixed = Some(f);
                        if source == "trim" {
                            source = "as_mixed";
                        }
                    }
                    let so = DeliverOpts { fixed_gain_db: fixed, ..o.clone() };
                    let direct = kind == "bus" || p.tracks.iter().find(|t| t.name == s.name).and_then(|t| t.output.as_deref()).map(|b| p.bus_index(b).is_err()).unwrap_or(true);
                    if audible && direct {
                        for i in 0..n.min(l.len()) { ql[i] += l[i]; qr[i] += r[i]; }
                    }
                    let path: PathBuf = stem_path(kind, s);
                    let v = deliver(&l, &r, &path, &so)?;
                    let steps = v["gain_steps_db"].clone();
                    let gain: f64 = steps.as_array().map(|x| x.iter().filter_map(|g| g.as_f64()).sum()).unwrap_or(0.0);
                    pin_map.insert(s.name.clone(), v["gain_steps_exact"].clone());
                    let mut entry = json!({"name": s.name, "kind": kind, "path": path, "audible": audible, "in_sum": audible && direct,
                        "routed_to": if kind == "bus" { json!("master") } else { json!(p.tracks.iter().find(|t| t.name == s.name).and_then(|t| t.output.clone()).unwrap_or_else(|| "master".into())) },
                        "lufs": v["after"]["integrated_lufs"], "true_peak_dbtp": v["after"]["true_peak_dbtp"], "silent": v["after"]["integrated_lufs"].as_f64().unwrap_or(-70.0) <= -69.0,
                        "gain_source": source, "gain_db": (gain * 1000.0).round() / 1000.0, "gain_steps_db": steps, "over_ceiling": v["over_ceiling"],
                        "clipped_samples": v["clipped_samples"], "warnings": v["warnings"]});
                    if let Some(t) = &tp {
                        entry["true_peak_before_dbtp"] = json!((t.before[si] as f64 * 100.0).round() / 100.0);
                    }
                    out.push(entry);
                    for w in v["warnings"].as_array().into_iter().flatten() {
                        if let Some(w) = w.as_str() { warns.push(json!(format!("{}: {w}", s.name))); }
                    }
                }
                let (sum_check, verdict) = if mode == "pre_master" {
                    (pre_resid, if pre_resid < -60.0 { "stems sum back to the pre-master mix (null within -60 dB)" } else { "stems do NOT fully null against the pre-master mix (check pre-fader sends / bus routing)" })
                } else {
                    let r = resid(&mix.left, &mix.right, &ql, &qr);
                    (r, if r < -60.0 { "post-master stems sum back to the master (null within -60 dB)" } else { "post-master stems do not null against the master: the master chain is nonlinear (compressor/limiter/clipper react to the full mix, each stem alone here)" })
                };
                let hottest = |v: &[f32]| -> (String, f64) {
                    let i = (0..v.len()).max_by(|&x, &y| v[x].total_cmp(&v[y]));
                    i.map(|i| (names[i].clone(), (v[i] as f64 * 100.0).round() / 100.0)).unwrap_or_default()
                };
                let tp_report = tp.as_ref().map(|t| {
                    let (b, a) = (hottest(&t.before), hottest(&t.after));
                    if !t.converged {
                        warns.push(json!(format!(
                            "tp_trim gave up after {} trims: the hottest stem ({}) is at {:+.2} dBTP, over the {:+.1} dBTP ceiling",
                            t.steps.len(), a.0, a.1, o.ceiling_dbtp
                        )));
                    }
                    let total: f32 = t.steps.iter().sum();
                    json!({"mode": "common", "ceiling_dbtp": o.ceiling_dbtp, "aim_dbtp": o.ceiling_dbtp - 0.1,
                        "trim_db": (total as f64 * 1000.0).round() / 1000.0,
                        "trim_steps_db": t.steps.iter().map(|g| *g as f64).collect::<Vec<f64>>(),
                        "trims": t.steps.len(), "measure_passes": t.measures, "max_trims": TP_TRIM_PASSES, "converged": t.converged,
                        "hottest_before": {"stem": b.0, "true_peak_dbtp": b.1}, "hottest_after": {"stem": a.0, "true_peak_dbtp": a.1},
                        "meter": "true peak read back from each written file: 4x oversampled, per channel, max of L/R"})
                });
                let mut report = json!({"dir": dir, "mode": mode, "format": o.format, "count": out.len(), "seconds": (mix.seconds * 100.0).round() / 100.0,
                    "sum_residual_db": sum_check, "sums_to": if mode == "pre_master" { "pre-master mix" } else { "master" }, "verdict": verdict,
                    "master_chain": p.master_effects.iter().map(|x| x.type_name()).collect::<Vec<_>>(),
                    "stem_gain": stem_gain, "common_gain_db": common_db.map(|c| c as f64), "stem_gain_db_pin": pin_map,
                    "stem_format": stem_format, "bits": o.bits, "exact_trim": o.exact_trim, "warnings": warns, "stems": out});
                if let Some(t) = tp_report {
                    report["tp_trim"] = t;
                }
                Ok(report)
            },
        },
        Tool {
            name: "master_assistant",
            description: "Set up the master for delivery: keeps/adds a true-peak limiter LAST on the master chain at the ceiling, an optional style stage (clean: nothing; punchy: gentle glue compressor; loud: soft clipper before the limiter), optional tonal match to a reference file (parametric EQ moves from compare_to_reference), then iterates a gain stage before the limiter (re-rendering) until integrated loudness is within 0.3 LU of target_lufs. dry_run:true reports the plan without changing anything.",
            mutates: true,
            schema: || obj(json!({
                "target_lufs": {"type": "number", "description": "default -14"},
                "true_peak_ceiling": {"type": "number", "description": "default -1 dBTP"},
                "style": {"type": "string", "enum": ["clean", "punchy", "loud"], "description": "default clean"},
                "reference": {"type": "string", "description": "Reference audio path: match its tonal balance (and loudness when target_lufs is omitted)"},
                "max_iterations": {"type": "integer", "description": "default 4"},
                "dry_run": {"type": "boolean"},
            }), &[]),
            run: |e, a| {
                let ceiling = f_opt(a, "true_peak_ceiling").unwrap_or(-1.0);
                let style = s_opt(a, "style").unwrap_or_else(|| "clean".into());
                let mut plan = Vec::new();
                let mut p = e.project.clone();
                let mut target = f_opt(a, "target_lufs");
                // reference tone match
                if let Some(rp) = s_opt(a, "reference") {
                    let path = e.resolve(&rp);
                    let (rl, rr) = samples::decode_stereo(&path).with_context(|| format!("reading reference {}", path.display()))?;
                    let rf = audio_edit::profile("reference", &rl, &rr);
                    if target.is_none() { target = Some(rf.integrated_lufs.clamp(-20.0, -6.0)); }
                    let m = e.render_version(&p)?;
                    let mp = audio_edit::profile("mix", &m.left, &m.right);
                    let (deltas, _) = audio_edit::compare(&mp, &rf);
                    let bands: Vec<Value> = deltas.iter().filter(|d| d.metric.starts_with("band_")).filter_map(|d| d.fix.as_ref()).filter_map(|f| f["args"]["params"]["bands"][0].as_object().cloned().map(Value::Object)).collect();
                    if !bands.is_empty() {
                        let fx: Effect = serde_json::from_value(json!({"type": "parametric_eq", "bands": bands})).context("tone-match eq")?;
                        let at = limiter_pos(&p.master_effects).unwrap_or(p.master_effects.len());
                        p.master_effects.insert(at, fx);
                        plan.push(json!({"step": "tone_match_eq", "bands": bands}));
                    }
                }
                let target = target.unwrap_or(-14.0);
                // style stage
                let has = |p: &crate::project::Project, f: fn(&Effect) -> bool| p.master_effects.iter().any(f);
                match style.as_str() {
                    "punchy" if !has(&p, |e| matches!(e, Effect::Compressor(_))) => {
                        let at = limiter_pos(&p.master_effects).unwrap_or(p.master_effects.len());
                        p.master_effects.insert(at, serde_json::from_value(json!({"type": "compressor", "threshold_db": -14.0, "ratio": 2.0, "attack_ms": 30.0, "release_ms": 150.0}))?);
                        plan.push(json!({"step": "glue_compressor"}));
                    }
                    "loud" if !has(&p, |e| matches!(e, Effect::SoftClipper(_))) => {
                        let at = limiter_pos(&p.master_effects).unwrap_or(p.master_effects.len());
                        p.master_effects.insert(at, serde_json::from_value(json!({"type": "soft_clipper", "threshold_db": -6.0, "ceiling_db": -0.3}))?);
                        plan.push(json!({"step": "soft_clipper"}));
                    }
                    "clean" | "punchy" | "loud" => {}
                    s => bail!("unknown style '{s}' (clean, punchy, loud)"),
                }
                // limiter last
                let lim_ceiling = ceiling - 0.3;
                match limiter_pos(&p.master_effects) {
                    Some(i) => {
                        let mut lim = p.master_effects.remove(i);
                        if let Effect::Limiter(l) = &mut lim { l.ceiling_db = lim_ceiling; l.true_peak = true; l.bypass = false; }
                        p.master_effects.push(lim);
                        plan.push(json!({"step": "limiter_last", "ceiling_db": lim_ceiling, "moved_from": i}));
                    }
                    None => {
                        p.master_effects.push(Effect::Limiter(LimiterFx { ceiling_db: lim_ceiling, release_ms: 80.0, true_peak: true, ..Default::default() }));
                        plan.push(json!({"step": "add_limiter", "ceiling_db": lim_ceiling}));
                    }
                }
                // gain stage directly before the limiter
                let li = p.master_effects.len() - 1;
                if !(li > 0 && matches!(p.master_effects[li - 1], Effect::Gain(_))) {
                    p.master_effects.insert(li, Effect::Gain(GainFx::default()));
                }
                let gi = p.master_effects.len() - 2;
                // name new effects now, as applying will: the rendered
                // candidate and the applied project then hash the same
                p.ensure_fx_ids();
                let mut iters = Vec::new();
                let max_it = u_or(a, "max_iterations", 4).clamp(1, 8);
                let mut last = analysis::loudness(&[], &[]);
                for k in 0..max_it {
                    let m = e.render_version(&p)?;
                    last = analysis::loudness(&m.left, &m.right);
                    let err = target - last.integrated_lufs;
                    let g = if let Effect::Gain(gfx) = &p.master_effects[gi] { gfx.db } else { 0.0 };
                    iters.push(json!({"iteration": k + 1, "gain_db": g, "integrated_lufs": last.integrated_lufs, "true_peak_dbtp": last.true_peak_dbtp}));
                    if err.abs() <= 0.3 || last.integrated_lufs <= -69.0 { break; }
                    // a limiter makes gain->loudness sub-linear; over-step a little when pushing
                    let step = if err > 0.0 { err * 1.15 } else { err };
                    if let Effect::Gain(gfx) = &mut p.master_effects[gi] { gfx.db = (gfx.db + step).clamp(-24.0, 24.0); }
                }
                let reached = (target - last.integrated_lufs).abs() <= 0.5;
                let chain: Vec<String> = p.master_effects.iter().map(|x| x.type_name()).collect();
                let dry = b_or(a, "dry_run", false);
                if !dry {
                    e.project.master_effects = p.master_effects.clone();
                }
                Ok(json!({
                    "applied": !dry, "target_lufs": target, "true_peak_ceiling": ceiling, "style": style,
                    "integrated_lufs": last.integrated_lufs, "true_peak_dbtp": last.true_peak_dbtp,
                    "reached_target": reached, "plan": plan, "iterations": iters, "master_chain": chain,
                    "note": if reached { "Now check_master, then export_audio (it re-measures after encoding)." } else { "Target not reached: the limiter is working hard or the mix is too dynamic. Lower the target, try style 'loud', or fix the arrangement level (analyze_sections)." },
                }))
            },
        },
        Tool {
            name: "analyze_reference",
            description: "Profile a reference track (file path; optional start_s/end_s to profile only e.g. its chorus): integrated/short-term LUFS, loudness range, true peak, crest, centroid, band balance, stereo width (side/mid), BPM and key, plus a 32-point log spectrum. Feed it to compare_to_reference or master_assistant.",
            mutates: false,
            schema: || obj(json!({"path": {"type": "string"}, "start_s": {"type": "number"}, "end_s": {"type": "number"}}), &["path"]),
            run: |e, a| {
                let path = e.resolve(&s_req(a, "path")?);
                let (l, r) = samples::decode_stereo(&path).with_context(|| format!("reading {}", path.display()))?;
                let (s0, s1) = window_s(a, l.len().min(r.len()));
                let mut v = serde_json::to_value(audio_edit::profile(&path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(), &l[s0..s1], &r[s0..s1]))?;
                v["path"] = json!(path);
                let mono: Vec<f32> = (s0..s1).map(|i| 0.5 * (l[i] + r[i])).collect();
                v["spectrum_db"] = json!(analysis::log_spectrum_db(&mono, 32).iter().map(|d| (d * 10.0).round() / 10.0).collect::<Vec<_>>());
                Ok(v)
            },
        },
        Tool {
            name: "compare_to_reference",
            description: "A/B the mix (or one section/window of it) against a reference file (optionally its own start_s/end_s window, e.g. the reference's chorus vs your hook): loudness, per-band tonal balance in dB, crest/dynamics and stereo width, each with a verdict and the exact tool call that closes the gap, plus a 0-100 match score. Matches SOUND only, never composition.",
            mutates: false,
            schema: || obj(merge_props(region_props(), json!({
                "reference": {"type": "string", "description": "Reference audio path"},
                "ref_start_s": {"type": "number"}, "ref_end_s": {"type": "number"},
            })), &["reference"]),
            run: |e, a| {
                let path = e.resolve(&s_req(a, "reference")?);
                let (rl, rr) = samples::decode_stereo(&path).with_context(|| format!("reading {}", path.display()))?;
                let n = rl.len().min(rr.len());
                let r0 = ((f_opt(a, "ref_start_s").unwrap_or(0.0).max(0.0) * SR) as usize).min(n);
                let r1 = (f_opt(a, "ref_end_s").map(|x| (x * SR) as usize).unwrap_or(n)).clamp(r0, n);
                if r1 <= r0 { return Err(anyhow!("empty reference window")); }
                let rf = audio_edit::profile("reference", &rl[r0..r1], &rr[r0..r1]);
                let m = e.mix()?;
                let p = e.project.clone();
                let (s0, s1, label) = region(&p, a, m.left.len().min(m.right.len()))?;
                let mp = audio_edit::profile("mix", &m.left[s0..s1], &m.right[s0..s1]);
                let (deltas, score) = audio_edit::compare(&mp, &rf);
                Ok(json!({"window": label, "reference": path, "score": score, "deltas": deltas, "mix": mp, "reference_profile": rf}))
            },
        },
    ]
}

fn window_s(a: &Value, n: usize) -> (usize, usize) {
    let s0 = ((f_opt(a, "start_s").unwrap_or(0.0).max(0.0) * SR) as usize).min(n);
    let s1 = f_opt(a, "end_s")
        .map(|x| (x * SR) as usize)
        .unwrap_or(n)
        .clamp(s0, n);
    if s1 <= s0 {
        (0, n)
    } else {
        (s0, s1)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn delivery_keeps_section_contrast() {
        // 8 s "verse" of one tone, 8 s "drop" 4 dB louder with dense peaks
        let sr = super::SR as usize;
        let mut l = Vec::new();
        for i in 0..16 * sr {
            let t = i as f32 / sr as f32;
            let base = (t * 220.0 * std::f32::consts::TAU).sin() * 0.2;
            let kick = if (t * 2.0).fract() < 0.05 {
                0.6 * (t * 60.0 * std::f32::consts::TAU).sin()
            } else {
                0.0
            };
            let lift = if t >= 8.0 { 1.58 } else { 1.0 };
            l.push(base * lift + kick);
        }
        let r = l.clone();
        let lift_in = super::section_lift_db(&l, &r);
        let run = |pres: f32, clip: f32| {
            let o = super::DeliverOpts {
                target_lufs: Some(-8.0),
                preserve_dynamics: pres,
                soft_clip_db: clip,
                format: "wav".into(),
                bits: 32,
                dither: false,
                ..Default::default()
            };
            let path = std::env::temp_dir().join(format!("bb_lift_{pres}_{clip}.wav"));
            let v = super::deliver(&l, &r, &path, &o).unwrap();
            v["section_lift_db"]["after"].as_f64().unwrap() as f32
        };
        let flat = run(0.0, 0.0);
        let kept = run(0.8, 3.0);
        // never flatter than the limiter alone, and most of the song's own lift survives
        assert!(
            kept >= flat - 0.2 && kept >= 0.7 * lift_in,
            "lift in {lift_in}, limiter only {flat}, preserved {kept}"
        );
    }

    use crate::engine::Engine;
    use serde_json::json;

    fn eng(dir: &str) -> Engine {
        let mut e = Engine::new(std::env::temp_dir().join(dir));
        e.call("generate_beat", &json!({"style": "lofi", "seed": 11}))
            .unwrap();
        e
    }

    #[test]
    fn export_formats_meet_ceiling() {
        let mut e = eng("beatbox_delivery_tests");
        let w = e.call("export_audio", &json!({"format": "wav", "bits": 16, "target_lufs": -10, "path": "out/a.wav", "section": 0})).unwrap();
        assert_eq!(w["passed"], true, "{w}");
        assert!(w["after"]["true_peak_dbtp"].as_f64().unwrap() <= -0.95);
        assert!(
            (w["after"]["integrated_lufs"].as_f64().unwrap() + 10.0).abs() < 1.0,
            "{w}"
        );
        let f = e
            .call(
                "export_audio",
                &json!({"format": "flac", "path": "out/a.flac", "section": 0}),
            )
            .unwrap();
        assert!(f["bytes"].as_u64().unwrap() > 1000);
        if super::ffmpeg_available() {
            let m = e.call("export_audio", &json!({"format": "mp3", "target_lufs": -9, "true_peak_ceiling": -1, "path": "out/a.mp3", "section": 0})).unwrap();
            assert!(
                m["after"]["true_peak_dbtp"].as_f64().unwrap() <= -0.95,
                "{m}"
            );
        }
        let s = e
            .call(
                "export_stems",
                &json!({"dir": "out/stems", "format": "flac", "bits": 16}),
            )
            .unwrap();
        assert!(s["count"].as_u64().unwrap() >= 3, "{s}");
    }

    /// Pre-master stems (direct tracks + every bus) null against the
    /// pre-master mix. render()'s master path runs a DC blocker (one-pole,
    /// about -3 dB at 35 Hz) before the master chain; the stem sum gets the
    /// same filter, or anything with bass reads as a failed null (IWBY v6
    /// read -15.8 dB with nothing misrouted).
    #[test]
    fn export_stems_pre_master_nulls_against_the_pre_master_mix() {
        let mut e = eng("beatbox_stems_pre_master_null");
        // a return bus fed by a pre-fader and a post-fader send
        e.call("add_bus", &json!({"name": "verb", "preset": "reverb"}))
            .unwrap();
        let t0 = e.project.tracks[0].name.clone();
        let t1 = e.project.tracks[1].name.clone();
        e.call(
            "set_send",
            &json!({"track": t0, "bus": "verb", "db": -6, "pre_fader": true}),
        )
        .unwrap();
        e.call("set_send", &json!({"track": t1, "bus": "verb", "db": -12}))
            .unwrap();
        e.project.tracks[0].volume_db -= 6.0;
        let s = e.call("export_stems", &json!({"dir": "out/n", "format": "wav", "stem_format": "f32", "mode": "pre_master"})).unwrap();
        let r = s["sum_residual_db"].as_f64().unwrap();
        assert!(
            r < -60.0,
            "pre-master stems do not null: {r} dB {}",
            s["verdict"]
        );
        assert_eq!(
            s["verdict"], "stems sum back to the pre-master mix (null within -60 dB)",
            "{s}"
        );
    }

    /// A lofi beat with one return bus (a pre- and a post-fader send) and a
    /// LINEAR master: the master chain cleared to a parametric EQ only, a
    /// master volume automation lane (0 dB -> -9 dB -> +2 dB over the song)
    /// and the static master fader moved off 0 dB, so a stub that ignored
    /// the lane, the DC blocker or the fader shows up in the null.
    fn linear_master_eng(dir: &str) -> Engine {
        let mut e = eng(dir);
        e.call("add_bus", &json!({"name": "verb", "preset": "reverb"}))
            .unwrap();
        let t0 = e.project.tracks[0].name.clone();
        let t1 = e.project.tracks[1].name.clone();
        e.call(
            "set_send",
            &json!({"track": t0, "bus": "verb", "db": -6, "pre_fader": true}),
        )
        .unwrap();
        e.call("set_send", &json!({"track": t1, "bus": "verb", "db": -12}))
            .unwrap();
        e.project.master_effects.clear();
        e.project
            .automation
            .retain(|l| !l.target.eq_ignore_ascii_case("master"));
        e.project.master_volume_db = -3.0;
        e.call(
            "add_effect",
            &json!({"track": "master", "type": "parametric_eq", "params": {"bands": [
                {"kind": "low_cut", "freq": 60},
                {"kind": "bell", "freq": 3200, "gain_db": 4, "q": 1.2},
                {"kind": "high_shelf", "freq": 9000, "gain_db": -3}
            ]}}),
        )
        .unwrap();
        let beats = e.project.song_beats();
        e.call("add_automation", &json!({"track": "master", "param": "volume", "points": [[0.0, 0.0], [beats * 0.5, -9.0], [beats, 2.0]]})).unwrap();
        e
    }

    /// post_master runs render()'s own master section on every stem (fader
    /// + volume lane -> DC blocker R 0.995 -> master chain with its lanes and
    ///   sidechain context): with a linear master the post-master stems
    ///   (direct tracks + the bus) sum back to the real master below -60 dB.
    #[test]
    fn export_stems_post_master_linear_master_nulls_against_the_master() {
        let mut e = linear_master_eng("beatbox_stems_post_master_null");
        assert!(
            e.project
                .automation
                .iter()
                .any(|l| l.target == "master" && l.param == "volume" && l.enabled),
            "the master volume lane is in place"
        );
        let s = e.call("export_stems", &json!({"dir": "out/pm", "format": "wav", "stem_format": "f32", "mode": "post_master"})).unwrap();
        let r = s["sum_residual_db"].as_f64().unwrap();
        eprintln!("post_master linear-master null: {r} dB");
        assert!(
            r < -60.0,
            "post-master stems do not null against the master: {r} dB {}",
            s["verdict"]
        );
        assert_eq!(
            s["verdict"], "post-master stems sum back to the master (null within -60 dB)",
            "{s}"
        );
        // the bus is exported and is in the sum
        let bus = stem(&s, "verb");
        assert_eq!(bus["kind"], "bus", "{s}");
        assert_eq!(bus["in_sum"], true, "{s}");
        // pre_master on the same project still nulls against the pre-master mix
        let q = e.call("export_stems", &json!({"dir": "out/pre", "format": "wav", "stem_format": "f32", "mode": "pre_master"})).unwrap();
        assert!(q["sum_residual_db"].as_f64().unwrap() < -60.0, "{q}");
    }

    /// The master chain gets the render's sidechain context: a trigger-keyed
    /// ducker on the master (its gain follows the key track's notes, not the
    /// signal, so it is linear) nulls per stem too. The old stub passed no
    /// triggers, so the ducker did nothing on the stems.
    #[test]
    fn export_stems_post_master_master_ducker_follows_the_render_triggers() {
        let mut e = linear_master_eng("beatbox_stems_post_master_duck");
        let key = e.project.tracks[0].name.clone();
        e.call("add_effect", &json!({"track": "master", "type": "sidechain", "params": {"source": key, "amount": 0.8}})).unwrap();
        let s = e.call("export_stems", &json!({"dir": "out/pmd", "format": "wav", "stem_format": "f32", "mode": "post_master"})).unwrap();
        let r = s["sum_residual_db"].as_f64().unwrap();
        eprintln!("post_master master-ducker null: {r} dB");
        assert!(
            r < -60.0,
            "post-master stems with a master ducker do not null: {r} dB {}",
            s["verdict"]
        );
    }

    /// A limiter on the master reacts to the full mix in render() and to
    /// each stem alone in post_master, so the stems cannot null; the report
    /// says so instead of claiming a match.
    #[test]
    fn export_stems_post_master_limiter_cannot_null_and_says_so() {
        let mut e = linear_master_eng("beatbox_stems_post_master_limiter");
        e.project.master_volume_db = 12.0;
        e.call(
            "add_effect",
            &json!({"track": "master", "type": "limiter", "params": {"ceiling_db": -6.0}}),
        )
        .unwrap();
        let s = e.call("export_stems", &json!({"dir": "out/pml", "format": "wav", "stem_format": "f32", "mode": "post_master"})).unwrap();
        let r = s["sum_residual_db"].as_f64().unwrap();
        eprintln!("post_master master-limiter residual: {r} dB");
        assert!(
            r > -60.0,
            "a hard-driven master limiter should not null per stem: {r} dB"
        );
        assert!(s["verdict"].as_str().unwrap().contains("nonlinear"), "{s}");
    }

    /// The master gain is applied exactly once on each path: stems are
    /// tapped before the master section and post_master adds the master
    /// fader once. With no master lane the static fader is what applies, so
    /// a stem path that doubled it (or skipped it) would miss the master by
    /// about -6 dB here instead of nulling. Also the defaults: an empty
    /// master chain with no automation, and a bypassed (hard-driven) master
    /// limiter, null in both paths exactly as the master sees them.
    #[test]
    fn export_stems_post_master_master_gain_applied_once_and_defaults_null() {
        let mut e = eng("beatbox_stems_post_master_gain_once");
        e.call("add_bus", &json!({"name": "verb", "preset": "reverb"}))
            .unwrap();
        let t0 = e.project.tracks[0].name.clone();
        e.call("set_send", &json!({"track": t0, "bus": "verb", "db": -6}))
            .unwrap();
        e.project.master_effects.clear();
        e.project
            .automation
            .retain(|l| !l.target.eq_ignore_ascii_case("master"));
        e.project.master_volume_db = 0.0;
        let null = |e: &mut Engine, d: &str| -> f64 {
            let s = e.call("export_stems", &json!({"dir": d, "format": "wav", "stem_format": "f32", "mode": "post_master"})).unwrap();
            s["sum_residual_db"].as_f64().unwrap()
        };
        let r0 = null(&mut e, "out/g0");
        assert!(
            r0 < -60.0,
            "empty master chain, 0 dB, no automation: {r0} dB"
        );
        e.project.master_volume_db = -6.0;
        let r6 = null(&mut e, "out/g6");
        assert!(
            r6 < -60.0,
            "static master fader -6 dB applied once per path: {r6} dB"
        );
        e.project.master_volume_db = 12.0;
        e.call(
            "add_effect",
            &json!({"track": "master", "type": "limiter", "params": {"ceiling_db": -6.0}}),
        )
        .unwrap();
        e.project
            .master_effects
            .last_mut()
            .unwrap()
            .set_bypass(true);
        let rb = null(&mut e, "out/gb");
        assert!(
            rb < -60.0,
            "a bypassed master limiter is skipped in both paths: {rb} dB"
        );
        eprintln!("post_master gain-once nulls: empty {r0} dB, -6 dB fader {r6} dB, bypassed limiter {rb} dB");
    }

    fn hot_stems(dir: &str) -> (Engine, String) {
        let mut e = eng(dir);
        // a hot first track: its stem is over 0 dBTP, so the default trims it
        e.project.tracks[0].volume_db += 12.0;
        let name = e.project.tracks[0].name.clone();
        (e, name)
    }

    fn stem<'a>(s: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
        s["stems"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == name)
            .unwrap()
    }

    /// Default unchanged: every stem is trimmed on its own; the report's pin
    /// map fed back as stem_gain_db rebuilds the same files bit for bit.
    #[test]
    fn export_stems_pinned_gain_rebuilds_the_trimmed_files() {
        let (mut e, hot) = hot_stems("beatbox_stems_pin_rebuild");
        let a = e
            .call(
                "export_stems",
                &json!({"dir": "out/a", "format": "wav", "bits": 24}),
            )
            .unwrap();
        assert_eq!(a["stem_gain"], "trim", "{a}");
        assert!(
            a["stems"]
                .as_array()
                .unwrap()
                .iter()
                .all(|x| x["gain_source"] == "trim"),
            "{a}"
        );
        assert!(
            stem(&a, &hot)["gain_db"].as_f64().unwrap() < -1.0,
            "the hot stem is trimmed: {a}"
        );
        let b = e.call("export_stems", &json!({"dir": "out/b", "format": "wav", "bits": 24, "stem_gain_db": a["stem_gain_db_pin"]})).unwrap();
        for (x, y) in a["stems"]
            .as_array()
            .unwrap()
            .iter()
            .zip(b["stems"].as_array().unwrap())
        {
            assert_eq!(y["gain_source"], "pinned", "{b}");
            let fx = std::fs::read(x["path"].as_str().unwrap()).unwrap();
            let fy = std::fs::read(y["path"].as_str().unwrap()).unwrap();
            assert!(fx == fy, "{} differs pinned vs trimmed", x["name"]);
        }
        // a name that is not a stem is refused, not ignored
        assert!(e
            .call(
                "export_stems",
                &json!({"dir": "out/c", "stem_gain_db": {"nobody": -3.0}})
            )
            .is_err());
        assert!(e
            .call(
                "export_stems",
                &json!({"dir": "out/c", "stem_gain": "loud"})
            )
            .is_err());
    }

    /// The coupling: lowering a stem's peaks (a limiter on its chain) makes
    /// the trimmed stem louder; with its gain pinned the level stays put.
    #[test]
    fn export_stems_pinned_gain_does_not_follow_the_stems_peak() {
        let (mut e, hot) = hot_stems("beatbox_stems_pin_decouple");
        let a = e
            .call(
                "export_stems",
                &json!({"dir": "out/a", "tracks": [hot.clone()]}),
            )
            .unwrap();
        e.call(
            "add_effect",
            &json!({"track": hot.clone(), "type": "limiter", "params": {"ceiling_db": -12.0}}),
        )
        .unwrap();
        let trim = e
            .call(
                "export_stems",
                &json!({"dir": "out/t", "tracks": [hot.clone()]}),
            )
            .unwrap();
        let pin = e.call("export_stems", &json!({"dir": "out/p", "tracks": [hot.clone()], "stem_gain_db": a["stem_gain_db_pin"]})).unwrap();
        let l = |s: &serde_json::Value| stem(s, &hot)["lufs"].as_f64().unwrap();
        let (d_trim, d_pin) = (l(&trim) - l(&a), l(&pin) - l(&a));
        assert!(
            d_pin <= 0.1,
            "pinned: the limiter only takes level away ({d_pin:+.2} dB)"
        );
        assert!(
            d_trim > d_pin + 1.0,
            "trim follows the lowered peak: {d_trim:+.2} vs pinned {d_pin:+.2}"
        );
        assert_eq!(
            stem(&pin, &hot)["gain_steps_db"],
            stem(&a, &hot)["gain_steps_db"]
        );
    }

    /// common: one gain for all stems, the hottest stem meets the ceiling,
    /// and the stems keep the mix's balance (same gain on each).
    #[test]
    fn export_stems_common_gain_keeps_the_balance() {
        let (mut e, hot) = hot_stems("beatbox_stems_common");
        let raw = e
            .call(
                "export_stems",
                &json!({"dir": "out/r", "format": "wav", "bits": 32}),
            )
            .unwrap();
        let mut zero = serde_json::Map::new();
        for x in raw["stems"].as_array().unwrap() {
            zero.insert(x["name"].as_str().unwrap().to_string(), json!(0.0));
        }
        let flat = e
            .call(
                "export_stems",
                &json!({"dir": "out/z", "format": "wav", "bits": 32, "stem_gain_db": zero}),
            )
            .unwrap();
        let c = e
            .call(
                "export_stems",
                &json!({"dir": "out/c", "format": "wav", "bits": 32, "stem_gain": "common"}),
            )
            .unwrap();
        let g = c["common_gain_db"].as_f64().unwrap();
        assert!(g < -1.0, "{c}");
        for x in c["stems"].as_array().unwrap() {
            assert_eq!(x["gain_source"], "common");
            assert!((x["gain_db"].as_f64().unwrap() - g).abs() < 1e-3, "{x}");
            assert_eq!(x["over_ceiling"], false, "{x}");
            let z = stem(&flat, x["name"].as_str().unwrap());
            if z["silent"] == false {
                let d = x["lufs"].as_f64().unwrap() - z["lufs"].as_f64().unwrap();
                assert!(
                    (d - g).abs() < 0.05,
                    "{}: {d:+.2} vs common {g:+.2}",
                    x["name"]
                );
            }
        }
        let tp = stem(&c, &hot)["true_peak_dbtp"].as_f64().unwrap();
        assert!(
            tp <= 0.05 && tp > -0.5,
            "the hottest stem sits at the ceiling: {tp}"
        );
    }

    /// stem_format f32: a stem pinned hot (over 0 dBFS) is written as float and
    /// carried exactly; the same pin in a 24-bit file clips, and says so.
    #[test]
    fn export_stems_f32_carries_a_hot_stem_unclipped() {
        let (mut e, hot) = hot_stems("beatbox_stems_f32");
        e.project.tracks[0].volume_db += 12.0;
        let pin = json!({hot.clone(): 0.0});
        let i24 = e
            .call(
                "export_stems",
                &json!({"dir": "out/i", "tracks": [hot.clone()], "stem_gain_db": pin}),
            )
            .unwrap();
        let f32s = e.call("export_stems", &json!({"dir": "out/f", "tracks": [hot.clone()], "stem_gain_db": pin, "stem_format": "f32"})).unwrap();
        let (a, b) = (stem(&i24, &hot), stem(&f32s, &hot));
        assert!(
            a["clipped_samples"].as_u64().unwrap() > 0,
            "24-bit at the hot pin clips: {a}"
        );
        assert!(
            a["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap().contains("full scale")),
            "{a}"
        );
        assert!(!i24["warnings"].as_array().unwrap().is_empty(), "{i24}");
        assert_eq!(f32s["stem_format"], "f32");
        assert_eq!(f32s["bits"], 32);
        assert!(
            b["clipped_samples"].is_null() && b["warnings"].as_array().unwrap().is_empty(),
            "{b}"
        );
        assert!(
            b["true_peak_dbtp"].as_f64().unwrap() > 3.0,
            "float keeps the peak over 0 dBTP: {b}"
        );
        let d =
            super::DecodedOutput::read(std::path::Path::new(b["path"].as_str().unwrap())).unwrap();
        let top = d
            .left
            .iter()
            .chain(d.right.iter())
            .fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(
            top > 1.4,
            "samples over full scale survive in the float file: {top}"
        );
        // float is wav only, and 32-bit only
        assert!(e
            .call(
                "export_stems",
                &json!({"dir": "out/x", "format": "flac", "stem_format": "f32"})
            )
            .is_err());
        assert!(e
            .call(
                "export_stems",
                &json!({"dir": "out/x", "bits": 24, "stem_format": "f32"})
            )
            .is_err());
        assert!(e
            .call(
                "export_stems",
                &json!({"dir": "out/x", "stem_format": "f64"})
            )
            .is_err());
    }

    /// The default trim reads the decoded peak, which a hard-clipped integer
    /// file under-reads, and gives up after 4 trims (with a warning);
    /// exact_trim meets 0 dBTP with nothing clipped.
    #[test]
    fn export_stems_exact_trim_meets_the_ceiling_unclipped() {
        let (mut e, hot) = hot_stems("beatbox_stems_exact_trim");
        e.project.tracks[0].volume_db += 12.0;
        let dflt = e
            .call(
                "export_stems",
                &json!({"dir": "out/d", "tracks": [hot.clone()]}),
            )
            .unwrap();
        let ex = e
            .call(
                "export_stems",
                &json!({"dir": "out/e", "tracks": [hot.clone()], "exact_trim": true}),
            )
            .unwrap();
        let (a, b) = (stem(&dflt, &hot), stem(&ex, &hot));
        assert_eq!(
            a["over_ceiling"], true,
            "the default gives up over the ceiling here: {a}"
        );
        assert!(
            a["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w.as_str().unwrap().contains("gave up")),
            "{a}"
        );
        assert_eq!(dflt["exact_trim"], false);
        assert_eq!(ex["exact_trim"], true);
        assert_eq!(b["over_ceiling"], false, "{b}");
        assert!(b["true_peak_dbtp"].as_f64().unwrap() <= 0.0, "{b}");
        assert_eq!(b["clipped_samples"], 0, "{b}");
        assert!(b["warnings"].as_array().unwrap().is_empty(), "{b}");
        assert!(
            b["gain_steps_db"].as_array().unwrap().len() <= 2,
            "one trim, two at most: {b}"
        );
        // the exact trim takes the whole excess; the default stops short
        assert!(
            b["gain_db"].as_f64().unwrap() < a["gain_db"].as_f64().unwrap(),
            "{a} {b}"
        );
        // its pin map rebuilds the same file bit for bit
        let pin = e.call("export_stems", &json!({"dir": "out/p", "tracks": [hot.clone()], "stem_gain_db": ex["stem_gain_db_pin"]})).unwrap();
        let fx = std::fs::read(b["path"].as_str().unwrap()).unwrap();
        let fy = std::fs::read(stem(&pin, &hot)["path"].as_str().unwrap()).unwrap();
        assert!(fx == fy, "pinned rebuild of the exact-trimmed stem differs");
    }

    // ---- tp_trim common (opt-in, float stems) ----

    fn f32_file(path: &str) -> super::DecodedOutput {
        super::DecodedOutput::read(std::path::Path::new(path)).unwrap()
    }

    fn energy(d: &super::DecodedOutput) -> f64 {
        d.left
            .iter()
            .chain(d.right.iter())
            .map(|x| (*x as f64) * (*x as f64))
            .sum()
    }

    /// (a) tp_trim absent: nothing changes. A hot pinned float stem stays over
    /// 0 dBTP with no tp_trim fields in the report; on a song whose stems are
    /// all under the ceiling, tp_trim takes no trim and writes the same bytes
    /// as the export without it. tp_trim needs f32 and only takes "common".
    #[test]
    fn export_stems_tp_trim_off_is_unchanged() {
        let (mut e, hot) = hot_stems("beatbox_stems_tp_off");
        let pin = json!({hot.clone(): 0.0});
        let a = e.call("export_stems", &json!({"dir": "out/a", "tracks": [hot.clone()], "stem_gain_db": pin, "stem_format": "f32"})).unwrap();
        let x = stem(&a, &hot);
        assert!(a.get("tp_trim").is_none(), "{a}");
        assert!(x.get("true_peak_before_dbtp").is_none(), "{x}");
        assert_eq!(x["gain_source"], "pinned");
        assert_eq!(x["over_ceiling"], true, "no trim without tp_trim: {x}");
        // all stems under the ceiling: same files with and without tp_trim
        for t in e.project.tracks.iter_mut() {
            t.volume_db -= 24.0;
        }
        let off = e
            .call(
                "export_stems",
                &json!({"dir": "out/off", "stem_format": "f32"}),
            )
            .unwrap();
        let on = e
            .call(
                "export_stems",
                &json!({"dir": "out/on", "stem_format": "f32", "tp_trim": "common"}),
            )
            .unwrap();
        assert_eq!(on["tp_trim"]["trims"], 0, "{}", on["tp_trim"]);
        assert_eq!(on["tp_trim"]["converged"], true);
        let so = off["stems"].as_array().unwrap();
        assert!(!so.is_empty());
        for x in so {
            assert!(x["true_peak_dbtp"].as_f64().unwrap() <= 0.0, "{x}");
            assert!(x["gain_steps_db"].as_array().unwrap().is_empty(), "{x}");
            let y = stem(&on, x["name"].as_str().unwrap());
            let fx = std::fs::read(x["path"].as_str().unwrap()).unwrap();
            let fy = std::fs::read(y["path"].as_str().unwrap()).unwrap();
            assert!(fx == fy, "{} differs with tp_trim at 0 trims", x["name"]);
        }
        assert!(
            e.call(
                "export_stems",
                &json!({"dir": "out/x", "tp_trim": "common"})
            )
            .is_err(),
            "tp_trim needs f32"
        );
        assert!(e
            .call(
                "export_stems",
                &json!({"dir": "out/x", "stem_format": "f32", "tp_trim": "per_stem"})
            )
            .is_err());
    }

    /// (b) Synthetic stems with inter-sample peaks at +2 dBTP (a tone at fs/4,
    /// 45 degrees, whose samples sit 3 dB under its true peak; one at 0.45 fs)
    /// written as float files: the common trim brings the hottest to 0.1 dB
    /// under 0 dBTP, measured on the files read back, in one trim.
    #[test]
    fn tp_trim_common_converges_on_inter_sample_peaks() {
        use std::f32::consts::{PI, TAU};
        let dir =
            std::env::temp_dir().join(format!("beatbox_tp_trim_synth_{}", std::process::id()));
        let sr = super::SR;
        let n = sr as usize;
        let tone = |f: f32, ph: f32, db: f32| -> Vec<f32> {
            let a = super::db_to_gain(db);
            (0..n)
                .map(|i| a * (TAU * f * i as f32 / sr + ph).sin())
                .collect()
        };
        let stems = [
            tone(sr / 4.0, PI / 4.0, 2.0),
            tone(0.45 * sr, 0.3, 2.0),
            tone(1000.0, 0.0, -6.0),
        ];
        let sample_peak = stems[0].iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(
            crate::dsp::gain_to_db(sample_peak) < -0.9,
            "the fs/4 stem's samples sit under 0 dBFS"
        );
        let o = super::DeliverOpts {
            bits: 32,
            dither: false,
            limit: false,
            ceiling_dbtp: 0.0,
            ..Default::default()
        };
        let paths: Vec<std::path::PathBuf> = (0..stems.len())
            .map(|i| dir.join(format!("s{i}.wav")))
            .collect();
        let t = super::common_tp_trim(0.0, super::TP_TRIM_PASSES, |extra| {
            stems
                .iter()
                .zip(&paths)
                .map(|(x, p)| {
                    let v = super::deliver(
                        x,
                        x,
                        p,
                        &super::DeliverOpts {
                            fixed_gain_db: Some(extra.to_vec()),
                            ..o.clone()
                        },
                    )?;
                    assert!(v["warnings"].as_array().unwrap().is_empty(), "{v}");
                    Ok(super::DecodedOutput::read(p)?.true_peak_db())
                })
                .collect()
        })
        .unwrap();
        assert!(
            t.before[0] > 1.8 && t.before[0] < 2.2,
            "the meter sees the +2 dBTP inter-sample peak: {:?}",
            t.before
        );
        assert!(t.before[1] > 0.0, "{:?}", t.before);
        assert!(t.converged, "{t:?}");
        assert_eq!(t.steps.len(), 1, "float scales exactly, so one trim: {t:?}");
        assert_eq!(t.measures, 2);
        let hottest = t.after.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (hottest + 0.1).abs() < 0.02,
            "aims 0.1 dB under the ceiling: {hottest}"
        );
        for (i, p) in paths.iter().enumerate() {
            let d = super::DecodedOutput::read(p).unwrap();
            assert!(
                d.true_peak_db() <= 0.0,
                "s{i} read back at {}",
                d.true_peak_db()
            );
            assert!((d.true_peak_db() - t.after[i]).abs() < 1e-6);
            // every stem moved by the same trim
            assert!(
                (t.after[i] - t.before[i] - t.steps[0]).abs() < 0.01,
                "{t:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (c) On a real export (float stems at pinned gains, as IWBY v6 writes
    /// them): the hot stem meets 0 dBTP, every stem moves by exactly the same
    /// gain (stem-to-stem balance within 1e-4 dB), and the report's pin map
    /// rebuilds the trimmed files bit for bit.
    #[test]
    fn export_stems_tp_trim_common_keeps_the_balance() {
        let (mut e, hot) = hot_stems("beatbox_stems_tp_common");
        let raw = e
            .call(
                "export_stems",
                &json!({"dir": "out/r", "stem_format": "f32"}),
            )
            .unwrap();
        let mut pins = serde_json::Map::new();
        for x in raw["stems"].as_array().unwrap() {
            pins.insert(x["name"].as_str().unwrap().to_string(), json!(0.0));
        }
        let pinned = e
            .call(
                "export_stems",
                &json!({"dir": "out/p", "stem_format": "f32", "stem_gain_db": pins}),
            )
            .unwrap();
        let tp = e.call("export_stems", &json!({"dir": "out/t", "stem_format": "f32", "stem_gain_db": pins, "tp_trim": "common"})).unwrap();
        let rep = &tp["tp_trim"];
        assert_eq!(rep["converged"], true, "{rep}");
        assert_eq!(rep["mode"], "common");
        assert_eq!(rep["hottest_before"]["stem"], hot.as_str(), "{rep}");
        assert!(
            rep["hottest_before"]["true_peak_dbtp"].as_f64().unwrap() > 0.5,
            "{rep}"
        );
        assert!(
            rep["trims"].as_u64().unwrap() >= 1 && rep["trims"].as_u64().unwrap() <= 2,
            "{rep}"
        );
        let after = rep["hottest_after"]["true_peak_dbtp"].as_f64().unwrap();
        assert!(after <= 0.0 && after > -0.2, "{rep}");
        assert!(tp["warnings"].as_array().unwrap().is_empty(), "{tp}");
        assert!(rep["trim_db"].as_f64().unwrap() < -0.5, "{rep}");
        // the exact common gain: every trim step as applied (f32)
        let trim: f64 = rep["trim_steps_db"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g.as_f64().unwrap())
            .sum();
        let mut moved: Vec<f64> = Vec::new();
        let mut checked = 0;
        for x in tp["stems"].as_array().unwrap() {
            let name = x["name"].as_str().unwrap();
            assert_eq!(x["over_ceiling"], false, "{x}");
            assert_eq!(x["gain_source"], "pinned");
            assert!(x["true_peak_dbtp"].as_f64().unwrap() <= 0.0, "{x}");
            assert!(x["true_peak_before_dbtp"].is_number(), "{x}");
            let (a, b) = (
                energy(&f32_file(stem(&pinned, name)["path"].as_str().unwrap())),
                energy(&f32_file(x["path"].as_str().unwrap())),
            );
            if a > 0.0 {
                let d = 10.0 * (b / a).log10();
                assert!(
                    (d - trim).abs() < 1e-4,
                    "{name}: moved {d:+.6} dB, common trim {trim:+.6}"
                );
                moved.push(d);
                checked += 1;
            }
        }
        assert!(checked >= 2, "balance checked on {checked} stems");
        // stem-to-stem balance: every stem moved by the same gain
        let spread = moved.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
            - moved.iter().cloned().fold(f64::INFINITY, f64::min);
        assert!(spread < 1e-4, "stems moved by different gains: {moved:?}");
        let re = e.call("export_stems", &json!({"dir": "out/re", "stem_format": "f32", "stem_gain_db": tp["stem_gain_db_pin"]})).unwrap();
        for x in tp["stems"].as_array().unwrap() {
            let fx = std::fs::read(x["path"].as_str().unwrap()).unwrap();
            let fy = std::fs::read(
                stem(&re, x["name"].as_str().unwrap())["path"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            assert!(fx == fy, "pinned rebuild of {} differs", x["name"]);
        }
        // unpinned stems are taken as mixed (0 dB), then trimmed together
        let mixed = e
            .call(
                "export_stems",
                &json!({"dir": "out/m", "stem_format": "f32", "tp_trim": "common"}),
            )
            .unwrap();
        assert_eq!(mixed["tp_trim"]["converged"], true);
        for x in mixed["stems"].as_array().unwrap() {
            assert_eq!(x["gain_source"], "as_mixed", "{x}");
            assert!(x["true_peak_dbtp"].as_f64().unwrap() <= 0.0, "{x}");
        }
    }

    /// (e) A meter that never comes down: the loop gives up after 12 trims
    /// (converged false) instead of looping forever; under the ceiling it
    /// takes no trim; a failed write is an error.
    #[test]
    fn tp_trim_gives_up_after_12_trims() {
        let mut calls = 0;
        let t = super::common_tp_trim(0.0, super::TP_TRIM_PASSES, |_| {
            calls += 1;
            Ok(vec![3.0, -1.0])
        })
        .unwrap();
        assert!(!t.converged);
        assert_eq!(super::TP_TRIM_PASSES, 12);
        assert_eq!(t.steps.len(), 12);
        assert_eq!(t.measures, 13);
        assert_eq!(calls, 13);
        assert!(
            t.steps.iter().all(|s| (*s + 3.1).abs() < 1e-6),
            "{:?}",
            t.steps
        );
        let t = super::common_tp_trim(0.0, 12, |_| Ok(vec![-3.0, f32::NEG_INFINITY])).unwrap();
        assert!(
            t.converged && t.steps.is_empty() && t.measures == 1,
            "{t:?}"
        );
        assert!(super::common_tp_trim(0.0, 12, |_| Err(anyhow::anyhow!("write failed"))).is_err());
    }

    /// Independent check of a delivered file: decode at its own rate.
    fn file_stats(path: &std::path::Path) -> (u32, f64, f64, f64) {
        let d = super::DecodedOutput::read(path).unwrap();
        // cross-check the native-rate loudness against an explicit
        // conversion to the engine rate and the 44.1 kHz meter
        let l44 = crate::resample::resample(&d.left, d.sample_rate, 44_100);
        let r44 = crate::resample::resample(&d.right, d.sample_rate, 44_100);
        let via44 = crate::analysis::loudness(&l44, &r44).integrated_lufs as f64;
        let native = d.loudness().integrated_lufs as f64;
        assert!(
            (via44 - native).abs() < 0.3,
            "native {native} vs resampled {via44}"
        );
        (
            d.sample_rate,
            d.seconds() as f64,
            d.true_peak_db() as f64,
            native,
        )
    }

    #[test]
    fn export_48k_wav_is_measured_from_the_file() {
        let mut e = eng("beatbox_delivery_48k_wav");
        let base = e.call("export_audio", &json!({"format": "wav", "bits": 24, "target_lufs": -10, "path": "out/b44.wav", "section": 0})).unwrap();
        let w = e.call("export_audio", &json!({"format": "wav", "bits": 24, "sample_rate": 48000, "target_lufs": -10, "path": "out/b48.wav", "section": 0})).unwrap();
        assert_eq!(w["passed"], true, "{w}");
        assert_eq!(w["decoded"]["sample_rate"], 48000, "{w}");
        // duration comes from the output frames at the output rate: the same
        // window must last the same time at 44.1 and 48 kHz
        let (s44, s48) = (
            base["seconds"].as_f64().unwrap(),
            w["seconds"].as_f64().unwrap(),
        );
        assert!((s44 - s48).abs() <= 0.011, "44.1k {s44}s vs 48k {s48}s");
        let path = e.resolve("out/b48.wav");
        let (rate, secs, tp, lufs) = file_stats(&path);
        assert_eq!(rate, 48000);
        assert!((secs - s48).abs() <= 0.01, "{secs} vs reported {s48}");
        assert!(tp <= -0.95, "decoded 48k true peak {tp}");
        assert!(
            (w["after"]["true_peak_dbtp"].as_f64().unwrap() - tp).abs() < 0.02,
            "{w}"
        );
        assert!(
            (w["after"]["integrated_lufs"].as_f64().unwrap() - lufs).abs() < 0.05,
            "{w}"
        );
        assert!((lufs + 10.0).abs() < 1.0, "{lufs}");
    }

    #[test]
    fn export_48k_mp3_catches_codec_overshoot() {
        if !super::ffmpeg_available() {
            eprintln!("ffmpeg not on PATH: skipping 48 kHz mp3 delivery test");
            return;
        }
        let mut e = eng("beatbox_delivery_48k_mp3");
        // a hot master: the limiter sits at -1.5 dBTP and LAME overshoots it
        // (-9 LUFS: since the EQ carve the lofi loop has less low-mid energy
        // and -8 is past what the limiter reaches in four passes)
        let m = e.call("export_audio", &json!({"format": "mp3", "sample_rate": 48000, "target_lufs": -9, "true_peak_ceiling": -1, "path": "out/c48.mp3", "section": 0})).unwrap();
        assert_eq!(m["decoded"]["sample_rate"], 48000, "{m}");
        let path = e.resolve("out/c48.mp3");
        let (rate, secs, tp, lufs) = file_stats(&path);
        assert_eq!(rate, 48000);
        // the independently decoded MP3 meets the ceiling and matches the report
        assert!(tp <= -0.95, "decoded 48k mp3 true peak {tp}: {m}");
        assert!(
            (m["after"]["true_peak_dbtp"].as_f64().unwrap() - tp).abs() < 0.02,
            "{m}"
        );
        assert!(
            (m["after"]["integrated_lufs"].as_f64().unwrap() - lufs).abs() < 0.05,
            "{m}"
        );
        assert_eq!(m["passed"], true, "{m}");
        // duration from the decoded frames at 48 kHz (MP3 adds encoder
        // delay/padding, a few tens of ms)
        let w = e
            .call(
                "export_audio",
                &json!({"format": "wav", "path": "out/c44.wav", "section": 0}),
            )
            .unwrap();
        let s44 = w["seconds"].as_f64().unwrap();
        assert!((secs - m["seconds"].as_f64().unwrap()).abs() <= 0.01, "{m}");
        assert!((secs - s44).abs() < 0.1, "mp3 {secs}s vs wav {s44}s");
    }

    #[test]
    fn master_assistant_hits_target_and_reference_compare() {
        let mut e = eng("beatbox_master_tests");
        let r = e
            .call(
                "master_assistant",
                &json!({"target_lufs": -11, "style": "punchy"}),
            )
            .unwrap();
        assert_eq!(r["reached_target"], true, "{r}");
        assert_eq!(
            e.project.master_effects.last().unwrap().type_name(),
            "limiter"
        );
        e.call("export_audio", &json!({"path": "ref.wav", "bits": 24}))
            .unwrap();
        let p = e
            .call("analyze_reference", &json!({"path": "ref.wav"}))
            .unwrap();
        assert!(p["integrated_lufs"].as_f64().unwrap() > -13.0, "{p}");
        let c = e
            .call("compare_to_reference", &json!({"reference": "ref.wav"}))
            .unwrap();
        assert!(c["score"].as_u64().unwrap() >= 90, "{c}");
    }
}
