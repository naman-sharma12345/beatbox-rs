//! critique_mix: the audio critic's measurements as one MCP call, so any AI
//! model driving beatbox can ask "does it sound fine?" and get numbers, a
//! 1-10 score, PASS (>= 7) or FAIL, and each fix as a ready-to-send tool call.
//! Ported from the critic agent that judged tonight's renders: loudness and
//! true peak, band balance against a genre reference, splice clicks, held
//! drones, hook-vs-verse contrast per section, and the vocal's level over the
//! beat. These are measurements, not ears.

use crate::dsp::SR;
use crate::engine::Engine;
use crate::render::{self, RenderOptions};
use crate::tools::{obj, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};

const BANDS: [(&str, f32, f32); 6] = [("sub", 20.0, 60.0), ("low", 60.0, 250.0), ("lowmid", 250.0, 800.0), ("mid", 800.0, 2000.0), ("harsh", 2000.0, 5000.0), ("air", 5000.0, 16000.0)];

/// Band power relative to the whole (dB) of typical modern mixes.
fn reference(genre: &str) -> Option<[f32; 6]> {
    match genre {
        "rnb" | "soul" | "pop" => Some([-6.0, -3.5, -10.0, -14.0, -18.0, -22.0]),
        // lo-fi (critic v16, from the beat 9 rounds; a rough profile): a dusty
        // top (air ~5 dB, 2-5 kHz ~2.5 dB under R&B) and a warmer low-mid
        "lofi" => Some([-6.0, -3.5, -8.5, -14.0, -20.5, -27.0]),
        "none" | "solo" => None,
        _ => Some([-4.5, -3.5, -11.0, -15.0, -19.0, -23.0]),
    }
}

/// Band shares (dB of the 20 Hz-16 kHz total) of a mono signal.
pub fn band_shares(mono: &[f32]) -> [f32; 6] {
    let ps = crate::analysis::power_spectrum(mono);
    let hz = SR / (2.0 * ps.len() as f32);
    let sum = |a: f32, b: f32| -> f32 { ps.iter().enumerate().filter(|(i, _)| { let f = *i as f32 * hz; f >= a && f < b }).map(|(_, v)| *v).sum() };
    let tot = sum(20.0, 16000.0).max(1e-20);
    let mut out = [0.0f32; 6];
    for (k, (_, a, b)) in BANDS.iter().enumerate() {
        out[k] = 10.0 * (sum(*a, *b) / tot).max(1e-20).log10();
    }
    out
}

/// Absolute band powers (same scale for every signal), for comparing tracks.
pub fn band_powers(mono: &[f32]) -> [f32; 6] {
    let ps = crate::analysis::power_spectrum(mono);
    let hz = SR / (2.0 * ps.len() as f32);
    let scale = mono.len() as f32;
    let mut out = [0.0f32; 6];
    for (i, v) in ps.iter().enumerate() {
        let f = i as f32 * hz;
        if let Some(k) = BANDS.iter().position(|(_, a, b)| f >= *a && f < *b) {
            out[k] += *v * scale;
        }
    }
    out
}

/// Which tracks own each band: per-track band power as a share (0..1) of
/// the band's total over all tracks, biggest first. Renders the project
/// once with stems (a project mix only).
pub fn band_owners(e: &mut Engine) -> Result<Vec<Vec<(String, f32)>>> {
    let p = e.project.clone();
    e.bank.sync(&p.samples);
    let m = render::render(&p, &e.bank, &RenderOptions { keep_stems: true, ..Default::default() })?;
    let mut per: Vec<(String, [f32; 6])> = Vec::new();
    for s in &m.stems {
        let mono: Vec<f32> = s.left.iter().zip(&s.right).map(|(x, y)| 0.5 * (x + y)).collect();
        per.push((s.name.clone(), band_powers(&mono)));
    }
    let mut out = Vec::new();
    for k in 0..BANDS.len() {
        let tot: f32 = per.iter().map(|(_, b)| b[k]).sum::<f32>().max(1e-20);
        let mut v: Vec<(String, f32)> = per.iter().map(|(n, b)| (n.clone(), b[k] / tot)).collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(3);
        out.push(v);
    }
    Ok(out)
}

