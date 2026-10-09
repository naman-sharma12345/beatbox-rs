//! Piano-roll operations on note lists: selection, quantize (strength,
//! swing), split / merge / legato, arpeggiator, strum, rolls / flams /
//! ratchets, chord voicings, diatonic harmonizing, key detection, velocity
//! shaping, pattern variation and counter-melodies.
//!
//! Everything is a pure function over `Vec<Note>` so the tools layer stays
//! thin and each operation is unit-testable.

use crate::dsp::Rng;
use crate::project::Note;
use anyhow::{bail, Result};

/// Which notes an edit applies to. Default = everything.
#[derive(Clone, Debug)]
pub struct Selection {
    pub from: f32,
    pub to: f32,
    pub pitch_min: u8,
    pub pitch_max: u8,
    pub pitches: Vec<u8>,
    pub vel_min: f32,
    pub vel_max: f32,
}

impl Default for Selection {
    fn default() -> Self {
        Selection {
            from: 0.0,
            to: f32::MAX,
            pitch_min: 0,
            pitch_max: 127,
            pitches: Vec::new(),
            vel_min: 0.0,
            vel_max: 1.0,
        }
    }
}

impl Selection {
    pub fn matches(&self, n: &Note) -> bool {
        n.start >= self.from - 1e-4
            && n.start < self.to - 1e-4
            && n.pitch >= self.pitch_min
            && n.pitch <= self.pitch_max
            && (self.pitches.is_empty() || self.pitches.contains(&n.pitch))
            && n.vel >= self.vel_min - 1e-4
            && n.vel <= self.vel_max + 1e-4
    }
}

pub fn sort(notes: &mut [Note]) {
    notes.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.pitch.cmp(&b.pitch))
    });
}

/// Indices of selected notes.
pub fn selected(notes: &[Note], sel: &Selection) -> Vec<usize> {
    (0..notes.len())
        .filter(|&i| sel.matches(&notes[i]))
        .collect()
}

// ---------------- edit ----------------

#[derive(Clone, Debug, Default)]
pub struct EditSpec {
    pub move_steps: f32,
    pub transpose: i32,
    pub len_scale: f32,
    pub set_len: Option<f32>,
    pub vel_scale: f32,
    pub vel_add: f32,
    pub set_vel: Option<f32>,
    pub set_prob: Option<f32>,
    pub set_offset: Option<f32>,
}

/// Move / resize / transpose / re-velocity selected notes; returns count.
pub fn edit(notes: &mut [Note], sel: &Selection, e: &EditSpec, max_steps: f32) -> usize {
    let mut k = 0;
    for n in notes.iter_mut().filter(|n| sel.matches(n)) {
        n.start = (n.start + e.move_steps).clamp(0.0, (max_steps - 0.01).max(0.0));
        n.pitch = (n.pitch as i32 + e.transpose).clamp(0, 127) as u8;
        if let Some(l) = e.set_len {
            n.len = l;
        }
        if e.len_scale > 0.0 {
            n.len *= e.len_scale;
        }
        n.len = n.len.max(0.05);
        if let Some(v) = e.set_vel {
            n.vel = v;
        }
        if e.vel_scale > 0.0 {
            n.vel *= e.vel_scale;
        }
        n.vel = (n.vel + e.vel_add).clamp(0.01, 1.0);
        if let Some(p) = e.set_prob {
            n.prob = p.clamp(0.0, 1.0);
        }
        if let Some(o) = e.set_offset {
            n.offset = o.clamp(-0.5, 0.5);
        }
        k += 1;
    }
    sort(notes);
    k
}

// ---------------- quantize ----------------

/// Snap starts to `grid` steps with `strength` 0..1 and optional swing on
/// every second grid line (0..1 = up to a triplet feel); `ends` also snaps
/// note ends. Microtiming offsets are cleared when strength is 1.
pub fn quantize(
    notes: &mut [Note],
    sel: &Selection,
    grid: f32,
    strength: f32,
    swing: f32,
    ends: bool,
) -> usize {
    let g = grid.max(0.0625);
    let s = strength.clamp(0.0, 1.0);
    let mut k = 0;
    for n in notes.iter_mut().filter(|n| sel.matches(n)) {
        let idx = (n.start / g).round();
        let mut target = idx * g;
        if (idx as i64) % 2 == 1 {
            target += swing.clamp(0.0, 1.0) * g / 3.0;
        }
        let new_start = n.start + (target - n.start) * s;
        if ends {
            let end = n.start + n.len;
            let te = ((end / g).round() * g).max(target + g);
            let new_end = end + (te - end) * s;
            n.len = (new_end - new_start).max(0.05);
        }
        n.start = new_start.max(0.0);
        if s >= 1.0 {
            n.offset = 0.0;
        }
        k += 1;
    }
    sort(notes);
    k
}

