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
    /// Only notes with these stable ids (empty = no id filter).
    pub ids: Vec<String>,
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
            ids: Vec::new(),
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
            && (self.ids.is_empty() || self.ids.contains(&n.id))
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
        n.pitch = (n.pitch as i32).saturating_add(e.transpose).clamp(0, 127) as u8;
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

/// FL Articulate: each selected note's length becomes `ratio` of the gap to
/// the next onset (staccato 0.35, portato 0.7, tenuto 0.95, legato 1.0), so
/// a whole part changes feel in one call; max_len caps it (steps, 0 = none).
pub const ARTICULATIONS: &[(&str, f32)] = &[
    ("staccatissimo", 0.18),
    ("staccato", 0.35),
    ("portato", 0.7),
    ("tenuto", 0.95),
    ("legato", 1.0),
];

pub fn articulate(
    notes: &mut [Note],
    sel: &Selection,
    ratio: f32,
    max_len: f32,
    pattern_steps: f32,
) -> usize {
    sort(notes);
    let mut starts: Vec<f32> = notes
        .iter()
        .filter(|n| sel.matches(n))
        .map(|n| n.start)
        .collect();
    starts.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
    let r = ratio.clamp(0.05, 1.5);
    let mut k = 0;
    for n in notes.iter_mut().filter(|n| sel.matches(n)) {
        let next = starts
            .iter()
            .find(|s| **s > n.start + 1e-3)
            .copied()
            .unwrap_or(pattern_steps);
        let mut len = ((next - n.start) * r).max(0.05);
        if max_len > 0.0 {
            len = len.min(max_len);
        }
        n.len = len;
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
        c[(n.pitch % 12) as usize] += n.len.clamp(0.25, 16.0) * (0.3 + n.vel);
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

// ---------------- flip / chop / limit (FL piano-roll tools) ----------------

/// Flip selected notes. mode "pitch": mirror pitches around `axis` (default
/// the middle of the selection's range), so a rising line falls. mode
/// "time": reverse their order inside the span they occupy (a phrase played
/// backwards, lengths kept). Returns how many notes moved.
pub fn flip(notes: &mut [Note], sel: &Selection, mode: &str, axis: Option<f32>) -> Result<usize> {
    let idx = selected(notes, sel);
    if idx.is_empty() {
        return Ok(0);
    }
    match mode {
        "pitch" => {
            let lo = idx.iter().map(|&i| notes[i].pitch).min().unwrap_or(60) as f32;
            let hi = idx.iter().map(|&i| notes[i].pitch).max().unwrap_or(60) as f32;
            let ax = axis.unwrap_or((lo + hi) / 2.0);
            for &i in &idx {
                notes[i].pitch = (2.0 * ax - notes[i].pitch as f32).round().clamp(0.0, 127.0) as u8;
            }
        }
        "time" => {
            let s0 = idx.iter().map(|&i| notes[i].start).fold(f32::MAX, f32::min);
            let e1 = idx.iter().map(|&i| notes[i].end()).fold(0.0, f32::max);
            for &i in &idx {
                notes[i].start = (s0 + e1 - notes[i].end()).max(0.0);
            }
        }
        other => bail!("unknown mode '{other}'. Options: pitch, time"),
    }
    Ok(idx.len())
}

/// Chop selected notes with a rhythm: `pattern` is one character per `step`
/// steps, read on the song grid ('x' = hit, '-' = keep holding the previous
/// hit, '.' = rest), repeating. A held chord or 808 becomes that rhythm
/// (FL's pattern-based Chop). Returns how many notes came out.
pub fn chop(notes: &mut Vec<Note>, sel: &Selection, pattern: &str, step: f32) -> Result<usize> {
    let pat: Vec<char> = pattern
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '|')
        .collect();
    if pat.is_empty() || pat.iter().any(|c| !matches!(c, 'x' | 'X' | '-' | '.')) {
        bail!("pattern uses x (hit), - (hold), . (rest), e.g. 'x.x-x..x'");
    }
    if !pat.iter().any(|c| *c == 'x' || *c == 'X') {
        bail!("the pattern has no hit (x)");
    }
    let step = step.max(0.125);
    let idx = selected(notes, sel);
    let mut out = Vec::new();
    let mut made = 0;
    for &i in idx.iter().rev() {
        let n = notes.remove(i);
        let mut t = (n.start / step).floor() * step;
        let mut cur: Option<Note> = None;
        while t < n.end() - 1e-4 {
            let k = ((t / step).round() as usize) % pat.len();
            let c = pat[k];
            let s = t.max(n.start);
            let e = (t + step).min(n.end());
            match c {
                'x' | 'X' => {
                    if let Some(c0) = cur.take() {
                        out.push(c0);
                    }
                    let mut m = n.clone();
                    m.start = s;
                    m.len = e - s;
                    m.vel = if c == 'X' { n.vel } else { n.vel * 0.85 };
                    cur = Some(m);
                }
                '-' => {
                    if let Some(c0) = cur.as_mut() {
                        c0.len = e - c0.start;
                    }
                }
                _ => {
                    if let Some(c0) = cur.take() {
                        out.push(c0);
                    }
                }
            }
            t += step;
        }
        if let Some(c0) = cur.take() {
            out.push(c0);
        }
    }
    for m in out {
        if m.len > 1e-3 {
            notes.push(m);
            made += 1;
        }
    }
    sort(notes);
    Ok(made)
}

/// Keep selected notes inside [lo, hi] by moving them whole octaves (FL's
/// Limit): a bass line that wandered too high, a lead too low for the mix.
pub fn limit_range(notes: &mut [Note], sel: &Selection, lo: u8, hi: u8) -> Result<usize> {
    if hi < lo || hi - lo < 11 {
        bail!("the range must span at least an octave (lo {lo}, hi {hi})");
    }
    let mut moved = 0;
    for i in selected(notes, sel) {
        let mut p = notes[i].pitch as i32;
        while p < lo as i32 {
            p += 12;
        }
        while p > hi as i32 {
            p -= 12;
        }
        if p != notes[i].pitch as i32 {
            notes[i].pitch = p as u8;
            moved += 1;
        }
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    #[test]
    fn flip_chop_limit() {
        let mut v = vec![
            Note::new(0.0, 2.0, 60, 0.8),
            Note::new(2.0, 2.0, 64, 0.8),
            Note::new(4.0, 4.0, 67, 0.8),
        ];
        flip(&mut v, &Selection::default(), "pitch", None).unwrap();
        // mirrored around 63.5: a major triad comes back as a minor one
        assert_eq!(
            v.iter().map(|n| n.pitch).collect::<Vec<_>>(),
            vec![67, 63, 60]
        );
        let mut t = vec![Note::new(0.0, 1.0, 60, 0.8), Note::new(1.0, 3.0, 62, 0.8)];
        flip(&mut t, &Selection::default(), "time", None).unwrap();
        assert!((t[0].start - 3.0).abs() < 1e-4 && t[1].start.abs() < 1e-4);
        let mut c = vec![Note::new(0.0, 8.0, 48, 1.0)];
        let made = chop(&mut c, &Selection::default(), "x-.x", 1.0).unwrap();
        sort(&mut c);
        assert_eq!(made, 4, "{c:?}");
        assert!((c[0].len - 2.0).abs() < 1e-4 && (c[1].start - 3.0).abs() < 1e-4);
        assert!(chop(&mut c, &Selection::default(), "..", 1.0).is_err());
        let mut l = vec![Note::new(0.0, 1.0, 80, 0.8), Note::new(1.0, 1.0, 30, 0.8)];
        assert_eq!(
            limit_range(&mut l, &Selection::default(), 36, 59).unwrap(),
            2
        );
        assert!(l.iter().all(|n| (36..=59).contains(&n.pitch)));
    }

    use super::*;

    #[test]
    fn articulate_sets_length_from_the_gap() {
        let mut n = vec![
            Note::new(0.0, 1.0, 60, 0.8),
            Note::new(4.0, 1.0, 62, 0.8),
            Note::new(6.0, 1.0, 64, 0.8),
        ];
        assert_eq!(articulate(&mut n, &Selection::default(), 0.5, 0.0, 16.0), 3);
        assert!(
            (n[0].len - 2.0).abs() < 1e-4
                && (n[1].len - 1.0).abs() < 1e-4
                && (n[2].len - 5.0).abs() < 1e-4
        );
        articulate(&mut n, &Selection::default(), 1.0, 3.0, 16.0);
        assert!((n[2].len - 3.0).abs() < 1e-4);
    }

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

/// Write editable delayed copies, never recursively echoing the new copies.
/// Originals and their stable IDs are unchanged. Copies get fresh IDs in Engine.
pub fn note_echoes(
    notes: &mut Vec<Note>,
    sel: &Selection,
    delay: f32,
    repeats: u32,
    decay: f32,
    transpose: i32,
    steps: f32,
) -> Result<usize> {
    if !delay.is_finite()
        || delay <= 0.0
        || !(1..=64).contains(&repeats)
        || !decay.is_finite()
        || !(0.0..=1.0).contains(&decay)
        || !(-127..=127).contains(&transpose)
    {
        bail!("delay must be positive, repeats1..64, decay0..1, transpose-127..127");
    }
    let source: Vec<Note> = notes.iter().filter(|n| sel.matches(n)).cloned().collect();
    if source.len().saturating_mul(repeats as usize) > 100_000 {
        bail!("echo would create more than100000 candidate notes; narrow the selection");
    }
    let mut echoes = Vec::new();
    for n in &source {
        for k in 1..=repeats {
            let start = n.start + delay * k as f32;
            let pitch = i32::from(n.pitch) + transpose * k as i32;
            let vel = n.vel * decay.powi(k as i32);
            if start >= steps || vel < 0.02 || !(0..=127).contains(&pitch) {
                continue;
            }
            let mut echo = n.clone();
            echo.id.clear();
            echo.start = start;
            echo.pitch = pitch as u8;
            echo.vel = vel;
            echo.len = echo.len.min(steps - start);
            echo.slide_to = echo.slide_to.and_then(|p| {
                let p = i32::from(p) + transpose * k as i32;
                (0..=127).contains(&p).then_some(p as u8)
            });
            echoes.push(echo);
        }
    }
    let count = echoes.len();
    notes.extend(echoes);
    sort(notes);
    Ok(count)
}

/// Add copies of a selected phrase without moving originals. All copies must fit.
/// Period is the distance between phrase starts, in sixteenth steps.
pub fn repeat_phrase(
    notes: &mut Vec<Note>,
    sel: &Selection,
    period: f32,
    repeats: u32,
    steps: f32,
) -> Result<usize> {
    if !period.is_finite() || period <= 0.0 || !(1..=64).contains(&repeats) {
        bail!("period must be positive and repeats must be1..64");
    }
    let source: Vec<_> = notes.iter().filter(|n| sel.matches(n)).cloned().collect();
    if source.len().saturating_mul(repeats as usize) > 100_000 {
        bail!("repeat would create more than100000 notes; narrow the selection");
    }
    if source.iter().any(|n| {
        !n.start.is_finite()
            || !n.len.is_finite()
            || n.len < 0.05
            || n.start + period * repeats as f32 + n.len > steps + 1e-4
    }) {
        bail!("repeated phrase does not fit the pattern; lengthen it or reduce repeats/period");
    }
    let mut added = Vec::new();
    for n in source {
        for k in 1..=repeats {
            let mut copy = n.clone();
            copy.id.clear();
            copy.start += period * k as f32;
            added.push(copy);
        }
    }
    let count = added.len();
    notes.extend(added);
    sort(notes);
    Ok(count)
}

/// Scale a phrase around an anchor without changing tempo, pitch or IDs.
/// Validate every selected note before changing any note.
pub fn stretch_phrase(
    notes: &mut [Note],
    sel: &Selection,
    factor: f32,
    anchor: Option<f32>,
    steps: f32,
) -> Result<usize> {
    if !factor.is_finite()
        || !(0.0625..=16.0).contains(&factor)
        || anchor.is_some_and(|a| !a.is_finite() || a < 0.0)
    {
        bail!("factor must be0.0625..16 and anchor must be nonnegative");
    }
    let idx = selected(notes, sel);
    if idx.is_empty() {
        return Ok(0);
    }
    let origin =
        anchor.unwrap_or_else(|| idx.iter().map(|i| notes[*i].start).fold(f32::MAX, f32::min));
    let edits: Vec<_> = idx
        .iter()
        .map(|i| {
            let n = &notes[*i];
            (*i, origin + (n.start - origin) * factor, n.len * factor)
        })
        .collect();
    if edits.iter().any(|(_, start, len)| {
        !start.is_finite()
            || !len.is_finite()
            || *start < 0.0
            || *start >= steps
            || *len < 0.05
            || *start + *len > steps + 1e-4
    }) {
        bail!("stretched phrase does not fit the pattern or makes a note shorter than0.05 steps; lengthen the pattern or choose a smaller stretch");
    }
    for (i, start, len) in edits {
        notes[i].start = start;
        notes[i].len = len;
    }
    sort(notes);
    Ok(idx.len())
}

/// Rotate note starts within their own bar. Lengths and note expression stay intact.
pub fn rotate_rhythm(
    notes: &mut [Note],
    sel: &Selection,
    shift: f32,
    bar_steps: f32,
) -> Result<usize> {
    if !shift.is_finite() || !bar_steps.is_finite() || bar_steps <= 0.0 {
        bail!("shift must be finite and bar length positive");
    }
    let idx = selected(notes, sel);
    for i in &idx {
        let n = &mut notes[*i];
        let bar = (n.start / bar_steps).floor();
        n.start = bar * bar_steps
            + (n.start.rem_euclid(bar_steps) + shift.rem_euclid(bar_steps)).rem_euclid(bar_steps);
    }
    sort(notes);
    Ok(idx.len())
}

/// Shorten selected same-pitch overlaps, never lengthen or delete a note.
/// Equal starts and gaps smaller than the minimum note length are skipped.
pub fn trim_overlaps(notes: &mut [Note], sel: &Selection) -> (usize, usize) {
    let mut starts: Vec<Vec<f32>> = vec![Vec::new(); 128];
    for n in notes.iter() {
        starts[n.pitch as usize].push(n.start);
    }
    for times in &mut starts {
        times.sort_by(f32::total_cmp);
    }
    let mut changed = 0;
    let mut skipped = 0;
    for n in notes.iter_mut().filter(|n| sel.matches(n)) {
        let times = &starts[n.pitch as usize];
        let first = times.partition_point(|t| *t < n.start);
        let after = times.partition_point(|t| *t <= n.start);
        if after - first > 1 {
            skipped += 1;
            continue;
        }
        if let Some(next) = times.get(after) {
            let gap = next - n.start;
            if n.len > gap {
                if gap < 0.05 {
                    skipped += 1;
                } else {
                    n.len = gap;
                    changed += 1;
                }
            }
        }
    }
    (changed, skipped)
}

/// Remove exact musical duplicates within the selection. First selected copy wins.
/// Unselected notes neither suppress selected copies nor get removed.
pub fn remove_exact_duplicates(notes: &mut Vec<Note>, sel: &Selection) -> usize {
    let mut seen = std::collections::HashSet::new();
    let before = notes.len();
    // Canonicalize signed zero, since Note equality considers -0 and +0 equal.
    let bits = |v: f32| if v == 0.0 { 0 } else { v.to_bits() };
    notes.retain(|n| {
        if !sel.matches(n) {
            return true;
        }
        // NaNs are never equal under Note's exact musical equality.
        if [n.start, n.len, n.vel, n.prob, n.offset]
            .iter()
            .any(|v| v.is_nan())
        {
            return true;
        }
        seen.insert((
            bits(n.start),
            bits(n.len),
            n.pitch,
            bits(n.vel),
            bits(n.prob),
            bits(n.offset),
            n.slide_to,
        ))
    });
    before - notes.len()
}

/// Keep one onset group out of every N selected groups, with a zero-based phase.
/// Notes starting together remain together, including chords. Unselected notes stay intact.
pub fn thin_rhythm(
    notes: &mut Vec<Note>,
    sel: &Selection,
    every: u32,
    phase: u32,
) -> Result<usize> {
    if !(2..=64).contains(&every) || phase >= every {
        bail!("every must be2..64 and phase must be less than every");
    }
    let mut idx = selected(notes, sel);
    if idx.iter().any(|i| !notes[*i].start.is_finite()) {
        bail!("selected note starts must be finite");
    }
    idx.sort_by(|a, b| notes[*a].start.total_cmp(&notes[*b].start));
    let mut remove = vec![false; notes.len()];
    let mut group = 0u32;
    let mut origin: Option<f32> = None;
    for i in idx {
        let start = notes[i].start;
        match origin {
            None => origin = Some(start),
            Some(t) if start - t > 1e-4 => {
                group += 1;
                origin = Some(start);
            }
            _ => {}
        }
        remove[i] = group % every != phase;
    }
    let count = remove.iter().filter(|x| **x).count();
    let mut i = 0;
    notes.retain(|_| {
        let keep = !remove[i];
        i += 1;
        keep
    });
    Ok(count)
}

/// Scale selected velocities to a target peak, keeping their relative dynamics.
/// All selected velocities must be positive and finite; validate before editing.
pub fn normalize_velocity(notes: &mut [Note], sel: &Selection, peak: f32) -> Result<usize> {
    if !peak.is_finite() || !(0.02..=1.0).contains(&peak) {
        bail!("target peak must be0.02..1");
    }
    let idx = selected(notes, sel);
    if idx
        .iter()
        .any(|i| !notes[*i].vel.is_finite() || notes[*i].vel <= 0.0)
    {
        bail!("selected velocities must be positive and finite");
    }
    let max = idx.iter().map(|i| notes[*i].vel).fold(0.0, f32::max);
    if max == 0.0 {
        return Ok(0);
    }
    for i in &idx {
        notes[*i].vel = (notes[*i].vel / max) * peak;
    }
    Ok(idx.len())
}

/// Move selected notes as a rigid phrase, rejecting any out-of-pattern result.
pub fn shift_phrase(notes: &mut [Note], sel: &Selection, shift: f32, steps: f32) -> Result<usize> {
    if !shift.is_finite() || !steps.is_finite() || steps <= 0.0 {
        bail!("shift must be finite and pattern length positive");
    }
    let idx = selected(notes, sel);
    if idx.iter().any(|i| {
        let n = &notes[*i];
        let start = n.start + shift;
        !start.is_finite()
            || !n.len.is_finite()
            || n.len < 0.05
            || start < 0.0
            || start >= steps
            || start + n.len > steps + 1e-4
    }) {
        bail!("shifted phrase does not fit the pattern; reduce the shift or extend the pattern");
    }
    for i in &idx {
        notes[*i].start += shift;
    }
    sort(notes);
    Ok(idx.len())
}

/// Transpose as a rigid phrase without clipping notes onto MIDI pitch limits.
pub fn transpose_phrase(notes: &mut [Note], sel: &Selection, semitones: i32) -> Result<usize> {
    if !(-127..=127).contains(&semitones) {
        bail!("semitones must be an integer from -127 to 127");
    }
    let idx = selected(notes, sel);
    if idx
        .iter()
        .any(|i| !(0..=127).contains(&(notes[*i].pitch as i32 + semitones)))
    {
        bail!("transposed phrase exceeds MIDI pitch range; reduce the shift");
    }
    for i in &idx {
        notes[*i].pitch = (notes[*i].pitch as i32 + semitones) as u8;
    }
    Ok(idx.len())
}

/// Quantize release positions only. Validate all proposed lengths before editing.
pub fn quantize_ends(
    notes: &mut [Note],
    sel: &Selection,
    grid: f32,
    strength: f32,
    steps: f32,
) -> Result<usize> {
    if !grid.is_finite()
        || grid < 0.0625
        || !strength.is_finite()
        || !(0.0..=1.0).contains(&strength)
        || !steps.is_finite()
        || steps <= 0.0
    {
        bail!("grid must be at least 0.0625, strength 0..1, pattern length positive");
    }
    let idx = selected(notes, sel);
    let mut edits = Vec::new();
    for i in idx {
        let n = &notes[i];
        let end = n.start + n.len;
        let target = (end / grid).round() * grid;
        let len = (end + (target - end) * strength) - n.start;
        if !n.start.is_finite()
            || n.start < 0.0
            || !len.is_finite()
            || len < 0.05
            || n.start + len > steps + 1e-4
        {
            bail!("quantized release does not fit; reduce strength or choose a finer grid");
        }
        if (len - n.len).abs() > 1e-5 {
            edits.push((i, len));
        }
    }
    for (i, len) in &edits {
        notes[*i].len = *len;
    }
    Ok(edits.len())
}

/// Keep the highest/lowest selected pitch in each onset group; retain tied layers.
pub fn reduce_chords(notes: &mut Vec<Note>, sel: &Selection, highest: bool) -> Result<usize> {
    let mut idx = selected(notes, sel);
    if idx.iter().any(|i| !notes[*i].start.is_finite()) {
        bail!("selected starts must be finite");
    }
    idx.sort_by(|a, b| notes[*a].start.total_cmp(&notes[*b].start));
    let mut remove = vec![false; notes.len()];
    let mut at = 0;
    while at < idx.len() {
        let origin = notes[idx[at]].start;
        let mut end = at + 1;
        while end < idx.len() && notes[idx[end]].start - origin <= 1e-4 {
            end += 1;
        }
        let pitch = idx[at..end]
            .iter()
            .map(|i| notes[*i].pitch)
            .reduce(|a, b| if highest { a.max(b) } else { a.min(b) })
            .unwrap();
        for i in &idx[at..end] {
            remove[*i] = notes[*i].pitch != pitch;
        }
        at = end;
    }
    let count = remove.iter().filter(|x| **x).count();
    let mut i = 0;
    notes.retain(|_| {
        let keep = !remove[i];
        i += 1;
        keep
    });
    Ok(count)
}

/// Fold selected pitches into a register by the smallest whole-octave move.
/// Rejects atomically when the range has no representative of a pitch class.
/// Only pitch changes. Slide targets and other expression stay unchanged.
pub fn fold_register(notes: &mut [Note], sel: &Selection, low: u8, high: u8) -> Result<usize> {
    if low > high || high > 127 {
        bail!("register must satisfy 0 <= low <= high <= 127");
    }
    let mut changes = Vec::new();
    for (i, n) in notes.iter().enumerate().filter(|(_, n)| sel.matches(n)) {
        if n.pitch >= low && n.pitch <= high {
            continue;
        }
        let target = (low..=high)
            .filter(|p| p % 12 == n.pitch % 12)
            .min_by_key(|p| (*p as i16 - n.pitch as i16).abs())
            .ok_or_else(|| {
                anyhow::anyhow!("pitch {} has no octave within the chosen register", n.pitch)
            })?;
        changes.push((i, target));
    }
    let count = changes.len();
    for (i, pitch) in changes {
        notes[i].pitch = pitch;
    }
    Ok(count)
}

/// Equalize selected onset-group spacing between the first and last groups.
/// Chord-relative microspacing and all fields except start stay unchanged.
/// Validate every result before applying any changes.
pub fn distribute_rhythm(notes: &mut [Note], sel: &Selection, steps: f32) -> Result<usize> {
    let idx = selected(notes, sel);
    if !steps.is_finite()
        || steps <= 0.0
        || idx.iter().any(|&i| {
            !notes[i].start.is_finite() || !notes[i].len.is_finite() || notes[i].len < 0.05
        })
    {
        bail!("invalid pattern or selected note bounds");
    }
    let groups = chord_groups(notes, &idx, 1e-4);
    if groups.len() < 3 {
        return Ok(0);
    }
    let first = notes[groups[0][0]].start;
    let last = notes[groups.last().unwrap()[0]].start;
    let mut changes = Vec::new();
    for (k, g) in groups.iter().enumerate() {
        let anchor = notes[g[0]].start;
        let target = if k == 0 || k + 1 == groups.len() {
            anchor
        } else {
            first + (last - first) * (k as f32 / (groups.len() - 1) as f32)
        };
        let delta = target - anchor;
        for &i in g {
            let n = &notes[i];
            let start = n.start + delta;
            if !start.is_finite() || start < 0.0 || start + n.len > steps + 1e-4 {
                bail!("distributed note would leave the pattern");
            }
            if (start - n.start).abs() > 1e-5 {
                changes.push((i, start));
            }
        }
    }
    let count = changes.len();
    for (i, start) in changes {
        notes[i].start = start;
    }
    Ok(count)
}

/// Align selected onset-group releases to an existing shortest/longest release.
/// Only lengths change. Validate every candidate before writing any note.
pub fn align_chord_ends(
    notes: &mut [Note],
    sel: &Selection,
    longest: bool,
    steps: f32,
) -> Result<usize> {
    let idx = selected(notes, sel);
    if !steps.is_finite()
        || steps <= 0.0
        || idx.iter().any(|&i| {
            !notes[i].start.is_finite()
                || notes[i].start < 0.0
                || !notes[i].len.is_finite()
                || notes[i].len < 0.05
                || notes[i].start + notes[i].len > steps + 1e-4
        })
    {
        bail!("invalid pattern or selected note bounds");
    }
    let mut changes = Vec::new();
    for g in chord_groups(notes, &idx, 1e-4) {
        if g.len() < 2 {
            continue;
        }
        let end = g
            .iter()
            .map(|&i| notes[i].start + notes[i].len)
            .reduce(|a, b| if longest { a.max(b) } else { a.min(b) })
            .unwrap();
        for i in g {
            let len = end - notes[i].start;
            if !len.is_finite() || len < 0.05 || end > steps + 1e-4 {
                bail!("aligned release would leave valid note bounds");
            }
            if (len - notes[i].len).abs() > 1e-5 {
                changes.push((i, len));
            }
        }
    }
    let count = changes.len();
    for (i, len) in changes {
        notes[i].len = len;
    }
    Ok(count)
}

/// Cycle a monophonic selected pitch sequence in chronological order.
/// Positive rotation moves each pitch later, wrapping to the first note.
/// Simultaneous selected notes are ambiguous and rejected before any write.
pub fn rotate_pitches(notes: &mut [Note], sel: &Selection, shift: i32) -> Result<usize> {
    let idx = selected(notes, sel);
    if idx.iter().any(|&i| !notes[i].start.is_finite()) {
        bail!("selected note starts must be finite");
    }
    let groups = chord_groups(notes, &idx, 1e-4);
    if groups.iter().any(|g| g.len() > 1) {
        bail!("select one note per onset to rotate melody pitches");
    }
    if groups.len() < 2 {
        return Ok(0);
    }
    let n = groups.len();
    let shift = (shift as i64).rem_euclid(n as i64) as usize;
    let pitches: Vec<u8> = groups.iter().map(|g| notes[g[0]].pitch).collect();
    let mut count = 0;
    for (k, g) in groups.iter().enumerate() {
        let pitch = pitches[(k + n - shift) % n];
        if notes[g[0]].pitch != pitch {
            notes[g[0]].pitch = pitch;
            count += 1;
        }
    }
    Ok(count)
}

/// Reverse the gaps between selected onset groups, not the notes or pitches.
/// Keep first/last anchors fixed and retain chord-relative microspacing.
pub fn reverse_rhythm_gaps(notes: &mut [Note], sel: &Selection, steps: f32) -> Result<usize> {
    let idx = selected(notes, sel);
    if !steps.is_finite()
        || steps <= 0.0
        || idx.iter().any(|&i| {
            !notes[i].start.is_finite() || !notes[i].len.is_finite() || notes[i].len < 0.05
        })
    {
        bail!("invalid pattern or selected note bounds");
    }
    let groups = chord_groups(notes, &idx, 1e-4);
    if groups.len() < 3 {
        return Ok(0);
    }
    let first = notes[groups[0][0]].start;
    let last = notes[groups.last().unwrap()[0]].start;
    let mut changes = Vec::new();
    for (k, g) in groups.iter().enumerate() {
        let anchor = notes[g[0]].start;
        let target = if k == 0 || k + 1 == groups.len() {
            anchor
        } else {
            first + (last - notes[groups[groups.len() - 1 - k][0]].start)
        };
        let delta = target - anchor;
        for &i in g {
            let n = &notes[i];
            let start = n.start + delta;
            if !start.is_finite() || start < 0.0 || start + n.len > steps + 1e-4 {
                bail!("reversed rhythm would leave the pattern");
            }
            if (start - n.start).abs() > 1e-5 {
                changes.push((i, start));
            }
        }
    }
    let count = changes.len();
    for (i, start) in changes {
        notes[i].start = start;
    }
    Ok(count)
}

/// Reverse selected pitches in time order without moving notes or expression.
pub fn reverse_pitches(notes: &mut [Note], sel: &Selection) -> Result<usize> {
    let idx = selected(notes, sel);
    if idx.iter().any(|&i| !notes[i].start.is_finite()) {
        bail!("selected note starts must be finite");
    }
    let groups = chord_groups(notes, &idx, 1e-4);
    if groups.iter().any(|g| g.len() > 1) {
        bail!("select one note per onset to reverse melody pitches");
    }
    let pitches: Vec<u8> = groups.iter().rev().map(|g| notes[g[0]].pitch).collect();
    let mut count = 0;
    for (g, pitch) in groups.iter().zip(pitches) {
        if notes[g[0]].pitch != pitch {
            notes[g[0]].pitch = pitch;
            count += 1;
        }
    }
    Ok(count)
}

/// Cycle existing selected velocities through chronological single-note onsets.
/// No values are generated or flattened; all non-velocity fields stay unchanged.
/// Simultaneous selected notes are rejected rather than choosing a chord order.
pub fn rotate_velocities(notes: &mut [Note], sel: &Selection, shift: i32) -> Result<usize> {
    let idx = selected(notes, sel);
    if idx.iter().any(|&i| {
        !notes[i].start.is_finite()
            || !notes[i].vel.is_finite()
            || !(0.0..=1.0).contains(&notes[i].vel)
    }) {
        bail!("selected note starts and velocities must be valid");
    }
    let groups = chord_groups(notes, &idx, 1e-4);
    if groups.iter().any(|g| g.len() > 1) {
        bail!("select one note per onset to rotate velocity accents");
    }
    if groups.len() < 2 {
        return Ok(0);
    }
    let n = groups.len();
    let shift = (shift as i64).rem_euclid(n as i64) as usize;
    let values: Vec<f32> = groups.iter().map(|g| notes[g[0]].vel).collect();
    let mut count = 0;
    for (k, g) in groups.iter().enumerate() {
        let vel = values[(k + n - shift) % n];
        if notes[g[0]].vel != vel {
            notes[g[0]].vel = vel;
            count += 1;
        }
    }
    Ok(count)
}

/// Reverse existing selected velocities without moving rhythm or pitch.
/// Selected simultaneous notes are ambiguous and rejected before any write.
pub fn reverse_velocities(notes: &mut [Note], sel: &Selection) -> Result<usize> {
    let idx = selected(notes, sel);
    if idx.iter().any(|&i| {
        !notes[i].start.is_finite()
            || !notes[i].vel.is_finite()
            || !(0.0..=1.0).contains(&notes[i].vel)
    }) {
        bail!("selected note starts and velocities must be valid");
    }
    let groups = chord_groups(notes, &idx, 1e-4);
    if groups.iter().any(|g| g.len() > 1) {
        bail!("select one note per onset to reverse velocity accents");
    }
    let values: Vec<f32> = groups.iter().rev().map(|g| notes[g[0]].vel).collect();
    let mut count = 0;
    for (g, vel) in groups.iter().zip(values) {
        if notes[g[0]].vel != vel {
            notes[g[0]].vel = vel;
            count += 1;
        }
    }
    Ok(count)
}

/// Mirror selected MIDI dynamics inside their existing [softest, loudest]
/// range. Chords are unambiguous: each velocity is mirrored independently.
pub fn invert_velocities(notes: &mut [Note], sel: &Selection) -> Result<usize> {
    let idx = selected(notes, sel);
    if idx
        .iter()
        .any(|&i| !notes[i].vel.is_finite() || !(0.0..=1.0).contains(&notes[i].vel))
    {
        bail!("selected velocities must be finite and between 0 and 1");
    }
    let low = idx
        .iter()
        .map(|&i| notes[i].vel)
        .fold(f32::INFINITY, f32::min);
    let high = idx
        .iter()
        .map(|&i| notes[i].vel)
        .fold(f32::NEG_INFINITY, f32::max);
    if idx.len() < 2 || high - low < 1e-6 {
        return Ok(0);
    }
    let mut changed = 0;
    for i in idx {
        let v =
            (low as f64 + high as f64 - notes[i].vel as f64).clamp(low as f64, high as f64) as f32;
        if v != notes[i].vel {
            notes[i].vel = v;
            changed += 1;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod invert_velocity_tests {
    use super::*;
    #[test]
    fn selected_chords_mirror_only_velocity_and_roundtrip() {
        let mut notes = vec![
            Note::new(0., 1., 60, 0.2),
            Note::new(0., 2., 64, 0.4),
            Note::new(4., 1., 67, 0.8),
            Note::new(8., 1., 72, 0.9),
        ];
        for (i, n) in notes.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.7;
            n.offset = 0.1;
            n.slide_to = Some(70);
        }
        let before = notes.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into(), "n2".into()],
            ..Default::default()
        };
        assert_eq!(invert_velocities(&mut notes, &sel).unwrap(), 3);
        for (i, expected) in [0.8, 0.6, 0.2, 0.9].iter().enumerate() {
            assert!((notes[i].vel - expected).abs() < 1e-6);
            let mut copy = notes[i].clone();
            copy.vel = before[i].vel;
            assert_eq!(copy, before[i]);
        }
        invert_velocities(&mut notes, &sel).unwrap();
        for (i, n) in notes.iter().enumerate() {
            assert!((n.vel - before[i].vel).abs() < 1e-6);
        }
        let mut equal = vec![Note::new(0., 1., 60, 0.5), Note::new(2., 1., 62, 0.5)];
        assert_eq!(
            invert_velocities(&mut equal, &Selection::default()).unwrap(),
            0
        );
        assert_eq!(
            invert_velocities(&mut [], &Selection::default()).unwrap(),
            0
        );
        let mut bad = before;
        bad[1].vel = 1.1;
        let copy = bad.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into()],
            vel_max: 2.,
            ..Default::default()
        };
        assert!(invert_velocities(&mut bad, &sel).is_err());
        assert_eq!(bad, copy);
    }
}

/// Set selected notes at each shared nominal onset to their mean velocity.
/// Solo onsets and other note fields are unchanged; validate before writing.
pub fn balance_chord_velocities(notes: &mut [Note], sel: &Selection) -> Result<usize> {
    let idx = selected(notes, sel);
    if idx.iter().any(|&i| {
        !notes[i].start.is_finite()
            || !notes[i].vel.is_finite()
            || !(0.0..=1.0).contains(&notes[i].vel)
    }) {
        bail!("selected starts and velocities must be valid");
    }
    let groups = chord_groups(notes, &idx, 1e-4);
    let mut changed = 0;
    for g in groups.iter().filter(|g| g.len() > 1) {
        let mean = (g.iter().map(|&i| notes[i].vel as f64).sum::<f64>() / g.len() as f64) as f32;
        for &i in g {
            if notes[i].vel != mean {
                notes[i].vel = mean;
                changed += 1;
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod balance_chord_velocity_tests {
    use super::*;
    #[test]
    fn chords_balance_individually_selection_and_fields_preserved() {
        let mut n = vec![
            Note::new(0., 1., 60, 0.2),
            Note::new(0., 2., 64, 0.8),
            Note::new(4., 1., 60, 0.4),
            Note::new(4., 2., 67, 0.8),
            Note::new(8., 1., 72, 0.9),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.7;
            n.offset = 0.1;
            n.slide_to = Some(74);
        }
        let before = n.clone();
        assert_eq!(
            balance_chord_velocities(&mut n, &Selection::default()).unwrap(),
            4
        );
        for (i, v) in [0.5, 0.5, 0.6, 0.6, 0.9].iter().enumerate() {
            assert!((n[i].vel - v).abs() < 1e-6);
            let mut c = n[i].clone();
            c.vel = before[i].vel;
            assert_eq!(c, before[i]);
        }
        assert_eq!(
            balance_chord_velocities(&mut n, &Selection::default()).unwrap(),
            0
        );
        n = before.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into(), "n2".into()],
            ..Default::default()
        };
        balance_chord_velocities(&mut n, &sel).unwrap();
        assert_eq!(n[2..], before[2..]);
        n = before;
        n[1].vel = 1.1;
        let before = n.clone();
        assert!(balance_chord_velocities(
            &mut n,
            &Selection {
                vel_max: 2.,
                ..Default::default()
            }
        )
        .is_err());
        assert_eq!(n, before);
    }
}

/// Shorten selected sustains at the pattern edge, never lengthen or delete.
/// Unrepresentable tiny remainders reject atomically rather than changing starts.
pub fn trim_pattern_ends(notes: &mut [Note], sel: &Selection, steps: f32) -> Result<usize> {
    if !steps.is_finite() || steps <= 0. {
        bail!("invalid pattern length");
    }
    let mut changes = Vec::new();
    for (i, n) in notes.iter().enumerate().filter(|(_, n)| sel.matches(n)) {
        if !n.start.is_finite() || !n.len.is_finite() || n.start < 0. || n.len < 0.05 {
            bail!("selected note has invalid bounds");
        }
        if n.start + n.len > steps {
            let len = steps - n.start;
            if len < 0.05 {
                bail!("selected note has less than0.05 steps before pattern end; move or remove it first");
            }
            changes.push((i, len));
        }
    }
    let count = changes.len();
    for (i, len) in changes {
        notes[i].len = len;
    }
    Ok(count)
}

#[cfg(test)]
mod trim_pattern_end_tests {
    use super::*;
    #[test]
    fn tails_only_selection_and_atomic_bounds() {
        let mut n = vec![
            Note::new(12., 8., 60, 0.4),
            Note::new(4., 2., 64, 0.6),
            Note::new(14., 8., 67, 0.8),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.7;
            n.offset = 0.1;
            n.slide_to = Some(72);
        }
        let before = n.clone();
        let sel = Selection {
            ids: vec!["n0".into()],
            ..Default::default()
        };
        assert_eq!(trim_pattern_ends(&mut n, &sel, 16.).unwrap(), 1);
        assert_eq!(n[0].len, 4.);
        let mut expected = before.clone();
        expected[0].len = 4.;
        assert_eq!(n, expected);
        assert_eq!(trim_pattern_ends(&mut n, &sel, 16.).unwrap(), 0);
        n = before;
        n.push(Note::new(15.98, 1., 72, 0.9));
        let before = n.clone();
        assert!(trim_pattern_ends(&mut n, &Selection::default(), 16.).is_err());
        assert_eq!(n, before);
        assert!(trim_pattern_ends(&mut n, &sel, 0.).is_err());
    }
}

/// Reverse selected durations in nominal onset order, without moving notes.
/// Chords are ambiguous. Validate every destination before writing any value.
pub fn reverse_lengths(notes: &mut [Note], sel: &Selection, steps: f32) -> Result<usize> {
    if !steps.is_finite() || steps <= 0. {
        bail!("pattern length must be positive and finite");
    }
    let idx = selected(notes, sel);
    if idx.iter().any(|&i| {
        !notes[i].start.is_finite()
            || notes[i].start < 0.
            || !notes[i].len.is_finite()
            || notes[i].len < 0.05
    }) {
        bail!("selected starts and lengths must be valid");
    }
    let groups = chord_groups(notes, &idx, 1e-4);
    if groups.iter().any(|g| g.len() > 1) {
        bail!("select one note per onset to reverse lengths");
    }
    let lengths: Vec<f32> = groups.iter().rev().map(|g| notes[g[0]].len).collect();
    if groups
        .iter()
        .zip(&lengths)
        .any(|(g, len)| notes[g[0]].start + len > steps)
    {
        bail!("reversed lengths must fit within the pattern");
    }
    let mut count = 0;
    for (g, len) in groups.iter().zip(lengths) {
        if notes[g[0]].len != len {
            notes[g[0]].len = len;
            count += 1;
        }
    }
    Ok(count)
}
#[cfg(test)]
mod reverse_length_tests {
    use super::*;
    #[test]
    fn selection_fields_chords_and_atomic_fit() {
        let mut n = vec![
            Note::new(8., 1., 67, 0.8),
            Note::new(0., 4., 60, 0.3),
            Note::new(4., 2., 64, 0.5),
            Note::new(12., 2., 72, 0.9),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.6;
            n.offset = 0.1;
            n.slide_to = Some(73);
        }
        let before = n.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into(), "n2".into()],
            ..Default::default()
        };
        assert_eq!(reverse_lengths(&mut n, &sel, 16.).unwrap(), 2);
        let mut expected = before.clone();
        expected[0].len = 4.;
        expected[1].len = 1.;
        assert_eq!(n, expected);
        reverse_lengths(&mut n, &sel, 16.).unwrap();
        assert_eq!(n, before);
        assert_eq!(reverse_lengths(&mut [], &sel, 16.).unwrap(), 0);
        n[0].start = 15.;
        let before = n.clone();
        assert!(reverse_lengths(&mut n, &sel, 16.).is_err());
        assert_eq!(n, before);
        n[0].start = 0.;
        let before = n.clone();
        assert!(reverse_lengths(&mut n, &sel, 16.).is_err());
        assert_eq!(n, before);
        assert!(reverse_lengths(&mut n, &sel, f32::NAN).is_err());
    }
}

/// Clear per-note timing offsets and/or restore guaranteed chance. This does
/// not reset global swing, groove routing, velocity, gates or slide targets.
pub fn reset_humanization(
    notes: &mut [Note],
    sel: &Selection,
    timing: bool,
    chance: bool,
) -> Result<usize> {
    if !timing && !chance {
        bail!("choose timing, chance or both");
    }
    let mut changed = 0;
    for i in selected(notes, sel) {
        let n = &mut notes[i];
        let diff = (timing && n.offset != 0.) || (chance && n.prob != 1.);
        if timing {
            n.offset = 0.;
        }
        if chance {
            n.prob = 1.;
        }
        if diff {
            changed += 1;
        }
    }
    Ok(changed)
}
#[cfg(test)]
mod reset_humanization_tests {
    use super::*;
    #[test]
    fn scope_modes_and_no_collateral_normalization() {
        let mut n = vec![
            Note::new(0., 0.02, 60, 0.),
            Note::new(4., 2., 64, 0.4),
            Note::new(8., 3., 67, 0.8),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.5;
            n.offset = 0.2;
            n.slide_to = Some(72);
        }
        let before = n.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into()],
            ..Default::default()
        };
        assert_eq!(reset_humanization(&mut n, &sel, true, false).unwrap(), 2);
        let mut expected = before.clone();
        for n in &mut expected[..2] {
            n.offset = 0.;
        }
        assert_eq!(n, expected);
        assert_eq!(reset_humanization(&mut n, &sel, true, false).unwrap(), 0);
        assert_eq!(reset_humanization(&mut n, &sel, false, true).unwrap(), 2);
        for n in &mut expected[..2] {
            n.prob = 1.;
        }
        assert_eq!(n, expected);
        n = before.clone();
        reset_humanization(&mut n, &sel, true, true).unwrap();
        assert_eq!(n, expected);
        let unchanged = n.clone();
        assert!(reset_humanization(&mut n, &sel, false, false).is_err());
        assert_eq!(n, unchanged);
    }
}

