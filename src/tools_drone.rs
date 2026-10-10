//! notch_drones: find narrow partials that ring through a whole track (a
//! held pad note, a resonant preset, a pedal tone that never moves with the
//! chords) and notch them a few dB. The critic heard these as a "whine near
//! 1.7 kHz" and a "constant band at 300 Hz" on a sad R&B beat.

use crate::analysis::fft;
use crate::dsp::SR;
use crate::engine::Engine;
use crate::tools::{obj, Tool};
use anyhow::Result;
use serde_json::{json, Value};

const N: usize = 4096;

/// (freq Hz, persistence 0..1, prominence dB) of partials that stand out of
/// their neighbourhood in most active frames.
pub fn find_drones(mono: &[f32], min_prom_db: f32, min_persist: f32, lo_hz: f32, hi_hz: f32) -> Vec<(f32, f32, f32)> {
    if mono.len() < N * 4 {
        return vec![];
    }
    let win: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos()).collect();
    let total = (mono.len() - N) / N + 1;
    let step = (total / 300).max(1);
    let bins = N / 2;
    let hz = SR / N as f32;
    let (b_lo, b_hi) = (((lo_hz / hz) as usize).max(3), ((hi_hz / hz) as usize).min(bins - 4));
    let mut frames: Vec<Vec<f32>> = Vec::new();
    let mut energies = Vec::new();
    for f in (0..total).step_by(step) {
        let off = f * N;
        let mut re: Vec<f32> = mono[off..off + N].iter().zip(&win).map(|(x, w)| x * w).collect();
        let mut im = vec![0.0f32; N];
        fft(&mut re, &mut im);
        let db: Vec<f32> = (0..bins).map(|k| 10.0 * (re[k] * re[k] + im[k] * im[k] + 1e-12).log10()).collect();
        let e: f32 = (0..bins).map(|k| re[k] * re[k] + im[k] * im[k]).sum();
        energies.push(10.0 * (e + 1e-12).log10());
        frames.push(db);
    }
    let top = energies.iter().cloned().fold(f32::MIN, f32::max);
    let active: Vec<usize> = (0..frames.len()).filter(|&i| energies[i] > top - 30.0).collect();
    if active.len() < 4 {
        return vec![];
    }
    let mut hits = vec![0u32; bins];
    let mut prom_sum = vec![0.0f32; bins];
    for &fi in &active {
        let db = &frames[fi];
        for k in b_lo..b_hi {
            // a local peak, and how far it stands over its 1/3-octave neighbourhood
            if db[k] < db[k - 1] || db[k] < db[k + 1] {
                continue;
            }
            let span = ((k as f32) * 0.12).max(4.0) as usize;
            let (a, b) = (k.saturating_sub(span), (k + span).min(bins - 1));
            let mut neigh: Vec<f32> = db[a..=b].to_vec();
            neigh.sort_by(|x, y| x.total_cmp(y));
            let med = neigh[neigh.len() / 2];
            let prom = db[k] - med;
            if prom >= min_prom_db {
                hits[k] += 1;
                prom_sum[k] += prom;
            }
        }
    }
    // a partial wobbles a bin: pool +-1 bin
    let n_act = active.len() as f32;
    let mut cands: Vec<(f32, f32, f32)> = Vec::new();
    for k in b_lo..b_hi {
        let h = hits[k - 1] + hits[k] + hits[k + 1];
        let persist = (h as f32 / n_act).min(1.0);
        if persist >= min_persist && hits[k] >= hits[k - 1] && hits[k] >= hits[k + 1] && hits[k] > 0 {
            let prom = (prom_sum[k - 1] + prom_sum[k] + prom_sum[k + 1]) / h.max(1) as f32;
            cands.push((k as f32 * hz, persist, prom));
        }
    }
    // keep the strongest per 1/6 octave
    cands.sort_by(|a, b| (b.1 * b.2).total_cmp(&(a.1 * a.2)));
    let mut out: Vec<(f32, f32, f32)> = Vec::new();
    for c in cands {
        if out.iter().all(|o| (c.0 / o.0).log2().abs() > 1.0 / 6.0) {
            out.push(c);
        }
    }
    out
}

const SUSTAINED: [&str; 6] = ["chords", "pad", "texture", "keys", "counter", "lead"];