// ---------------- split / merge / legato ----------------

/// Split selected notes every `every` steps (or once at absolute `at`).
pub fn split(notes: &mut Vec<Note>, sel: &Selection, every: Option<f32>, at: Option<f32>) -> usize {
    let mut out = Vec::with_capacity(notes.len());
    let mut k = 0;
    for n in notes.drain(..) {
        if !sel.matches(&n) {
            out.push(n);
            continue;
        }
        let cuts: Vec<f32> = if let Some(e) = every.filter(|e| *e > 0.01) {
            let mut v = Vec::new();
            let mut t = n.start + e;
            while t < n.end() - 0.01 {
                v.push(t);
                t += e;
            }
            v
        } else if let Some(a) = at.filter(|a| *a > n.start + 0.01 && *a < n.end() - 0.01) {
            vec![a]
        } else {
            Vec::new()
        };
        if cuts.is_empty() {
            out.push(n);
            continue;
        }
        k += 1;
        let mut s = n.start;
        for c in cuts.iter().chain(std::iter::once(&n.end())) {
            out.push(Note {
                start: s,
                len: c - s,
                ..n.clone()
            });
            s = *c;
        }
    }
    *notes = out;
    sort(notes);
    k
}

/// Merge consecutive same-pitch selected notes whose gap is <= `max_gap`.
pub fn merge(notes: &mut Vec<Note>, sel: &Selection, max_gap: f32) -> usize {
    sort(notes);
    let mut merged = 0;
    let mut i = 0;
    while i < notes.len() {
        if !sel.matches(&notes[i]) {
            i += 1;
            continue;
        }
        let j = (i + 1..notes.len()).find(|&j| {
            notes[j].pitch == notes[i].pitch
                && sel.matches(&notes[j])
                && notes[j].start >= notes[i].start
                && notes[j].start - notes[i].end() <= max_gap + 1e-4
        });
        match j {
            Some(j) => {
                let end = notes[j].end().max(notes[i].end());
                notes[i].len = end - notes[i].start;
                notes[i].vel = notes[i].vel.max(notes[j].vel);
                notes.remove(j);
                merged += 1;
            }
            None => i += 1,
        }
    }
    merged
}

/// Extend each selected note (or chord) to the start of the next one;
/// `overlap` steps extra gives true legato / 303 slides.
pub fn legato(notes: &mut [Note], sel: &Selection, overlap: f32, pattern_steps: f32) -> usize {
    sort(notes);
    let starts: Vec<f32> = {
        let mut v: Vec<f32> = notes
            .iter()
            .filter(|n| sel.matches(n))
            .map(|n| n.start)
            .collect();
        v.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
        v
    };
    let mut k = 0;
    for n in notes.iter_mut().filter(|n| sel.matches(n)) {
        let next = starts
            .iter()
            .find(|s| **s > n.start + 1e-3)
            .copied()
            .unwrap_or(pattern_steps);
        n.len = (next - n.start + overlap).max(0.05);
        k += 1;
    }
    k
}

// ---------------- chords: grouping, arp, strum, voicing ----------------

/// Groups of notes that start together (within `tol` steps) = chords.
pub fn chord_groups(notes: &[Note], idx: &[usize], tol: f32) -> Vec<Vec<usize>> {
    let mut v: Vec<usize> = idx.to_vec();
    v.sort_by(|&a, &b| {
        notes[a]
            .start
            .partial_cmp(&notes[b].start)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(notes[a].pitch.cmp(&notes[b].pitch))
    });
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in v {
        match groups.last_mut() {
            Some(g) if (notes[g[0]].start - notes[i].start).abs() <= tol => g.push(i),
            _ => groups.push(vec![i]),
        }
    }
    groups
}

pub const ARP_PATTERNS: [&str; 7] = [
    "up", "down", "updown", "downup", "random", "converge", "chord",
];