/// Set per-note trigger chance only, with exact preservation of other fields.
pub fn set_note_chance(notes: &mut [Note], sel: &Selection, chance: f32) -> Result<usize> {
    if !chance.is_finite() || !(0.0..=1.0).contains(&chance) {
        bail!("chance must be finite and between0 and1");
    }
    let mut count = 0;
    for i in selected(notes, sel) {
        if notes[i].prob != chance {
            notes[i].prob = chance;
            count += 1;
        }
    }
    Ok(count)
}
#[cfg(test)]
mod set_note_chance_tests {
    use super::*;
    #[test]
    fn only_chance_changes_with_strict_bounds() {
        let mut n = vec![
            Note::new(0., 0.02, 60, 0.),
            Note::new(4., 2., 64, 0.4),
            Note::new(8., 3., 67, 0.8),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.offset = 0.2;
            n.slide_to = Some(72);
        }
        let before = n.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into()],
            ..Default::default()
        };
        assert_eq!(set_note_chance(&mut n, &sel, 0.5).unwrap(), 2);
        let mut expected = before.clone();
        expected[0].prob = 0.5;
        expected[1].prob = 0.5;
        assert_eq!(n, expected);
        assert_eq!(set_note_chance(&mut n, &sel, 0.5).unwrap(), 0);
        for chance in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
            assert!(set_note_chance(&mut n, &sel, chance).is_err());
            assert_eq!(n, expected);
        }
        assert_eq!(set_note_chance(&mut n, &sel, 0.).unwrap(), 2);
        assert_eq!(set_note_chance(&mut n, &sel, 1.).unwrap(), 2);
        assert_eq!(n, before);
    }
}

