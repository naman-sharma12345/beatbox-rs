//! Groove extraction: steal the feel of a recording (a drum loop, a
//! reference, the singer) as a 16-slot template of microtiming offsets and
//! accents, and lay it onto patterns.

use crate::project::{Project, STEPS_PER_BAR};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct GrooveTemplate {
    /// Mean offset of hits at each 16th of the bar, in steps (-0.5..0.5; + = late).
    pub offsets: Vec<f32>,
    /// Relative accent per 16th (mean 1.0 over slots that have hits).
    pub accents: Vec<f32>,
    /// Hits measured per slot.
    pub counts: Vec<u32>,
    /// Swing: how much later the off-16ths sit than the on-16ths, in steps.
    pub swing: f32,
    /// RMS of the offsets, ms at the source tempo.
    pub looseness_ms: f32,
    pub bpm: f32,
}

/// Build a template from onsets `(time in steps from a bar line, strength)`.
pub fn from_steps(onsets: &[(f32, f32)], bpm: f32) -> GrooveTemplate {
    let n = STEPS_PER_BAR as usize;
    let mut off = vec![0.0f32; n];
    let mut wsum = vec![0.0f32; n];
    let mut acc = vec![0.0f32; n];
    let mut cnt = vec![0u32; n];
    for &(pos, w) in onsets {
        let r = pos.round();
        let d = pos - r;
        if d.abs() > 0.45 {
            continue;
        }
        let slot = (r as i64).rem_euclid(n as i64) as usize;
        let w = w.max(1e-3);
        off[slot] += d * w;
        wsum[slot] += w;
        acc[slot] += w;
        cnt[slot] += 1;
    }
    for i in 0..n {
        if wsum[i] > 0.0 {
            // few hits are weak evidence: shrink toward the grid
            let c = cnt[i] as f32;
            off[i] = off[i] / wsum[i] * c / (c + 1.5);
            acc[i] /= c;
        }
    }
    let used: Vec<usize> = (0..n).filter(|&i| cnt[i] > 0).collect();
    let mean_acc = used.iter().map(|&i| acc[i]).sum::<f32>() / used.len().max(1) as f32;
    let accents: Vec<f32> = (0..n).map(|i| if cnt[i] > 0 { acc[i] / mean_acc.max(1e-6) } else { 1.0 }).collect();
    let mean = |odd: bool| -> f32 {
        let v: Vec<f32> = (0..n).filter(|i| (i % 2 == 1) == odd && cnt[*i] > 0).map(|i| off[i]).collect();
        if v.is_empty() { 0.0 } else { v.iter().sum::<f32>() / v.len() as f32 }
    };
    let swing = mean(true) - mean(false);
    let step_ms = 60_000.0 / bpm.max(1.0) / 4.0;
    let rms = (used.iter().map(|&i| off[i] * off[i]).sum::<f32>() / used.len().max(1) as f32).sqrt();
    GrooveTemplate {
        offsets: off.iter().map(|x| (x * 1000.0).round() / 1000.0).collect(),
        accents: accents.iter().map(|x| (x * 100.0).round() / 100.0).collect(),
        counts: cnt,
        swing: (swing * 1000.0).round() / 1000.0,
        looseness_ms: (rms * step_ms * 10.0).round() / 10.0,
        bpm,
    }
}

/// Lay a template onto tracks: each note on a 16th takes that slot's offset
/// (blended by `amount`) and accent. Returns how many notes moved.
pub fn apply(p: &mut Project, t: &GrooveTemplate, tracks: &[String], patterns: &[usize], amount: f32) -> usize {
    let a = amount.clamp(0.0, 1.0);
    let n = STEPS_PER_BAR as i64;
    let mut moved = 0;
    for &pi in patterns {
        let Some(pat) = p.patterns.get_mut(pi) else { continue };
        for (name, notes) in pat.clips.iter_mut() {
            if !tracks.iter().any(|t| t.eq_ignore_ascii_case(name)) {
                continue;
            }
            for note in notes.iter_mut() {
                if (note.start - note.start.round()).abs() > 0.01 {
                    continue; // already off the grid: a fill, a roll
                }
                let slot = (note.start.round() as i64).rem_euclid(n) as usize;
                if t.counts.get(slot).copied().unwrap_or(0) == 0 {
                    continue;
                }
                let o = t.offsets[slot];
                note.offset = ((1.0 - a) * note.offset + a * o).clamp(-0.5, 0.5);
                let acc = t.accents[slot];
                note.vel = (note.vel * (1.0 + a * 0.35 * (acc - 1.0))).clamp(0.05, 1.0);
                moved += 1;
            }
        }
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Note, Pattern};

    #[test]
    fn extracts_swing_and_lays_it_on_hats() {
        // a swung 16th hat loop: off-16ths 0.3 steps late, accents on beats
        let mut on = Vec::new();
        for bar in 0..16 {
            for s in 0..16 {
                let late = if s % 2 == 1 { 0.3 } else { 0.0 };
                let w = if s % 4 == 0 { 1.0 } else { 0.5 };
                on.push(((bar * 16 + s) as f32 + late, w));
            }
        }
        let t = from_steps(&on, 90.0);
        assert!((t.swing - 0.3).abs() < 0.04, "swing {}", t.swing);
        assert!(t.accents[0] > t.accents[1]);
        let mut p = Project::new("g", 90.0);
        let mut pat = Pattern::new("A", 1);
        *pat.notes_mut("hat") = (0..16).map(|s| Note::new(s as f32, 1.0, 60, 0.7)).collect();
        p.patterns = vec![pat];
        let moved = apply(&mut p, &t, &["hat".to_string()], &[0], 1.0);
        assert_eq!(moved, 16);
        let hats = p.patterns[0].notes("hat");
        assert!((hats[1].offset - 0.3).abs() < 0.04 && hats[0].offset.abs() < 0.01);
        assert!(hats[0].vel > hats[1].vel);
    }
}
