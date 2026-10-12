//! Groove feel: per-instrument micro-timing and velocity shaping.
//!
//! One global swing and one random jitter make every part breathe the same
//! way, which is how a programmed beat gives itself away. A drummer and a
//! band lean differently: the kick sits on the grid, the snare lays back a
//! hair, the hats carry the swing and an accent cycle, the bass locks to the
//! kick, chords strum and sit behind the beat. This module bakes those feels
//! into each note's `offset` (the start stays on the grid, so edits, the step
//! sequencer and the global swing rule keep working) and shapes velocities:
//! beat-position accents, ghost notes kept soft, 4-bar phrase arcs and a
//! build into each 8-bar boundary. Deterministic: the same project and seed
//! give the same feel.

use crate::project::{Note, Project};

/// How one role leans against the grid.
#[derive(Debug, Clone, Copy)]
pub struct Feel {
    /// Constant lean in steps (16ths): + is late (laid back), - is early.
    pub lean: f32,
    /// Share of the project's swing this role takes (0 = straight, 1 = full).
    pub swing: f32,
    /// Deterministic per-note timing spread in steps (+-).
    pub spread: f32,
    /// Velocity multiplier per 16th inside a beat (downbeat, e, and, a).
    pub accents: [f32; 4],
    /// Deterministic per-note velocity spread (fraction, +-).
    pub vel_spread: f32,
    /// How much the phrase arc / 8-bar build moves this role (0..1).
    pub phrase: f32,
}

/// The role a track plays, from its name.
pub fn role_of(track: &str) -> &'static str {
    let t = track.to_ascii_lowercase();
    let has = |ks: &[&str]| ks.iter().any(|k| t.contains(k));
    if has(&["vocal", "vox", "voice", "adlib"]) {
        "vocal"
    } else if has(&["kick", "bd"]) {
        "kick"
    } else if has(&["snare", "clap", "rim"]) {
        "snare"
    } else if has(&["hat", "shaker", "ride", "cym", "tamb"]) {
        "hat"
    } else if has(&["perc", "tom", "conga", "bongo", "cowbell", "fx"]) {
        "perc"
    } else if has(&["808", "bass", "sub"]) {
        "bass"
    } else if has(&["harmony", "pad", "keys", "chord", "piano", "rhodes", "organ", "string"]) {
        "keys"
    } else {
        "lead"
    }
}

/// The feel for a role. Leans are in steps: at 140 bpm a step is 107 ms,
/// so 0.08 is about 9 ms.
pub fn feel_for(role: &str) -> Feel {
    match role {
        "kick" => Feel { lean: 0.0, swing: 0.0, spread: 0.012, accents: [1.0, 0.86, 0.92, 0.86], vel_spread: 0.03, phrase: 0.3 },
        "snare" => Feel { lean: 0.07, swing: 0.4, spread: 0.02, accents: [1.0, 0.8, 0.88, 0.8], vel_spread: 0.04, phrase: 0.4 },
        "hat" => Feel { lean: 0.02, swing: 1.0, spread: 0.03, accents: [1.0, 0.62, 0.8, 0.66], vel_spread: 0.07, phrase: 1.0 },
        "perc" => Feel { lean: 0.04, swing: 0.8, spread: 0.035, accents: [1.0, 0.7, 0.85, 0.72], vel_spread: 0.08, phrase: 0.8 },
        "bass" => Feel { lean: 0.0, swing: 0.3, spread: 0.008, accents: [1.0, 0.9, 0.94, 0.9], vel_spread: 0.03, phrase: 0.35 },
        "keys" => Feel { lean: 0.06, swing: 0.6, spread: 0.025, accents: [1.0, 0.88, 0.93, 0.88], vel_spread: 0.05, phrase: 0.7 },
        "lead" => Feel { lean: 0.03, swing: 0.7, spread: 0.03, accents: [1.0, 0.88, 0.94, 0.88], vel_spread: 0.06, phrase: 0.8 },
        _ => Feel { lean: 0.0, swing: 0.0, spread: 0.0, accents: [1.0; 4], vel_spread: 0.0, phrase: 0.0 },
    }
}

/// Deterministic noise in -1..1 from a seed and two indices.
fn noise(seed: u64, a: u64, b: u64) -> f32 {
    let mut x = seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 23) as f32 - 1.0
}