/// Set signed per-note microtiming only. Nominal starts and global swing stay.
pub fn set_note_offset(notes: &mut [Note], sel: &Selection, offset: f32) -> Result<usize> {
    if !offset.is_finite() || !(-0.5..=0.5).contains(&offset) {
        bail!("offset must be finite and between-0.5 and0.5 steps");
    }
    let mut count = 0;
    for i in selected(notes, sel) {
        if notes[i].offset != offset {
            notes[i].offset = offset;
            count += 1;
        }
    }
    Ok(count)
}
#[cfg(test)]
mod set_note_offset_tests {
    use super::*;
    #[test]
    fn only_offset_changes_with_strict_bounds() {
        let mut n = vec![
            Note::new(0., 0.02, 60, 0.),
            Note::new(4., 2., 64, 0.4),
            Note::new(8., 3., 67, 0.8),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.5;
            n.slide_to = Some(72);
        }
        let before = n.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into()],
            ..Default::default()
        };
        assert_eq!(set_note_offset(&mut n, &sel, -0.25).unwrap(), 2);
        let mut expected = before.clone();
        expected[0].offset = -0.25;
        expected[1].offset = -0.25;
        assert_eq!(n, expected);
        assert_eq!(set_note_offset(&mut n, &sel, -0.25).unwrap(), 0);
        for offset in [-0.51, 0.51, f32::NAN, f32::INFINITY] {
            assert!(set_note_offset(&mut n, &sel, offset).is_err());
            assert_eq!(n, expected);
        }
        for value in [-0.5, 0.5, 0.] {
            assert_eq!(set_note_offset(&mut n, &sel, value).unwrap(), 2);
        }
        assert_eq!(n, before);
    }
}