/// Replace each selected chord with an arpeggio across its length.
pub fn arpeggiate(
    notes: &mut Vec<Note>,
    sel: &Selection,
    pattern: &str,
    rate: f32,
    octaves: u32,
    gate: f32,
    rng: &mut Rng,
) -> Result<usize> {
    if !ARP_PATTERNS.contains(&pattern) {
        bail!(
            "unknown arp pattern '{pattern}'. Options: {}",
            ARP_PATTERNS.join(", ")
        );
    }
    let rate = rate.max(0.125);
    let idx = selected(notes, sel);
    let groups = chord_groups(notes, &idx, 0.25);
    let mut add = Vec::new();
    let mut remove = Vec::new();
    for g in &groups {
        let mut pitches: Vec<u8> = g.iter().map(|&i| notes[i].pitch).collect();
        pitches.sort_unstable();
        pitches.dedup();
        let base = pitches.clone();
        for o in 1..octaves.clamp(1, 4) {
            for p in &base {
                let q = *p as u32 + 12 * o;
                if q <= 127 {
                    pitches.push(q as u8);
                }
            }
        }
        let start = notes[g[0]].start;
        let len = g.iter().map(|&i| notes[i].len).fold(0.0f32, f32::max);
        let vel = g.iter().map(|&i| notes[i].vel).fold(0.0f32, f32::max);
        let seq: Vec<u8> = match pattern {
            "up" => pitches.clone(),
            "down" => pitches.iter().rev().copied().collect(),
            "updown" => {
                let mut s = pitches.clone();
                if pitches.len() > 2 {
                    s.extend(pitches[1..pitches.len() - 1].iter().rev());
                }
                s
            }
            "downup" => {
                let mut s: Vec<u8> = pitches.iter().rev().copied().collect();
                if pitches.len() > 2 {
                    s.extend(pitches[1..pitches.len() - 1].iter());
                }
                s
            }
            "converge" => {
                let mut s = Vec::new();
                let (mut lo, mut hi) = (0usize, pitches.len());
                while lo < hi {
                    s.push(pitches[lo]);
                    lo += 1;
                    if lo < hi {
                        hi -= 1;
                        s.push(pitches[hi]);
                    }
                }
                s
            }
            _ => pitches.clone(),
        };
        if seq.is_empty() {
            continue;
        }
        let count = ((len / rate).floor() as usize).max(1);
        for k in 0..count {
            let t = start + k as f32 * rate;
            let accent = if k % 4 == 0 { 1.0 } else { 0.85 };
            if pattern == "chord" {
                for p in &base {
                    add.push(Note::new(t, rate * gate.clamp(0.1, 1.0), *p, vel * accent));
                }
                continue;
            }
            let p = if pattern == "random" {
                seq[rng.below(seq.len())]
            } else {
                seq[k % seq.len()]
            };
            add.push(Note::new(t, rate * gate.clamp(0.1, 1.0), p, vel * accent));
        }
        remove.extend(g.iter().copied());
    }
    remove.sort_unstable();
    for i in remove.iter().rev() {
        notes.remove(*i);
    }
    let n = groups.len();
    notes.extend(add);
    sort(notes);
    Ok(n)
}

/// Spread chord notes in time like a strummed guitar. direction up|down|alternate.
pub fn strum(
    notes: &mut [Note],
    sel: &Selection,
    spread: f32,
    direction: &str,
    vel_taper: f32,
) -> usize {
    let idx = selected(notes, sel);
    let groups = chord_groups(notes, &idx, 0.25);
    for (gi, g) in groups.iter().enumerate() {
        let down = match direction {
            "down" => true,
            "alternate" => gi % 2 == 1,
            _ => false,
        };
        let mut order = g.clone();
        order.sort_by_key(|&i| notes[i].pitch);
        if down {
            order.reverse();
        }
        let k = order.len().max(1);
        for (j, &i) in order.iter().enumerate() {
            let d = spread * j as f32 / k as f32;
            notes[i].start += d;
            notes[i].len = (notes[i].len - d).max(0.1);
            notes[i].vel = (notes[i].vel * (1.0 - vel_taper * j as f32 / k as f32)).max(0.05);
        }
    }
    sort(notes);
    groups.len()
}

/// Inversion / drop-2 / drop-3 / spread / close voicings for selected chords.
pub fn voice(notes: &mut [Note], sel: &Selection, mode: &str, inversion: i32) -> Result<usize> {
    let idx = selected(notes, sel);
    let groups = chord_groups(notes, &idx, 0.25);
    for g in &groups {
        if g.len() < 2 {
            continue;
        }
        let mut ord = g.clone();
        ord.sort_by_key(|&i| notes[i].pitch);
        let mut p: Vec<i32> = ord.iter().map(|&i| notes[i].pitch as i32).collect();
        match mode {
            "inversion" => {
                for _ in 0..inversion.abs().min(8) {
                    if inversion > 0 {
                        let lo = p.remove(0);
                        p.push(lo + 12);
                    } else {
                        let hi = p.pop().unwrap_or(0);
                        p.insert(0, hi - 12);
                    }
                }
            }
            "close" => {
                let root = p[0];
                for v in p.iter_mut().skip(1) {
                    while *v - root >= 12 {
                        *v -= 12;
                    }
                    while *v <= root {
                        *v += 12;
                    }
                }
                p.sort_unstable();
            }
            "drop2" | "drop3" => {
                let k = if mode == "drop2" { 2 } else { 3 };
                if p.len() >= k {
                    let i = p.len() - k;
                    p[i] -= 12;
                    p.sort_unstable();
                }
            }
            "spread" | "open" => {
                // root low, every other voice up an octave
                for (j, v) in p.iter_mut().enumerate().skip(1) {
                    if j % 2 == 1 {
                        *v += 12;
                    }
                }
                p.sort_unstable();
            }
            other => {
                bail!("unknown voicing '{other}'. Options: inversion, close, drop2, drop3, spread")
            }
        }
        for (k, &i) in ord.iter().enumerate() {
            notes[i].pitch = p[k].clamp(0, 127) as u8;
        }
    }
    Ok(groups.len())
}

