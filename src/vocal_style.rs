//! Read a vocal take's style before a beat is chosen for it: the phrase map
//! (with stressed syllables), the tempo feel (half/double-time candidates
//! and swing), key and mode, sung vs rap, legato vs staccato, a tone
//! descriptor, and a ranked genre / arrangement suggestion. Then
//! `fit_beat_to_vocal` conforms an existing beat's groove and accents to it.

use crate::dsp::SR;
use crate::engine::Engine;
use crate::tools::{b_or, f_or, obj, s_opt, Tool};
use crate::vocal_flex::{band_db, nearest_grid, phrases, syllable_onsets, HOP};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::f32::consts::TAU;

fn r2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}

/// Phase coherence of onsets with a pulse of `period_s` (0 = random, 1 = all on it).
fn coherence(on: &[f32], period_s: f32) -> f32 {
    if on.is_empty() || period_s <= 0.0 {
        return 0.0;
    }
    let (mut c, mut s) = (0.0f32, 0.0f32);
    for t in on {
        let ph = TAU * t / period_s;
        c += ph.cos();
        s += ph.sin();
    }
    (c * c + s * s).sqrt() / on.len() as f32
}

/// Tempo score: onsets locking to the beat, the 8th and the 16th.
pub fn tempo_score(on: &[f32], bpm: f32) -> f32 {
    let b = 60.0 / bpm;
    0.45 * coherence(on, b) + 0.35 * coherence(on, b / 2.0) + 0.2 * coherence(on, b / 4.0)
}

/// Best bpm in 60..180 plus its half/double-time readings with scores.
pub fn tempo_feel(on: &[f32]) -> (f32, Vec<(f32, f32)>) {
    let mut best = (90.0f32, -1.0f32);
    let mut bpm = 60.0f32;
    while bpm <= 180.0 {
        let s = tempo_score(on, bpm);
        if s > best.1 {
            best = (bpm, s);
        }
        bpm += 0.5;
    }
    // refine
    let mut b2 = best;
    let mut x = best.0 - 0.5;
    while x <= best.0 + 0.5 {
        let s = tempo_score(on, x);
        if s > b2.1 {
            b2 = (x, s);
        }
        x += 0.05;
    }
    let mut alts = vec![(r2(b2.0), r2(b2.1))];
    for f in [0.5f32, 2.0, 2.0 / 3.0, 1.5] {
        let c = b2.0 * f;
        if (50.0..=200.0).contains(&c) {
            alts.push((r2(c), r2(tempo_score(on, c))));
        }
    }
    (b2.0, alts)
}

/// Swing 0 (straight 8ths) .. 1 (triplet shuffle), from the long-short
/// ratio of consecutive syllable pairs that fill one beat (measured at the
/// beat level between 70 and 140 bpm, so a half/double-time tempo reading
/// does not turn 8ths into "swing"). Delaying the off-beat by s/6 of a beat
/// gives a ratio R = (3+s)/(3-s), so s = 3(R-1)/(R+1).
pub fn swing_of(on: &[f32], bpm: f32, _phase_s: f32) -> f32 {
    if bpm <= 0.0 || on.len() < 4 {
        return 0.0;
    }
    // the tempo reading may be the triplet grid (x1.5) or half/double time:
    // measure at whichever beat candidate most syllable pairs fill
    let pairs_at = |cand: f32| -> Vec<f32> {
        let mut beat_bpm = cand;
        while beat_bpm < 70.0 {
            beat_bpm *= 2.0;
        }
        while beat_bpm >= 140.0 {
            beat_bpm /= 2.0;
        }
        let b = 60.0 / beat_bpm;
        on.windows(3)
            .filter_map(|w| {
                let (i1, i2) = (w[1] - w[0], w[2] - w[1]);
                let ok = (i1 + i2 - b).abs() < 0.15 * b && i1 > 0.2 * b && i2 > 0.2 * b && i1 < 0.8 * b && i2 < 0.8 * b;
                ok.then(|| if i1 > i2 { i1 / i2 } else { i2 / i1 })
            })
            .collect()
    };
    let mut ratios: Vec<f32> = Vec::new();
    for f in [1.0f32, 2.0 / 3.0, 1.5, 0.75, 4.0 / 3.0] {
        let r = pairs_at(bpm * f);
        if r.len() > ratios.len() {
            ratios = r;
        }
    }
    if ratios.len() < 3 {
        return 0.0;
    }
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let r = ratios[ratios.len() / 2];
    (3.0 * (r - 1.0) / (r + 1.0)).clamp(0.0, 1.0)
}