/// Rotate selected durations through onset order, with atomic fit validation.
pub fn rotate_lengths(
    notes: &mut [Note],
    sel: &Selection,
    shift: i32,
    steps: f32,
) -> Result<usize> {
    if !steps.is_finite() || steps <= 0. {
        bail!("pattern length must be positive and finite");
    }
    let idx = selected(notes, sel);
    if idx.iter().any(|&i| {
        !notes[i].start.is_finite()
            || notes[i].start < 0.
            || !notes[i].len.is_finite()
            || notes[i].len < 0.05
    }) {
        bail!("selected starts and lengths must be valid");
    }
    let groups = chord_groups(notes, &idx, 1e-4);
    if groups.iter().any(|g| g.len() > 1) {
        bail!("select one note per onset to rotate lengths");
    }
    let mut lengths: Vec<f32> = groups.iter().map(|g| notes[g[0]].len).collect();
    if !lengths.is_empty() {
        let shift = (i64::from(shift)).rem_euclid(lengths.len() as i64) as usize;
        lengths.rotate_right(shift);
    }
    if groups
        .iter()
        .zip(&lengths)
        .any(|(g, len)| notes[g[0]].start + len > steps)
    {
        bail!("rotated lengths must fit within the pattern");
    }
    let mut count = 0;
    for (g, len) in groups.iter().zip(lengths) {
        if notes[g[0]].len != len {
            notes[g[0]].len = len;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod rotate_length_tests {
    use super::*;
    #[test]
    fn fields_order_wrap_noop_and_rejection() {
        let mut n = vec![
            Note::new(8., 1., 67, 0.8),
            Note::new(0., 4., 60, 0.),
            Note::new(4., 2., 64, 0.5),
            Note::new(12., 2., 72, 0.9),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.6;
            n.offset = 0.1;
            n.slide_to = Some(73);
        }
        let before = n.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into(), "n2".into()],
            ..Default::default()
        };
        assert_eq!(rotate_lengths(&mut n, &sel, 1, 16.).unwrap(), 3);
        let mut expected = before.clone();
        expected[0].len = 2.;
        expected[1].len = 1.;
        expected[2].len = 4.;
        assert_eq!(n, expected);
        rotate_lengths(&mut n, &sel, -1, 16.).unwrap();
        assert_eq!(n, before);
        assert_eq!(rotate_lengths(&mut n, &sel, 3, 16.).unwrap(), 0);
        rotate_lengths(&mut n, &sel, i32::MIN, 16.).unwrap();
        rotate_lengths(&mut n, &sel, 2, 16.).unwrap();
        assert_eq!(n, before);
        assert_eq!(rotate_lengths(&mut [], &sel, 1, 16.).unwrap(), 0);
        n[0].start = 15.;
        let b = n.clone();
        assert!(rotate_lengths(&mut n, &sel, 1, 16.).is_err());
        assert_eq!(n, b);
        n = before;
        n.push(Note::new(0., 2., 50, 0.5));
        let b = n.clone();
        assert!(rotate_lengths(&mut n, &Selection::default(), 1, 16.).is_err());
        assert_eq!(n, b);
        assert!(rotate_lengths(&mut n, &sel, 1, f32::NAN).is_err());
    }
}

/// Mirror selected durations within their min/max range. Validate before writing.
pub fn invert_lengths(notes: &mut [Note], sel: &Selection, steps: f32) -> Result<usize> {
    if !steps.is_finite() || steps <= 0. {
        bail!("pattern length must be positive and finite");
    }
    let idx = selected(notes, sel);
    if idx.is_empty() {
        return Ok(0);
    }
    if idx.iter().any(|&i| {
        !notes[i].start.is_finite()
            || notes[i].start < 0.
            || !notes[i].len.is_finite()
            || notes[i].len < 0.05
    }) {
        bail!("selected starts and lengths must be valid");
    }
    let lo = idx
        .iter()
        .map(|&i| notes[i].len)
        .fold(f32::INFINITY, f32::min);
    let hi = idx.iter().map(|&i| notes[i].len).fold(0., f32::max);
    let values: Vec<f32> = idx
        .iter()
        .map(|&i| (f64::from(lo) + f64::from(hi) - f64::from(notes[i].len)) as f32)
        .collect();
    if idx
        .iter()
        .zip(&values)
        .any(|(&i, &len)| !len.is_finite() || len < 0.05 || notes[i].start + len > steps)
    {
        bail!("inverted lengths must fit within the pattern");
    }
    let mut count = 0;
    for (i, len) in idx.into_iter().zip(values) {
        if notes[i].len != len {
            notes[i].len = len;
            count += 1;
        }
    }
    Ok(count)
}
#[cfg(test)]
mod invert_length_tests {
    use super::*;
    #[test]
    fn fields_chords_partial_noop_and_atomic_fit() {
        let mut n = vec![
            Note::new(0., 4., 60, 0.),
            Note::new(0., 2., 64, 0.5),
            Note::new(8., 1., 67, 0.8),
            Note::new(12., 2., 72, 0.9),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.6;
            n.offset = 0.1;
            n.slide_to = Some(73);
        }
        let before = n.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into(), "n2".into()],
            ..Default::default()
        };
        assert_eq!(invert_lengths(&mut n, &sel, 16.).unwrap(), 3);
        let mut expected = before.clone();
        expected[0].len = 1.;
        expected[1].len = 3.;
        expected[2].len = 4.;
        assert_eq!(n, expected);
        invert_lengths(&mut n, &sel, 16.).unwrap();
        assert_eq!(n, before);
        assert_eq!(invert_lengths(&mut [], &sel, 16.).unwrap(), 0);
        let single = Selection {
            ids: vec!["n0".into()],
            ..Default::default()
        };
        assert_eq!(invert_lengths(&mut n, &single, 16.).unwrap(), 0);
        n[2].start = 15.;
        let b = n.clone();
        assert!(invert_lengths(&mut n, &sel, 16.).is_err());
        assert_eq!(n, b);
        assert!(invert_lengths(&mut n, &sel, f32::NAN).is_err());
        n[0].len = f32::INFINITY;
        assert!(invert_lengths(&mut n, &sel, 16.).is_err());
    }
}