/// Velocity across a 4-bar phrase and into the 8-bar boundary: bar 1 lands,
/// bars 2-3 settle, bar 4 lifts; the last bar of 8 builds over its 16 steps.
fn phrase_gain(bar: u32, step_in_bar: f32, steps_per_bar: f32, amount: f32) -> f32 {
    let arc = match bar % 4 {
        0 => 1.0,
        1 => 0.93,
        2 => 0.96,
        _ => 0.98,
    };
    let build = if bar % 8 == 7 { 0.9 + 0.14 * (step_in_bar / steps_per_bar).clamp(0.0, 1.0) } else { 1.0 };
    1.0 + (arc * build - 1.0) * amount
}

/// What `apply` did, for the log and the tests.
#[derive(Debug, Default, Clone)]
pub struct FeelReport {
    pub notes: usize,
    pub roles: Vec<(String, String)>,
    pub swing_baked: f32,
}

/// Bake per-role feel into every pattern of the project. The project's global
/// swing is folded into each role's offsets (hats take all of it, the kick
/// none) and then set to 0, so the swing is not applied twice.
pub fn apply(p: &mut Project, seed: u64, amount: f32) -> FeelReport {
    let amount = amount.clamp(0.0, 1.5);
    let swing_steps = p.swing.clamp(0.0, 1.0) * 0.5;
    let mut rep = FeelReport { swing_baked: p.swing, ..Default::default() };
    if amount <= 0.0 {
        return rep;
    }
    for t in &p.tracks {
        rep.roles.push((t.name.clone(), role_of(&t.name).to_string()));
    }
    for (pi, pat) in p.patterns.iter_mut().enumerate() {
        let spb = pat.steps_per_bar() as f32;
        for (ti, (track, notes)) in pat.clips.iter_mut().enumerate() {
            let role = role_of(track);
            if role == "vocal" {
                continue;
            }
            let f = feel_for(role);
            shape(notes, &f, role, seed ^ (pi as u64) << 20 ^ (ti as u64) << 40, swing_steps, spb, amount);
            rep.notes += notes.len();
        }
    }
    p.swing = 0.0;
    rep
}

fn shape(notes: &mut [Note], f: &Feel, role: &str, seed: u64, swing_steps: f32, spb: f32, amount: f32) {
    // chord strum: notes sharing a start, low to high, a few ms apart; the
    // top voice sings a little over the inner ones
    let mut order: Vec<usize> = (0..notes.len()).collect();
    order.sort_by(|&a, &b| notes[a].start.partial_cmp(&notes[b].start).unwrap().then(notes[a].pitch.cmp(&notes[b].pitch)));
    let mut strum_idx = vec![0usize; notes.len()];
    let mut top = vec![false; notes.len()];
    let mut stack = vec![1usize; notes.len()];
    let mut i = 0;
    while i < order.len() {
        let mut j = i;
        while j + 1 < order.len() && (notes[order[j + 1]].start - notes[order[i]].start).abs() < 1e-3 {
            j += 1;
        }
        for (k, &ix) in order[i..=j].iter().enumerate() {
            strum_idx[ix] = k;
            top[ix] = k == j - i;
            stack[ix] = j - i + 1;
        }
        i = j + 1;
    }
    for (ni, n) in notes.iter_mut().enumerate() {
        let pos = n.start;
        let on_grid = (pos - pos.round()).abs() < 1e-3;
        let step = pos.round() as i64;
        let in_beat = step.rem_euclid(4) as usize;
        let bar = (pos / spb).floor().max(0.0) as u32;
        let in_bar = pos - bar as f32 * spb;
        // timing
        let mut off = f.lean;
        if on_grid && step.rem_euclid(2) == 1 {
            off += swing_steps * f.swing;
        }
        off += f.spread * noise(seed, ni as u64, 1);
        if role == "keys" && stack[ni] > 1 {
            off += 0.035 * strum_idx[ni] as f32;
        }
        n.offset = (n.offset + off * amount).clamp(-0.5, 0.5);
        // velocity
        let ghost = n.vel < 0.4;
        let acc = if on_grid { f.accents[in_beat] } else { (f.accents[1] + f.accents[3]) * 0.5 };
        // the snare's backbeat is its downbeat: accents follow the beat it
        // lands on, not only the 16th inside it
        let acc = if role == "snare" && on_grid && in_beat == 0 && (step.rem_euclid(8) == 4) { 1.0 } else { acc };
        let mut v = n.vel * (1.0 + (acc - 1.0) * amount);
        v *= phrase_gain(bar, in_bar, spb, f.phrase * amount);
        if role == "keys" && stack[ni] > 1 {
            v *= if top[ni] { 1.06 } else { 0.94 };
        }
        v *= 1.0 + f.vel_spread * amount * noise(seed, ni as u64, 2);
        n.vel = if ghost { v.clamp(0.05, 0.39) } else { v.clamp(0.12, 1.0) };
    }
}