/// Phase of the beat grid (s) that best fits the onsets at `bpm`.
fn beat_phase(on: &[f32], bpm: f32) -> f32 {
    let b = 60.0 / bpm;
    let (mut c, mut s) = (0.0f32, 0.0f32);
    for t in on {
        let ph = TAU * t / b;
        c += ph.cos();
        s += ph.sin();
    }
    (s.atan2(c) / TAU).rem_euclid(1.0) * b
}

const MAJOR: [f32; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const MINOR: [f32; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];
const NAMES: [&str; 12] = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];

fn corr(a: &[f32; 12], b: &[f32]) -> f32 {
    let ma = a.iter().sum::<f32>() / 12.0;
    let mb = b.iter().sum::<f32>() / 12.0;
    let (mut n, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        n += (a[i] - ma) * (b[i] - mb);
        da += (a[i] - ma).powi(2);
        db += (b[i] - mb).powi(2);
    }
    n / (da * db).sqrt().max(1e-9)
}

/// Key from the voiced pitch-class histogram (Krumhansl profiles):
/// (tonic pc, minor?, confidence 0..1, cents off 12-TET, histogram).
pub fn key_of(x: &[f32]) -> (u8, bool, f32, f32, Vec<f32>) {
    let fr = crate::vocal::pitch_track(x, SR);
    let mut h = vec![0.0f32; 12];
    let mut cents = Vec::new();
    for f in fr.iter().filter(|f| f.midi > 0.0) {
        let m = f.midi;
        let pc = (m.round() as i32).rem_euclid(12) as usize;
        h[pc] += f.rms;
        cents.push((m - m.round()) * 100.0);
    }
    let tot: f32 = h.iter().sum();
    if tot <= 0.0 {
        return (0, true, 0.0, 0.0, h);
    }
    h.iter_mut().for_each(|v| *v /= tot);
    let mut scores: Vec<(f32, u8, bool)> = Vec::new();
    for k in 0..12u8 {
        let rot: Vec<f32> = (0..12).map(|i| h[(i + k as usize) % 12]).collect();
        scores.push((corr(&MAJOR, &rot), k, false));
        scores.push((corr(&MINOR, &rot), k, true));
    }
    scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let conf = (scores[0].0 - scores[1].0).max(0.0) * 4.0 + scores[0].0 * 0.5;
    cents.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let c = cents.get(cents.len() / 2).copied().unwrap_or(0.0);
    (scores[0].1, scores[0].2, conf.clamp(0.0, 1.0), c, h)
}

/// Stressed syllables: onsets whose 80 ms level is 3 dB over the phrase median.
fn stresses(x: &[f32], on: &[f32]) -> Vec<f32> {
    let db = band_db(x);
    let lvl = |t: f32| -> f32 {
        let a = (t / HOP) as usize;
        let b = (a + (0.08 / HOP) as usize).min(db.len());
        db.get(a..b).map(|s| s.iter().cloned().fold(-200.0f32, f32::max)).unwrap_or(-200.0)
    };
    let v: Vec<f32> = on.iter().map(|t| lvl(*t)).collect();
    if v.is_empty() {
        return Vec::new();
    }
    let mut s = v.clone();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med = s[s.len() / 2];
    on.iter().zip(v.iter()).filter(|(_, l)| **l >= med + 3.0).map(|(t, _)| *t).collect()
}

pub struct Style {
    pub bpm: f32,
    pub phase_s: f32,
    pub swing: f32,
    pub onsets: Vec<f32>,
    pub stress: Vec<f32>,
    pub report: Value,
}

