//! AI ears v2 (from AI_EARS_RESEARCH.md P0/P1): render ids bound to a
//! project hash, loudness_report (I / LRA / PLR / per-section PSR / codec
//! true peak / platform gain), masking_matrix on ERB bands (directional,
//! time-gated, with fix calls), groove_analysis and hook_analysis on the
//! exact MIDI, structure (section similarity + labels), and the ears_report
//! digest + diff_renders that close the revise loop.

use crate::analysis;
use crate::dsp::SR;
use crate::engine::Engine;
use crate::project::{Note, Project, STEPS_PER_BAR};
use crate::render::{self, RenderOptions};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------- render ids

/// FNV-1a hash of the project JSON: same project, same id.
pub fn project_hash(p: &Project) -> String {
    let s = serde_json::to_string(p).unwrap_or_default();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("r{:012x}", h & 0xffff_ffff_ffff)
}

/// Compact numbers kept per render id so diff_renders can compare.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RenderSummary {
    pub render_id: String,
    pub integrated_lufs: f32,
    pub true_peak_dbtp: f32,
    pub lra_lu: f32,
    pub plr_db: f32,
    pub psr_min_db: f32,
    pub sections: Vec<(String, f32)>,
    pub bands: Vec<(String, f32)>,
    pub correlation: f32,
    pub masking: Vec<(String, f32)>,
    pub technical: f32,
    pub musical: f32,
    pub findings: Vec<String>,
}

fn store() -> &'static Mutex<BTreeMap<String, RenderSummary>> {
    static S: OnceLock<Mutex<BTreeMap<String, RenderSummary>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub fn remember(s: RenderSummary) {
    if let Ok(mut m) = store().lock() {
        if m.len() > 64 {
            if let Some(k) = m.keys().next().cloned() {
                m.remove(&k);
            }
        }
        m.insert(s.render_id.clone(), s);
    }
}

pub fn recall(id: &str) -> Option<RenderSummary> {
    store().lock().ok().and_then(|m| m.get(id).cloned())
}

fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}
fn r2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}

/// Mix role of a track (kick, bass, snare, hats, perc, lead, keys, pad, fx).
pub fn track_role(p: &Project, name: &str) -> &'static str {
    match p.track_index(name) {
        Ok(i) => crate::tools_mix::role_of(name, &p.tracks[i].instrument),
        Err(_) => "other",
    }
}

// ---------------------------------------------------------------- loudness

fn st_series(l: &[f32], r: &[f32]) -> Vec<f32> {
    let win = (3.0 * SR) as usize;
    let hop = SR as usize;
    let mut out = Vec::new();
    let mut i = 0;
    while i + win <= l.len().min(r.len()) {
        out.push(analysis::loudness(&l[i..i + win], &r[i..i + win]).integrated_lufs);
        i += hop;
    }
    out
}

pub fn loudness_report(e: &mut Engine, codec: bool) -> Result<Value> {
    let m = e.mix()?;
    let (l, r) = (&m.left, &m.right);
    let lo = analysis::loudness(l, r);
    let peak = l.iter().chain(r.iter()).fold(0.0f32, |a, x| a.max(x.abs()));
    let peak_db = 20.0 * peak.max(1e-9).log10();
    let plr = lo.true_peak_dbtp - lo.integrated_lufs;
    let st = st_series(l, r);
    let spans = crate::ears::spans(&e.project);
    let mut per = Vec::new();
    let mut psr_min = f32::MAX;
    for s in &spans {
        let a = ((s.start_s * SR) as usize).min(l.len());
        let b = ((s.end_s * SR) as usize).min(l.len());
        if b <= a + SR as usize {
            continue;
        }
        let ml = analysis::loudness(&l[a..b], &r[a..b]);
        let psr = ml.true_peak_dbtp - ml.short_term_max_lufs;
        if ml.short_term_max_lufs > -60.0 {
            psr_min = psr_min.min(psr);
        }
        per.push(json!({"section": s.pattern, "index": s.index, "integrated_lufs": r1(ml.integrated_lufs), "short_term_max_lufs": r1(ml.short_term_max_lufs), "true_peak_dbtp": r1(ml.true_peak_dbtp), "psr_db": r1(psr), "lra_lu": r1(ml.loudness_range_lu)}));
    }
    let mut findings = Vec::new();
    if psr_min < 8.0 && psr_min != f32::MAX {
        findings.push(json!({"severity": 0.5, "message": format!("PSR {psr_min:.1} dB in the loudest section (< 8): squashed transients"), "suggested_call": {"tool": "master_assistant", "args": {"target_lufs": r1(lo.integrated_lufs - 1.0), "style": "punchy"}}}));
    }
    let tp_ceiling = if lo.integrated_lufs > -14.0 {
        -2.0
    } else {
        -1.0
    };
    if lo.true_peak_dbtp > tp_ceiling {
        findings.push(json!({"severity": 0.4, "message": format!("true peak {:.2} dBTP above {tp_ceiling} (lossy codecs overshoot at this loudness)", lo.true_peak_dbtp), "suggested_call": {"tool": "export_audio", "args": {"format": "mp3", "true_peak_ceiling": -1.0}}}));
    }
    let codec_v = if codec && crate::tools_delivery::ffmpeg_available() {
        codec_true_peak(l, r).unwrap_or_else(|err| json!({"error": err.to_string()}))
    } else {
        Value::Null
    };
    let spotify_gain = -14.0 - lo.integrated_lufs;
    Ok(json!({
        "render_id": project_hash(&e.project),
        "integrated_lufs": r1(lo.integrated_lufs), "lra_lu": r1(lo.loudness_range_lu),
        "max_short_term_lufs": r1(lo.short_term_max_lufs), "true_peak_dbtp": r2(lo.true_peak_dbtp), "sample_peak_dbfs": r2(peak_db),
        "plr_db": r1(plr), "psr_min_db": if psr_min == f32::MAX { Value::Null } else { json!(r1(psr_min)) },
        "short_term_series_1s": st.iter().map(|x| r1(*x)).collect::<Vec<_>>(),
        "per_section": per,
        "codec": codec_v,
        "platform_playback": [
            {"platform": "spotify_normal (-14)", "gain_db": r1(spotify_gain), "limited": spotify_gain > 0.0 && lo.true_peak_dbtp + spotify_gain > -1.0},
            {"platform": "apple_music (-16)", "gain_db": r1(-16.0 - lo.integrated_lufs)},
            {"platform": "youtube (-14, down only)", "gain_db": r1(spotify_gain.min(0.0))},
        ],
        "findings": findings,
    }))
}