// ---------------- harmony ----------------

/// Scale degree index (and octave) of `pitch`, snapping down to the scale.
fn degree_of(pitch: u8, key_pc: u8, scale: &[u8]) -> (i32, i32) {
    let rel = pitch as i32 - key_pc as i32;
    let oct = rel.div_euclid(12);
    let pc = rel.rem_euclid(12) as u8;
    let mut d = 0;
    for (i, s) in scale.iter().enumerate() {
        if *s <= pc {
            d = i;
        }
    }
    (d as i32, oct)
}

fn pitch_of_degree(deg: i32, oct: i32, key_pc: u8, scale: &[u8]) -> i32 {
    let n = scale.len() as i32;
    let o = oct + deg.div_euclid(n);
    let d = deg.rem_euclid(n) as usize;
    key_pc as i32 + o * 12 + scale[d] as i32
}

/// Move a pitch by `steps` scale degrees (diatonic transposition).
pub fn diatonic_shift(pitch: u8, steps: i32, key_pc: u8, scale: &[u8]) -> u8 {
    let (d, o) = degree_of(pitch, key_pc, scale);
    pitch_of_degree(d + steps, o, key_pc, scale).clamp(0, 127) as u8
}

/// Snap a pitch to the nearest scale tone.
pub fn snap_to_scale(pitch: u8, key_pc: u8, scale: &[u8]) -> u8 {
    let mut best = pitch as i32;
    let mut bd = 99;
    for c in pitch as i32 - 6..=pitch as i32 + 6 {
        let pc = (c - key_pc as i32).rem_euclid(12) as u8;
        if scale.contains(&pc) && (c - pitch as i32).abs() < bd {
            bd = (c - pitch as i32).abs();
            best = c;
        }
    }
    best.clamp(0, 127) as u8
}

/// Diatonic interval name -> scale-degree steps.
pub fn interval_steps(name: &str) -> Result<i32> {
    Ok(match name.trim().to_lowercase().as_str() {
        "2nd" | "second" => 1,
        "3rd" | "third" | "thirds" => 2,
        "4th" | "fourth" => 3,
        "5th" | "fifth" => 4,
        "6th" | "sixth" | "sixths" => 5,
        "7th" | "seventh" => 6,
        "octave" | "8ve" => 7,
        "10th" | "tenth" => 9,
        other => {
            bail!("unknown interval '{other}'. Options: 2nd, 3rd, 4th, 5th, 6th, 7th, octave, 10th")
        }
    })
}

/// Add a diatonic harmony voice to every selected note. Returns new notes.
pub fn harmonize(
    notes: &[Note],
    sel: &Selection,
    steps: i32,
    below: bool,
    key_pc: u8,
    scale: &[u8],
    vel_scale: f32,
) -> Vec<Note> {
    let s = if below { -steps } else { steps };
    notes
        .iter()
        .filter(|n| sel.matches(n))
        .map(|n| {
            let mut h = n.clone();
            if steps == 7 {
                h.pitch = (n.pitch as i32 + if below { -12 } else { 12 }).clamp(0, 127) as u8;
            } else {
                h.pitch = diatonic_shift(snap_to_scale(n.pitch, key_pc, scale), s, key_pc, scale);
            }
            h.vel = (n.vel * vel_scale).clamp(0.05, 1.0);
            h
        })
        .collect()
}

// ---------------- key detection ----------------

const MAJOR_PROFILE: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR_PROFILE: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

fn pearson(a: &[f32; 12], b: &[f32]) -> f32 {
    let ma = a.iter().sum::<f32>() / 12.0;
    let mb = b.iter().sum::<f32>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        num += (a[i] - ma) * (b[i] - mb);
        da += (a[i] - ma).powi(2);
        db += (b[i] - mb).powi(2);
    }
    num / (da * db).sqrt().max(1e-9)
}

