//! Record transcription, analysis only: drums per bar, the bass line, chords
//! and a top-line riff or melody, turned into notes, chord names and MIDI from
//! any audio file (a stem from separate_stems, or a full mix). Nothing from the
//! input's audio is kept: out come timings, pitches, velocities and names that
//! the engine's own instruments then play.

use crate::dsp::{Biquad, BiquadKind, Fft, SR};
use crate::smf::{Smf, SmfNote, SmfTrack};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

pub const HOP: usize = 512;

pub fn fps() -> f32 {
    SR / HOP as f32
}

pub const PCN: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

pub fn note_name(m: u8) -> String {
    format!("{}{}", PCN[(m % 12) as usize], m as i32 / 12 - 1)
}

fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos()).collect()
}

fn pct(v: &[f32], p: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    s[((s.len() - 1) as f32 * p.clamp(0.0, 1.0)).round() as usize]
}

fn db(x: f32) -> f32 {
    10.0 * x.max(1e-12).log10()
}

// ---------------------------------------------------------------- grid

/// The song's beat grid. `downbeat_s` is normalised to the first bar line at
/// or before the audio's start (up to half a second of lead-in is allowed), so
/// every tool numbers bars the same way for the same grid; bar 1 = index 0.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Grid {
    pub bpm: f32,
    pub downbeat_s: f32,
    pub beats_per_bar: usize,
}

impl Grid {
    pub fn new(bpm: f32, downbeat_s: f32, beats_per_bar: usize) -> Grid {
        let mut g = Grid { bpm: bpm.clamp(30.0, 300.0), downbeat_s, beats_per_bar: beats_per_bar.clamp(2, 12) };
        // the bar line at or before the audio's start, allowing a short lead-in
        let tol = g.beat_s().min(0.5);
        let k = ((g.downbeat_s - tol) / g.bar_s()).ceil();
        g.downbeat_s -= k * g.bar_s();
        g
    }
    pub fn beat_s(&self) -> f32 {
        60.0 / self.bpm
    }
    pub fn bar_s(&self) -> f32 {
        self.beat_s() * self.beats_per_bar as f32
    }
    /// Start of bar `b` (0-based).
    pub fn bar_t(&self, b: usize) -> f32 {
        self.downbeat_s + b as f32 * self.bar_s()
    }
    /// 0-based bar index of a time.
    pub fn bar_of(&self, t: f32) -> usize {
        ((t - self.downbeat_s) / self.bar_s()).floor().max(0.0) as usize
    }
    /// "bar.beat", 1-based, the way a DAW shows it.
    pub fn label(&self, t: f32) -> String {
        let b = (t - self.downbeat_s) / self.bar_s();
        let bar = b.floor().max(0.0);
        let beat = ((b - bar) * self.beats_per_bar as f32).floor() as usize + 1;
        format!("{}.{}", bar as usize + 1, beat.min(self.beats_per_bar))
    }
    pub fn bars_in(&self, dur: f32) -> usize {
        ((dur - self.downbeat_s) / self.bar_s()).ceil().max(1.0) as usize
    }
}

/// Grid of a piece of audio: tempo and downbeat from the song analyser.
pub fn estimate_grid(x: &[f32]) -> Result<(Grid, f32)> {
    let a = crate::cover::analyze_song(x, "input")?;
    Ok((Grid::new(a.bpm, a.downbeat_s, 4), a.tempo_confidence))
}

// ---------------------------------------------------------------- bands

/// Per-band energy and log spectral flux per HOP frame.
pub struct Bands {
    pub frames: usize,
    pub energy: Vec<Vec<f32>>,
    pub flux: Vec<Vec<f32>>,
}