/// Give selected notes at each shared onset their mean duration.
pub fn balance_chord_lengths(notes: &mut [Note], sel: &Selection, steps: f32) -> Result<usize> {
    if !steps.is_finite() || steps <= 0. {
        bail!("pattern length must be positive and finite");
    }
    let idx = selected(notes, sel);
    if idx.iter().any(|&i| {
        !notes[i].start.is_finite()
            || notes[i].start < 0.
            || !notes[i].len.is_finite()
            || notes[i].len < 0.05
            || notes[i].start + notes[i].len > steps
    }) {
        bail!("selected starts and lengths must be valid and fit the pattern");
    }
    let mut changes = Vec::new();
    for g in chord_groups(notes, &idx, 1e-4)
        .iter()
        .filter(|g| g.len() > 1)
    {
        let mean =
            (g.iter().map(|&i| f64::from(notes[i].len)).sum::<f64>() / g.len() as f64) as f32;
        for &i in g {
            if !mean.is_finite() || notes[i].start + mean > steps {
                bail!("averaged lengths must fit within the pattern");
            }
            if notes[i].len != mean {
                changes.push((i, mean));
            }
        }
    }
    let count = changes.len();
    for (i, len) in changes {
        notes[i].len = len;
    }
    Ok(count)
}
#[cfg(test)]
mod balance_chord_length_tests {
    use super::*;
    #[test]
    fn partial_chords_fields_solo_noop_and_atomic_validation() {
        let mut n = vec![
            Note::new(0., 4., 60, 0.),
            Note::new(0., 2., 64, 0.5),
            Note::new(0., 1., 67, 0.8),
            Note::new(8., 2., 72, 0.9),
            Note::new(8., 4., 76, 0.7),
            Note::new(12., 2., 80, 0.5),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.6;
            n.offset = 0.1;
            n.slide_to = Some(73);
        }
        let before = n.clone();
        let sel = Selection {
            ids: vec![
                "n0".into(),
                "n1".into(),
                "n3".into(),
                "n4".into(),
                "n5".into(),
            ],
            ..Default::default()
        };
        assert_eq!(balance_chord_lengths(&mut n, &sel, 16.).unwrap(), 4);
        let mut expected = before.clone();
        for i in [0, 1, 3, 4] {
            expected[i].len = 3.;
        }
        assert_eq!(n, expected);
        assert_eq!(balance_chord_lengths(&mut n, &sel, 16.).unwrap(), 0);
        assert_eq!(balance_chord_lengths(&mut [], &sel, 16.).unwrap(), 0);
        n = before;
        n[4].len = 9.;
        let b = n.clone();
        assert!(balance_chord_lengths(&mut n, &sel, 16.).is_err());
        assert_eq!(n, b);
        assert!(balance_chord_lengths(&mut n, &sel, f32::NAN).is_err());
        n[4].len = f32::INFINITY;
        assert!(balance_chord_lengths(&mut n, &sel, 16.).is_err());
    }
}