/// Krumhansl-Schmuckler key finding on a 12-bin pitch-class histogram.
/// Returns (root pc, "major"/"minor", correlation) best-first.
pub fn key_from_chroma(chroma: &[f32; 12]) -> Vec<(u8, &'static str, f32)> {
    let mut out = Vec::with_capacity(24);
    for root in 0..12u8 {
        let rot: Vec<f32> = (0..12).map(|i| chroma[(i + root as usize) % 12]).collect();
        out.push((root, "major", pearson(&MAJOR_PROFILE, &rot)));
        out.push((root, "minor", pearson(&MINOR_PROFILE, &rot)));
    }
    out.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Duration- and velocity-weighted pitch-class histogram of notes.
pub fn chroma_of(notes: &[Note]) -> [f32; 12] {
    let mut c = [0.0f32; 12];
    for n in notes {
        c[(n.pitch % 12) as usize] += n.len.max(0.25).min(16.0) * (0.3 + n.vel);
    }
    c
}

// ---------------- velocity ----------------

/// Shape velocities: ramp_up / ramp_down (across the selection span), accent
/// (pattern string like "X..x" repeated per step), compress (toward the
/// mean by `amount`), expand, random, set.
pub fn velocity_curve(
    notes: &mut [Note],
    sel: &Selection,
    shape: &str,
    amount: f32,
    accents: &str,
    lo: f32,
    hi: f32,
    rng: &mut Rng,
) -> Result<usize> {
    let idx = selected(notes, sel);
    if idx.is_empty() {
        return Ok(0);
    }
    let t0 = idx.iter().map(|&i| notes[i].start).fold(f32::MAX, f32::min);
    let t1 = idx.iter().map(|&i| notes[i].start).fold(f32::MIN, f32::max);
    let span = (t1 - t0).max(1e-3);
    let mean = idx.iter().map(|&i| notes[i].vel).sum::<f32>() / idx.len() as f32;
    let acc: Vec<char> = accents
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '|')
        .collect();
    for &i in &idx {
        let n = &mut notes[i];
        let t = (n.start - t0) / span;
        n.vel = match shape {
            "ramp_up" | "crescendo" => lo + (hi - lo) * t,
            "ramp_down" | "decrescendo" => hi - (hi - lo) * t,
            "accent" => {
                if acc.is_empty() {
                    bail!("accent needs accents like 'X..x' (X = accent, x = normal, . = soft)");
                }
                let c = acc[(n.start.round() as usize) % acc.len()];
                match c {
                    'X' => hi,
                    'x' => (lo + hi) * 0.5 + (hi - lo) * 0.15,
                    'o' => lo + (hi - lo) * 0.3,
                    _ => lo,
                }
            }
            "compress" => n.vel + (mean - n.vel) * amount.clamp(0.0, 1.0),
            "expand" => mean + (n.vel - mean) * (1.0 + amount.max(0.0)),
            "random" => n.vel + rng.bipolar() * amount.clamp(0.0, 1.0) * 0.5,
            "set" => hi,
            other => bail!("unknown shape '{other}'. Options: ramp_up, ramp_down, accent, compress, expand, random, set"),
        }
        .clamp(0.02, 1.0);
    }
    Ok(idx.len())
}

// ---------------- rolls / flams / ratchets ----------------

/// mode roll: replace each note with `count` evenly spaced hits filling
/// its length (velocity ramps from `vel_start` to 1x); ratchet: `count`
/// fast repeats inside `len` steps keeping the original velocity contour;
/// flam: add a quiet grace hit just before.
pub fn ratchet(
    notes: &mut Vec<Note>,
    sel: &Selection,
    mode: &str,
    count: u32,
    vel_start: f32,
    pitch_step: i32,
) -> Result<usize> {
    let mut out = Vec::new();
    let mut k = 0;
    for n in notes.drain(..) {
        if !sel.matches(&n) {
            out.push(n);
            continue;
        }
        k += 1;
        match mode {
            "flam" => {
                out.push(Note {
                    start: (n.start - 0.12).max(0.0),
                    len: 0.25,
                    vel: n.vel * 0.55,
                    ..n.clone()
                });
                out.push(n);
            }
            "roll" | "ratchet" => {
                let c = count.clamp(2, 32);
                let sub = n.len.max(0.25) / c as f32;
                for j in 0..c {
                    let f = j as f32 / (c - 1).max(1) as f32;
                    let v = if mode == "roll" {
                        n.vel * (vel_start + (1.0 - vel_start) * f)
                    } else {
                        n.vel * if j == 0 { 1.0 } else { 0.8 }
                    };
                    out.push(Note {
                        start: n.start + sub * j as f32,
                        len: sub * 0.9,
                        vel: v.clamp(0.05, 1.0),
                        pitch: (n.pitch as i32 + pitch_step * j as i32).clamp(0, 127) as u8,
                        ..n.clone()
                    });
                }
            }
            other => bail!("unknown mode '{other}'. Options: roll, ratchet, flam"),
        }
    }
    *notes = out;
    sort(notes);
    Ok(k)
}