pub fn band_analysis(x: &[f32], nfft: usize, edges: &[(f32, f32)]) -> Bands {
    let bins = nfft / 2 + 1;
    let frames = x.len() / HOP + 1;
    let w = hann(nfft);
    let fft = Fft::new(nfft);
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    let scale = 400.0 / (nfft as f32 * peak);
    let rng: Vec<(usize, usize)> = edges
        .iter()
        .map(|(lo, hi)| {
            let a = ((lo * nfft as f32 / SR).round() as usize).clamp(1, bins - 1);
            let b = ((hi * nfft as f32 / SR).round() as usize).clamp(a + 1, bins);
            (a, b)
        })
        .collect();
    let mut energy = vec![vec![0.0f32; frames]; edges.len()];
    let mut flux = vec![vec![0.0f32; frames]; edges.len()];
    let mut prev = vec![0.0f32; bins];
    let mut cur = vec![0.0f32; bins];
    let (mut re, mut im) = (vec![0.0f32; nfft], vec![0.0f32; nfft]);
    for f in 0..frames {
        for k in 0..nfft {
            let i = (f * HOP + k) as isize - (nfft / 2) as isize;
            re[k] = if i >= 0 && (i as usize) < x.len() { x[i as usize] * w[k] } else { 0.0 };
            im[k] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        for k in 0..bins {
            cur[k] = (1.0 + scale * (re[k] * re[k] + im[k] * im[k]).sqrt()).ln();
        }
        for (b, (lo, hi)) in rng.iter().enumerate() {
            let mut e = 0.0;
            let mut fl = 0.0;
            for k in *lo..*hi {
                let m = re[k] * re[k] + im[k] * im[k];
                e += m;
                if f > 0 {
                    fl += (cur[k] - prev[k]).max(0.0);
                }
            }
            energy[b][f] = e / (nfft as f32 * nfft as f32);
            flux[b][f] = fl / (hi - lo) as f32;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    Bands { frames, energy, flux }
}

/// Peaks of an onset curve above an adaptive threshold (local median + k *
/// global spread). Returns (frame, strength) with strength normalised so the
/// curve's typical strong peak (90th percentile of peaks) is 1.0.
pub fn pick_peaks(f: &[f32], k: f32, min_gap: usize) -> Vec<(usize, f32)> {
    let n = f.len();
    if n < 8 {
        return Vec::new();
    }
    // local median every 16 frames over +-64 frames
    let step = 16usize;
    let mut med = vec![0.0f32; n / step + 2];
    for (i, m) in med.iter_mut().enumerate() {
        let c = i * step;
        let a = c.saturating_sub(64);
        let b = (c + 65).min(n);
        if a < b {
            *m = pct(&f[a..b], 0.5);
        }
    }
    let gmed = pct(f, 0.5);
    let spread = (pct(f, 0.9) - gmed).max(1e-9);
    let floor = pct(f, 0.98) * 0.06;
    let mut out: Vec<(usize, f32)> = Vec::new();
    for i in 1..n - 1 {
        let lm = med[(i + step / 2) / step];
        let thr = lm + k * spread + floor;
        if f[i] > thr && f[i] >= f[i - 1] && f[i] > f[i + 1] {
            let h = f[i] - lm;
            if let Some(last) = out.last_mut() {
                if i - last.0 < min_gap {
                    if h > last.1 {
                        *last = (i, h);
                    }
                    continue;
                }
            }
            out.push((i, h));
        }
    }
    let hs: Vec<f32> = out.iter().map(|p| p.1).collect();
    let ref90 = pct(&hs, 0.9).max(1e-9);
    out.iter().map(|(i, h)| (*i, h / ref90)).collect()
}

// ---------------------------------------------------------------- drums

pub const DRUM_KINDS: [&str; 6] = ["kick", "snare", "closed_hat", "open_hat", "crash", "tom"];
/// General MIDI drum notes for DRUM_KINDS.
pub const DRUM_GM: [u8; 6] = [36, 38, 42, 46, 49, 45];

#[derive(Clone, Debug, Serialize)]
pub struct DrumHit {
    pub t: f32,
    pub kind: usize,
    pub strength: f32,
    pub vel: u8,
    pub ghost: bool,
    /// 0-based bar and step on the grid, and the offset from the step (fraction of a step)
    pub bar: usize,
    pub step: usize,
    pub offset: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct DrumScore {
    pub grid: Grid,
    pub steps_per_bar: usize,
    pub hits: Vec<DrumHit>,
    pub bars: usize,
    /// per bar, per kind: velocity per step (0 = no hit)
    pub pattern: Vec<Vec<Vec<u8>>>,
    /// MPC-style swing of the off-16ths (50 = straight, 66 = triplet feel)
    pub swing_pct: f32,
    /// mean timing of each kind against the grid, ms (+ = behind the beat)
    pub push_ms: Vec<f32>,
    /// letter per bar ('-' = no drums), letters shared by bars playing the same pattern
    pub letters: Vec<char>,
    pub fills: Vec<usize>,
}

const DRUM_BANDS: [(f32, f32); 6] = [
    (30.0, 110.0),    // 0 kick
    (110.0, 260.0),   // 1 tom / body
    (160.0, 400.0),   // 2 snare body
    (1200.0, 5000.0), // 3 snare crack
    (6000.0, 16000.0),// 4 hats
    (3500.0, 9000.0), // 5 cymbal body
];

fn strength_near(peaks: &[(usize, f32)], f: usize, w: usize) -> f32 {
    // peaks are sorted by frame
    let lo = f.saturating_sub(w);
    let i = peaks.partition_point(|p| p.0 < lo);
    let mut m = 0.0f32;
    for p in &peaks[i..] {
        if p.0 > f + w {
            break;
        }
        m = m.max(p.1);
    }
    m
}

/// Transcribe a drum stem (or a mix) onto a grid.
pub fn transcribe_drums(x: &[f32], grid: Grid, steps_per_bar: usize) -> DrumScore {
    let fps = fps();
    let b = band_analysis(x, 2048, &DRUM_BANDS);
    let gap = ((0.045 * fps) as usize).max(2);
    let peaks: Vec<Vec<(usize, f32)>> = b.flux.iter().enumerate().map(|(i, f)| pick_peaks(f, if i == 3 || i == 2 { 0.3 } else { 0.6 }, gap)).collect();
    // candidate events: every band peak, clustered within +-2 frames
    let mut cand: Vec<usize> = peaks.iter().flat_map(|p| p.iter().map(|q| q.0)).collect();
    cand.sort_unstable();
    let mut events: Vec<usize> = Vec::new();
    for c in cand {
        match events.last() {
            Some(&l) if c <= l + 2 => {}
            _ => events.push(c),
        }
    }
    let energy_at = |band: usize, f: usize| b.energy[band].get(f).copied().unwrap_or(0.0);
    let mut raw: Vec<(f32, usize, f32)> = Vec::new(); // (t, kind, strength)
    for &f in &events {
        let s: Vec<f32> = (0..6).map(|k| strength_near(&peaks[k], f + 1, 3)).collect();
        let t = f as f32 / fps;
        // a kick is a low-end onset whose low end is mostly sub (30-110 Hz over 160-400 Hz): a fat
        // snare also moves the low band but its weight sits in its body, and a ringing kick's
        // decay moves the low band again without any new energy, so both shape and rise decide
        let e0 = (f..f + 5).map(|i| energy_at(0, i)).fold(0.0f32, f32::max);
        let e2 = (f..f + 5).map(|i| energy_at(2, i)).fold(0.0f32, f32::max);
        // the loudest low end of the ~90 ms before: a kick's own tail never rises above it
        let before = {
            let a = f.saturating_sub(8);
            let b = f.saturating_sub(1).max(a + 1);
            (a..b).map(|i| energy_at(0, i)).fold(0.0f32, f32::max)
        };
        let sub_db = 10.0 * ((e0 + 1e-12) / (e2 + 1e-12)).log10();
        let rise = (e0 + 1e-12) / (before + 1e-12);
        let kick = s[0] >= 0.3 && sub_db > -1.0 && rise >= 1.5 && (s[0] >= 0.4 * s[1] || sub_db > 3.0);
        let snare = (s[3] >= 0.35 && s[2] >= 0.2) || s[3] >= 0.7 || (s[3] >= 0.15 && s[2] >= 0.1 && s[3] >= 0.8 * s[4] && s[0] < 0.3);
        let tom = !kick && !snare && s[1] >= 0.45 && s[3] < 0.3;
        if kick {
            raw.push((t, 0, s[0]));
        }
        if snare {
            raw.push((t, 1, s[3].max(s[2])));
        }
        if tom {
            raw.push((t, 5, s[1]));
        }
        let hat_ok = s[4] >= 0.3 && (!snare || (s[4] >= 1.2 * s[3] && s[4] >= 0.5));
        if hat_ok {
            // decay of the hat band: min over 60..95 ms and 450..550 ms against the attack
            let pk = (f..f + 4).map(|i| energy_at(4, i)).fold(0.0f32, f32::max).max(1e-12);
            let short = (f + (0.06 * fps) as usize..=f + (0.095 * fps) as usize).map(|i| energy_at(4, i)).fold(f32::MAX, f32::min);
            let long = (f + (0.45 * fps) as usize..=f + (0.55 * fps) as usize).map(|i| energy_at(4, i)).fold(f32::MAX, f32::min);
            let crash = long / pk > 0.06 && s[4].max(s[5]) >= 0.6;
            let kind = if crash {
                4
            } else if short / pk > 0.35 {
                3
            } else {
                2
            };
            raw.push((t, kind, s[4]));
        }
    }
    // one hit per kind within 60 ms (the low band's onset trails the crack by a few frames,
    // so one drum can show up as two candidate events)
    raw.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut dedup: Vec<(f32, usize, f32)> = Vec::new();
    for r in raw {
        if let Some(p) = dedup.iter_mut().rev().take(8).find(|p| p.1 == r.1 && r.0 - p.0 < 0.06) {
            if r.2 > p.2 {
                p.2 = r.2;
            }
            continue;
        }
        dedup.push(r);
    }
    // a tom right after a snare or kick is that drum's body ringing on, not a second drum
    let hits_sk: Vec<f32> = dedup.iter().filter(|r| r.1 <= 1).map(|r| r.0).collect();
    dedup.retain(|r| r.1 != 5 || !hits_sk.iter().any(|t| r.0 - t >= -0.01 && r.0 - t < 0.07));
    let raw = dedup;
    // velocities per kind relative to that kind's median strength; quiet snares are ghosts
    let mut med = [1.0f32; 6];
    for (k, m) in med.iter_mut().enumerate() {
        let v: Vec<f32> = raw.iter().filter(|r| r.1 == k).map(|r| r.2).collect();
        if !v.is_empty() {
            *m = pct(&v, 0.5).max(1e-6);
        }
    }
    let step_s = grid.bar_s() / steps_per_bar as f32;
    let mut hits: Vec<DrumHit> = Vec::new();
    for (t, kind, s) in raw {
        let rel = s / med[kind];
        let ghost = kind == 1 && rel < 0.4;
        let vel = if ghost { (25.0 + 40.0 * rel).round() } else { (92.0 * rel.powf(0.6)).round() }.clamp(15.0, 127.0) as u8;
        let pos = (t - grid.downbeat_s) / step_s;
        if pos < -0.5 {
            continue;
        }
        let n = pos.round().max(0.0) as usize;
        hits.push(DrumHit { t, kind, strength: s, vel, ghost, bar: n / steps_per_bar, step: n % steps_per_bar, offset: pos - n as f32 });
    }
    let dur = x.len() as f32 / SR;
    let bars = grid.bars_in(dur).max(hits.iter().map(|h| h.bar + 1).max().unwrap_or(0));
    let mut pattern = vec![vec![vec![0u8; steps_per_bar]; 6]; bars];
    for h in &hits {
        let c = &mut pattern[h.bar][h.kind][h.step];
        *c = (*c).max(h.vel);
    }
    // swing: off-16ths (odd steps on a 16-step bar) late by d steps -> 50 + 50 d
    let per16 = (steps_per_bar / 16).max(1);
    let offs: Vec<f32> = hits
        .iter()
        .filter(|h| (h.kind == 2 || h.kind == 3 || h.kind == 1) && (h.step / per16) % 2 == 1 && h.step % per16 == 0)
        .map(|h| h.offset * per16 as f32)
        .collect();
    let swing_pct = if offs.len() >= 8 { (50.0 + 50.0 * pct(&offs, 0.5)).clamp(40.0, 75.0) } else { 50.0 };
    let push_ms: Vec<f32> = (0..6)
        .map(|k| {
            let v: Vec<f32> = hits.iter().filter(|h| h.kind == k && (h.step / per16) % 2 == 0).map(|h| h.offset * step_s * 1000.0).collect();
            if v.is_empty() { 0.0 } else { (pct(&v, 0.5) * 10.0).round() / 10.0 }
        })
        .collect();
    let (letters, fills) = group_bars(&pattern, steps_per_bar);
    DrumScore { grid, steps_per_bar, hits, bars, pattern, swing_pct: (swing_pct * 10.0).round() / 10.0, push_ms, letters, fills }
}

/// Letter per bar (bars within a small hit distance share a letter) and the bars that look like fills.
pub fn group_bars(pattern: &[Vec<Vec<u8>>], steps: usize) -> (Vec<char>, Vec<usize>) {
    let sig = |p: &Vec<Vec<u8>>| -> Vec<bool> { p.iter().flat_map(|row| row.iter().map(|v| *v > 0)).collect() };
    let mut reps: Vec<Vec<bool>> = Vec::new();
    let mut letters = Vec::new();
    for p in pattern {
        let s = sig(p);
        let hits = s.iter().filter(|x| **x).count();
        if hits == 0 {
            letters.push('-');
            continue;
        }
        let tol = (hits / 10).max(1);
        let best = reps
            .iter()
            .enumerate()
            .map(|(i, r)| (i, r.iter().zip(s.iter()).filter(|(a, b)| a != b).count()))
            .min_by_key(|x| x.1);
        match best {
            Some((i, d)) if d <= tol => letters.push(if i < 26 { (b'A' + i as u8) as char } else { '?' }),
            _ => {
                reps.push(s);
                let i = reps.len() - 1;
                letters.push(if i < 26 { (b'A' + i as u8) as char } else { '?' });
            }
        }
    }
    // fills: a rare letter (<= 2 bars) with toms, or with a busy last beat of snares/toms
    let mut fills = Vec::new();
    for (b, p) in pattern.iter().enumerate() {
        let l = letters[b];
        if l == '-' {
            continue;
        }
        let count = letters.iter().filter(|x| **x == l).count();
        let last = steps * 3 / 4;
        let busy = (last..steps).filter(|&s| p[1][s] > 0 || p[5][s] > 0).count();
        let toms = p[5].iter().filter(|v| **v > 0).count();
        if count <= 2 && (toms >= 2 || busy >= 3) {
            fills.push(b);
        }
    }
    (letters, fills)
}

/// One row of a pattern as text: X accent, x hit, g ghost, . rest.
pub fn row_text(row: &[u8], ghost_max: u8) -> String {
    row.iter()
        .map(|v| match *v {
            0 => '.',
            v if v <= ghost_max => 'g',
            v if v >= 110 => 'X',
            _ => 'x',
        })
        .collect()
}

pub fn drums_midi(s: &DrumScore, keep_timing: bool) -> Smf {
    let ppq = 480u32;
    let step_ticks = ppq as f32 * s.grid.beats_per_bar as f32 / s.steps_per_bar as f32;
    let mut notes = Vec::new();
    for h in &s.hits {
        let n = (h.bar * s.steps_per_bar + h.step) as f32 + if keep_timing { h.offset } else { 0.0 };
        notes.push(SmfNote { tick: (n * step_ticks).max(0.0).round() as u32, len: (step_ticks * 0.5).round() as u32, pitch: DRUM_GM[h.kind], vel: h.vel.max(1), channel: 9 });
    }
    notes.sort_by_key(|n| (n.tick, n.pitch));
    Smf { division: ppq as u16, bpm: s.grid.bpm, time_sig: (s.grid.beats_per_bar as u8, 4), tracks: vec![SmfTrack { name: "Drums".into(), program: None, notes }] }
}

// ---------------------------------------------------------------- pitched lines

#[derive(Clone, Debug, Serialize)]
pub struct PNote {
    pub t0: f32,
    pub t1: f32,
    pub pitch: u8,
    pub vel: u8,
    /// quantised start step (0-based from bar 1) and length in steps
    pub step: usize,
    pub len: usize,
}

/// Turn a frame-wise MIDI-pitch track (0 = unvoiced) into notes, splitting
/// repeated pitches at onsets. `fr` = frames per second of the track.
pub fn segment_notes(midi: &[f32], level: &[f32], onsets: &[usize], fr: f32, t_off: f32, min_s: f32, grid: Grid, steps_per_bar: usize) -> Vec<PNote> {
    let n = midi.len();
    let step_s = grid.bar_s() / steps_per_bar as f32;
    let lmax = pct(level, 0.95).max(1e-9);
    let mut is_onset = vec![false; n + 1];
    for &o in onsets {
        if o < n {
            is_onset[o] = true;
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if midi[i] <= 0.0 {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut vals = vec![midi[i]];
        while j < n && midi[j] > 0.0 {
            let m = pct(&vals, 0.5);
            if (midi[j] - m).abs() > 0.75 || (is_onset[j] && j - i >= 3) {
                break;
            }
            vals.push(midi[j]);
            j += 1;
        }
        let (t0, t1) = (i as f32 / fr + t_off, j as f32 / fr + t_off);
        if t1 - t0 >= min_s {
            let pitch = pct(&vals, 0.5).round().clamp(0.0, 127.0) as u8;
            let lv = level[i..j].iter().cloned().fold(0.0f32, f32::max);
            let vel = (40.0 + 87.0 * (lv / lmax).sqrt()).clamp(30.0, 127.0) as u8;
            let s0 = ((t0 - grid.downbeat_s) / step_s).round().max(0.0) as usize;
            let s1 = ((t1 - grid.downbeat_s) / step_s).round().max(0.0) as usize;
            out.push(PNote { t0, t1, pitch, vel, step: s0, len: s1.saturating_sub(s0).max(1) });
        }
        i = j;
    }
    // quantised notes must not overlap: trim each to the next start; merge exact repeats glued by rounding
    let mut q: Vec<PNote> = Vec::new();
    for nt in out {
        if let Some(last) = q.last_mut() {
            if nt.step <= last.step {
                if nt.len > last.len {
                    *last = nt;
                }
                continue;
            }
            if last.step + last.len > nt.step {
                last.len = nt.step - last.step;
            }
        }
        q.push(nt);
    }
    q
}

fn decimate4(x: &[f32], cutoff: f32) -> Vec<f32> {
    let mut a = Biquad::new(BiquadKind::HighCut, cutoff, 0.707, 0.0);
    let mut b = Biquad::new(BiquadKind::HighCut, cutoff, 0.707, 0.0);
    x.iter().map(|v| b.process(a.process(*v))).step_by(4).collect()
}

fn fix_octaves(m: &mut [f32]) {
    let src = m.to_vec();
    for i in 0..m.len() {
        if src[i] <= 0.0 {
            continue;
        }
        let w: Vec<f32> = (i.saturating_sub(3)..(i + 4).min(src.len())).filter(|&j| src[j] > 0.0).map(|j| src[j]).collect();
        if w.len() >= 4 {
            let med = pct(&w, 0.5);
            let mut v = src[i];
            while v - med > 7.0 {
                v -= 12.0;
            }
            while med - v > 7.0 {
                v += 12.0;
            }
            m[i] = v;
        }
    }
    // drop single-frame islands
    for i in 1..m.len().saturating_sub(1) {
        if m[i] > 0.0 && m[i - 1] <= 0.0 && m[i + 1] <= 0.0 {
            m[i] = 0.0;
        }
    }
}

/// Bass line: monophonic YIN pitch on a 4x-decimated copy (30-400 Hz).
pub fn transcribe_bass(x: &[f32], grid: Grid, steps_per_bar: usize) -> Vec<PNote> {
    let psr = SR / 4.0;
    let y = decimate4(x, 900.0);
    let hop = 256usize; // 23 ms at 11025 Hz
    let win = 1024usize;
    let fr = psr / hop as f32;
    let nfr = y.len().saturating_sub(win) / hop + 1;
    let rms: Vec<f32> = (0..nfr)
        .map(|f| {
            let s = &y[(f * hop).min(y.len())..(f * hop + win).min(y.len())];
            (s.iter().map(|v| v * v).sum::<f32>() / s.len().max(1) as f32).sqrt()
        })
        .collect();
    let gate = pct(&rms, 0.95) * 0.12;
    let mut midi: Vec<f32> = (0..nfr)
        .map(|f| {
            if rms[f] <= gate || f * hop + win > y.len() {
                return 0.0;
            }
            match crate::sc_dsp::yin(&y[f * hop..f * hop + win], psr, 30.0, 400.0, 0.2) {
                Some(hz) if hz > 0.0 => 69.0 + 12.0 * (hz / 440.0).log2(),
                _ => 0.0,
            }
        })
        .collect();
    fix_octaves(&mut midi);
    // onsets from the level curve (rises of > 3 dB within two frames)
    let mut onsets = Vec::new();
    for f in 2..nfr {
        if rms[f] > gate && db(rms[f] * rms[f]) - db(rms[f - 2] * rms[f - 2]) > 3.0 && (onsets.last().map(|&l: &usize| f - l > 3).unwrap_or(true)) {
            onsets.push(f);
        }
    }
    let min_s = (grid.bar_s() / steps_per_bar as f32 * 0.4).max(0.05);
    let mut notes = segment_notes(&midi, &rms, &onsets, fr, win as f32 / 2.0 / psr, min_s, grid, steps_per_bar);
    refine_onsets(&y, psr, &mut notes, grid, steps_per_bar);
    notes
}

/// Attack times (s) from a log-spectral flux: each bin's rise in dB, so a soft
/// re-pick in a quiet passage counts as much as a loud one in a busy passage
/// (a linear flux with one global threshold misses the quiet ones). Peaks
/// must top the mean of the surrounding 0.1 s by `delta` (0..1 of the curve's
/// range; 0.07 is a good default; lower finds softer attacks).
pub fn log_flux_onsets(x: &[f32], delta: f32) -> Vec<f32> {
    let (nfft, hop) = (2048usize, 512usize);
    if x.len() < nfft {
        return Vec::new();
    }
    let bins = ((8000.0 / SR * nfft as f32) as usize).min(nfft / 2);
    let n = x.len() / hop + 1;
    let w = hann(nfft);
    let fft = Fft::new(nfft);
    let (mut re, mut im) = (vec![0.0f32; nfft], vec![0.0f32; nfft]);
    let mut spec = vec![0.0f32; n * bins];
    let mut pmax = 1e-20f32;
    for i in 0..n {
        for k in 0..nfft {
            let j = (i * hop + k) as isize - (nfft / 2) as isize;
            re[k] = if j >= 0 && (j as usize) < x.len() { x[j as usize] * w[k] } else { 0.0 };
            im[k] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        for k in 1..bins {
            let p = re[k] * re[k] + im[k] * im[k];
            spec[i * bins + k] = p;
            pmax = pmax.max(p);
        }
    }
    let floor = pmax * 1e-8;
    for v in spec.iter_mut() {
        *v = 10.0 * v.max(floor).log10();
    }
    let mut env = vec![0.0f32; n];
    for i in 1..n {
        let mut s = 0.0;
        for k in 1..bins {
            s += (spec[i * bins + k] - spec[(i - 1) * bins + k]).max(0.0);
        }
        env[i] = s / bins as f32;
    }
    let (lo, hi) = env.iter().fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    if hi - lo <= 1e-9 {
        return Vec::new();
    }
    for v in env.iter_mut() {
        *v = (*v - lo) / (hi - lo);
    }
    let fr = SR / hop as f32;
    let (pm, pa) = (((0.03 * fr).round() as usize).max(1), ((0.1 * fr).round() as usize).max(1));
    let wait = ((0.03 * fr).round() as usize).max(1);
    let mut out = Vec::new();
    let mut last: Option<usize> = None;
    for i in 1..n {
        let a = i.saturating_sub(pm);
        if env[a..=i].iter().any(|v| *v > env[i]) || (i + 1 < n && env[i + 1] > env[i]) {
            continue;
        }
        let (a2, z2) = (i.saturating_sub(pa), (i + pa + 1).min(n));
        let mean = env[a2..z2].iter().sum::<f32>() / (z2 - a2) as f32;
        if env[i] < mean + delta {
            continue;
        }
        if last.map(|l| i - l <= wait).unwrap_or(false) {
            continue;
        }
        last = Some(i);
        out.push(i as f32 / fr);
    }
    out
}

/// Split held notes where the part is struck again on the same pitch. A pitch
/// tracker hears a re-picked note as one long note, and on a low string the
/// level barely rises, so the re-attack is found from onsets (broadband
/// attacks, the times in `onsets`, s) that fall inside a note: at least
/// three quarters of a step after its start and half a step before its end.
pub fn split_reattacks(onsets: &[f32], notes: &mut Vec<PNote>, grid: Grid, steps_per_bar: usize) -> usize {
    let step_s = grid.bar_s() / steps_per_bar as f32;
    let mut out: Vec<PNote> = Vec::with_capacity(notes.len());
    let mut added = 0;
    for n in notes.iter() {
        let end_step = n.step + n.len;
        let mut cur = n.clone();
        let lo = onsets.partition_point(|t| *t < n.t0 + step_s * 0.75);
        for &t in onsets[lo..].iter().take_while(|t| **t < n.t1 - step_s * 0.5) {
            let s = ((t - grid.downbeat_s) / step_s).round().max(0.0) as usize;
            if s <= cur.step || s >= end_step {
                continue;
            }
            cur.len = s - cur.step;
            cur.t1 = t;
            out.push(cur.clone());
            cur = PNote { t0: t, t1: n.t1, pitch: n.pitch, vel: n.vel, step: s, len: end_step - s };
            added += 1;
        }
        out.push(cur);
    }
    *notes = out;
    added
}

/// Move each note's start to the steepest level rise just before it, measured
/// with a short (23 ms) window. The long pitch window places low notes up to
/// ~100 ms late, which rounds them onto the next 16th; this pulls them back.
pub fn refine_onsets(y: &[f32], psr: f32, notes: &mut [PNote], grid: Grid, steps_per_bar: usize) {
    let (win, hop) = (256usize, 32usize);
    if y.len() < win * 2 || notes.is_empty() {
        return;
    }
    let n = (y.len() - win) / hop + 1;
    let e: Vec<f32> = (0..n)
        .map(|k| {
            let s = &y[k * hop..k * hop + win];
            db(s.iter().map(|v| v * v).sum::<f32>() / win as f32 + 1e-12)
        })
        .collect();
    let center = |k: usize| (k * hop) as f32 / psr + win as f32 / 2.0 / psr;
    let step_s = grid.bar_s() / steps_per_bar as f32;
    let lag = 3usize; // rise measured over ~9 ms
    let mut prev_t = f32::MIN;
    for i in 0..notes.len() {
        let t0 = notes[i].t0;
        let lo = ((t0 - 0.11 - win as f32 / 2.0 / psr) * psr / hop as f32).floor().max(lag as f32) as usize;
        let hi = (((t0 + 0.02 - win as f32 / 2.0 / psr) * psr / hop as f32).ceil().max(0.0) as usize).min(n - 1);
        let mut best = (0.0f32, None);
        for k in lo..=hi.max(lo) {
            if k >= n {
                break;
            }
            let r = e[k] - e[k - lag];
            if r > best.0 {
                best = (r, Some(k));
            }
        }
        if let (r, Some(k)) = best {
            let t = center(k - lag / 2);
            if r >= 3.0 && t < t0 && t > prev_t + step_s * 0.5 {
                let end = notes[i].step + notes[i].len;
                let s0 = ((t - grid.downbeat_s) / step_s).round().max(0.0) as usize;
                let ok_prev = i == 0 || s0 > notes[i - 1].step;
                if ok_prev && s0 < end {
                    notes[i].t0 = t;
                    notes[i].step = s0;
                    notes[i].len = end - s0;
                    if i > 0 && notes[i - 1].step + notes[i - 1].len > s0 {
                        notes[i - 1].len = s0 - notes[i - 1].step;
                    }
                }
            }
        }
        prev_t = notes[i].t0;
    }
}

/// Top line of a (possibly polyphonic) part: the most salient pitch per frame
/// by harmonic summation over spectral peaks, in lo..hi Hz.
pub fn transcribe_top_line(x: &[f32], grid: Grid, steps_per_bar: usize, lo: f32, hi: f32) -> Vec<PNote> {
    line_notes(x, grid, steps_per_bar, lo, hi, None)
}

/// A picked / arpeggiated part (clean guitar arpeggios, harp, plucked keys):
/// one note per pluck, the pitch whose salience rose most at that onset, so
/// strings that keep ringing under it do not hide the newly picked one. A
/// top-line tracker follows the loudest sustained string and misses the
/// pattern; this follows the picking hand. Each note rings to the next pluck.
/// `sensitivity` 0..1 (0.5 default): higher finds softer plucks under chorus and reverb.
pub fn transcribe_picked(x: &[f32], grid: Grid, steps_per_bar: usize, lo: f32, hi: f32, sensitivity: f32) -> Vec<PNote> {
    line_notes(x, grid, steps_per_bar, lo, hi, Some(sensitivity.clamp(0.0, 1.0)))
}

fn line_notes(x: &[f32], grid: Grid, steps_per_bar: usize, lo: f32, hi: f32, picked_sens: Option<f32>) -> Vec<PNote> {
    let picked = picked_sens.is_some();
    let sens = picked_sens.unwrap_or(0.5);
    let nfft = 4096usize;
    let bins = nfft / 2 + 1;
    let frames = x.len() / HOP + 1;
    let w = hann(nfft);
    let fft = Fft::new(nfft);
    let (mut re, mut im) = (vec![0.0f32; nfft], vec![0.0f32; nfft]);
    // salience on a 1/5-semitone grid from MIDI 24 to 108
    let res = 5.0f32;
    let mlo = 24.0f32;
    let nsal = ((108.0 - mlo) * res) as usize;
    let mut best = vec![0.0f32; frames];
    let mut sal_v = vec![0.0f32; frames];
    let mut lvl = vec![0.0f32; frames];
    let (mlo_q, mhi_q) = (69.0 + 12.0 * (lo.max(30.0) / 440.0).log2(), 69.0 + 12.0 * (hi.min(4000.0) / 440.0).log2());
    let mut mag = vec![0.0f32; bins];
    let mut sal = vec![0.0f32; nsal];
    // picked: salience folded to whole semitones (MIDI 24..108) for every frame
    let nsemi = 85usize;
    let mut semis: Vec<f32> = if picked { vec![0.0; frames * nsemi] } else { Vec::new() };
    for f in 0..frames {
        for k in 0..nfft {
            let i = (f * HOP + k) as isize - (nfft / 2) as isize;
            re[k] = if i >= 0 && (i as usize) < x.len() { x[i as usize] * w[k] } else { 0.0 };
            im[k] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        let mut tot = 0.0;
        for k in 0..bins {
            mag[k] = (re[k] * re[k] + im[k] * im[k]).sqrt();
            tot += mag[k] * mag[k];
        }
        lvl[f] = tot.sqrt();
        sal.iter_mut().for_each(|s| *s = 0.0);
        let mx = mag.iter().cloned().fold(0.0f32, f32::max).max(1e-12);
        let kmax = ((5000.0 * nfft as f32 / SR) as usize).min(bins - 2);
        for k in 3..kmax {
            let m = mag[k];
            if m < mx * 0.02 || m < mag[k - 1] || m < mag[k + 1] {
                continue;
            }
            // parabolic peak frequency
            let (a, b, c) = (mag[k - 1], m, mag[k + 1]);
            let d = a - 2.0 * b + c;
            let off = if d.abs() > 1e-12 { 0.5 * (a - c) / d } else { 0.0 };
            let hz = (k as f32 + off.clamp(-0.5, 0.5)) * SR / nfft as f32;
            let wgt = (m / mx).sqrt();
            for h in 1..=8 {
                let f0 = hz / h as f32;
                if f0 < 30.0 {
                    break;
                }
                let mm = 69.0 + 12.0 * (f0 / 440.0).log2();
                let idx = ((mm - mlo) * res).round();
                if idx >= 0.0 && (idx as usize) < nsal {
                    sal[idx as usize] += wgt * 0.84f32.powi(h - 1);
                }
            }
        }
        let a = (((mlo_q - mlo) * res).max(0.0)) as usize;
        let b = ((((mhi_q - mlo) * res).max(0.0)) as usize).min(nsal);
        let mut bi = a;
        for i in a..b {
            // a little smoothing across neighbouring bins
            let s = sal[i] + 0.5 * (sal.get(i.wrapping_sub(1)).copied().unwrap_or(0.0) + sal.get(i + 1).copied().unwrap_or(0.0));
            let sb = sal[bi] + 0.5 * (sal.get(bi.wrapping_sub(1)).copied().unwrap_or(0.0) + sal.get(bi + 1).copied().unwrap_or(0.0));
            if s > sb {
                bi = i;
            }
        }
        if b > a {
            best[f] = mlo + bi as f32 / res;
            sal_v[f] = sal[bi];
        }
        if picked {
            for (i, v) in sal.iter().enumerate() {
                let si = (i as f32 / res).round() as usize;
                if si < nsemi {
                    semis[f * nsemi + si] += v;
                }
            }
        }
    }
    let sgate = pct(&sal_v, 0.9) * 0.3;
    let lgate = pct(&lvl, 0.95) * 0.06;
    let mut midi: Vec<f32> = (0..frames).map(|f| if sal_v[f] > sgate && lvl[f] > lgate { best[f] } else { 0.0 }).collect();
    fix_octaves(&mut midi);
    let fl: Vec<f32> = {
        let b = band_analysis(x, 2048, &[(lo.max(30.0), (hi * 4.0).min(12000.0))]);
        b.flux.into_iter().next().unwrap_or_default()
    };
    let onsets: Vec<usize> = pick_peaks(&fl, if picked { 1.2 - 0.8 * sens } else { 0.8 }, 3).into_iter().filter(|p| p.1 > if picked { 0.3 * (1.0 - sens) } else { 0.25 }).map(|p| p.0).collect();
    let min_s = (grid.bar_s() / steps_per_bar as f32 * 0.5).max(0.05);
    if !picked {
        return segment_notes(&midi, &lvl, &onsets, fps(), 0.0, min_s, grid, steps_per_bar);
    }
    let (sa, sb) = ((mlo_q - mlo).round().max(0.0) as usize, ((mhi_q - mlo).round().max(0.0) as usize).min(nsemi));
    let at = |f: usize, p: usize| semis[f.min(frames - 1) * nsemi + p];
    let lmax = lvl.iter().cloned().fold(1e-12f32, f32::max);
    let step_s = grid.bar_s() / steps_per_bar as f32;
    let fr = fps();
    let mut out: Vec<PNote> = Vec::new();
    for (k, &o) in onsets.iter().enumerate() {
        if lvl[o.min(frames - 1)] < lgate || sb <= sa {
            continue;
        }
        let mut bp = sa;
        let mut br = f32::MIN;
        let mut bafter = 0.0;
        for p in sa..sb {
            let after = (o..o + 5).map(|f| at(f, p)).fold(0.0f32, f32::max);
            let before = if o >= 4 { (o - 4..o).map(|f| at(f, p)).sum::<f32>() / 4.0 } else { 0.0 };
            let rise = after - before;
            if rise > br {
                br = rise;
                bp = p;
                bafter = after;
            }
        }
        if br <= 0.0 || bafter < sgate * (1.5 - sens) {
            continue;
        }
        let next = onsets.get(k + 1).copied().unwrap_or(frames).min(o + (grid.bar_s() * fr) as usize);
        let (t0, t1) = (o as f32 / fr, next as f32 / fr);
        let s0 = ((t0 - grid.downbeat_s) / step_s).round().max(0.0) as usize;
        let s1 = ((t1 - grid.downbeat_s) / step_s).round().max(0.0) as usize;
        let vel = (40.0 + 87.0 * (lvl[o.min(frames - 1)] / lmax).sqrt()).clamp(30.0, 127.0) as u8;
        let nt = PNote { t0, t1, pitch: (mlo as usize + bp).min(127) as u8, vel, step: s0, len: s1.saturating_sub(s0).max(1) };
        match out.last_mut() {
            Some(l) if l.step == nt.step => {
                if nt.vel > l.vel {
                    *l = nt;
                }
            }
            Some(l) => {
                if l.step + l.len > nt.step {
                    l.len = nt.step - l.step;
                }
                out.push(nt);
            }
            None => out.push(nt),
        }
    }
    out
}

pub fn notes_midi(notes: &[PNote], grid: Grid, steps_per_bar: usize, name: &str, program: Option<u8>, keep_timing: bool) -> Smf {
    let ppq = 480u32;
    let step_ticks = ppq as f32 * grid.beats_per_bar as f32 / steps_per_bar as f32;
    let tick_of = |t: f32| (((t - grid.downbeat_s) / grid.beat_s()) * ppq as f32).max(0.0).round() as u32;
    let ns = notes
        .iter()
        .map(|n| {
            if keep_timing {
                let a = tick_of(n.t0);
                SmfNote { tick: a, len: tick_of(n.t1).saturating_sub(a).max(10), pitch: n.pitch, vel: n.vel, channel: 0 }
            } else {
                SmfNote { tick: (n.step as f32 * step_ticks).round() as u32, len: (n.len as f32 * step_ticks).round().max(1.0) as u32, pitch: n.pitch, vel: n.vel, channel: 0 }
            }
        })
        .collect();
    Smf { division: ppq as u16, bpm: grid.bpm, time_sig: (grid.beats_per_bar as u8, 4), tracks: vec![SmfTrack { name: name.into(), program, notes: ns }] }
}

/// Notes grouped per bar as text: "D2:4 F2:2 ." (pitch:length in steps, '.' = empty bar).
pub fn notes_per_bar(notes: &[PNote], steps_per_bar: usize, bars: usize) -> Vec<String> {
    let mut out = vec![String::new(); bars];
    for n in notes {
        let b = n.step / steps_per_bar;
        if b < bars {
            if !out[b].is_empty() {
                out[b].push(' ');
            }
            out[b].push_str(&format!("{}@{}:{}", note_name(n.pitch), n.step % steps_per_bar, n.len));
        }
    }
    out.iter().map(|s| if s.is_empty() { ".".to_string() } else { s.clone() }).collect()
}

// ---------------------------------------------------------------- chords and key

pub const QUALITIES: [(&str, &[u8], f32); 9] = [
    ("", &[0, 4, 7], 1.0),
    ("m", &[0, 3, 7], 1.0),
    ("7", &[0, 4, 7, 10], 0.96),
    ("maj7", &[0, 4, 7, 11], 0.96),
    ("m7", &[0, 3, 7, 10], 0.96),
    ("sus4", &[0, 5, 7], 0.94),
    ("sus2", &[0, 2, 7], 0.94),
    ("dim", &[0, 3, 6], 0.92),
    ("5", &[0, 7], 0.88),
];

pub fn chord_label(state: usize) -> String {
    if state >= 12 * QUALITIES.len() {
        return "N".into();
    }
    format!("{}{}", PCN[state % 12], QUALITIES[state / 12].0)
}

/// Pitch classes of a chord state (empty for N).
pub fn chord_pcs(state: usize) -> Vec<u8> {
    if state >= 12 * QUALITIES.len() {
        return Vec::new();
    }
    let r = (state % 12) as u8;
    QUALITIES[state / 12].1.iter().map(|i| (r + i) % 12).collect()
}

/// Chroma (60 Hz-5 kHz) and bass chroma (35-220 Hz) per beat on the grid.
pub fn beat_chroma(x: &[f32], grid: Grid) -> (Vec<[f32; 12]>, Vec<[f32; 12]>, Vec<f32>) {
    let nfft = 8192usize;
    let hop = 2048usize;
    let bins = nfft / 2 + 1;
    let w = hann(nfft);
    let fft = Fft::new(nfft);
    let dur = x.len() as f32 / SR;
    let nbeats = (((dur - grid.downbeat_s) / grid.beat_s()).ceil().max(1.0)) as usize;
    let mut ch = vec![[0.0f32; 12]; nbeats];
    let mut bch = vec![[0.0f32; 12]; nbeats];
    let mut en = vec![0.0f32; nbeats];
    let pc_of: Vec<i32> = (0..bins)
        .map(|k| {
            let hz = k as f32 * SR / nfft as f32;
            if hz < 35.0 || hz > 5000.0 { -1 } else { ((69.0 + 12.0 * (hz / 440.0).log2()).round() as i32).rem_euclid(12) }
        })
        .collect();
    let (mut re, mut im) = (vec![0.0f32; nfft], vec![0.0f32; nfft]);
    let frames = x.len() / hop + 1;
    for f in 0..frames {
        let tc = (f * hop) as f32 / SR;
        let bpos = (tc - grid.downbeat_s) / grid.beat_s();
        if bpos < 0.0 {
            continue;
        }
        let bi = bpos as usize;
        if bi >= nbeats {
            break;
        }
        for k in 0..nfft {
            let i = (f * hop + k) as isize - (nfft / 2) as isize;
            re[k] = if i >= 0 && (i as usize) < x.len() { x[i as usize] * w[k] } else { 0.0 };
            im[k] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        for k in 1..bins {
            let p = pc_of[k];
            if p < 0 {
                continue;
            }
            let m = (re[k] * re[k] + im[k] * im[k]).sqrt();
            let hz = k as f32 * SR / nfft as f32;
            let v = m.sqrt();
            if hz >= 60.0 {
                ch[bi][p as usize] += v;
            }
            if hz <= 220.0 {
                bch[bi][p as usize] += m;
            }
            en[bi] += m * m;
        }
    }
    (ch, bch, en)
}

fn norm12(c: &[f32; 12]) -> [f32; 12] {
    let mn = c.iter().cloned().fold(f32::MAX, f32::min);
    let mut o = [0.0f32; 12];
    let mut n = 0.0;
    for i in 0..12 {
        o[i] = c[i] - mn;
        n += o[i] * o[i];
    }
    let n = n.sqrt().max(1e-9);
    o.iter_mut().for_each(|v| *v /= n);
    o
}

/// Key from total chroma (Krumhansl-Kessler): (tonic pc, minor?, confidence).
pub fn key_of(chroma: &[[f32; 12]]) -> (u8, bool, f32) {
    const MAJ: [f32; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
    const MIN: [f32; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];
    let mut tot = [0.0f32; 12];
    for c in chroma {
        let n = norm12(c);
        for i in 0..12 {
            tot[i] += n[i];
        }
    }
    let corr = |p: &[f32; 12], r: usize| -> f32 {
        let mx = tot.iter().sum::<f32>() / 12.0;
        let mp = p.iter().sum::<f32>() / 12.0;
        let (mut s, mut a, mut b) = (0.0, 0.0, 0.0);
        for i in 0..12 {
            let x = tot[(i + r) % 12] - mx;
            let y = p[i] - mp;
            s += x * y;
            a += x * x;
            b += y * y;
        }
        s / (a * b).sqrt().max(1e-9)
    };
    let mut best = (0u8, false, f32::MIN);
    let mut second = f32::MIN;
    for r in 0..12 {
        for (minor, p) in [(false, &MAJ), (true, &MIN)] {
            let c = corr(p, r);
            if c > best.2 {
                second = best.2;
                best = (r as u8, minor, c);
            } else if c > second {
                second = c;
            }
        }
    }
    (best.0, best.1, (best.2 - second).max(0.0))
}

/// Chord state per beat by Viterbi over chord templates (+ bass note), with a
/// cost for changing. Returns a state per beat (see chord_label).
/// `bass_w` weighs the bass's pitch class as the chord root (0.25 on a full mix;
/// higher when `bch` comes from an isolated bass stem).
pub fn chords_per_beat(ch: &[[f32; 12]], bch: &[[f32; 12]], en: &[f32], change_cost: f32, bass_w: f32) -> Vec<usize> {
    let nq = QUALITIES.len();
    let ns = 12 * nq + 1;
    let n = ch.len();
    if n == 0 {
        return Vec::new();
    }
    let egate = pct(en, 0.9) * 0.02;
    let emis: Vec<Vec<f32>> = (0..n)
        .map(|t| {
            let c = norm12(&ch[t]);
            let bmax = bch.get(t).map(|b| b.iter().cloned().fold(0.0f32, f32::max)).unwrap_or(0.0).max(1e-12);
            let mut row = vec![0.0f32; ns];
            for q in 0..nq {
                let (_, iv, w) = QUALITIES[q];
                for r in 0..12 {
                    let mut tpl = [0.0f32; 12];
                    for (j, i) in iv.iter().enumerate() {
                        tpl[(r + *i as usize) % 12] = if j == 0 { 1.0 } else if j < 3 { 0.9 } else { 0.6 };
                    }
                    let tn = norm12(&tpl);
                    let cos: f32 = (0..12).map(|i| c[i] * tn[i]).sum();
                    // a 7th only counts when it is clearly there (overtones of the triad put a weak one in)
                    let mut wq = w;
                    if iv.len() == 4 {
                        let triad = (c[r] + c[(r + iv[1] as usize) % 12] + c[(r + iv[2] as usize) % 12]) / 3.0;
                        // the 7th's pitch class minus what the note a fifth below puts there as its third harmonic
                        let e7 = (r + iv[3] as usize) % 12;
                        if c[e7] - 0.4 * c[(e7 + 5) % 12] < 0.75 * triad {
                            wq *= 0.85;
                        }
                    }
                    row[q * 12 + r] = wq * cos + bass_w * bch.get(t).map(|b| b[r]).unwrap_or(0.0) / bmax;
                }
            }
            row[ns - 1] = if en[t] < egate { 1.0 } else { 0.25 };
            row
        })
        .collect();
    let mut score = emis[0].clone();
    let mut back = vec![vec![0usize; ns]; n];
    for t in 1..n {
        let (bi, bv) = score.iter().enumerate().fold((0, f32::MIN), |a, (i, v)| if *v > a.1 { (i, *v) } else { a });
        let mut next = vec![0.0f32; ns];
        for s in 0..ns {
            let stay = score[s];
            let jump = bv - change_cost;
            if stay >= jump {
                next[s] = stay + emis[t][s];
                back[t][s] = s;
            } else {
                next[s] = jump + emis[t][s];
                back[t][s] = bi;
            }
        }
        score = next;
    }
    let mut s = score.iter().enumerate().fold((0, f32::MIN), |a, (i, v)| if *v > a.1 { (i, *v) } else { a }).0;
    let mut path = vec![0usize; n];
    for t in (0..n).rev() {
        path[t] = s;
        s = back[t][s];
    }
    path
}

/// Shortest repeating chord loop over bars (1, 2, 4 or 8 bars) and how often it holds.
pub fn chord_loop(per_bar: &[String]) -> (usize, f32) {
    let bars: Vec<&String> = per_bar.iter().filter(|b| b.as_str() != "N").collect();
    if bars.len() < 4 {
        return (bars.len(), 0.0);
    }
    for p in [1usize, 2, 4, 8] {
        if p * 2 > bars.len() {
            break;
        }
        let same = (p..bars.len()).filter(|&i| bars[i] == bars[i - p]).count();
        let frac = same as f32 / (bars.len() - p) as f32;
        if frac >= 0.75 {
            return (p, frac);
        }
    }
    (0, 0.0)
}

/// Close voicing around C4 for a chord state.
pub fn voicing(state: usize) -> Vec<u8> {
    let pcs = chord_pcs(state);
    if pcs.is_empty() {
        return Vec::new();
    }
    let root = pcs[0] as i32;
    let base = 48 + root + if root > 6 { 0 } else { 12 };
    let mut v: Vec<u8> = Vec::new();
    for pc in &pcs {
        let mut m = base + ((*pc as i32 - root).rem_euclid(12));
        if m > 72 {
            m -= 12;
        }
        v.push(m as u8);
    }
    v.push((36 + root) as u8);
    v.sort_unstable();
    v.dedup();
    v
}

pub fn check_audio(x: &[f32], min_s: f32) -> Result<()> {
    if x.len() < (SR * min_s) as usize {
        bail!("less than {min_s} s of audio");
    }
    if x.len() > (SR * 900.0) as usize {
        bail!("over 15 minutes; trim it first (edit_sample)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Rng;

    fn kit(bpm: f32, bars: usize, ghost_bar: Option<usize>) -> Vec<f32> {
        // 16th grid: kick 0,8,10 ; snare 4,12 ; closed hat every 2 ; crash on bar 1 step 0
        let step = 60.0 / bpm / 4.0;
        let n = (SR * step * 16.0 * bars as f32) as usize + (SR * 0.5) as usize;
        let mut x = vec![0.0f32; n];
        let mut rng = Rng::new(7);
        let mut add = |t: f32, kind: &str, amp: f32, x: &mut Vec<f32>| {
            let i0 = (t * SR) as usize;
            let len = match kind { "kick" => 0.25, "snare" => 0.18, "hat" => 0.04, _ => 1.2 };
            let mut hp = Biquad::new(BiquadKind::LowCut, 7000.0, 0.7, 0.0);
            let mut bp = Biquad::new(BiquadKind::LowCut, 1500.0, 0.7, 0.0);
            for k in 0..(len * SR) as usize {
                let tt = k as f32 / SR;
                if i0 + k >= x.len() { break; }
                let v = match kind {
                    "kick" => (2.0 * PI * (50.0 + 120.0 * (-tt * 30.0).exp()) * tt).sin() * (-tt * 14.0).exp(),
                    "snare" => 0.5 * (2.0 * PI * 200.0 * tt).sin() * (-tt * 25.0).exp() + 0.7 * bp.process(rng.bipolar()) * (-tt * 18.0).exp(),
                    "hat" => hp.process(rng.bipolar()) * (-tt * 90.0).exp(),
                    _ => 0.6 * hp.process(rng.bipolar()) * (-tt * 1.5).exp(),
                };
                x[i0 + k] += amp * v;
            }
        };
        for b in 0..bars {
            for s in 0..16 {
                let t = 0.1 + (b * 16 + s) as f32 * step;
                if [0, 8, 10].contains(&s) { add(t, "kick", 0.8, &mut x); }
                if [4, 12].contains(&s) { add(t, "snare", 0.6, &mut x); }
                if s % 2 == 0 { add(t, "hat", 0.25, &mut x); }
                if Some(b) == ghost_bar && s == 7 { add(t, "snare", 0.12, &mut x); }
            }
            if b == 0 { add(0.1, "crash", 0.5, &mut x); }
        }
        x
    }

    #[test]
    fn drums_land_on_their_steps() {
        let bpm = 120.0;
        let x = kit(bpm, 8, Some(5));
        let g = Grid::new(bpm, 0.1, 4);
        let s = transcribe_drums(&x, g, 16);
        let row = |b: usize, k: usize| row_text(&s.pattern[b][k], 60);
        // bar 3 (index 2): plain groove
        assert_eq!(&row(3, 0), "x.......x.x.....", "kick {:?}", row(3, 0));
        assert_eq!(&row(3, 1)[4..5], "x");
        assert_eq!(&row(3, 1)[12..13], "x");
        let hats = s.pattern[3][2].iter().filter(|v| **v > 0).count() + s.pattern[3][3].iter().filter(|v| **v > 0).count();
        assert!(hats >= 6, "hats {hats}: {}", row(3, 2));
        // the ghost note in bar 6 shows up as a quiet snare
        assert!(s.pattern[5][1][7] > 0 && s.pattern[5][1][7] < 60, "ghost {:?}", s.pattern[5][1]);
        // crash on the first downbeat
        assert!(s.pattern[0][4][0] > 0, "crash {:?}", s.pattern[0][4]);
        assert!((s.swing_pct - 50.0).abs() < 6.0, "swing {}", s.swing_pct);
        // repeated bars share a letter
        assert_eq!(s.letters[2], s.letters[3]);
        let m = drums_midi(&s, false);
        assert!(m.tracks[0].notes.iter().any(|n| n.pitch == 36 && n.tick == 4 * 480 * 3));
    }

    fn tone(x: &mut [f32], t0: f32, t1: f32, midi: f32, amp: f32, harmonics: usize) {
        let hz = 440.0 * 2f32.powf((midi - 69.0) / 12.0);
        let (a, b) = ((t0 * SR) as usize, ((t1 * SR) as usize).min(x.len()));
        for i in a..b {
            let tt = (i - a) as f32 / SR;
            let env = (tt * 200.0).min(1.0) * (-tt * 1.5).exp();
            let mut v = 0.0;
            for h in 1..=harmonics {
                v += (2.0 * PI * hz * h as f32 * tt).sin() / h as f32;
            }
            x[i] += amp * env * v;
        }
    }

    #[test]
    fn low_bass_8ths_land_on_their_steps() {
        // 124 bpm, downbeat at 0.167 s, plucked low 8ths (E1/G1/A1) repeated and changing
        let g = Grid::new(124.0, 0.167, 4);
        let step = 60.0 / 124.0 / 4.0;
        let mut x = vec![0.0f32; (SR * 8.5) as usize];
        let pitches = [28.0f32, 28.0, 28.0, 31.0, 33.0, 33.0, 31.0, 28.0];
        for bar in 0..4 {
            for (i, m) in pitches.iter().enumerate() {
                let t0 = 0.167 + ((bar * 16 + i * 2) as f32) * step;
                tone(&mut x, t0, t0 + 2.0 * step, *m, 0.5, 6);
            }
        }
        let notes = transcribe_bass(&x, g, 16);
        let odd: Vec<usize> = notes.iter().map(|n| n.step).filter(|s| s % 2 == 1).collect();
        assert!(notes.len() >= 16, "too few notes: {}", notes.len());
        assert!(odd.len() * 10 <= notes.len(), "notes landed between the 8ths: {odd:?}");
    }

    #[test]
    fn bass_line_comes_out_note_for_note() {
        let bpm = 120.0; // 16th = 0.125 s
        let g = Grid::new(bpm, 0.0, 4);
        let line = [(0usize, 4usize, 38.0f32), (4, 4, 41.0), (8, 2, 45.0), (10, 2, 43.0), (12, 4, 36.0)];
        let mut x = vec![0.0f32; (SR * 4.5) as usize];
        for bar in 0..2 {
            for (s, l, m) in line {
                let t0 = (bar * 16 + s) as f32 * 0.125;
                tone(&mut x, t0, t0 + l as f32 * 0.125 - 0.01, m, 0.5, 4);
            }
        }
        let notes = transcribe_bass(&x, g, 16);
        let got: Vec<(usize, u8)> = notes.iter().map(|n| (n.step, n.pitch)).collect();
        for bar in 0..2 {
            for (s, _, m) in line {
                assert!(got.contains(&(bar * 16 + s, m as u8)), "missing {} at {}: {:?}", m, bar * 16 + s, got);
            }
        }
        assert!(notes.len() <= 12, "too many notes {got:?}");
    }

    #[test]
    fn picked_arpeggio_follows_each_new_string() {
        // a clean-guitar style arpeggio: G3 Bb3 D4 G4 D4 Bb3 on 8ths, every string left ringing to the bar's end
        let g = Grid::new(120.0, 0.0, 4);
        let mut x = vec![0.0f32; (SR * 4.5) as usize];
        let arp = [55.0f32, 58.0, 62.0, 67.0, 62.0, 58.0, 55.0, 62.0];
        for bar in 0..2 {
            for (i, m) in arp.iter().enumerate() {
                let t0 = bar as f32 * 2.0 + i as f32 * 0.25;
                tone(&mut x, t0, bar as f32 * 2.0 + 1.99, *m, 0.3, 5);
            }
        }
        let notes = transcribe_picked(&x, g, 16, 120.0, 1500.0, 0.5);
        let got: Vec<(usize, u8)> = notes.iter().map(|n| (n.step, n.pitch)).collect();
        let want: Vec<(usize, u8)> = (0..2).flat_map(|b| arp.iter().enumerate().map(move |(i, m)| (b * 16 + i * 2, *m as u8))).collect();
        let hit = want.iter().filter(|w| got.contains(w)).count();
        assert!(hit >= 13, "picked {hit}/16: {got:?}");
        assert!(notes.len() <= 18, "too many {got:?}");
    }

    #[test]
    fn riff_top_line_over_a_pad() {
        let bpm = 120.0;
        let g = Grid::new(bpm, 0.0, 4);
        let mut x = vec![0.0f32; (SR * 2.5) as usize];
        // quiet pad chord underneath (D minor) and a louder riff on top
        for m in [50.0, 53.0, 57.0] {
            tone(&mut x, 0.0, 2.2, m, 0.08, 3);
        }
        let riff = [74.0f32, 77.0, 81.0, 79.0, 77.0, 76.0, 74.0, 72.0];
        for (i, m) in riff.iter().enumerate() {
            let t0 = i as f32 * 0.25;
            tone(&mut x, t0, t0 + 0.24, *m, 0.3, 5);
        }
        let notes = transcribe_top_line(&x, g, 16, 200.0, 1500.0);
        let got: Vec<(usize, u8)> = notes.iter().map(|n| (n.step, n.pitch)).collect();
        let hit = riff.iter().enumerate().filter(|(i, m)| got.contains(&(i * 2, **m as u8))).count();
        assert!(hit >= 7, "riff {hit}/8: {got:?}");
    }

    #[test]
    fn chords_and_key_from_a_progression() {
        let bpm = 120.0; // bar = 2 s
        let g = Grid::new(bpm, 0.0, 4);
        let prog: [(&[f32], &str); 4] = [(&[50.0, 53.0, 57.0], "Dm"), (&[46.0, 50.0, 53.0], "A#"), (&[53.0, 57.0, 60.0], "F"), (&[48.0, 52.0, 55.0], "C")];
        let mut x = vec![0.0f32; (SR * 16.5) as usize];
        for rep in 0..2 {
            for (i, (ns, _)) in prog.iter().enumerate() {
                let t0 = (rep * 4 + i) as f32 * 2.0;
                for m in ns.iter() {
                    tone(&mut x, t0, t0 + 1.98, *m, 0.15, 4);
                }
                tone(&mut x, t0, t0 + 1.98, ns[0] - 12.0, 0.2, 2);
            }
        }
        let (ch, bch, en) = beat_chroma(&x, g);
        let path = chords_per_beat(&ch, &bch, &en, 0.15, 0.25);
        for rep in 0..2 {
            for (i, (_, name)) in prog.iter().enumerate() {
                let beat = (rep * 4 + i) * 4 + 1;
                assert_eq!(chord_label(path[beat]), *name, "bar {} {:?}", rep * 4 + i + 1, path.iter().map(|s| chord_label(*s)).collect::<Vec<_>>());
            }
        }
        let (k, minor, _) = key_of(&ch);
        // D minor and F major share notes; either is an acceptable reading, D minor preferred
        assert!((k == 2 && minor) || (k == 5 && !minor), "key {} {}", PCN[k as usize], minor);
        let per_bar: Vec<String> = (0..8).map(|b| chord_label(path[b * 4 + 1])).collect();
        assert_eq!(chord_loop(&per_bar).0, 4);
    }

    #[test]
    fn grid_normalises_the_downbeat() {
        let g = Grid::new(120.0, 4.3, 4);
        assert!((g.downbeat_s - 0.3).abs() < 1e-4, "{}", g.downbeat_s);
        assert_eq!(g.label(4.3), "3.1");
        assert_eq!(g.bar_of(4.31), 2);
        // a short lead-in keeps the first downbeat as bar 1
        assert!((Grid::new(120.0, 0.1, 4).downbeat_s - 0.1).abs() < 1e-4);
    }
}

#[cfg(test)]
mod reattack_tests {
    use super::*;
    #[test]
    fn a_held_note_struck_again_is_split_on_its_step() {
        let g = Grid { bpm: 120.0, downbeat_s: 0.0, beats_per_bar: 4 };
        // one bar = 2 s, a 16th = 0.125 s; a note held the whole bar, re-struck on step 12
        let mut ns = vec![PNote { t0: 0.0, t1: 2.0, pitch: 33, vel: 90, step: 0, len: 16 }];
        let added = split_reattacks(&[0.01, 0.06, 1.51, 1.97], &mut ns, g, 16);
        assert_eq!(added, 1, "{ns:?}");
        assert_eq!((ns[0].step, ns[0].len), (0, 12));
        assert_eq!((ns[1].step, ns[1].len, ns[1].pitch), (12, 4, 33));
    }

    #[test]
    fn log_flux_finds_a_soft_repick_as_well_as_a_loud_one() {
        // a decaying 110 Hz pluck at 0.5 s (loud) and again at 2.0 s, 30 dB softer
        let n = (SR * 3.0) as usize;
        let mut x = vec![0.0f32; n];
        for (t0, amp) in [(0.5f32, 0.8f32), (2.0, 0.025)] {
            let s0 = (t0 * SR) as usize;
            for i in s0..n {
                let t = (i - s0) as f32 / SR;
                x[i] += amp * (-t * 3.0).exp() * ((2.0 * PI * 110.0 * t).sin() + 0.3 * (2.0 * PI * 220.0 * t).sin() + 0.2 * (2.0 * PI * 1760.0 * t).sin() * (-t * 40.0).exp());
            }
        }
        let on = log_flux_onsets(&x, 0.07);
        for t0 in [0.5f32, 2.0] {
            assert!(on.iter().any(|t| (t - t0).abs() < 0.04), "{t0} not in {on:?}");
        }
    }
}