/// Scale selected durations only, with atomic minimum-length and fit validation.
pub fn scale_lengths(
    notes: &mut [Note],
    sel: &Selection,
    factor: f32,
    steps: f32,
) -> Result<usize> {
    if !factor.is_finite() || !(0.0625..=16.).contains(&factor) {
        bail!("factor must be between0.0625 and16");
    }
    if !steps.is_finite() || steps <= 0. {
        bail!("pattern length must be positive and finite");
    }
    let idx = selected(notes, sel);
    let mut changes = Vec::new();
    for i in idx {
        let n = &notes[i];
        let len = n.len * factor;
        if !n.start.is_finite()
            || n.start < 0.
            || !n.len.is_finite()
            || n.len < 0.05
            || !len.is_finite()
            || len < 0.05
            || n.start + len > steps
        {
            bail!("scaled lengths must be valid and fit within the pattern");
        }
        if n.len != len {
            changes.push((i, len));
        }
    }
    let count = changes.len();
    for (i, len) in changes {
        notes[i].len = len;
    }
    Ok(count)
}
#[cfg(test)]
mod scale_length_tests {
    use super::*;
    #[test]
    fn fields_chords_selection_bounds_and_atomic_fit() {
        let mut n = vec![
            Note::new(0., 4., 60, 0.),
            Note::new(0., 2., 64, 0.5),
            Note::new(8., 1., 67, 0.8),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.6;
            n.offset = 0.1;
            n.slide_to = Some(73);
        }
        let before = n.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into()],
            ..Default::default()
        };
        assert_eq!(scale_lengths(&mut n, &sel, 0.5, 16.).unwrap(), 2);
        let mut expected = before.clone();
        expected[0].len = 2.;
        expected[1].len = 1.;
        assert_eq!(n, expected);
        scale_lengths(&mut n, &sel, 2., 16.).unwrap();
        assert_eq!(n, before);
        assert_eq!(scale_lengths(&mut n, &sel, 1., 16.).unwrap(), 0);
        assert_eq!(scale_lengths(&mut [], &sel, 2., 16.).unwrap(), 0);
        for factor in [0., 0.01, 16.1, f32::NAN, f32::INFINITY] {
            assert!(scale_lengths(&mut n, &sel, factor, 16.).is_err());
            assert_eq!(n, before);
        }
        assert!(scale_lengths(&mut n, &sel, 16., 16.).is_err());
        assert_eq!(n, before);
        n[1].len = 0.05;
        let b = n.clone();
        assert!(scale_lengths(&mut n, &sel, 0.5, 16.).is_err());
        assert_eq!(n, b);
    }
}