// ---------------- variation ----------------

/// Vary a part while keeping its identity: `rhythmic` shifts/splits/drops a
/// few notes on the grid, `melodic` moves some notes by scale steps (never
/// the downbeats), `both` does both. `amount` 0..1 ~ share of notes touched.
pub fn variation(
    notes: &[Note],
    kind: &str,
    amount: f32,
    key_pc: u8,
    scale: &[u8],
    steps: f32,
    is_drum: bool,
    rng: &mut Rng,
) -> Result<Vec<Note>> {
    if !["rhythmic", "melodic", "both"].contains(&kind) {
        bail!("kind must be rhythmic, melodic or both");
    }
    let amt = amount.clamp(0.0, 1.0);
    let mut out: Vec<Note> = Vec::new();
    for n in notes {
        let on_beat = (n.start % 4.0).abs() < 1e-3;
        let mut m = n.clone();
        let touch = rng.chance(amt) && !(on_beat && rng.chance(0.7));
        if !touch {
            out.push(m);
            continue;
        }
        let rhythmic = kind != "melodic" && (kind == "rhythmic" || is_drum || rng.chance(0.5));
        if rhythmic {
            match rng.below(4) {
                0 if n.len >= 1.0 => {
                    // split into two
                    let h = (n.len / 2.0).max(0.5);
                    m.len = h;
                    out.push(m.clone());
                    out.push(Note {
                        start: n.start + h,
                        len: h,
                        vel: n.vel * 0.85,
                        ..n.clone()
                    });
                    continue;
                }
                1 => {
                    // drop (rest)
                    if !on_beat {
                        continue;
                    }
                }
                2 => {
                    // anticipate / delay by a 16th
                    let d = if rng.chance(0.5) { -1.0 } else { 1.0 };
                    m.start = (n.start + d).clamp(0.0, steps - 0.25);
                }
                _ => {
                    // ghost echo a 16th later
                    out.push(m.clone());
                    if n.start + 1.0 < steps {
                        out.push(Note {
                            start: n.start + 1.0,
                            len: n.len.min(1.0),
                            vel: n.vel * 0.55,
                            ..n.clone()
                        });
                    }
                    continue;
                }
            }
        } else if !is_drum {
            let s = [-2, -1, 1, 2][rng.below(4)];
            m.pitch = diatonic_shift(snap_to_scale(n.pitch, key_pc, scale), s, key_pc, scale);
        }
        out.push(m);
    }
    // de-duplicate identical start+pitch
    sort(&mut out);
    out.dedup_by(|a, b| (a.start - b.start).abs() < 1e-3 && a.pitch == b.pitch);
    Ok(out)
}

// ---------------- counter melody ----------------