pub fn analyze(x: &[f32]) -> Style {
    let on = syllable_onsets(x, 0.5, 0.08);
    let ph = phrases(x, 0.3, None);
    let st = stresses(x, &on);
    let (bpm, alts) = tempo_feel(&on);
    let phase = beat_phase(&on, bpm);
    let swing = swing_of(&on, bpm, phase);
    let (tonic, minor, kconf, cents, _) = key_of(x);
    let styles = crate::vocal_sections::styles(x, 0.35, 0.3, 2.0);
    let sung_s: f32 = styles.iter().filter(|s| s.style == "melodic").map(|s| s.end_s - s.start_s).sum();
    let all_s: f32 = styles.iter().map(|s| s.end_s - s.start_s).sum::<f32>().max(1e-3);
    let sung = sung_s / all_s;
    // legato: share of each phrase that is voiced (held) vs gaps between syllables
    let db = band_db(x);
    let thr = crate::vocal_flex::voice_threshold(&db, None);
    let (mut voiced, mut inside) = (0usize, 0usize);
    for p in &ph {
        let a = (p.start_s / HOP) as usize;
        let b = ((p.end_s / HOP) as usize).min(db.len());
        for d in db.get(a..b).unwrap_or(&[]) {
            inside += 1;
            if *d >= thr {
                voiced += 1;
            }
        }
    }
    let legato = voiced as f32 / inside.max(1) as f32;
    let dur = x.len() as f32 / SR;
    let rate = on.len() as f32 / ph.iter().map(|p| p.end_s - p.start_s).sum::<f32>().max(1e-3);
    // tone
    let t = crate::vocal_clarity::tone(x);
    let g = |k: &str| t[k].as_f64().unwrap_or(0.0) as f32;
    let (body, pres, bril, air) = (g("body_200_500"), g("presence_2k_5k"), g("brilliance_5k_8k"), g("air_8k_16k"));
    let fr: Vec<f32> = db.iter().copied().filter(|d| *d >= thr).collect();
    let mut sorted = fr.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let dyn_db = if sorted.len() > 10 { sorted[sorted.len() * 95 / 100] - sorted[sorted.len() / 10] } else { 0.0 };
    let mut tone = Vec::new();
    if pres < -15.0 || g("top_hz") < 6000.0 {
        tone.push("dark");
    } else if pres > -6.0 {
        tone.push("bright");
    }
    if air > bril + 2.0 && air > -30.0 {
        tone.push("breathy");
    }
    if body > 3.0 {
        tone.push("warm/boxy");
    }
    if dyn_db < 12.0 && legato > 0.6 {
        tone.push("intimate");
    } else if dyn_db > 20.0 {
        tone.push("dynamic/punchy");
    }
    if tone.is_empty() {
        tone.push("neutral");
    }
    let half = (bpm / 2.0).max(1.0);
    let fast = if bpm < 100.0 { bpm * 2.0 } else { bpm };
    let slow = if bpm >= 120.0 { half } else { bpm };
    let delivery = if sung > 0.6 { "sung" } else if sung > 0.25 { "mixed sung + rap" } else { "rap" };
    let articulation = if legato > 0.65 { "legato" } else if legato < 0.45 { "staccato" } else { "mixed" };
    // ranked suggestions
    let mut sug: Vec<(f32, Value)> = Vec::new();
    let mut add = |score: f32, genre: &str, bpm_use: f32, groove: &str, feel: &str, palette: &[&str], why: String| {
        sug.push((score, json!({"genre": genre, "score": r2(score), "bpm": r2(bpm_use), "drum_groove": groove, "feel": feel, "palette": palette, "why": why})));
    };
    let swingy = swing > 0.3;
    add(
        (1.0 - sung) * if (130.0..=150.0).contains(&fast) || (65.0..=75.0).contains(&slow) { 1.0 } else { 0.4 } * if minor { 1.0 } else { 0.7 },
        "drill",
        if (130.0..=150.0).contains(&fast) { fast } else { 142.0 },
        "drill",
        "half-time drill (kick 1/4/7/11, snare on 3), sliding 808",
        &["kick_tight", "snare_drill", "hat_drill", "808_slide", "felt_piano", "choir_ooh", "bell_dark"],
        format!("{delivery}, flow fits {fast:.0}/{slow:.0} bpm{}", if minor { ", minor" } else { "" }),
    );
    add(
        (1.0 - sung * 0.5) * if (125.0..=170.0).contains(&fast) { 0.9 } else { 0.4 } * if swingy { 0.6 } else { 1.0 },
        "trap",
        fast.clamp(130.0, 160.0),
        "trap",
        "half-time, 16th hats with rolls",
        &["kick_punchy", "snare_crack", "hat_crisp", "808_grit", "dark_keys", "pad_dusty"],
        format!("{delivery} at {fast:.0} double-time"),
    );
    add(
        (1.0 - sung) * if (82.0..=100.0).contains(&bpm) { 1.0 } else { 0.35 } * if swingy { 1.2 } else { 0.8 },
        "boom_bap",
        bpm.clamp(82.0, 100.0),
        "boom_bap",
        if swingy { "swung 16ths, laid-back snare" } else { "straight, laid-back snare" },
        &["kick_dusty", "snare_dusty", "hat_dusty", "bass_round", "keys_dusty"],
        format!("rap at {bpm:.0} bpm, swing {swing:.2}"),
    );
    add(
        sung * if (60.0..=80.0).contains(&slow) || (120.0..=160.0).contains(&fast) { 1.0 } else { 0.5 } * if minor { 1.0 } else { 0.8 },
        "melodic_rap",
        if (60.0..=80.0).contains(&slow) { slow * 2.0 } else { fast },
        "trap",
        "half-time, sparse kick, 8th hats",
        &["kick_deep", "snare_soft", "hat_crisp", "808_clean", "bell_glass", "pad_airy", "keys_airy"],
        format!("{delivery}, {articulation}, {}", if minor { "minor" } else { "major" }),
    );
    add(
        sung * legato * if (60.0..=95.0).contains(&bpm) || (60.0..=95.0).contains(&half) { 1.0 } else { 0.4 },
        "rnb",
        if bpm <= 95.0 { bpm } else { half },
        "rnb",
        if swingy { "swung, lazy" } else { "straight, lazy" },
        &["kick_deep", "snare_soft", "hat_crisp", "808_clean", "epiano", "pad_airy"],
        format!("sung {articulation}, {}", tone.join("/")),
    );
    add(
        sung * if (95.0..=130.0).contains(&bpm) { 1.0 } else { 0.4 } * if minor { 0.8 } else { 1.0 },
        "pop / indie",
        bpm.clamp(95.0, 130.0),
        "pop",
        "straight four, big chorus lift",
        &["kick", "snare", "hat", "sub_bass", "guitar_pluck", "warm_pad", "strings"],
        format!("sung at {bpm:.0} bpm, {}", if minor { "minor" } else { "major" }),
    );
    add(
        if swingy { 0.9 } else { 0.3 } * if bpm < 95.0 { 1.0 } else { 0.5 },
        "lofi",
        bpm.clamp(70.0, 92.0),
        "lofi",
        "swung, dusty",
        &["kick_dusty", "snare_dusty", "hat_dusty", "keys_dusty", "pad_dusty"],
        format!("swing {swing:.2}, {}", tone.join("/")),
    );
    sug.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let phrases_v: Vec<Value> = ph
        .iter()
        .map(|p| {
            let s: Vec<f32> = st.iter().copied().filter(|t| *t >= p.start_s && *t <= p.end_s).map(r2).collect();
            json!({"start_s": r2(p.start_s), "end_s": r2(p.end_s), "stress_s": s})
        })
        .collect();
    let report = json!({
        "seconds": r2(dur),
        "phrases": phrases_v,
        "syllables": on.len(),
        "syllables_per_s": r2(rate),
        "tempo": {"bpm": r2(bpm), "beat_phase_s": r2(phase), "candidates": alts.iter().map(|(b, s)| json!({"bpm": b, "score": s})).collect::<Vec<_>>(), "half_time": r2(half), "double_time": r2(bpm * 2.0), "swing": r2(swing), "note": "candidates at x0.5 / x2 / x2/3 / x1.5 score how well the flow also locks there: close scores mean the feel is ambiguous"},
        "key": {"tonic": NAMES[tonic as usize], "mode": if minor { "minor" } else { "major" }, "confidence": r2(kconf), "cents_off": (cents * 10.0).round() / 10.0},
        "delivery": {"style": delivery, "sung_share": r2(sung), "articulation": articulation, "legato": r2(legato), "sections": styles.iter().map(|s| json!({"start_s": r2(s.start_s), "end_s": r2(s.end_s), "style": s.style})).collect::<Vec<_>>()},
        "tone": {"words": tone, "body_db": body, "presence_db": pres, "brilliance_db": bril, "air_db": air, "dynamics_db": r2(dyn_db)},
        "suggestions": sug.iter().map(|s| s.1.clone()).collect::<Vec<_>>(),
    });
    Style { bpm, phase_s: phase, swing, onsets: on, stress: st, report }
}