/// Velocity spread of a part: the standard deviation of its velocities.
/// A flat part reads 0.
pub fn vel_sd(notes: &[Note]) -> f32 {
    if notes.len() < 2 {
        return 0.0;
    }
    let m = notes.iter().map(|n| n.vel).sum::<f32>() / notes.len() as f32;
    (notes.iter().map(|n| (n.vel - m).powi(2)).sum::<f32>() / notes.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Pattern, Project};

    fn grid(n: usize, vel: f32) -> Vec<Note> {
        (0..n).map(|i| Note { start: i as f32, len: 1.0, pitch: 60, vel, ..Default::default() }).collect()
    }

    fn proj() -> Project {
        let mut p = Project::new("t", 140.0);
        p.patterns.clear();
        let mut pat = Pattern::new("a", 8);
        pat.clips.insert("kick".into(), grid(128, 0.9));
        pat.clips.insert("hat".into(), grid(128, 0.8));
        pat.clips.insert("snare".into(), (0..16).map(|b| Note { start: (b * 8 + 4) as f32, len: 1.0, pitch: 60, vel: 0.9, ..Default::default() }).collect());
        pat.clips.insert("harmony".into(), [60u8, 64, 67].iter().map(|&k| Note { start: 0.0, len: 16.0, pitch: k, vel: 0.7, ..Default::default() }).collect());
        p.patterns.push(pat);
        p.swing = 0.3;
        p
    }

    #[test]
    fn roles_from_names() {
        assert_eq!(role_of("Kick"), "kick");
        assert_eq!(role_of("open_hat"), "hat");
        assert_eq!(role_of("808"), "bass");
        assert_eq!(role_of("harmony"), "keys");
        assert_eq!(role_of("counter"), "lead");
        assert_eq!(role_of("vocal_main"), "vocal");
    }

    #[test]
    fn feel_is_per_role_and_deterministic() {
        let mut a = proj();
        let mut b = proj();
        let r = apply(&mut a, 7, 1.0);
        apply(&mut b, 7, 1.0);
        assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
        assert!(r.notes > 0);
        assert_eq!(a.swing, 0.0, "the swing is baked once");
        let pat = &a.patterns[0];
        // starts stay on the grid
        assert!(pat.notes("hat").iter().all(|n| n.start.fract() == 0.0));
        // hats take the swing on the off 16ths, the kick does not
        let hat_off: f32 = pat.notes("hat").iter().filter(|n| n.start as i64 % 2 == 1).map(|n| n.offset).sum::<f32>() / 64.0;
        let kick_off: f32 = pat.notes("kick").iter().filter(|n| n.start as i64 % 2 == 1).map(|n| n.offset).sum::<f32>() / 64.0;
        assert!(hat_off > 0.12 && kick_off.abs() < 0.02, "hat {hat_off} kick {kick_off}");
        // the snare lays back
        let sn: f32 = pat.notes("snare").iter().map(|n| n.offset).sum::<f32>() / 16.0;
        assert!(sn > 0.04, "{sn}");
        // no flat velocities: hats get an accent cycle
        assert!(vel_sd(pat.notes("hat")) > 0.08, "{}", vel_sd(pat.notes("hat")));
        let h = pat.notes("hat");
        assert!(h[0].vel > h[1].vel && h[2].vel > h[1].vel);
        // chords strum low to high
        let mut c: Vec<_> = pat.notes("harmony").to_vec();
        c.sort_by_key(|n| n.pitch);
        assert!(c[0].offset < c[1].offset && c[1].offset < c[2].offset);
        assert!(c[2].vel > c[1].vel, "the top voice sings");
    }

    #[test]
    fn ghosts_stay_ghosts_and_amount_zero_is_identity() {
        let mut p = proj();
        p.patterns[0].clips.insert("snare".into(), vec![Note { start: 3.0, len: 1.0, pitch: 60, vel: 0.3, ..Default::default() }]);
        let before = serde_json::to_value(&p.patterns).unwrap();
        let mut q = p.clone();
        apply(&mut q, 1, 0.0);
        assert_eq!(serde_json::to_value(&q.patterns).unwrap(), before);
        apply(&mut p, 1, 1.0);
        assert!(p.patterns[0].notes("snare")[0].vel < 0.4);
    }
}