pub fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "notch_drones",
        description: "Find partials that ring through a whole track without moving (a held pad note, a resonant preset, a pedal tone: the 'whine' or 'constant band' a listener hears) and notch them: a narrow bell of depth_db at each, as one parametric EQ with id 'drones' (re-running replaces it). tracks: default the sustained ones (chords, pad, texture, keys, counter, lead). apply:false only reports. Returns each track's drones: frequency, how much of the track it rings (persistence) and how far it stands out (dB).",
        mutates: true,
        schema: || obj(json!({
            "tracks": {"type": "array", "items": {"type": "string"}, "description": "Tracks to check (default the sustained ones)"},
            "max_notches": {"type": "integer", "minimum": 1, "maximum": 6, "default": 3},
            "depth_db": {"type": "number", "minimum": -12, "maximum": -1, "default": -5},
            "min_prominence_db": {"type": "number", "minimum": 4, "maximum": 24, "default": 9},
            "min_persistence": {"type": "number", "minimum": 0.3, "maximum": 1, "default": 0.8},
            "apply": {"type": "boolean", "default": true}
        }), &[]),
        run: |e, a| run(e, a),
    }]
}

fn run(e: &mut Engine, a: &Value) -> Result<Value> {
    let list: Vec<String> = match a.get("tracks").and_then(|v| v.as_array()) {
        Some(v) => v.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        None => SUSTAINED.iter().filter(|t| e.project.track_index(t).is_ok()).map(|t| t.to_string()).collect(),
    };
    let max_n = a.get("max_notches").and_then(|v| v.as_u64()).unwrap_or(3).clamp(1, 6) as usize;
    let depth = a.get("depth_db").and_then(|v| v.as_f64()).unwrap_or(-5.0).clamp(-12.0, -1.0) as f32;
    let prom = a.get("min_prominence_db").and_then(|v| v.as_f64()).unwrap_or(9.0) as f32;
    let pers = a.get("min_persistence").and_then(|v| v.as_f64()).unwrap_or(0.8) as f32;
    let apply = a.get("apply").and_then(|v| v.as_bool()).unwrap_or(true);
    let mut report = Vec::new();
    for t in &list {
        e.project.track_index(t)?;
        let m = crate::tools_ears::render_track(e, t)?;
        let mono: Vec<f32> = m.left.iter().zip(&m.right).map(|(x, y)| 0.5 * (x + y)).collect();
        let found: Vec<(f32, f32, f32)> = find_drones(&mono, prom, pers, 120.0, 8000.0).into_iter().take(max_n).collect();
        let mut notched = false;
        if apply {
            let ti = e.project.track_index(t)?;
            e.project.tracks[ti].effects.retain(|f| f.id() != "drones");
            if !found.is_empty() {
                let bands: Vec<Value> = found.iter().map(|(f, _, _)| json!({"kind": "bell", "freq": (f * 10.0).round() / 10.0, "gain_db": depth, "q": 8.0})).collect();
                let mut fx: crate::fx::Effect = serde_json::from_value(json!({"type": "parametric_eq", "bands": bands}))?;
                fx.set_id("drones");
                e.project.tracks[ti].effects.push(fx);
                notched = true;
            }
        }
        report.push(json!({
            "track": t,
            "drones": found.iter().map(|(f, p, d)| json!({"hz": f.round(), "persistence": (p * 100.0).round() / 100.0, "stands_out_db": (d * 10.0).round() / 10.0})).collect::<Vec<_>>(),
            "notched": notched,
        }));
    }
    Ok(json!({"tracks": report, "depth_db": depth}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_held_partial_in_moving_material() {
        // chords that move (A minor -> F -> C -> G) plus one 1700 Hz whine all the way through
        let n = (SR * 8.0) as usize;
        let chords = [[220.0, 261.6, 329.6], [174.6, 220.0, 261.6], [261.6, 329.6, 392.0], [196.0, 246.9, 293.7]];
        let mut seed = 7u32;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / SR;
                let c = chords[(t / 2.0) as usize % 4];
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let nz = (seed as f32 / u32::MAX as f32 - 0.5) * 0.02;
                c.iter().map(|f| (t * f * std::f32::consts::TAU).sin() * 0.2).sum::<f32>() + (t * 1700.0 * std::f32::consts::TAU).sin() * 0.05 + nz
            })
            .collect();
        let d = find_drones(&x, 9.0, 0.8, 120.0, 8000.0);
        assert!(!d.is_empty());
        assert!((d[0].0 - 1700.0).abs() < 25.0, "{d:?}");
        // chord tones that come and go are music, not drones
        assert!(d.iter().all(|(f, _, _)| (f - 220.0).abs() > 15.0 && (f - 174.6).abs() > 15.0), "{d:?}");
    }
}