/// Set selected velocities only, including silence, without normalizing lengths.
pub fn set_velocity(notes: &mut [Note], sel: &Selection, velocity: f32) -> Result<usize> {
    if !velocity.is_finite() || !(0.0..=1.0).contains(&velocity) {
        bail!("velocity must be finite and between0 and1");
    }
    let mut count = 0;
    for i in selected(notes, sel) {
        if notes[i].vel != velocity {
            notes[i].vel = velocity;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod fixed_velocity_tests {
    use super::*;
    #[test]
    fn only_velocity_changes_with_strict_bounds() {
        let mut n = vec![
            Note::new(0., 0.02, 60, 0.),
            Note::new(4., 2., 64, 0.4),
            Note::new(8., 3., 67, 0.8),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("n{i}");
            n.prob = 0.6;
            n.offset = 0.1;
            n.slide_to = Some(73);
        }
        let before = n.clone();
        let sel = Selection {
            ids: vec!["n0".into(), "n1".into()],
            ..Default::default()
        };
        assert_eq!(set_velocity(&mut n, &sel, 0.5).unwrap(), 2);
        let mut expected = before.clone();
        expected[0].vel = 0.5;
        expected[1].vel = 0.5;
        assert_eq!(n, expected);
        assert_eq!(set_velocity(&mut n, &sel, 0.5).unwrap(), 0);
        for value in [0., 1.] {
            assert_eq!(set_velocity(&mut n, &sel, value).unwrap(), 2);
        }
        let b = n.clone();
        for value in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
            assert!(set_velocity(&mut n, &sel, value).is_err());
            assert_eq!(n, b);
        }
        assert_eq!(set_velocity(&mut [], &sel, 0.).unwrap(), 0);
    }
}

/// Remove selected notes explicitly set to zero velocity or zero probability.
/// Surviving note fields/order are unchanged. Deletion can reseed index-based chance.
pub fn remove_zero_notes(notes: &mut Vec<Note>, sel: &Selection) -> usize {
    let before = notes.len();
    notes.retain(|n| !(sel.matches(n) && (n.vel == 0.0 || n.prob == 0.0)));
    before - notes.len()
}

#[cfg(test)]
mod zero_note_cleanup_tests {
    use super::*;
    #[test]
    fn exact_zero_only_selection_and_survivor_identity() {
        let mut n = vec![
            Note::new(0., 0.02, 60, 0.),
            Note::new(2., 2., 62, 0.6),
            Note::new(4., 3., 64, 0.00001),
            Note::new(6., 4., 67, 0.),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("z{i}");
            n.offset = 0.1;
            n.slide_to = Some(73);
            n.prob = 0.00001;
        }
        n[1].prob = 0.;
        let b = n.clone();
        let sel = Selection {
            ids: vec!["z0".into(), "z1".into(), "z2".into()],
            ..Default::default()
        };
        assert_eq!(remove_zero_notes(&mut n, &sel), 2);
        assert_eq!(n, vec![b[2].clone(), b[3].clone()]);
        assert_eq!(remove_zero_notes(&mut n, &sel), 0);
        assert_eq!(remove_zero_notes(&mut n, &Default::default()), 1);
        assert_eq!(n, vec![b[2].clone()]);
        let mut invalid = vec![
            Note::new(0., 1., 60, -0.01),
            Note::new(2., 1., 62, f32::NAN),
        ];
        assert_eq!(remove_zero_notes(&mut invalid, &Default::default()), 0);
        assert_eq!(remove_zero_notes(&mut vec![], &sel), 0);
    }
}

/// Set one fixed duration, validating every selected note before any change.
pub fn set_lengths(notes: &mut [Note], sel: &Selection, length: f32, steps: f32) -> Result<usize> {
    if !length.is_finite() || length < 0.05 || !steps.is_finite() || steps <= 0.0 {
        bail!("length must be finite and at least0.05 steps, with valid pattern bounds");
    }
    for n in notes.iter().filter(|n| sel.matches(n)) {
        if !n.start.is_finite()
            || n.start < 0.0
            || n.start >= steps
            || !n.len.is_finite()
            || n.len <= 0.0
            || n.start + length > steps
        {
            bail!("fixed duration does not fit selected notes");
        }
    }
    let mut count = 0;
    for n in notes.iter_mut().filter(|n| sel.matches(n)) {
        if n.len != length {
            n.len = length;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod fixed_length_tests {
    use super::*;
    #[test]
    fn selection_chords_expression_and_atomic_fit() {
        let mut n = vec![
            Note::new(0., 4., 60, 0.),
            Note::new(0., 0.02, 64, 0.3),
            Note::new(8., 1., 67, 0.8),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("s{i}");
            n.prob = 0.2;
            n.offset = -0.2;
            n.slide_to = Some(72);
        }
        let b = n.clone();
        let sel = Selection {
            ids: vec!["s0".into(), "s1".into()],
            ..Default::default()
        };
        assert_eq!(set_lengths(&mut n, &sel, 2., 16.).unwrap(), 2);
        let mut expected = b.clone();
        expected[0].len = 2.;
        expected[1].len = 2.;
        assert_eq!(n, expected);
        assert_eq!(set_lengths(&mut n, &sel, 2., 16.).unwrap(), 0);
        assert_eq!(set_lengths(&mut [], &sel, 2., 16.).unwrap(), 0);
        let b = n.clone();
        for length in [0., 0.04, -1., f32::NAN, f32::INFINITY, 17.] {
            assert!(set_lengths(&mut n, &sel, length, 16.).is_err());
            assert_eq!(n, b);
        }
        assert!(set_lengths(&mut n, &sel, 2., 0.).is_err());
        assert_eq!(n, b);
        let mut late = b.clone();
        late[2].start = 15.;
        let b = late.clone();
        assert!(set_lengths(&mut late, &Default::default(), 2., 16.).is_err());
        assert_eq!(late, b);
        set_lengths(&mut late, &Default::default(), 1., 16.).unwrap();
        assert!(late.iter().all(|n| n.len == 1.));
    }
}

/// Linear velocity envelope over selected nominal onset time; one onset uses start.
pub fn ramp_velocities(notes: &mut [Note], sel: &Selection, start: f32, end: f32) -> Result<usize> {
    if [start, end]
        .iter()
        .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        bail!("velocity endpoints must be finite between0 and1");
    }
    let idx = selected(notes, sel);
    if idx.is_empty() {
        return Ok(0);
    }
    if idx
        .iter()
        .any(|i| !notes[*i].start.is_finite() || notes[*i].start < 0.0)
    {
        bail!("invalid selected onset");
    }
    let first = idx
        .iter()
        .map(|i| notes[*i].start)
        .fold(f32::INFINITY, f32::min);
    let last = idx
        .iter()
        .map(|i| notes[*i].start)
        .fold(f32::NEG_INFINITY, f32::max);
    let mut count = 0;
    for i in idx {
        let n = &mut notes[i];
        let t = if last > first {
            (n.start - first) / (last - first)
        } else {
            0.0
        };
        let v = if t == 0.0 {
            start
        } else if t == 1.0 {
            end
        } else {
            (start + (end - start) * t).clamp(0.0, 1.0)
        };
        if n.vel != v {
            n.vel = v;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod velocity_ramp_tests {
    use super::*;
    #[test]
    fn time_based_chords_selection_fields_endpoints() {
        let mut n = vec![
            Note::new(8., 0.02, 67, 0.),
            Note::new(0., 4., 60, 0.6),
            Note::new(2., 2., 62, 0.8),
            Note::new(2., 3., 64, 0.7),
            Note::new(12., 1., 72, 0.9),
        ];
        for (i, n) in n.iter_mut().enumerate() {
            n.id = format!("r{i}");
            n.prob = 0.3;
            n.offset = -0.1;
            n.slide_to = Some(74);
        }
        let b = n.clone();
        let sel = Selection {
            ids: vec!["r0".into(), "r1".into(), "r2".into(), "r3".into()],
            ..Default::default()
        };
        assert_eq!(ramp_velocities(&mut n, &sel, 0., 1.).unwrap(), 4);
        let mut expected = b.clone();
        for (i, v) in [1., 0., 0.25, 0.25].iter().enumerate() {
            expected[i].vel = *v;
        }
        assert_eq!(n, expected);
        assert_eq!(ramp_velocities(&mut n, &sel, 0., 1.).unwrap(), 0);
        ramp_velocities(&mut n, &sel, 1., 0.).unwrap();
        assert_eq!(n[0].vel, 0.);
        assert_eq!(n[1].vel, 1.);
        assert_eq!(n[2].vel, 0.75);
        let one = Selection {
            ids: vec!["r2".into(), "r3".into()],
            ..Default::default()
        };
        ramp_velocities(&mut n, &one, 0.4, 0.8).unwrap();
        assert_eq!(n[2].vel, 0.4);
        assert_eq!(n[3].vel, 0.4);
        let b = n.clone();
        for x in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
            assert!(ramp_velocities(&mut n, &sel, x, 0.5).is_err());
            assert!(ramp_velocities(&mut n, &sel, 0.5, x).is_err());
            assert_eq!(n, b);
        }
        n[0].start = -1.;
        let sel = Selection { from: -2., ..sel };
        let b = n.clone();
        assert!(ramp_velocities(&mut n, &sel, 0., 1.).is_err());
        assert_eq!(n, b);
        assert_eq!(ramp_velocities(&mut [], &sel, 0., 1.).unwrap(), 0);
    }
}

/// Fit selected dynamics into an explicit range without changing other note data.
pub fn fit_velocity_range(
    notes: &mut [Note],
    sel: &Selection,
    low: f32,
    high: f32,
) -> Result<usize> {
    if !low.is_finite()
        || !high.is_finite()
        || !(0.0..=1.0).contains(&low)
        || !(low..=1.0).contains(&high)
    {
        bail!("velocity range must satisfy0 <= low <= high <=1");
    }
    let ids = selected(notes, sel);
    if ids
        .iter()
        .any(|i| !notes[*i].vel.is_finite() || !(0.0..=1.0).contains(&notes[*i].vel))
    {
        bail!("selected velocities must be finite between0 and1");
    }
    let min = ids
        .iter()
        .map(|i| notes[*i].vel)
        .fold(f32::INFINITY, f32::min);
    let max = ids
        .iter()
        .map(|i| notes[*i].vel)
        .fold(f32::NEG_INFINITY, f32::max);
    let mut count = 0;
    for i in ids {
        let value = if max > min {
            low + (notes[i].vel - min) / (max - min) * (high - low)
        } else {
            low
        };
        let value = value.clamp(low, high);
        if notes[i].vel != value {
            notes[i].vel = value;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod fit_velocity_range_tests {
    use super::*;
    #[test]
    fn range_mapping_preserves_other_fields_and_validates_atomically() {
        let mut n: Vec<_> = [0., 0.2, 0.6, 1.]
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let mut n = Note::new(i as f32, 0.02, 60, *v);
                n.id = format!("fit{i}");
                n.prob = 0.5;
                n.offset = 0.1;
                n.slide_to = Some(72);
                n
            })
            .collect();
        let before = n.clone();
        let sel = Selection {
            ids: vec!["fit0".into(), "fit1".into(), "fit2".into()],
            ..Default::default()
        };
        assert_eq!(fit_velocity_range(&mut n, &sel, 0.3, 0.9).unwrap(), 3);
        for (i, v) in [0.3, 0.5, 0.9, 1.].iter().enumerate() {
            assert!((n[i].vel - v).abs() < 0.00001);
            let mut expected = before[i].clone();
            expected.vel = n[i].vel;
            assert_eq!(n[i], expected);
        }
        let saved = n.clone();
        for (low, high) in [(0.9, 0.2), (-0.1, 1.), (0., 1.1), (f32::NAN, 1.)] {
            assert!(fit_velocity_range(&mut n, &sel, low, high).is_err());
            assert_eq!(n, saved);
        }
        let mut flat = vec![Note::new(0., 1., 60, 0.5); 2];
        assert_eq!(
            fit_velocity_range(&mut flat, &Selection::default(), 0., 1.).unwrap(),
            2
        );
        assert!(flat.iter().all(|n| n.vel == 0.));
        assert_eq!(
            fit_velocity_range(&mut flat, &Selection::default(), 0., 1.).unwrap(),
            0
        );
        assert_eq!(
            fit_velocity_range(&mut [], &Selection::default(), 0., 1.).unwrap(),
            0
        );
    }
}

/// Linear trigger-chance envelope over selected nominal onsets.
pub fn ramp_note_chance(
    notes: &mut [Note],
    sel: &Selection,
    start: f32,
    end: f32,
) -> Result<usize> {
    if [start, end]
        .iter()
        .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        bail!("chance endpoints must be finite between0 and1");
    }
    let ids = selected(notes, sel);
    if ids
        .iter()
        .any(|i| !notes[*i].start.is_finite() || notes[*i].start < 0.)
    {
        bail!("invalid selected onset");
    }
    let first = ids
        .iter()
        .map(|i| notes[*i].start)
        .fold(f32::INFINITY, f32::min);
    let last = ids
        .iter()
        .map(|i| notes[*i].start)
        .fold(f32::NEG_INFINITY, f32::max);
    let mut count = 0;
    for i in ids {
        let t = if last > first {
            (notes[i].start - first) / (last - first)
        } else {
            0.
        };
        let chance = if t == 0. {
            start
        } else if t == 1. {
            end
        } else {
            (start + (end - start) * t).clamp(0., 1.)
        };
        if notes[i].prob != chance {
            notes[i].prob = chance;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod note_chance_ramp_tests {
    use super::*;
    #[test]
    fn chord_onsets_endpoints_partial_selection_and_fields() {
        let mut notes: Vec<_> = [8., 0., 2., 2., 10.]
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut n = Note::new(*s, 0.02, 60 + i as u8, 0.7);
                n.id = format!("ramp{i}");
                n.prob = 0.6;
                n.offset = 0.1;
                n.slide_to = Some(72);
                n
            })
            .collect();
        let before = notes.clone();
        let sel = Selection {
            ids: (0..4).map(|i| format!("ramp{i}")).collect(),
            ..Default::default()
        };
        assert_eq!(ramp_note_chance(&mut notes, &sel, 0., 1.).unwrap(), 4);
        for (i, v) in [1., 0., 0.25, 0.25, 0.6].iter().enumerate() {
            assert_eq!(notes[i].prob, *v);
            let mut expected = before[i].clone();
            expected.prob = *v;
            assert_eq!(notes[i], expected);
        }
        assert_eq!(ramp_note_chance(&mut notes, &sel, 0., 1.).unwrap(), 0);
        let saved = notes.clone();
        for (a, b) in [(-0.1, 1.), (0., 1.1), (f32::NAN, 1.)] {
            assert!(ramp_note_chance(&mut notes, &sel, a, b).is_err());
            assert_eq!(notes, saved);
        }
        let mut chord = vec![Note::new(4., 1., 60, 0.5); 2];
        assert_eq!(
            ramp_note_chance(&mut chord, &Selection::default(), 0.3, 0.9).unwrap(),
            2
        );
        assert!(chord.iter().all(|n| n.prob == 0.3));
        assert_eq!(
            ramp_note_chance(&mut [], &Selection::default(), 0., 1.).unwrap(),
            0
        );
    }
}