/// Write a counter-line against `melody`: consonant (3rd/6th/5th/octave)
/// against whatever the melody holds, preferring contrary motion, and
/// answering in the melody's gaps (call and response). `register` is the
/// MIDI centre of the new line.
pub fn counter_melody(
    melody: &[Note],
    key_pc: u8,
    scale: &[u8],
    steps: f32,
    register: i32,
    density: f32,
    rng: &mut Rng,
) -> Vec<Note> {
    let mut mel = melody.to_vec();
    sort(&mut mel);
    let sounding = |t: f32| -> Option<&Note> {
        mel.iter()
            .rev()
            .find(|n| n.start <= t + 1e-3 && n.end() > t + 1e-3)
    };
    let mut out: Vec<Note> = Vec::new();
    let mut prev: Option<i32> = None;
    let mut prev_mel: Option<i32> = None;
    let grid = 2.0; // 8th notes
    let mut t = 0.0;
    while t < steps - 0.01 {
        let m = sounding(t);
        let in_gap = m.is_none();
        // denser in the melody's gaps, sparse under busy melody
        let p_play = if in_gap {
            (density * 1.6).min(0.95)
        } else {
            density * 0.6
        };
        let strong = (t % 8.0).abs() < 1e-3;
        if !(strong || rng.chance(p_play)) {
            t += grid;
            continue;
        }
        let candidates: Vec<i32> = (register - 9..=register + 9)
            .filter(|c| scale.contains(&((*c - key_pc as i32).rem_euclid(12) as u8)))
            .collect();
        let mut best = candidates.first().copied().unwrap_or(register);
        let mut best_score = f32::MIN;
        for &c in &candidates {
            let mut score = 0.0f32;
            if let Some(mn) = m {
                let iv = (mn.pitch as i32 - c).rem_euclid(12);
                score += match iv {
                    3 | 4 | 8 | 9 => 3.0,
                    0 | 7 => 1.5,
                    5 => 0.5,
                    1 | 2 | 10 | 11 => -4.0,
                    _ => -2.0,
                };
                if c >= mn.pitch as i32 {
                    score -= 2.0; // stay under the melody
                }
                if let (Some(pm), Some(pp)) = (prev_mel, prev) {
                    let dm = mn.pitch as i32 - pm;
                    let dc = c - pp;
                    if dm != 0 && dc != 0 && dm.signum() != dc.signum() {
                        score += 1.5; // contrary motion
                    }
                }
            } else {
                // answering phrase: chord-ish tones of the key
                let d = (c - key_pc as i32).rem_euclid(12);
                if [0, 3, 4, 7].contains(&d) {
                    score += 1.0;
                }
            }
            if let Some(pp) = prev {
                let leap = (c - pp).abs();
                score -= leap as f32 * 0.35;
                if leap == 0 {
                    score -= 0.8;
                }
            }
            score -= (c - register).abs() as f32 * 0.1;
            score += rng.f32() * 0.6;
            if score > best_score {
                best_score = score;
                best = c;
            }
        }
        let len = if in_gap { grid } else { grid * 2.0 };
        out.push(Note::new(
            t,
            len.min(steps - t),
            best.clamp(0, 127) as u8,
            if strong { 0.75 } else { 0.62 },
        ));
        prev = Some(best);
        prev_mel = m.map(|n| n.pitch as i32).or(prev_mel);
        t += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINOR: [u8; 7] = [0, 2, 3, 5, 7, 8, 10];
    const MAJOR: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];

    fn chord(t: f32, ps: &[u8]) -> Vec<Note> {
        ps.iter().map(|p| Note::new(t, 8.0, *p, 0.8)).collect()
    }

    #[test]
    fn quantize_strength_and_swing() {
        let mut v = vec![Note::new(0.3, 1.0, 60, 0.8), Note::new(2.4, 1.0, 60, 0.8)];
        quantize(&mut v, &Selection::default(), 1.0, 0.5, 0.0, false);
        assert!((v[0].start - 0.15).abs() < 1e-4);
        quantize(&mut v, &Selection::default(), 2.0, 1.0, 0.0, false);
        assert_eq!(v[0].start, 0.0);
        assert_eq!(v[1].start, 2.0);
        let mut w = vec![Note::new(2.1, 1.0, 60, 0.8)];
        quantize(&mut w, &Selection::default(), 2.0, 1.0, 1.0, false);
        assert!((w[0].start - (2.0 + 2.0 / 3.0)).abs() < 1e-4); // odd grid line -> swung
        let mut w = vec![Note::new(2.1, 1.0, 60, 0.8)];
        quantize(&mut w, &Selection::default(), 1.0, 1.0, 1.0, false);
        assert!((w[0].start - 2.0).abs() < 1e-4);
        let mut w = vec![Note::new(3.1, 1.0, 60, 0.8)];
        quantize(&mut w, &Selection::default(), 1.0, 1.0, 1.0, false);
        assert!((w[0].start - (3.0 + 1.0 / 3.0)).abs() < 1e-4);
    }

    #[test]
    fn split_merge_legato() {
        let mut v = vec![Note::new(0.0, 4.0, 60, 0.8)];
        assert_eq!(split(&mut v, &Selection::default(), Some(1.0), None), 1);
        assert_eq!(v.len(), 4);
        assert_eq!(merge(&mut v, &Selection::default(), 0.0), 3);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].len, 4.0);
        let mut w = vec![Note::new(0.0, 1.0, 60, 0.8), Note::new(4.0, 1.0, 62, 0.8)];
        legato(&mut w, &Selection::default(), 0.0, 16.0);
        assert_eq!(w[0].len, 4.0);
        assert_eq!(w[1].len, 12.0);
    }

    #[test]
    fn arpeggiate_and_strum_and_voicings() {
        let mut v = chord(0.0, &[60, 64, 67]);
        let mut rng = Rng::new(1);
        arpeggiate(&mut v, &Selection::default(), "up", 1.0, 1, 0.8, &mut rng).unwrap();
        assert_eq!(v.len(), 8);
        assert_eq!(
            v.iter().map(|n| n.pitch).collect::<Vec<_>>()[..4],
            [60, 64, 67, 60]
        );
        let mut s = chord(0.0, &[60, 64, 67]);
        strum(&mut s, &Selection::default(), 0.6, "up", 0.0);
        assert!(s[0].start < s[1].start && s[1].start < s[2].start);
        let mut c = chord(0.0, &[60, 64, 67, 71]);
        voice(&mut c, &Selection::default(), "drop2", 0).unwrap();
        let mut p: Vec<u8> = c.iter().map(|n| n.pitch).collect();
        p.sort();
        assert_eq!(p, vec![55, 60, 64, 71]);
        let mut c = chord(0.0, &[60, 64, 67]);
        voice(&mut c, &Selection::default(), "inversion", 1).unwrap();
        let mut p: Vec<u8> = c.iter().map(|n| n.pitch).collect();
        p.sort();
        assert_eq!(p, vec![64, 67, 72]);
    }

    #[test]
    fn harmonize_in_key_and_detect_key() {
        // A minor: A C E -> thirds above = C E G
        let v = vec![
            Note::new(0.0, 2.0, 57, 0.8),
            Note::new(2.0, 2.0, 60, 0.8),
            Note::new(4.0, 2.0, 64, 0.8),
        ];
        let h = harmonize(&v, &Selection::default(), 2, false, 9, &MINOR, 0.8);
        assert_eq!(
            h.iter().map(|n| n.pitch).collect::<Vec<_>>(),
            vec![60, 64, 67]
        );
        // a C major scale melody with tonic emphasis
        let mut m = Vec::new();
        for (i, p) in [60u8, 64, 67, 72, 65, 69, 67, 60, 62, 71, 60, 64]
            .iter()
            .enumerate()
        {
            m.push(Note::new(
                i as f32 * 2.0,
                if *p == 60 { 4.0 } else { 2.0 },
                *p,
                0.8,
            ));
        }
        let k = key_from_chroma(&chroma_of(&m));
        assert_eq!((k[0].0, k[0].1), (0, "major"));
        let _ = MAJOR;
    }

    #[test]
    fn ratchet_roll_flam() {
        let mut v = vec![Note::new(12.0, 4.0, 60, 1.0)];
        ratchet(&mut v, &Selection::default(), "roll", 8, 0.3, 0).unwrap();
        assert_eq!(v.len(), 8);
        assert!(v[0].vel < v[7].vel);
        let mut f = vec![Note::new(4.0, 1.0, 60, 1.0)];
        ratchet(&mut f, &Selection::default(), "flam", 0, 0.0, 0).unwrap();
        assert_eq!(f.len(), 2);
        assert!(f[0].start < 4.0 && f[0].vel < f[1].vel);
    }

    #[test]
    fn velocity_shapes() {
        let mut v: Vec<Note> = (0..8).map(|i| Note::new(i as f32, 1.0, 60, 0.5)).collect();
        let mut rng = Rng::new(2);
        velocity_curve(
            &mut v,
            &Selection::default(),
            "ramp_up",
            0.0,
            "",
            0.2,
            1.0,
            &mut rng,
        )
        .unwrap();
        assert!(v[0].vel < 0.25 && v[7].vel > 0.95);
        velocity_curve(
            &mut v,
            &Selection::default(),
            "accent",
            0.0,
            "X...",
            0.3,
            1.0,
            &mut rng,
        )
        .unwrap();
        assert_eq!(v[4].vel, 1.0);
        assert!(v[1].vel < 0.5);
    }

    #[test]
    fn variation_keeps_key_and_counter_melody_is_consonant() {
        let mel: Vec<Note> = (0..8)
            .map(|i| {
                Note::new(
                    i as f32 * 4.0,
                    3.0,
                    [69u8, 72, 71, 69, 67, 69, 72, 76][i],
                    0.8,
                )
            })
            .collect();
        let mut rng = Rng::new(3);
        let var = variation(&mel, "melodic", 0.8, 9, &MINOR, 32.0, false, &mut rng).unwrap();
        assert!(var.iter().all(
            |n| MINOR.contains(&(((n.pitch as i32 - 9).rem_euclid(12)) as u8))
                || mel.iter().any(|m| m.pitch == n.pitch)
        ));
        let cm = counter_melody(&mel, 9, &MINOR, 32.0, 60, 0.5, &mut rng);
        assert!(!cm.is_empty());
        for c in &cm {
            assert!(MINOR.contains(&(((c.pitch as i32 - 9).rem_euclid(12)) as u8)));
            if let Some(m) = mel.iter().find(|m| m.start <= c.start && m.end() > c.start) {
                let iv = (m.pitch as i32 - c.pitch as i32).rem_euclid(12);
                assert!(![1, 2, 10, 11].contains(&iv), "dissonant {iv}");
            }
        }
    }
}