/// Click-like transients (seconds): a 1 ms window whose energy above 6 kHz
/// jumps 18 dB over its neighbourhood median and dies within ~4 ms (a hat
/// rings longer). The same rule the critic agent counts with.
pub fn find_clicks(mono: &[f32]) -> Vec<f32> {
    let w = (0.001 * SR) as usize;
    let n = mono.len() / w.max(1);
    if n < 100 {
        return Vec::new();
    }
    let mut hp = crate::dsp::Biquad::new(crate::dsp::BiquadKind::LowCut, 6000.0, 0.7, 0.0);
    let mut hp2 = crate::dsp::Biquad::new(crate::dsp::BiquadKind::LowCut, 6000.0, 0.7, 0.0);
    let h: Vec<f32> = mono.iter().map(|&v| hp2.process(hp.process(v))).collect();
    let e: Vec<f32> = (0..n).map(|i| h[i * w..(i + 1) * w].iter().map(|v| v * v).sum::<f32>() / w as f32 + 1e-12).collect();
    let mut sorted = e.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = sorted[n / 2];
    let mut out = Vec::new();
    let mut last: i64 = -100;
    for i in 0..n {
        if (i as i64) - last < 20 || e[i] < 4.0 * floor {
            continue;
        }
        let lo = i.saturating_sub(25);
        let hi = (i + 26).min(n);
        let mut nb: Vec<f32> = e[lo..hi].to_vec();
        nb.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = nb[nb.len() / 2];
        if e[i] < 63.0 * med {
            continue;
        }
        let after = if i + 8 < n { e[i + 4..i + 8].iter().copied().fold(0.0, f32::max) } else { e[i] };
        if after < 0.15 * e[i] {
            out.push(i as f32 * 0.001);
            last = i as i64;
        }
    }
    out
}

fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

fn kind_of(pattern: &str) -> String {
    pattern.trim_end_matches(|c: char| c.is_ascii_digit() || c == '_').to_string()
}

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "critique_mix",
        description: "Ask an AI listener's measurements whether the song sounds fine: loudness (LUFS, true peak, range), band balance vs a genre reference (sub/low/lowmid/mid/harsh/air), splice clicks, held drones, hook-vs-verse contrast per section, and the vocal's level over the beat (when there is a vocal track). Returns a 1-10 score, PASS (>= 7) or FAIL, the problems in plain words, and each fix as a ready-to-send tool call. A band that is off is fixed on the track that makes it (numbers.bands.<band>.owners lists the top 3 tracks by share), not on the master. path: judge an audio file instead of the project. Measurements, not ears.",
        mutates: false,
        schema: || obj(json!({
            "genre": {"type": "string", "description": "Reference balance: hiphop (default; trap, drill, boom_bap too), rnb (soul, pop), lofi (lo-fi, chill, jazz_rap: darker top, warmer low-mid), none (skip the band check)"},
            "vocal_track": {"type": "string", "description": "Track holding the vocal (default 'vocal' when it exists)"},
            "path": {"type": "string", "description": "Judge this audio file instead of the project mix"},
            "per_track": {"type": "boolean", "description": "Find which track owns an off band and aim the fix at it (default true; one extra render with stems; project mix only)"}
        }), &[]),
        run: |e, a| critique(e, a),
    }]
}