fn codec_true_peak(l: &[f32], r: &[f32]) -> Result<Value> {
    let dir = std::env::temp_dir().join(format!("beatbox_codec_{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let (wav, mp3, back) = (dir.join("a.wav"), dir.join("a.mp3"), dir.join("b.wav"));
    render::write_wav(&wav, l, r)?;
    let run = |args: &[&std::ffi::OsStr]| -> Result<bool> {
        Ok(std::process::Command::new("ffmpeg")
            .args(args)
            .status()?
            .success())
    };
    let ok = run(&[
        "-y".as_ref(),
        "-loglevel".as_ref(),
        "error".as_ref(),
        "-i".as_ref(),
        wav.as_os_str(),
        "-b:a".as_ref(),
        "320k".as_ref(),
        mp3.as_os_str(),
    ])? && run(&[
        "-y".as_ref(),
        "-loglevel".as_ref(),
        "error".as_ref(),
        "-i".as_ref(),
        mp3.as_os_str(),
        back.as_os_str(),
    ])?;
    if !ok {
        return Err(anyhow!("ffmpeg round trip failed"));
    }
    let (bl, br) = crate::samples::decode_stereo(&back)?;
    let tp = analysis::true_peak(&bl).max(analysis::true_peak(&br));
    let overs = bl
        .iter()
        .chain(br.iter())
        .filter(|x| x.abs() >= 1.0)
        .count();
    let _ = std::fs::remove_dir_all(&dir);
    Ok(
        json!([{"codec": "mp3_320", "true_peak_dbtp": r2(20.0 * tp.max(1e-9).log10()), "overs": overs}]),
    )
}

// ---------------------------------------------------------------- masking

const NB: usize = 40;

fn erb_edges() -> Vec<f32> {
    // 40 bands equally spaced on the ERB-rate scale, 30 Hz .. 16 kHz
    let erb = |f: f32| 21.4 * (1.0 + 0.00437 * f).log10();
    let inv = |e: f32| (10f32.powf(e / 21.4) - 1.0) / 0.00437;
    let (a, b) = (erb(30.0), erb(16000.0));
    (0..=NB)
        .map(|i| inv(a + (b - a) * i as f32 / NB as f32))
        .collect()
}

/// Per-frame ERB band energies (dB) of a mono signal.
fn band_frames(x: &[f32], edges: &[f32]) -> Vec<[f32; NB]> {
    const N: usize = 2048;
    let hop = 1024;
    let win: Vec<f32> = (0..N)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos())
        .collect();
    let bins: Vec<(usize, usize)> = (0..NB)
        .map(|b| {
            let lo = ((edges[b] / SR * N as f32) as usize).max(1);
            let hi = ((edges[b + 1] / SR * N as f32) as usize)
                .max(lo + 1)
                .min(N / 2);
            (lo, hi)
        })
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + N <= x.len() {
        let mut re: Vec<f32> = (0..N).map(|k| x[i + k] * win[k]).collect();
        let mut im = vec![0.0f32; N];
        analysis::fft(&mut re, &mut im);
        let mut f = [0.0f32; NB];
        for (b, (lo, hi)) in bins.iter().enumerate() {
            let mut en = 0.0;
            for k in *lo..*hi {
                en += re[k] * re[k] + im[k] * im[k];
            }
            f[b] = 10.0 * (en / (*hi - *lo) as f32 + 1e-12).log10();
        }
        out.push(f);
        i += hop;
    }
    out
}

#[derive(Clone, Debug, Serialize)]
pub struct MaskPair {
    pub masker: String,
    pub maskee: String,
    pub masking_db: f32,
    pub active_ratio: f32,
    pub worst_hz: [f32; 2],
    pub fix: Value,
}

fn priority(role: &str) -> i32 {
    match role {
        "kick" => 0,
        "bass" => 1,
        "lead" | "snare" => 2,
        "keys" => 4,
        "perc" => 4,
        "hats" | "cymbal" => 5,
        "pad" => 6,
        "fx" => 7,
        _ => 4,
    }
}

/// Directional masking between track stems in one section (default the
/// busiest). For every frame where the maskee plays, its important bands
/// (within 12 dB of its own frame max) count as masked by the amount the
/// masker exceeds them (masker-to-signal ratio, with a 6 dB spreading
/// allowance). Fixes cut the lower-priority track (kick > bass > lead/snare
/// > keys/perc > hats > pads > fx), or sidechain for kick vs bass.
pub fn masking(
    e: &mut Engine,
    section: Option<&str>,
    top_k: usize,
) -> Result<(Vec<MaskPair>, String)> {
    let p = e.project.clone();
    let spans = crate::ears::spans(&p);
    if spans.is_empty() {
        return Ok((Vec::new(), String::new()));
    }
    let span = match section {
        Some(s) => spans
            .iter()
            .find(|x| x.pattern == s || x.index.to_string() == s)
            .ok_or_else(|| anyhow!("no section '{s}'"))?,
        None => spans
            .iter()
            .max_by_key(|s| {
                p.pattern_index(&s.pattern)
                    .map(|i| p.patterns[i].clips.len())
                    .unwrap_or(0)
                    * 100
                    + s.index
            })
            .unwrap(),
    };
    let a = (span.start_beat * 4.0).round() as u32;
    let b = (span.end_beat * 4.0).round() as u32;
    e.bank.sync(&p.samples);
    let m = render::render(
        &p,
        &e.bank,
        &RenderOptions {
            keep_stems: true,
            tail: 0.2,
            step_range: Some((a, b.min(a + 16 * STEPS_PER_BAR))),
            ..Default::default()
        },
    )?;
    let edges = erb_edges();
    let frames: Vec<(String, Vec<[f32; NB]>)> = m
        .stems
        .iter()
        .map(|s| {
            let mono: Vec<f32> = s
                .left
                .iter()
                .zip(&s.right)
                .map(|(a, b)| 0.5 * (a + b))
                .collect();
            (s.name.clone(), band_frames(&mono, &edges))
        })
        .filter(|(_, f)| !f.is_empty())
        .collect();
    drop(m);
    let mut pairs = Vec::new();
    for (bi, (bn, bf)) in frames.iter().enumerate() {
        for (ai, (an, af)) in frames.iter().enumerate() {
            if ai == bi {
                continue;
            }
            let n = af.len().min(bf.len());
            let (mut sum, mut active, mut cnt) = (0.0f32, 0usize, 0usize);
            let mut per_band = [0.0f32; NB];
            for t in 0..n {
                let fmax = bf[t].iter().cloned().fold(-200.0, f32::max);
                if fmax < -70.0 {
                    continue;
                }
                active += 1;
                for k in 0..NB {
                    if bf[t][k] < fmax - 12.0 || bf[t][k] < -80.0 {
                        continue;
                    }
                    let msr = af[t][k] - bf[t][k] + 6.0;
                    if msr > 0.0 {
                        sum += msr;
                        per_band[k] += msr;
                    }
                    cnt += 1;
                }
            }
            if active == 0 || cnt == 0 {
                continue;
            }
            let masking_db = sum / cnt as f32;
            if masking_db < 1.0 {
                continue;
            }
            let wb = (0..NB)
                .max_by(|x, y| per_band[*x].partial_cmp(&per_band[*y]).unwrap())
                .unwrap_or(0);
            let (lo, hi) = (edges[wb], edges[(wb + 2).min(NB)]);
            let centre = (lo * hi).sqrt();
            let (ra, rb) = (track_role(&p, an), track_role(&p, bn));
            let fix = if (ra == "kick" && rb == "bass") || (ra == "bass" && rb == "kick") {
                let (bass, kick) = if ra == "bass" { (an, bn) } else { (bn, an) };
                json!({"tool": "add_effect", "args": {"track": bass, "type": "sidechain", "params": {"source": kick, "amount": 0.6}}, "why": "kick and bass share the sub: duck the bass under the kick"})
            } else {
                let (cut, keep) = if priority(ra) >= priority(rb) {
                    (an, bn)
                } else {
                    (bn, an)
                };
                json!({"tool": "add_effect", "args": {"track": cut, "type": "parametric_eq", "params": {"bands": [{"kind": "bell", "freq": centre.round(), "gain_db": -(masking_db.min(6.0) * 0.6 + 1.0).round(), "q": 1.4}]}}, "why": format!("carve {centre:.0} Hz out of '{cut}' so '{keep}' reads")})
            };
            pairs.push(MaskPair {
                masker: an.clone(),
                maskee: bn.clone(),
                masking_db: r1(masking_db),
                active_ratio: r2(active as f32 / n.max(1) as f32),
                worst_hz: [lo.round(), hi.round()],
                fix,
            });
        }
    }
    pairs.sort_by(|a, b| {
        (b.masking_db * b.active_ratio)
            .partial_cmp(&(a.masking_db * a.active_ratio))
            .unwrap()
    });
    pairs.truncate(top_k.max(1));
    Ok((pairs, span.pattern.clone()))
}

// ---------------------------------------------------------------- symbolic ears

/// Notes of one track per arrangement section, repeats unrolled.
fn song_notes(p: &Project, track: &str) -> Vec<(usize, String, Vec<Note>, f32)> {
    let secs: Vec<(String, u32)> = if p.arrangement.is_empty() {
        p.patterns
            .first()
            .map(|x| vec![(x.name.clone(), 1)])
            .unwrap_or_default()
    } else {
        p.arrangement
            .iter()
            .map(|s| (s.pattern.clone(), s.repeats.max(1)))
            .collect()
    };
    let mut out = Vec::new();
    for (i, (pat, reps)) in secs.iter().enumerate() {
        if let Ok(pi) = p.pattern_index(pat) {
            let len = p.patterns[pi].steps() as f32;
            let mut v = Vec::new();
            for r in 0..*reps {
                for n in p.patterns[pi].notes(track) {
                    let mut m = n.clone();
                    m.start += r as f32 * len;
                    v.push(m);
                }
            }
            out.push((i, pat.clone(), v, len * *reps as f32));
        }
    }
    out
}

fn entropy(counts: &[usize]) -> f32 {
    let t: usize = counts.iter().sum();
    if t == 0 {
        return 0.0;
    }
    -counts
        .iter()
        .filter(|c| **c > 0)
        .map(|c| {
            let p = *c as f32 / t as f32;
            p * p.log2()
        })
        .sum::<f32>()
}

/// Melody features on the exact notes (FANTASTIC-style).
pub fn hook_analysis(p: &Project, track: &str) -> Result<Value> {
    p.track_index(track)?;
    let secs = song_notes(p, track);
    let mut all: Vec<Note> = Vec::new();
    let mut off = 0.0;
    for (_, _, v, len) in &secs {
        all.extend(v.iter().map(|n| {
            let mut m = n.clone();
            m.start += off;
            m
        }));
        off += len;
    }
    if all.len() < 3 {
        return Ok(
            json!({"track": track, "notes": all.len(), "hook_score": 0, "findings": ["too few notes to analyse"]}),
        );
    }
    let key_pc = crate::theory::pitch_class(&p.key_root).unwrap_or(0);
    let iv = crate::theory::scale_intervals(&p.scale)
        .unwrap_or(&[0, 2, 3, 5, 7, 8, 10])
        .to_vec();
    let lo = all.iter().map(|n| n.pitch).min().unwrap();
    let hi = all.iter().map(|n| n.pitch).max().unwrap();
    let in_scale = all
        .iter()
        .filter(|n| iv.contains(&(((n.pitch as i32 - key_pc as i32).rem_euclid(12)) as u8)))
        .count();
    // monophonic line: highest note per onset
    let mut sorted = all.clone();
    sorted.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap()
            .then(b.pitch.cmp(&a.pitch))
    });
    let mut line: Vec<Note> = Vec::new();
    for n in sorted {
        if line
            .last()
            .map(|l| (l.start - n.start).abs() < 0.01)
            .unwrap_or(false)
        {
            continue;
        }
        line.push(n);
    }
    let ints: Vec<i32> = line
        .windows(2)
        .map(|w| w[1].pitch as i32 - w[0].pitch as i32)
        .collect();
    let steps = ints.iter().filter(|i| i.abs() <= 2).count();
    let leaps = ints.iter().filter(|i| i.abs() > 4).count();
    let mut icount = [0usize; 25];
    for i in &ints {
        icount[((*i).clamp(-12, 12) + 12) as usize] += 1;
    }
    let sync = line
        .iter()
        .filter(|n| (n.start.round() as u32) % 2 == 1)
        .count() as f32
        / line.len() as f32;
    let gram = |w: &[Note]| -> String {
        w.windows(2)
            .map(|x| {
                format!(
                    "{}:{}",
                    x[1].pitch as i32 - x[0].pitch as i32,
                    ((x[1].start - x[0].start) * 2.0).round() as i32
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut counts: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    for (_, name, v, _) in &secs {
        let mut v = v.clone();
        v.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap()
                .then(b.pitch.cmp(&a.pitch))
        });
        v.dedup_by(|a, b| (a.start - b.start).abs() < 0.01);
        for w in v.windows(4) {
            let ent = counts.entry(gram(w)).or_insert((0, Vec::new()));
            ent.0 += 1;
            if !ent.1.contains(name) {
                ent.1.push(name.clone());
            }
        }
    }
    let mut motifs: Vec<(String, usize, Vec<String>)> = counts
        .into_iter()
        .map(|(k, (c, s))| (k, c, s))
        .filter(|m| m.1 > 1)
        .collect();
    motifs.sort_by_key(|m| std::cmp::Reverse(m.1));
    let top_rep = motifs.first().map(|m| m.1).unwrap_or(0);
    let mut sec_score: BTreeMap<String, usize> = BTreeMap::new();
    for m in motifs.iter().take(5) {
        for s in &m.2 {
            *sec_score.entry(s.clone()).or_insert(0) += m.1;
        }
    }
    let mut cands: Vec<(String, usize)> = sec_score.into_iter().collect();
    cands.sort_by_key(|c| std::cmp::Reverse(c.1));
    let range = hi as i32 - lo as i32;
    let singable = range <= 12 && ints.iter().all(|i| i.abs() <= 7);
    let mut findings = Vec::new();
    if top_rep < 2 {
        findings
            .push("no motif repeats: a hook needs a short idea stated 2-4x per chorus".to_string());
    }
    if range > 19 {
        findings.push(format!(
            "range {range} semitones is wide for a hook (aim <= 12-15)"
        ));
    }
    if (in_scale as f32) < 0.85 * all.len() as f32 {
        findings.push(format!(
            "only {}% of notes in {} {}",
            100 * in_scale / all.len(),
            p.key_root,
            p.scale
        ));
    }
    if leaps as f32 > 0.4 * ints.len() as f32 {
        findings.push("more than 40% leaps: jumpy, hard to remember".into());
    }
    let nint = ints.len().max(1) as f32;
    let score = (40.0 * (top_rep.min(6) as f32 / 6.0)
        + 20.0 * (steps as f32 / nint)
        + 20.0 * if singable { 1.0 } else { 0.4 }
        + 20.0 * (in_scale as f32 / all.len() as f32))
        .round();
    Ok(json!({
        "track": track, "notes": all.len(),
        "range_semitones": range, "lowest": crate::theory::note_name(lo), "highest": crate::theory::note_name(hi),
        "scale_fit_percent": 100 * in_scale / all.len(),
        "contour": {"step_ratio": r2(steps as f32 / nint), "leap_ratio": r2(leaps as f32 / nint), "interval_entropy_bits": r2(entropy(&icount)), "mean_abs_interval": r2(ints.iter().map(|i| i.abs() as f32).sum::<f32>() / nint)},
        "rhythm": {"notes_per_bar": r2(line.len() as f32 * STEPS_PER_BAR as f32 / off.max(16.0)), "syncopation": r2(sync)},
        "motifs": motifs.iter().take(5).map(|m| json!({"intervals_and_gaps": m.0, "count": m.1, "sections": m.2})).collect::<Vec<_>>(),
        "hook_candidates": cands.iter().take(3).map(|c| json!({"section": c.0, "motif_hits": c.1})).collect::<Vec<_>>(),
        "singable": singable,
        "hook_score": score,
        "findings": findings,
    }))
}

/// Groove on the exact data of drum + bass tracks.
pub fn groove_analysis(p: &Project) -> Value {
    let step_ms = p.step_secs() * 1000.0;
    let mut tracks = Vec::new();
    let mut findings = Vec::new();
    let mut onsets: BTreeMap<&'static str, Vec<f32>> = BTreeMap::new();
    for t in &p.tracks {
        let role = crate::tools_mix::role_of(&t.name, &t.instrument);
        if !matches!(role, "kick" | "snare" | "hats" | "perc" | "cymbal" | "bass") {
            continue;
        }
        let mut notes: Vec<Note> = Vec::new();
        let mut off = 0.0;
        for (_, _, v, len) in song_notes(p, &t.name) {
            notes.extend(v.into_iter().map(|mut m| {
                m.start += off;
                m
            }));
            off += len;
        }
        if notes.is_empty() {
            continue;
        }
        let devs: Vec<f32> = notes
            .iter()
            .map(|n| ((n.start - n.start.round()) + n.offset) * step_ms)
            .collect();
        let mean = devs.iter().sum::<f32>() / devs.len() as f32;
        let sd = (devs.iter().map(|d| (d - mean).powi(2)).sum::<f32>() / devs.len() as f32).sqrt();
        let mut vel = [0.0f32; 16];
        let mut cnt = [0usize; 16];
        for n in &notes {
            let s = (n.start.round() as usize) % 16;
            vel[s] += n.vel;
            cnt[s] += 1;
        }
        let prof: Vec<f32> = (0..16)
            .map(|i| {
                if cnt[i] > 0 {
                    r2(vel[i] / cnt[i] as f32)
                } else {
                    0.0
                }
            })
            .collect();
        let ghosts = notes.iter().filter(|n| n.vel < 0.4).count();
        let mut acc = [0usize; 4];
        for n in &notes {
            acc[((n.vel * 4.0) as usize).min(3)] += 1;
        }
        onsets
            .entry(role)
            .or_default()
            .extend(notes.iter().map(|n| (n.start + n.offset) * step_ms));
        tracks.push(json!({
            "track": t.name, "role": role, "hits": notes.len(),
            "timing_offset_ms": {"mean": r1(mean), "sd": r1(sd)},
            "velocity_profile": prof, "ghost_note_ratio": r2(ghosts as f32 / notes.len() as f32),
            "accent_entropy_bits": r2(entropy(&acc)),
            "hits_per_bar": r1(notes.len() as f32 * 16.0 / off.max(16.0)),
        }));
        if sd > 25.0 {
            findings.push(format!(
                "'{}' timing spread {sd:.0} ms: sloppy rather than human (aim 5-15 ms)",
                t.name
            ));
        }
    }
    let mut flams = Vec::new();
    for (a, b) in [("kick", "bass"), ("snare", "perc")] {
        let (Some(x), Some(y)) = (onsets.get(a), onsets.get(b)) else {
            continue;
        };
        let n = x
            .iter()
            .filter(|t| {
                y.iter()
                    .any(|u| (*t - u).abs() > 5.0 && (*t - u).abs() < 30.0)
            })
            .count();
        if n > 0 {
            flams.push(json!({"roles": [a, b], "count": n}));
            if n > 4 {
                findings.push(format!(
                    "{n} near-miss {a}/{b} hits 5-30 ms apart (read as flams)"
                ));
            }
        }
    }
    json!({"swing": p.swing, "step_ms": r1(step_ms), "tracks": tracks, "flam_risks": flams, "findings": findings})
}

type Sig = BTreeMap<String, Vec<(u32, u8)>>;

fn sim(a: &Sig, b: &Sig) -> f32 {
    let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    if keys.is_empty() {
        return 1.0;
    }
    let mut s = 0.0;
    for k in &keys {
        s += match (a.get(*k), b.get(*k)) {
            (Some(x), Some(y)) => {
                let inter = x.iter().filter(|e| y.contains(e)).count() as f32;
                let uni = (x.len() + y.len()) as f32 - inter;
                if uni == 0.0 {
                    1.0
                } else {
                    inter / uni
                }
            }
            _ => 0.0,
        };
    }
    s / keys.len() as f32
}

/// Arrangement structure from the notes: similarity matrix, A/B/A' labels,
/// distinct ideas, density, "no development" flags.
pub fn structure(p: &Project) -> Value {
    let mut sigs: Vec<(String, Sig, usize)> = Vec::new();
    for s in &p.arrangement {
        if let Ok(pi) = p.pattern_index(&s.pattern) {
            let pat = &p.patterns[pi];
            let mut m = BTreeMap::new();
            let mut n = 0;
            for (t, v) in &pat.clips {
                let mut x: Vec<(u32, u8)> = v
                    .iter()
                    .map(|n| ((n.start * 4.0) as u32, n.pitch))
                    .collect();
                x.sort();
                n += x.len();
                m.insert(t.clone(), x);
            }
            sigs.push((s.pattern.clone(), m, n / pat.bars.max(1) as usize));
        }
    }
    let mut labels: Vec<String> = Vec::new();
    let mut protos: Vec<usize> = Vec::new();
    let mut matrix = Vec::new();
    for i in 0..sigs.len() {
        let row: Vec<f32> = (0..sigs.len())
            .map(|j| r2(sim(&sigs[i].1, &sigs[j].1)))
            .collect();
        let best = protos
            .iter()
            .map(|&k| (k, row[k]))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let label = match best {
            Some((k, s)) if s > 0.95 => labels[k].trim_end_matches('\'').to_string(),
            Some((k, s)) if s > 0.6 => format!("{}'", labels[k].trim_end_matches('\'')),
            _ => {
                protos.push(i);
                ((b'A' + (protos.len() as u8 - 1).min(25)) as char).to_string()
            }
        };
        labels.push(label);
        matrix.push(row);
    }
    let mut findings = Vec::new();
    for i in 1..sigs.len() {
        if matrix[i][i - 1] > 0.98 && sigs[i].0 != sigs[i - 1].0 {
            findings.push(format!(
                "'{}' is identical to the section before it: no development",
                sigs[i].0
            ));
        }
    }
    json!({
        "sections": sigs.iter().zip(&labels).map(|(s, l)| json!({"section": s.0, "label": l, "notes_per_bar": s.2})).collect::<Vec<_>>(),
        "similarity": matrix,
        "distinct_ideas": protos.len(),
        "findings": findings,
    })
}

// ---------------------------------------------------------------- digest + diff

pub fn ears_report(e: &mut Engine, focus_track: Option<&str>) -> Result<Value> {
    let rid = project_hash(&e.project);
    let lr = loudness_report(e, false)?;
    let m = e.mix()?;
    let report = analysis::analyze(&m);
    let sec = crate::ears::analyze_sections(&e.project, &m.left, &m.right, &m.track_info, 3);
    let (arts, _) = crate::ears::detect_artifacts(&m.left, &m.right, &Default::default());
    drop(m);
    let clicks = arts.iter().filter(|a| a.kind == "click").count();
    let (mask, mask_sec) = masking(e, None, 5).unwrap_or_default();
    let lead = focus_track.map(String::from).or_else(|| {
        ["lead", "melody", "chops", "flip"]
            .iter()
            .find(|t| e.project.track_index(t).is_ok())
            .map(|t| t.to_string())
    });
    let hook = lead
        .as_deref()
        .and_then(|t| hook_analysis(&e.project, t).ok());
    let groove = groove_analysis(&e.project);
    let st = structure(&e.project);
    let mut f: Vec<(f32, String, Value)> = Vec::new();
    for x in lr["findings"].as_array().cloned().unwrap_or_default() {
        f.push((
            x["severity"].as_f64().unwrap_or(0.4) as f32,
            x["message"].as_str().unwrap_or("").into(),
            x["suggested_call"].clone(),
        ));
    }
    for pm in mask.iter().filter(|p| p.masking_db >= 3.0) {
        f.push((
            (pm.masking_db / 12.0).min(0.8) * pm.active_ratio.max(0.3),
            format!(
                "'{}' masks '{}' by {:.1} dB around {:.0}-{:.0} Hz in {}",
                pm.masker, pm.maskee, pm.masking_db, pm.worst_hz[0], pm.worst_hz[1], mask_sec
            ),
            pm.fix.clone(),
        ));
    }
    if clicks > 0 {
        f.push((
            0.5,
            format!("{clicks} click(s)"),
            json!({"tool": "detect_artifacts", "args": {"per_track": true}}),
        ));
    }
    for x in sec["flags"].as_array().cloned().unwrap_or_default() {
        let msg = x.as_str().map(String::from).unwrap_or_else(|| {
            x["detail"]
                .as_str()
                .or(x["flag"].as_str())
                .unwrap_or("")
                .to_string()
        });
        f.push((0.45, msg, json!({"tool": "set_section_mix", "args": {}})));
    }
    for s in report.suggestions.iter().take(3) {
        f.push((0.35, s.clone(), Value::Null));
    }
    if let Some(h) = &hook {
        for x in h["findings"].as_array().cloned().unwrap_or_default() {
            f.push((
                0.4,
                format!("hook: {}", x.as_str().unwrap_or("")),
                json!({"tool": "hook_analysis", "args": {"track": lead}}),
            ));
        }
    }
    for x in groove["findings"].as_array().cloned().unwrap_or_default() {
        f.push((
            0.3,
            format!("groove: {}", x.as_str().unwrap_or("")),
            json!({"tool": "groove_analysis", "args": {}}),
        ));
    }
    for x in st["findings"].as_array().cloned().unwrap_or_default() {
        f.push((
            0.4,
            format!("structure: {}", x.as_str().unwrap_or("")),
            json!({"tool": "vary_section", "args": {}}),
        ));
    }
    f.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let tp = lr["true_peak_dbtp"].as_f64().unwrap_or(0.0) as f32;
    let mut technical = report.score as f32;
    if tp > -1.0 {
        technical -= 10.0;
    }
    technical -= (clicks as f32 * 1.5).min(10.0);
    technical -= mask
        .iter()
        .map(|p| (p.masking_db - 3.0).max(0.0) * p.active_ratio)
        .sum::<f32>()
        .min(15.0);
    let distinct = st["distinct_ideas"].as_u64().unwrap_or(1) as f32;
    let mut musical = 60.0 + (distinct.min(4.0) - 1.0) * 6.0;
    if let Some(h) = &hook {
        musical += (h["hook_score"].as_f64().unwrap_or(50.0) as f32 - 50.0) * 0.4;
    }
    musical -= sec["flags"].as_array().map(|v| v.len()).unwrap_or(0) as f32 * 4.0;
    musical -= st["findings"].as_array().map(|v| v.len()).unwrap_or(0) as f32 * 5.0;
    let technical = technical.clamp(0.0, 100.0);
    let musical = musical.clamp(0.0, 100.0);
    let summary = RenderSummary {
        render_id: rid.clone(),
        integrated_lufs: lr["integrated_lufs"].as_f64().unwrap_or(0.0) as f32,
        true_peak_dbtp: tp,
        lra_lu: lr["lra_lu"].as_f64().unwrap_or(0.0) as f32,
        plr_db: lr["plr_db"].as_f64().unwrap_or(0.0) as f32,
        psr_min_db: lr["psr_min_db"].as_f64().unwrap_or(0.0) as f32,
        sections: sec["sections"]
            .as_array()
            .map(|v| {
                v.iter()
                    .map(|s| {
                        (
                            s["pattern"].as_str().unwrap_or("").to_string(),
                            s["metrics"]["short_term_max_lufs"]
                                .as_f64()
                                .unwrap_or(-70.0) as f32,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
        bands: report
            .master
            .bands
            .iter()
            .map(|b| (b.band.to_string(), b.percent))
            .collect(),
        correlation: report.stereo_correlation,
        masking: mask
            .iter()
            .map(|p| (format!("{}>{}", p.masker, p.maskee), p.masking_db))
            .collect(),
        technical,
        musical,
        findings: f.iter().take(7).map(|x| x.1.clone()).collect(),
    };
    remember(summary.clone());
    Ok(json!({
        "render_id": rid,
        "scores": {"technical": r1(technical), "musical": r1(musical), "by_area": {"mix": report.score, "loudness_lufs": summary.integrated_lufs, "true_peak_dbtp": tp, "psr_min_db": summary.psr_min_db, "masking_pairs": mask.len(), "distinct_sections": distinct, "hook_score": hook.as_ref().map(|h| h["hook_score"].clone())}},
        "top_findings": f.iter().take(7).map(|x| json!({"severity": r2(x.0), "message": x.1, "suggested_call": x.2})).collect::<Vec<_>>(),
        "next_best_action": f.iter().find(|x| !x.2.is_null()).map(|x| x.2.clone()),
        "details": {"loudness": lr, "masking": {"section": mask_sec, "pairs": mask}, "hook": hook, "groove": groove, "structure": st},
        "note": "technical and musical scores are kept separate; pass this render_id to diff_renders after your next change",
    }))
}

pub fn diff(a: &RenderSummary, b: &RenderSummary) -> Value {
    let mut improvements = Vec::new();
    let mut regressions = Vec::new();
    let d = |x: f32, y: f32| r2(y - x);
    if b.true_peak_dbtp > -1.0 && a.true_peak_dbtp <= -1.0 {
        regressions.push(format!(
            "true peak now {:.2} dBTP (over -1)",
            b.true_peak_dbtp
        ));
    } else if a.true_peak_dbtp > -1.0 && b.true_peak_dbtp <= -1.0 {
        improvements.push("true peak back under -1 dBTP".to_string());
    }
    if b.psr_min_db + 1.0 < a.psr_min_db {
        regressions.push(format!(
            "PSR fell {:.1} -> {:.1} dB (less punch)",
            a.psr_min_db, b.psr_min_db
        ));
    } else if b.psr_min_db > a.psr_min_db + 1.0 {
        improvements.push(format!(
            "PSR up {:.1} -> {:.1} dB",
            a.psr_min_db, b.psr_min_db
        ));
    }
    let ma: f32 = a.masking.iter().map(|m| m.1).sum();
    let mb: f32 = b.masking.iter().map(|m| m.1).sum();
    if mb + 1.5 < ma {
        improvements.push(format!("masking down {ma:.1} -> {mb:.1} dB (top pairs)"));
    } else if mb > ma + 1.5 {
        regressions.push(format!("masking up {ma:.1} -> {mb:.1} dB"));
    }
    for (name, x, y) in [
        ("technical", a.technical, b.technical),
        ("musical", a.musical, b.musical),
    ] {
        if y > x + 2.0 {
            improvements.push(format!("{name} {x:.0} -> {y:.0}"));
        } else if y + 2.0 < x {
            regressions.push(format!("{name} {x:.0} -> {y:.0}"));
        }
    }
    let sections: Vec<Value> = a
        .sections
        .iter()
        .zip(&b.sections)
        .map(|(x, y)| json!({"section": y.0, "short_term_max_delta_lu": d(x.1, y.1)}))
        .collect();
    let bands: Vec<Value> = a
        .bands
        .iter()
        .zip(&b.bands)
        .map(|(x, y)| json!({"band": y.0, "delta_percent": r1(y.1 - x.1)}))
        .collect();
    let dl = d(a.integrated_lufs, b.integrated_lufs);
    let audible = dl.abs() >= 0.5
        || sections
            .iter()
            .any(|s| s["short_term_max_delta_lu"].as_f64().unwrap_or(0.0).abs() >= 0.5)
        || bands
            .iter()
            .any(|b| b["delta_percent"].as_f64().unwrap_or(0.0).abs() >= 3.0);
    let verdict = match (improvements.is_empty(), regressions.is_empty()) {
        (false, true) => "better",
        (true, false) => "worse",
        (false, false) => "mixed",
        (true, true) if audible => "changed_neutral",
        _ => "no_audible_change",
    };
    json!({
        "a": a.render_id, "b": b.render_id, "verdict": verdict,
        "deltas": {"integrated_lufs": dl, "true_peak_dbtp": d(a.true_peak_dbtp, b.true_peak_dbtp), "lra_lu": d(a.lra_lu, b.lra_lu), "psr_min_db": d(a.psr_min_db, b.psr_min_db), "correlation": d(a.correlation, b.correlation), "technical": d(a.technical, b.technical), "musical": d(a.musical, b.musical), "sections": sections, "bands": bands},
        "improvements": improvements, "regressions": regressions,
        "fixed_findings": a.findings.iter().filter(|x| !b.findings.contains(x)).collect::<Vec<_>>(),
        "new_findings": b.findings.iter().filter(|x| !a.findings.contains(x)).collect::<Vec<_>>(),
    })
}

/// Summary for a render id, a snapshot name or "current" (analysed on demand).
pub fn summary_of(e: &mut Engine, key: &str) -> Result<RenderSummary> {
    if let Some(s) = recall(key) {
        return Ok(s);
    }
    let p = e.version(key).map_err(|_| {
        anyhow!("'{key}' is not a known render_id (run ears_report first) or snapshot name")
    })?;
    let id = project_hash(&p);
    if let Some(s) = recall(&id) {
        return Ok(s);
    }
    let saved = std::mem::replace(&mut e.project, p);
    e.revision += 1;
    let r = ears_report(e, None);
    e.project = saved;
    e.revision += 1;
    r?;
    recall(&id).ok_or_else(|| anyhow!("render summary missing"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erb_edges_monotonic() {
        let e = erb_edges();
        assert_eq!(e.len(), NB + 1);
        assert!(e.windows(2).all(|w| w[1] > w[0]));
        assert!((e[0] - 30.0).abs() < 1.0 && (e[NB] - 16000.0).abs() < 50.0);
    }

    #[test]
    fn hash_changes_with_project() {
        let mut p = Project::new("a", 120.0);
        let h1 = project_hash(&p);
        p.bpm = 121.0;
        assert_ne!(h1, project_hash(&p));
    }

    #[test]
    fn ears_on_a_generated_beat() {
        let d = std::env::temp_dir().join("beatbox_listen_tests");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("generate_beat", &json!({"style": "trap", "seed": 4}))
            .unwrap();
        let r = ears_report(&mut e, None).unwrap();
        assert!(r["scores"]["technical"].as_f64().is_some());
        assert!(r["top_findings"].as_array().is_some());
        let id = r["render_id"].as_str().unwrap().to_string();
        let h = hook_analysis(&e.project, "lead").unwrap();
        assert!(h["notes"].as_u64().unwrap() > 3);
        let g = groove_analysis(&e.project);
        assert!(!g["tracks"].as_array().unwrap().is_empty());
        let s = structure(&e.project);
        assert!(s["distinct_ideas"].as_u64().unwrap() >= 2);
        e.call("set_mixer", &json!({"track": "master", "volume_db": -6.0}))
            .unwrap();
        ears_report(&mut e, None).unwrap();
        let b = recall(&project_hash(&e.project)).unwrap();
        let a = recall(&id).unwrap();
        let df = diff(&a, &b);
        assert!(
            df["deltas"]["integrated_lufs"].as_f64().unwrap() < -2.0,
            "{df}"
        );
        let (pairs, _) = masking(&mut e, None, 5).unwrap();
        for p in pairs {
            assert!(p.masking_db >= 1.0);
        }
    }

    #[test]
    fn masking_finds_two_tones_on_top_of_each_other() {
        // pad and lead playing the same note: the louder masks the quieter
        let d = std::env::temp_dir().join("beatbox_listen_tests2");
        std::fs::create_dir_all(&d).unwrap();
        let mut e = Engine::new(d);
        e.call("new_project", &json!({"name": "m", "bpm": 120}))
            .unwrap();
        e.call(
            "add_track",
            &json!({"name": "pad", "preset": "warm_pad", "volume_db": 0.0}),
        )
        .unwrap();
        e.call(
            "add_track",
            &json!({"name": "lead", "preset": "supersaw", "volume_db": -20.0}),
        )
        .unwrap();
        e.call(
            "add_notes",
            &json!({"track": "pad", "notes": [{"start": 0, "len": 32, "pitch": 60}]}),
        )
        .unwrap();
        e.call(
            "add_notes",
            &json!({"track": "lead", "notes": [{"start": 0, "len": 32, "pitch": 60}]}),
        )
        .unwrap();
        let (pairs, _) = masking(&mut e, None, 4).unwrap();
        let p = pairs
            .iter()
            .find(|p| p.masker == "pad" && p.maskee == "lead")
            .expect("pad masks lead");
        assert!(p.masking_db > 3.0);
        assert_eq!(p.fix["args"]["track"], "pad");
    }
}