fn audio(e: &mut Engine, a: &Value) -> Result<(String, Vec<f32>)> {
    if let Some(s) = s_opt(a, "sample") {
        let (info, x) = crate::tools_sound::sample_data(e, &s)?;
        return Ok((info.name, x));
    }
    if s_opt(a, "path").is_some() {
        let (name, l, r) = crate::tools_sound::audio_for(e, a)?;
        return Ok((name, l.iter().zip(r.iter()).map(|(x, y)| 0.5 * (x + y)).collect()));
    }
    bail!("give sample (or path)")
}

fn analyze_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (name, x) = audio(e, a)?;
    if x.len() < (1.0 * SR) as usize {
        bail!("the take is shorter than a second");
    }
    let s = analyze(&x);
    let mut out = s.report;
    out["source"] = json!(name);
    out["next"] = json!("make_beat / make_drill_beat at the top suggestion's bpm and groove, then fit_beat_to_vocal");
    Ok(out)
}

/// Conform the beat to the vocal: swing from the flow, accents on the
/// stressed syllables, kicks under stresses that land on 8th notes with no
/// kick near them, and quieter hats/percussion inside phrases (room for
/// the words). Every pattern position is voted across the pattern's plays.
fn fit_tool(e: &mut Engine, a: &Value) -> Result<Value> {
    let (name, x) = audio(e, a)?;
    let s = analyze(&x);
    let start_beat = f_or(a, "start_beat", 0.0);
    let bpm = e.project.bpm;
    let bs = 60.0 / bpm;
    let strength = f_or(a, "strength", 0.7).clamp(0.0, 1.0);
    let only = s_opt(a, "pattern");
    if e.project.arrangement.is_empty() {
        e.project.arrangement = e.project.song_sections();
    }
    // song step of every stress / syllable onset
    let to_step = |t: f32| (start_beat + t / bs) * 4.0;
    let stress_steps: Vec<f32> = s.stress.iter().map(|t| to_step(*t)).collect();
    let syl_steps: Vec<f32> = s.onsets.iter().map(|t| to_step(*t)).collect();
    let phr: Vec<(f32, f32)> = phrases(&x, 0.3, None).iter().map(|p| (to_step(p.start_s), to_step(p.end_s))).collect();
    // per pattern: votes per step position
    let mut placed: std::collections::BTreeMap<String, Vec<f32>> = std::collections::BTreeMap::new();
    let mut acc = 0.0f32;
    for sec in &e.project.arrangement {
        let pi = e.project.pattern_index(&sec.pattern)?;
        let len = e.project.patterns[pi].steps() as f32;
        for _ in 0..sec.repeats.max(1) {
            placed.entry(sec.pattern.to_lowercase()).or_default().push(acc);
            acc += len;
        }
    }
    let roles: Vec<(String, &'static str)> = e.project.tracks.iter().map(|t| (t.name.to_lowercase(), crate::tools_mix::role_of(&t.name, &t.instrument))).collect();
    let (mut accented, mut softened, mut kicks_added) = (0usize, 0usize, 0usize);
    let mut touched = Vec::new();
    for (pname, starts) in &placed {
        if let Some(o) = &only {
            if !o.eq_ignore_ascii_case(pname) {
                continue;
            }
        }
        let pi = e.project.pattern_index(pname)?;
        let len = e.project.patterns[pi].steps() as usize;
        let plays = starts.len() as f32;
        let mut stress_votes = vec![0.0f32; len];
        let mut inside_votes = vec![0.0f32; len];
        let mut vocal_any = false;
        for &st0 in starts {
            for &ss in &stress_steps {
                let k = (ss - st0).round();
                if k >= 0.0 && (k as usize) < len {
                    stress_votes[k as usize] += 1.0;
                    vocal_any = true;
                }
            }
            for &(a0, a1) in &phr {
                for k in 0..len {
                    let g = st0 + k as f32;
                    if g >= a0 && g < a1 {
                        inside_votes[k] += 1.0;
                    }
                }
            }
        }
        let _ = &syl_steps;
        if !vocal_any {
            continue;
        }
        let pat = &mut e.project.patterns[pi];
        let mut kick_track: Option<String> = None;
        for (tn, notes) in pat.clips.iter_mut() {
            let role = roles.iter().find(|(n, _)| n == tn).map(|(_, r)| *r).unwrap_or("other");
            if !matches!(role, "kick" | "snare" | "hats" | "perc" | "bass") {
                continue;
            }
            if role == "kick" {
                kick_track = Some(tn.clone());
            }
            for n in notes.iter_mut() {
                let k = (n.start.round() as usize).min(len - 1);
                let near = (k.saturating_sub(1)..=(k + 1).min(len - 1)).map(|j| stress_votes[j]).fold(0.0f32, f32::max) / plays;
                if near >= 0.5 && matches!(role, "kick" | "snare" | "bass" | "hats") {
                    n.vel = (n.vel * (1.0 + 0.2 * strength)).min(1.0);
                    accented += 1;
                } else if inside_votes[k] / plays >= 0.5 && matches!(role, "hats" | "perc") {
                    n.vel = (n.vel * (1.0 - 0.18 * strength)).max(0.1);
                    softened += 1;
                }
            }
        }
        if b_or(a, "add_kicks", true) {
            if let Some(kt) = kick_track {
                let notes = pat.notes_mut(&kt);
                let mut per_bar = std::collections::HashMap::new();
                for k in (0..len).step_by(2) {
                    if stress_votes[k] / plays >= 0.6 && !notes.iter().any(|n| (n.start - k as f32).abs() <= 2.0) {
                        let bar = k / 16;
                        let c = per_bar.entry(bar).or_insert(0);
                        if *c < 1 {
                            *c += 1;
                            notes.push(crate::project::Note::new(k as f32, 1.0, 60, 0.72));
                            kicks_added += 1;
                        }
                    }
                }
                notes.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
            }
        }
        touched.push(pname.clone());
    }
    let mut swing_set = Value::Null;
    if b_or(a, "swing", true) && s.swing > 0.15 {
        let sw = (s.swing * 0.6 * strength).clamp(0.0, 0.6);
        e.project.swing = sw;
        swing_set = json!(r2(sw));
    }
    // how the vocal's own tempo reading relates to the project
    let ratio = s.bpm / bpm;
    let rel = [0.5f32, 1.0, 2.0, 2.0 / 3.0, 1.5].iter().map(|f| (f, (ratio / f - 1.0).abs())).min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)).unwrap();
    let mut warn = Vec::new();
    if rel.1 > 0.04 {
        warn.push(format!("the flow reads {:.1} bpm, which does not fit the project's {bpm:.1} (or its half/double): set_tempo first, or align_vocal_to_grid", s.bpm));
    }
    let on_grid = {
        let tol = (0.25 * bs * 0.25).min(0.03);
        s.onsets.iter().filter(|t| nearest_grid(**t, bpm, start_beat, 0.25).0.abs() <= tol).count() as f32 / s.onsets.len().max(1) as f32
    };
    Ok(json!({
        "source": name,
        "patterns_fitted": touched,
        "accented_hits": accented,
        "softened_hits_inside_phrases": softened,
        "kicks_added_under_stresses": kicks_added,
        "swing_set": swing_set,
        "vocal_bpm": r2(s.bpm),
        "vocal_on_16th_grid_pct": (on_grid * 1000.0).round() / 10.0,
        "warnings": warn,
        "next": "render or measure_vocal_vs_beat; undo restores the beat",
    }))
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "analyze_vocal_style",
            description: "Read a vocal take before choosing a beat: phrase map (start/end and stressed syllables), tempo feel (best bpm with half/double/triplet candidates and their scores, beat phase, swing 0..1), key and mode (with confidence and cents off), sung vs rap share and legato vs staccato, a tone descriptor (dark/bright/breathy/warm/intimate/punchy, with band levels and dynamics), and ranked genre/arrangement suggestions (bpm, drum groove for generate_drums/make_drill_beat, feel, instrument palette, why). Read-only; sample or path.",
            mutates: false,
            schema: || obj(json!({"sample": {"type": "string"}, "path": {"type": "string"}}), &[]),
            run: analyze_tool,
        },
        Tool {
            name: "fit_beat_to_vocal",
            description: "Conform the existing beat's groove and accents to a vocal's phrase map: swing from the flow (when it swings), drum hits under stressed syllables accented, hats/percussion softened inside phrases so the words sit on top, and a kick added under a stress on an 8th note with no kick near it (max one per bar). Votes across every play of a pattern so repeated patterns follow the majority. start_beat = song beat where the take starts; strength 0..1 (default 0.7); pattern limits it to one pattern; add_kicks/swing (default true). Undo restores the beat.",
            mutates: true,
            schema: || obj(json!({"sample": {"type": "string"}, "path": {"type": "string"}, "start_beat": {"type": "number"}, "strength": {"type": "number"}, "pattern": {"type": "string"}, "add_kicks": {"type": "boolean"}, "swing": {"type": "boolean"}}), &[]),
            run: fit_tool,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Syllables at `bpm` on 8th notes (off-beats delayed by `swing`), pitched on an A minor scale.
    fn take(bpm: f32, swing: f32, secs: f32) -> Vec<f32> {
        let mut x = vec![0.0f32; (secs * SR) as usize];
        let b = 60.0 / bpm;
        let scale = [57.0f32, 60.0, 62.0, 64.0, 65.0, 67.0, 69.0, 57.0, 64.0, 60.0];
        let mut k = 0usize;
        let mut beat = 0.5f32;
        while (beat + 1.0) * b < secs - 0.3 {
            // phrases of 3 beats, then a 1-beat pause
            if (beat as usize) % 4 != 3 {
                for (half, off) in [(0usize, 0.0f32), (1, 0.5 + swing / 6.0)] {
                    let t0 = (beat + off) * b;
                    let f = 440.0 * 2f32.powf((scale[k % scale.len()] - 69.0) / 12.0);
                    let accent = if half == 0 && (beat as usize) % 2 == 0 { 1.0 } else { 0.45 };
                    let n = (0.14 * SR) as usize;
                    let i0 = (t0 * SR) as usize;
                    for i in 0..n {
                        let t = i as f32 / SR;
                        let env = (t / 0.008).min(1.0) * (-t / 0.09).exp();
                        if i0 + i < x.len() {
                            x[i0 + i] += accent * 0.4 * env * ((TAU * f * t).sin() + 0.4 * (TAU * 2.0 * f * t).sin());
                        }
                    }
                    k += 1;
                }
            }
            beat += 1.0;
        }
        x
    }

    #[test]
    fn tempo_swing_and_key_are_read() {
        let x = take(92.0, 0.0, 16.0);
        let s = analyze(&x);
        let b = s.bpm;
        let ok = [92.0f32, 46.0, 184.0, 138.0, 61.33].iter().any(|c| (b / c - 1.0).abs() < 0.03);
        assert!(ok, "bpm {b}: {}", s.report["tempo"]);
        assert!(s.swing < 0.3, "straight take reads swing {}", s.swing);
        assert!(!s.report["phrases"].as_array().unwrap().is_empty());
        let key = &s.report["key"];
        let t = key["tonic"].as_str().unwrap();
        assert!((t == "A" && key["mode"] == "minor") || (t == "C" && key["mode"] == "major"), "{key}");
        assert!(!s.stress.is_empty(), "accented syllables found");
        assert!(s.report["suggestions"].as_array().unwrap().len() >= 5);
        let sw = analyze(&take(92.0, 1.0, 16.0));
        assert!(sw.swing > s.swing + 0.3, "swung take {} vs straight {}", sw.swing, s.swing);
    }

    #[test]
    fn fit_beat_accents_and_swings() {
        let dir = std::env::temp_dir().join("beatbox_vocal_style_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Engine::new(dir.clone());
        e.call("new_project", &json!({"name": "fit", "bpm": 92})).unwrap();
        let x = take(92.0, 1.0, 12.0);
        let wav = dir.join("take.wav");
        crate::render::write_wav(&wav, &x, &x).unwrap();
        e.call("import_sample", &json!({"path": wav.to_string_lossy(), "name": "take"})).unwrap();
        e.call("generate_drums", &json!({"style": "boom_bap"})).unwrap();
        e.project.bpm = 92.0;
        let a = e.call("analyze_vocal_style", &json!({"sample": "take"})).unwrap();
        assert!(a["tempo"]["swing"].as_f64().unwrap() > 0.3, "{}", a["tempo"]);
        let before = e.project.patterns.clone();
        let r = e.call("fit_beat_to_vocal", &json!({"sample": "take"})).unwrap();
        assert!(r["accented_hits"].as_u64().unwrap() + r["softened_hits_inside_phrases"].as_u64().unwrap() > 0, "{r}");
        assert!(r["swing_set"].as_f64().unwrap() > 0.1, "{r}");
        assert_ne!(before, e.project.patterns);
    }
}