fn critique(e: &mut Engine, a: &Value) -> Result<Value> {
    let genre_in = a.get("genre").and_then(|v| v.as_str()).unwrap_or("hiphop").to_lowercase();
    let genre = match genre_in.as_str() {
        "rnb" | "r&b" | "soul" | "pop" => "rnb",
        "lofi" | "lo-fi" | "lo_fi" | "chill" | "jazz_rap" | "study" => "lofi",
        "none" | "solo" => "none",
        _ => "hiphop",
    };
    let from_file = a.get("path").and_then(|v| v.as_str()).map(|s| e.resolve(s));
    let (l, r): (Vec<f32>, Vec<f32>) = match &from_file {
        Some(p) => {
            if !p.exists() {
                bail!("no file at {}", p.display());
            }
            let x = crate::samples::decode_file(p)?;
            (x.clone(), x)
        }
        None => {
            let m = e.mix()?;
            (m.left.clone(), m.right.clone())
        }
    };
    if l.len() < (SR * 2.0) as usize {
        bail!("less than 2 seconds of audio to judge");
    }
    let mono: Vec<f32> = l.iter().zip(&r).map(|(x, y)| 0.5 * (x + y)).collect();
    let mut score = 10.0f32;
    let mut problems: Vec<String> = Vec::new();
    let mut fixes: Vec<Value> = Vec::new();

    // 1. loudness
    let ld = crate::analysis::loudness(&l, &r);
    if (ld.integrated_lufs + 14.0).abs() > 1.0 {
        // a project mix is mastered at export; only a finished file is judged on it
        if from_file.is_some() {
            score -= 0.5;
        }
        problems.push(format!("integrated loudness {:.1} LUFS (streaming target -14; export_audio masters to it)", ld.integrated_lufs));
        fixes.push(json!({"why": "master to -14 LUFS", "tool": "export_audio", "args": {"path": "song.mp3", "target_lufs": -14.0, "true_peak_ceiling": -1.0}}));
    }
    if ld.true_peak_dbtp > -1.0 {
        score -= 1.0;
        problems.push(format!("true peak {:.1} dBTP (over -1: clips on phones and in encoding)", ld.true_peak_dbtp));
        fixes.push(json!({"why": "peak ceiling", "tool": "add_effect", "args": {"track": "master", "type": "limiter", "params": {"ceiling_db": -1.2}}}));
    }
    let clipped = mono.iter().filter(|v| v.abs() >= 0.999).count();
    if clipped > 50 {
        score -= 1.0;
        problems.push(format!("{clipped} samples at full scale (clipping)"));
    }

    // 2. band balance
    let shares = band_shares(&mono);
    let mut bands = serde_json::Map::new();
    // a band that is off is fixed on the track that makes it (critic, beat 8:
    // the bell lead owned 53% of 2-5 kHz, the clap 53% of 800 Hz-2 kHz), not
    // with a master EQ that dulls everything else in the band too
    let per_track = a.get("per_track").and_then(|v| v.as_bool()).unwrap_or(true);
    let any_off = reference(genre).map(|rf| (0..6).any(|k| (shares[k] - rf[k]).abs() > 4.0)).unwrap_or(false);
    let owners = if from_file.is_none() && per_track && any_off { band_owners(e).ok() } else { None };
    if let Some(rf) = reference(genre) {
        let mut off = 0.0f32;
        for (k, (name, a0, b0)) in BANDS.iter().enumerate() {
            let d = shares[k] - rf[k];
            let own: Vec<Value> = owners.as_ref().map(|o| o[k].iter().map(|(t, s)| json!({"track": t, "share_pct": (s * 100.0).round()})).collect()).unwrap_or_default();
            if own.is_empty() {
                bands.insert(name.to_string(), json!({"share_db": r1(shares[k]), "vs_ref_db": r1(d)}));
            } else {
                bands.insert(name.to_string(), json!({"share_db": r1(shares[k]), "vs_ref_db": r1(d), "owners": own}));
            }
            if d.abs() > 4.0 {
                off += 0.5 + 0.12 * (d.abs() - 4.0);
                let fc = (a0 * b0).sqrt();
                let hot = d > 0.0;
                problems.push(format!("{name} ({a0:.0}-{b0:.0} Hz) {}{:.1} dB vs a {genre} reference", if hot { "+" } else { "" }, d));
                let (fallback, gain) = if hot { ("master", -(d - 2.0).min(6.0)) } else if *a0 < 250.0 { ("bass", (-d - 2.0).min(5.0)) } else { ("master", (-d - 2.0).min(4.0)) };
                // the owner: the track with the biggest share of the band
                // (hot: cut it there; weak: lift the one already playing in it)
                let owner = owners.as_ref().and_then(|o| o[k].first().filter(|(_, s)| *s >= 0.25).map(|(t, s)| (t.clone(), *s)));
                let (track, why) = match &owner {
                    Some((t, s)) => (t.clone(), format!("{name} {}: {t} makes {:.0}% of it", if hot { "too hot" } else { "too weak" }, s * 100.0)),
                    None => (fallback.to_string(), format!("{name} {}", if hot { "too hot" } else { "too weak" })),
                };
                // a single owner takes the whole correction; a shared band is cut a bit less
                let gain = match &owner { Some((_, s)) if *s < 0.5 => gain * 0.75, _ => gain };
                fixes.push(json!({"why": why, "tool": "add_effect",
                    "args": {"track": track, "type": "parametric_eq", "params": {"bands": [{"kind": "bell", "freq": fc.round(), "gain_db": r1(gain), "q": 0.8}]}}}));
            }
        }
        score -= off.min(3.5);
    } else {
        for (k, (name, _, _)) in BANDS.iter().enumerate() {
            bands.insert(name.to_string(), json!({"share_db": r1(shares[k])}));
        }
    }

    // 3. clicks
    let clicks = find_clicks(&mono);
    if clicks.len() > 10 {
        score -= if clicks.len() > 40 { 1.5 } else { 0.75 };
        let first: Vec<String> = clicks.iter().take(5).map(|t| format!("{t:.2}")).collect();
        problems.push(format!("{} click-like transients (first at {} s)", clicks.len(), first.join(", ")));
        fixes.push(json!({"why": "find which track clicks", "tool": "detect_artifacts", "args": {"per_track": true}}));
    }

    // 4. drones: a partial ringing through the whole mix
    let drones = crate::tools_drone::find_drones(&mono, 9.0, 0.8, 120.0, 8000.0);
    if !drones.is_empty() {
        score -= 1.0;
        problems.push(format!("held drone(s) through the song at {} Hz", drones.iter().take(3).map(|d| format!("{:.0}", d.0)).collect::<Vec<_>>().join(", ")));
        fixes.push(json!({"why": "notch the held partials", "tool": "notch_drones", "args": {}}));
    }

    // 5. arrangement: loudness per section (project only)
    let mut sections = Vec::new();
    if from_file.is_none() {
        let p = &e.project;
        let step = p.step_secs();
        let mut t = 0.0f32;
        let mut hook = Vec::new();
        let mut verse = Vec::new();
        for s in p.song_sections() {
            let Ok(pi) = p.pattern_index(&s.pattern) else { continue };
            let dur = p.patterns[pi].steps() as f32 * s.repeats.max(1) as f32 * step;
            let (i0, i1) = (((t * SR) as usize).min(l.len()), (((t + dur) * SR) as usize).min(l.len()));
            if i1 > i0 + (SR * 0.5) as usize {
                let lu = crate::analysis::loudness(&l[i0..i1], &r[i0..i1]).integrated_lufs;
                let k = kind_of(&s.pattern);
                if k == "hook" || k == "chorus" || k == "drop" {
                    hook.push(lu);
                } else if k == "verse" {
                    verse.push(lu);
                }
                sections.push(json!({"section": s.pattern, "start_s": r1(t), "lufs": r1(lu)}));
            }
            t += dur;
        }
        let mean = |v: &Vec<f32>| v.iter().sum::<f32>() / v.len().max(1) as f32;
        if !hook.is_empty() && !verse.is_empty() {
            let c = mean(&hook) - mean(&verse);
            if c < 2.0 {
                score -= 1.5;
                problems.push(format!("hooks only {c:.1} dB over the verses (wants 3-5)"));
                fixes.push(json!({"why": "verses step back so the hook lands", "tool": "set_section_mix", "args": {"section": "verse1", "track": "master", "volume_db": -3.0, "all_occurrences": true}}));
            }
        }
        if hook.len() == 1 && sections.len() >= 4 {
            score -= 0.5;
            problems.push("the hook comes only once".into());
        }
    }

    // 6. the vocal over the beat (project with a vocal track)
    let mut vocal = Value::Null;
    let vt = a.get("vocal_track").and_then(|v| v.as_str()).unwrap_or("vocal").to_string();
    if from_file.is_none() && e.project.track_index(&vt).is_ok() {
        let mut beat = e.project.clone();
        let mut solo = e.project.clone();
        let vi = beat.track_index(&vt)?;
        beat.tracks[vi].mute = true;
        for (i, t) in solo.tracks.iter_mut().enumerate() {
            t.solo = i == vi;
            if i == vi {
                t.mute = false;
            }
        }
        e.bank.sync(&beat.samples);
        let mb = render::render(&beat, &e.bank, &RenderOptions::default())?;
        let mv = render::render(&solo, &e.bank, &RenderOptions::default())?;
        let lb = crate::analysis::loudness(&mb.left, &mb.right).integrated_lufs;
        let lv = crate::analysis::loudness(&mv.left, &mv.right).integrated_lufs;
        let d = lv - lb;
        vocal = json!({"vocal_lufs": r1(lv), "beat_lufs": r1(lb), "vocal_over_beat_db": r1(d)});
        if d < 0.0 {
            score -= 2.0;
            problems.push(format!("the vocal sits {:.1} dB UNDER the beat (words get masked)", -d));
        } else if d < 1.5 {
            score -= 1.0;
            problems.push(format!("the vocal is only {d:.1} dB over the beat (aim about +3)"));
        }
        if d < 1.5 {
            let vol = e.project.tracks[vi].volume_db;
            fixes.push(json!({"why": "vocal on top", "tool": "set_mixer", "args": {"track": vt, "volume_db": r1(vol + 3.0 - d)}}));
        }
    }

    let score = score.clamp(1.0, 10.0);
    let verdict = if score >= 7.0 { "PASS" } else { "FAIL" };
    Ok(json!({
        "verdict": verdict,
        "score": r1(score),
        "problems": problems,
        "fixes": fixes,
        "numbers": {
            "integrated_lufs": r1(ld.integrated_lufs),
            "true_peak_dbtp": r1(ld.true_peak_dbtp),
            "loudness_range_lu": r1(ld.loudness_range_lu),
            "bands": bands,
            "clicks": clicks.len(),
            "drones_hz": drones.iter().take(5).map(|d| d.0.round()).collect::<Vec<_>>(),
            "sections": sections,
            "vocal": vocal,
        },
        "reference": genre,
        "note": "measurements, not ears: a PASS means nothing measurable is off",
        "next": ["send each fixes[].tool with its args, then critique_mix again", "export_audio {path:'song.mp3'} when it passes"],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_powers_put_a_tone_in_its_band() {
        let n = (SR * 1.0) as usize;
        let hi: Vec<f32> = (0..n).map(|i| 0.3 * (i as f32 * 3000.0 * std::f32::consts::TAU / SR).sin()).collect();
        let lo: Vec<f32> = (0..n).map(|i| 0.3 * (i as f32 * 100.0 * std::f32::consts::TAU / SR).sin()).collect();
        let (h, l) = (band_powers(&hi), band_powers(&lo));
        assert!(h[4] > 100.0 * h[1], "3 kHz lands in harsh: {h:?}");
        assert!(l[1] > 100.0 * l[4], "100 Hz lands in low: {l:?}");
        // same scale for both: equal-level tones carry equal power
        assert!((h[4] / l[1] - 1.0).abs() < 0.2, "{} vs {}", h[4], l[1]);
    }

    #[test]
    fn clicks_are_found_and_hats_are_not() {
        let n = (SR * 3.0) as usize;
        let mut x: Vec<f32> = (0..n).map(|i| 0.2 * (i as f32 * 220.0 * std::f32::consts::TAU / SR).sin()).collect();
        // two hard steps (clicks)
        for at in [SR as usize, 2 * SR as usize] {
            for v in x[at..at + 40].iter_mut() {
                *v += 0.6;
            }
        }
        let c = find_clicks(&x);
        assert!(!c.is_empty() && c.len() <= 4, "clicks {c:?}");
        let s = band_shares(&x);
        assert!(s[1] > s[5], "a 220 Hz tone lives in the low band: {s:?}");
    }
}
