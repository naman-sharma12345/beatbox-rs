//! Vocal to song: listen to a sung vocal (pitch contour, notes, key, tempo,
//! downbeat, lyrics) and build a whole arranged, mixed song around it.
//!
//! Everything here is local DSP except lyrics, which come from a small local
//! Whisper model run by an embedded Python helper (faster-whisper) when it is
//! installed; without it the song is still built from the melody alone.

use crate::sc_dsp::yin;
use crate::theory::{self, Chord};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// Analysis rate for pitch tracking (vocals sit well below 5.5 kHz).
const PSR: f32 = 11_025.0;
/// Pitch / onset frame hop in seconds.
pub const HOP_S: f32 = 0.01;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct PitchFrame {
    pub t: f32,
    /// Fundamental in Hz, 0 when unvoiced.
    pub hz: f32,
    /// Fractional MIDI pitch, 0 when unvoiced.
    pub midi: f32,
    pub rms: f32,
}

/// A sung note: times in seconds in the vocal file.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VNote {
    pub start: f32,
    pub end: f32,
    /// Median fractional MIDI pitch (tuning-corrected).
    pub midi: f32,
    pub pitch: u8,
    pub strength: f32,
}

impl VNote {
    pub fn dur(&self) -> f32 {
        self.end - self.start
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Word {
    pub start: f32,
    pub end: f32,
    pub word: String,
    #[serde(default)]
    pub prob: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct LyricLine {
    pub start: f32,
    pub end: f32,
    pub text: String,
    #[serde(default)]
    pub words: Vec<Word>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Lyrics {
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub segments: Vec<LyricLine>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TempoEstimate {
    pub bpm: f32,
    /// Time of a beat (seconds) in the vocal file; beats fall at phase + k * 60/bpm.
    pub phase: f32,
    /// Time of a downbeat (bar line) in the vocal file.
    pub downbeat: f32,
    pub confidence: f32,
    /// RMS distance of note onsets from the nearest 8th-note grid line, ms.
    pub timing_spread_ms: f32,
    pub candidates: Vec<(f32, f32)>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VocalAnalysis {
    pub duration: f32,
    pub tuning_cents: f32,
    pub notes: Vec<VNote>,
    pub key_root: String,
    pub scale: String,
    pub key_confidence: f32,
    pub key_alternatives: Vec<String>,
    pub tempo: TempoEstimate,
    pub range: (String, String),
    pub voiced_percent: f32,
    /// Sung phrases (start, end) in seconds, split at breaths/silences.
    pub phrases: Vec<(f32, f32)>,
}

fn hz_to_midi(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

/// YIN pitch track at 10 ms hops on a downsampled copy, with an RMS gate
/// relative to the take's loud parts and a 5-frame median to remove octave
/// blips.
pub fn pitch_track(x: &[f32], sr: f32) -> Vec<PitchFrame> {
    let y = if (sr - PSR).abs() > 1.0 {
        crate::samples::resample(x, sr, PSR)
    } else {
        x.to_vec()
    };
    let win = (PSR * 0.04) as usize; // 40 ms: two periods of 50 Hz... enough for >= 65 Hz
    let hop = (PSR * HOP_S) as usize;
    let mut raw = Vec::new();
    let mut pos = 0usize;
    while pos + win <= y.len() {
        let f = &y[pos..pos + win];
        let rms = (f.iter().map(|s| s * s).sum::<f32>() / win as f32).sqrt();
        raw.push((pos as f32 / PSR, rms, f));
        pos += hop;
    }
    let mut lv: Vec<f32> = raw.iter().map(|r| r.1).collect();
    lv.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p95 = lv.get(lv.len() * 95 / 100).copied().unwrap_or(0.0);
    let gate = (p95 * 0.08).max(1e-3);
    let mut frames: Vec<PitchFrame> = raw
        .iter()
        .map(|(t, rms, f)| {
            let hz = if *rms > gate {
                yin(f, PSR, 65.0, 1100.0, 0.18).unwrap_or(0.0)
            } else {
                0.0
            };
            PitchFrame {
                t: *t + 0.02,
                hz,
                midi: if hz > 0.0 { hz_to_midi(hz) } else { 0.0 },
                rms: *rms,
            }
        })
        .collect();
    // median-smooth voiced runs (and fix single octave jumps)
    let m: Vec<f32> = frames.iter().map(|f| f.midi).collect();
    for i in 0..frames.len() {
        if m[i] <= 0.0 {
            continue;
        }
        let mut w: Vec<f32> = (i.saturating_sub(2)..(i + 3).min(m.len()))
            .filter(|&j| m[j] > 0.0)
            .map(|j| m[j])
            .collect();
        if w.len() >= 3 {
            let med = median(&mut w);
            let mut v = m[i];
            while v - med > 7.0 {
                v -= 12.0;
            }
            while med - v > 7.0 {
                v += 12.0;
            }
            frames[i].midi = if (v - med).abs() > 1.5 { med } else { v };
            frames[i].hz = 440.0 * 2f32.powf((frames[i].midi - 69.0) / 12.0);
        }
    }
    // drop isolated voiced frames (consonant noise)
    for i in 0..frames.len() {
        let prev = i > 0 && frames[i - 1].midi > 0.0;
        let next = i + 1 < frames.len() && frames[i + 1].midi > 0.0;
        if frames[i].midi > 0.0 && !prev && !next {
            frames[i].midi = 0.0;
            frames[i].hz = 0.0;
        }
    }
    frames
}

/// Global tuning offset of the take in cents (circular mean of how far each
/// voiced frame sits from the equal-tempered grid).
pub fn tuning_cents(frames: &[PitchFrame]) -> f32 {
    let (mut s, mut c) = (0.0f32, 0.0f32);
    for f in frames.iter().filter(|f| f.midi > 0.0) {
        let a = (f.midi - f.midi.round()) * std::f32::consts::TAU;
        s += a.sin() * f.rms;
        c += a.cos() * f.rms;
    }
    if s == 0.0 && c == 0.0 {
        return 0.0;
    }
    s.atan2(c) / std::f32::consts::TAU * 100.0
}

/// Split the pitch track into notes: a note ends at a gap, at a stable move
/// of more than ~0.7 semitone away from its running median (vibrato and
/// scoops stay inside one note), or at a fresh syllable attack.
pub fn segment_notes(frames: &[PitchFrame], tuning: f32) -> Vec<VNote> {
    let off = tuning / 100.0;
    let mut notes = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let flush = |cur: &mut Vec<usize>, notes: &mut Vec<VNote>| {
        if cur.len() >= 6 {
            let mut p: Vec<f32> = cur.iter().map(|&i| frames[i].midi - off).collect();
            let med = median(&mut p);
            let strength = cur.iter().map(|&i| frames[i].rms).fold(0.0f32, f32::max);
            notes.push(VNote {
                start: frames[cur[0]].t,
                end: frames[*cur.last().unwrap()].t + HOP_S,
                midi: med,
                pitch: med.round().clamp(0.0, 127.0) as u8,
                strength,
            });
        }
        cur.clear();
    };
    let mut away = 0usize;
    let mut gap = 0usize;
    for i in 0..frames.len() {
        let f = frames[i];
        if f.midi <= 0.0 {
            gap += 1;
            if gap > 3 {
                flush(&mut cur, &mut notes);
            }
            continue;
        }
        gap = 0;
        if cur.is_empty() {
            cur.push(i);
            away = 0;
            continue;
        }
        let mut p: Vec<f32> = cur.iter().rev().take(25).map(|&j| frames[j].midi).collect();
        let med = median(&mut p);
        // a fresh attack: energy jumps 2.5x within 30 ms after a dip
        let attack = i >= 3
            && f.rms > 2.5 * frames[i - 3].rms
            && cur.len() > 10;
        if (f.midi - med).abs() > 0.7 {
            away += 1;
        } else {
            away = 0;
        }
        if away >= 4 || attack {
            // the note ended where it left its pitch
            let back = if attack { 0 } else { away - 1 };
            let keep = cur.len().saturating_sub(back);
            let tail: Vec<usize> = cur.split_off(keep);
            flush(&mut cur, &mut notes);
            cur = tail;
            away = 0;
        }
        cur.push(i);
    }
    flush(&mut cur, &mut notes);
    notes
}

/// Amateur takes drift in tuning over a song: re-round each note against the
/// tuning of the notes around it (+-4 s), then merge the short fragments a
/// between-semitones note leaves behind.
pub fn retune_and_merge(notes: &mut Vec<VNote>) {
    let snapshot: Vec<(f32, f32, f32)> = notes.iter().map(|n| (n.start, n.midi, n.dur())).collect();
    for n in notes.iter_mut() {
        let (mut s, mut c) = (0.0f32, 0.0f32);
        for &(t, m, d) in &snapshot {
            if (t - n.start).abs() <= 4.0 {
                let a = (m - m.round()) * std::f32::consts::TAU;
                s += a.sin() * d;
                c += a.cos() * d;
            }
        }
        let off = if s == 0.0 && c == 0.0 { 0.0 } else { s.atan2(c) / std::f32::consts::TAU };
        n.pitch = (n.midi - off).round().clamp(0.0, 127.0) as u8;
    }
    let mut out: Vec<VNote> = Vec::with_capacity(notes.len());
    for n in notes.drain(..) {
        if let Some(prev) = out.last_mut() {
            let gap = n.start - prev.end;
            let short = n.dur() < 0.12 || prev.dur() < 0.12;
            if gap < 0.05 && short && (prev.pitch == n.pitch || (prev.dur() < 0.12 && (prev.midi - n.midi).abs() < 1.2)) {
                // keep the longer note's pitch
                if n.dur() > prev.dur() {
                    prev.pitch = n.pitch;
                    prev.midi = n.midi;
                }
                prev.end = n.end;
                prev.strength = prev.strength.max(n.strength);
                continue;
            }
        }
        out.push(n);
    }
    *notes = out;
}

/// Phrases: voiced regions separated by at least `min_gap` seconds of
/// silence.
pub fn phrases(notes: &[VNote], min_gap: f32) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::new();
    for n in notes {
        match out.last_mut() {
            Some(last) if n.start - last.1 < min_gap => last.1 = last.1.max(n.end),
            _ => out.push((n.start, n.end)),
        }
    }
    out
}

/// Key from a duration-weighted pitch-class histogram of the sung notes.
pub fn vocal_key(notes: &[VNote]) -> Vec<(u8, &'static str, f32)> {
    let mut c = [0.0f32; 12];
    for n in notes {
        c[(n.pitch % 12) as usize] += n.dur().min(1.5) * (0.5 + n.strength.min(1.0));
    }
    // phrase-final notes weigh more: the tonic is where melodies come to rest
    for w in notes.windows(2) {
        if w[1].start - w[0].end > 0.35 {
            c[(w[0].pitch % 12) as usize] += w[0].dur().min(1.5) * 0.8;
        }
    }
    if let Some(l) = notes.last() {
        c[(l.pitch % 12) as usize] += l.dur().min(1.5);
    }
    crate::midi_ops::key_from_chroma(&c)
}

fn onset_envelope(onsets: &[(f32, f32)], len_s: f32) -> Vec<f32> {
    let n = ((len_s / HOP_S) as usize).max(1) + 8;
    let mut env = vec![0.0f32; n];
    for &(t, w) in onsets {
        let c = t / HOP_S;
        let ci = c.round() as i64;
        for d in -4i64..=4 {
            let i = ci + d;
            if i >= 0 && (i as usize) < n {
                let x = (i as f32 - c) / 2.0; // sigma = 20 ms
                env[i as usize] += w * (-0.5 * x * x).exp();
            }
        }
    }
    env
}

/// Tempo, beat phase and downbeat of an a-cappella take from its note
/// onsets (and word starts when lyrics are known): comb-filtered
/// autocorrelation with a mild prior around 100 BPM, then a least-squares
/// grid fit and a downbeat chosen where long, strong notes land.
pub fn estimate_tempo(notes: &[VNote], words: &[Word], len_s: f32, bpm_hint: Option<f32>) -> TempoEstimate {
    let mut onsets: Vec<(f32, f32)> = notes
        .iter()
        .map(|n| (n.start, (n.dur().min(1.0)).sqrt() * (0.3 + n.strength.min(1.0))))
        .collect();
    for w in words {
        onsets.push((w.start, 0.6));
    }
    onsets.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let env = onset_envelope(&onsets, len_s.max(1.0));
    let ac = |lag: usize| -> f32 {
        if lag == 0 || lag >= env.len() {
            return 0.0;
        }
        let mut s = 0.0;
        for i in 0..env.len() - lag {
            s += env[i] * env[i + lag];
        }
        s
    };
    let score = |bpm: f32| -> f32 {
        let lag = 60.0 / bpm / HOP_S;
        let mut s = 0.0;
        for k in 1..=4 {
            let l = lag * k as f32;
            let (a, b) = (l.floor() as usize, l.ceil() as usize);
            let fr = l - l.floor();
            s += (ac(a) * (1.0 - fr) + ac(b) * fr) / k as f32;
        }
        // half-beat (8th) support
        let h = lag * 0.5;
        s += 0.25 * ac(h.round() as usize);
        let prior = (-0.5 * ((bpm / 100.0).log2() / 0.55).powi(2)).exp();
        s * (0.55 + 0.45 * prior)
    };
    let mut cands: Vec<(f32, f32)> = Vec::new();
    let (lo, hi) = match bpm_hint {
        Some(b) => (b * 0.97, b * 1.03),
        None => (60.0, 180.0),
    };
    let mut b = lo;
    while b <= hi {
        cands.push((b, score(b)));
        b += 0.25;
    }
    let total: f32 = cands.iter().map(|c| c.1).sum::<f32>().max(1e-9);
    let mut sorted = cands.clone();
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut picks: Vec<(f32, f32)> = Vec::new();
    for c in &sorted {
        if picks.iter().all(|p| (p.0 - c.0).abs() > 4.0) {
            picks.push(*c);
        }
        if picks.len() >= 4 {
            break;
        }
    }
    let mut bpm = picks.first().map(|p| p.0).unwrap_or(100.0);
    let conf = picks.first().map(|p| p.1 / (total / cands.len() as f32) / 3.0).unwrap_or(0.0).min(1.0);
    // phase: maximize the envelope on the beat grid
    let best_phase = |bpm: f32| -> (f32, f32) {
        let period = 60.0 / bpm;
        let steps = (period / HOP_S) as usize;
        let mut best = (0.0f32, -1.0f32);
        for ph in 0..steps.max(1) {
            let mut t = ph as f32 * HOP_S;
            let mut s = 0.0;
            while t < len_s {
                let i = (t / HOP_S).round() as usize;
                s += env.get(i).copied().unwrap_or(0.0);
                t += period;
            }
            if s > best.1 {
                best = (ph as f32 * HOP_S, s);
            }
        }
        best
    };
    let (mut phase, _) = best_phase(bpm);
    // least-squares refinement on onsets that sit near a beat
    for _ in 0..2 {
        let period = 60.0 / bpm;
        let (mut sx, mut sy, mut sxx, mut sxy, mut n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for &(t, w) in &onsets {
            let k = ((t - phase) / period).round();
            let d = t - (phase + k * period);
            if d.abs() < period * 0.15 {
                let w = w as f64;
                let k = k as f64;
                sx += w * k;
                sy += w * t as f64;
                sxx += w * k * k;
                sxy += w * k * t as f64;
                n += w;
            }
        }
        let den = n * sxx - sx * sx;
        if n > 3.0 && den.abs() > 1e-9 {
            let slope = (n * sxy - sx * sy) / den;
            let icpt = (sy - slope * sx) / n;
            if slope > 0.25 && slope < 1.2 {
                let nb = 60.0 / slope as f32;
                if (nb / bpm - 1.0).abs() < 0.04 {
                    bpm = nb;
                    phase = icpt as f32;
                }
            }
        }
    }
    let period = 60.0 / bpm;
    phase = phase.rem_euclid(period);
    // timing spread against the 8th grid
    let half = period / 2.0;
    let mut se = 0.0;
    let mut wn = 0.0;
    for n in notes {
        let k = ((n.start - phase) / half).round();
        let d = n.start - (phase + k * half);
        se += d * d * n.dur();
        wn += n.dur();
    }
    let spread = if wn > 0.0 { (se / wn).sqrt() * 1000.0 } else { 0.0 };
    // downbeat: which of the 4 beats carries the long / strong notes and phrase starts
    let mut bar_score = [0.0f32; 4];
    let ph_starts: Vec<f32> = phrases(notes, 0.35).iter().map(|p| p.0).collect();
    for n in notes {
        let k = ((n.start - phase) / period).round();
        let d = n.start - (phase + k * period);
        if d.abs() < period * 0.2 {
            let slot = (k as i64).rem_euclid(4) as usize;
            bar_score[slot] += 0.3 * n.dur().min(2.0) * (0.4 + n.strength.min(1.0));
        }
    }
    for &s in &ph_starts {
        // phrases usually start on (or just before) a downbeat
        let k = ((s - phase) / period).round();
        if ((s - phase) - k * period).abs() < period * 0.25 {
            bar_score[(k as i64).rem_euclid(4) as usize] += 2.0;
        }
    }
    let slot = (0..4)
        .max_by(|&a, &b| bar_score[a].partial_cmp(&bar_score[b]).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0);
    let downbeat = phase + slot as f32 * period;
    TempoEstimate {
        bpm: (bpm * 10.0).round() / 10.0,
        phase,
        downbeat,
        confidence: conf,
        timing_spread_ms: spread,
        candidates: picks.iter().map(|p| ((p.0 * 10.0).round() / 10.0, p.1 / picks[0].1.max(1e-9))).collect(),
    }
}

pub fn analyze(x: &[f32], sr: f32, words: &[Word], bpm_hint: Option<f32>) -> VocalAnalysis {
    let frames = pitch_track(x, sr);
    let tuning = tuning_cents(&frames);
    let mut notes = segment_notes(&frames, tuning);
    retune_and_merge(&mut notes);
    let keys = vocal_key(&notes);
    let (pc, sc, kc) = keys.first().copied().unwrap_or((0, "major", 0.0));
    let duration = x.len() as f32 / sr;
    let tempo = estimate_tempo(&notes, words, duration, bpm_hint);
    let lo = notes.iter().map(|n| n.pitch).min().unwrap_or(60);
    let hi = notes.iter().map(|n| n.pitch).max().unwrap_or(60);
    let voiced = frames.iter().filter(|f| f.midi > 0.0).count() as f32 / frames.len().max(1) as f32;
    VocalAnalysis {
        duration,
        tuning_cents: (tuning * 10.0).round() / 10.0,
        key_root: theory::note_name(pc + 60).trim_end_matches(char::is_numeric).to_string(),
        scale: sc.to_string(),
        key_confidence: kc,
        key_alternatives: keys
            .iter()
            .skip(1)
            .take(2)
            .map(|k| format!("{} {}", theory::note_name(k.0 + 60).trim_end_matches(char::is_numeric), k.1))
            .collect(),
        tempo,
        range: (theory::note_name(lo), theory::note_name(hi)),
        voiced_percent: (voiced * 1000.0).round() / 10.0,
        phrases: phrases(&notes, 0.35),
        notes,
    }
}

// ------------------------------------------------------------ auto-warp

/// Starts of sung phrases (after at least `min_gap` s of silence), at least
/// `min_space` s apart.
pub fn phrase_anchors(notes: &[VNote], min_gap: f32, min_space: f32) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::new();
    let mut prev_end = f32::NEG_INFINITY;
    for n in notes {
        if n.start - prev_end >= min_gap && out.last().map_or(true, |&a| n.start - a >= min_space) {
            out.push(n.start);
        }
        prev_end = prev_end.max(n.end);
    }
    out
}

/// A piecewise-linear map from vocal time to song beats: each anchor
/// (a phrase start) lands on a bar or half-bar line.
#[derive(Clone, Debug, Serialize)]
pub struct Warp {
    pub bpm: f32,
    /// (vocal seconds, beats relative to the first anchor)
    pub anchors: Vec<(f32, f32)>,
    /// Largest local stretch applied (output / input duration), and the smallest.
    pub max_stretch: f32,
    pub min_stretch: f32,
}

impl Warp {
    pub fn beat_at(&self, t: f32) -> f32 {
        let a = &self.anchors;
        let bps = self.bpm / 60.0;
        if a.is_empty() {
            return t * bps;
        }
        if t <= a[0].0 {
            return a[0].1 - (a[0].0 - t) * bps;
        }
        for w in a.windows(2) {
            if t <= w[1].0 {
                let f = (t - w[0].0) / (w[1].0 - w[0].0).max(1e-6);
                return w[0].1 + f * (w[1].1 - w[0].1);
            }
        }
        let l = a[a.len() - 1];
        l.1 + (t - l.0) * bps
    }
    pub fn time_at_beat(&self, b: f32) -> f32 {
        let a = &self.anchors;
        let bps = self.bpm / 60.0;
        if a.is_empty() {
            return b / bps;
        }
        if b <= a[0].1 {
            return a[0].0 - (a[0].1 - b) / bps;
        }
        for w in a.windows(2) {
            if b <= w[1].1 {
                let f = (b - w[0].1) / (w[1].1 - w[0].1).max(1e-6);
                return w[0].0 + f * (w[1].0 - w[0].0);
            }
        }
        let l = a[a.len() - 1];
        l.0 + (b - l.1) / bps
    }
}

/// Fit a tempo near `bpm_guess` under which the gaps between phrase starts
/// are whole bars (or half bars, at a cost), then pin each phrase start to
/// its line. Local stretches stay within 0.8..1.25.
pub fn fit_warp(anchors: &[f32], bpm_guess: f32) -> Warp {
    let quant = |beats: f32| -> (f32, f32) {
        let bar = (beats / 4.0).round().max(1.0) * 4.0;
        let half = (beats / 2.0).round().max(2.0) * 2.0;
        let cb = (beats - bar).powi(2);
        let ch = (beats - half).powi(2) + 0.35;
        if cb <= ch { (bar, cb) } else { (half, ch) }
    };
    let mut best = (bpm_guess, f32::INFINITY);
    let mut b = bpm_guess * 0.88;
    while b <= bpm_guess * 1.12 {
        let mut cost = 0.0;
        for w in anchors.windows(2) {
            let beats = (w[1] - w[0]) * b / 60.0;
            cost += quant(beats).1.min(4.0);
        }
        cost += 0.02 * ((b / bpm_guess - 1.0) * 100.0).powi(2) / 10.0;
        if cost < best.1 {
            best = (b, cost);
        }
        b += 0.1;
    }
    let bpm = (best.0 * 10.0).round() / 10.0;
    let mut out = vec![(anchors.first().copied().unwrap_or(0.0), 0.0f32)];
    let (mut mx, mut mn) = (1.0f32, 1.0f32);
    for &a in anchors.iter().skip(1) {
        let (lt, lb) = *out.last().unwrap();
        let g = a - lt;
        if g <= 0.0 {
            continue;
        }
        let (q0, _) = quant(g * bpm / 60.0);
        let ratio = |q: f32| q * 60.0 / bpm / g;
        // keep the stretch musical: try the neighbours when it is too far
        let Some(q) = [q0, q0 + 2.0, q0 - 2.0, q0 + 4.0, q0 - 4.0]
            .into_iter()
            .find(|&q| q >= 2.0 && (0.8..=1.25).contains(&ratio(q)))
        else {
            continue; // this phrase rides its neighbours
        };
        let r = ratio(q);
        mx = mx.max(r);
        mn = mn.min(r);
        out.push((a, lb + q));
    }
    Warp { bpm, anchors: out, max_stretch: mx, min_stretch: mn }
}

/// Time-stretch with a time-varying ratio (WSOLA, pitch kept): output time
/// `o` (seconds) plays input time `map(o)`.
pub fn warp_audio(x: &[f32], sr: f32, out_len_s: f32, map: impl Fn(f32) -> f32) -> Vec<f32> {
    let n = 1536usize;
    let ha = n / 4;
    let tol = 288i64;
    let w: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect();
    let out_len = (out_len_s * sr).max(0.0) as usize;
    let mut out = vec![0.0f32; out_len + n];
    let mut wsum = vec![0.0f32; out_len + n];
    let xl = x.len() as i64;
    let get = |i: i64| -> f32 { if i >= 0 && i < xl { x[i as usize] } else { 0.0 } };
    let mut prev: Option<i64> = None;
    let mut o = 0usize;
    while o < out_len {
        let center = map((o + n / 2) as f32 / sr) * sr;
        let target = center as i64 - (n / 2) as i64;
        let pos = match prev {
            None => target,
            Some(p) => {
                let natural = p + ha as i64;
                let score = |c: i64, step: usize| -> f32 {
                    let mut s = 0.0;
                    let mut k = 0usize;
                    while k < n / 2 {
                        s += get(c + k as i64) * get(natural + k as i64);
                        k += step;
                    }
                    s
                };
                let mut best = (target, f32::NEG_INFINITY);
                let mut d = -tol;
                while d <= tol {
                    let sc = score(target + d, 4);
                    if sc > best.1 {
                        best = (target + d, sc);
                    }
                    d += 4;
                }
                let c0 = best.0;
                for d in -3..=3 {
                    let sc = score(c0 + d, 2);
                    if sc > best.1 {
                        best = (c0 + d, sc);
                    }
                }
                best.0
            }
        };
        for i in 0..n {
            out[o + i] += get(pos + i as i64) * w[i];
            wsum[o + i] += w[i];
        }
        prev = Some(pos);
        o += ha;
    }
    for (v, s) in out.iter_mut().zip(wsum.iter()) {
        if *s > 1e-3 {
            *v /= *s;
        }
    }
    out.truncate(out_len);
    out
}

// ------------------------------------------------------------ auto-tune

#[derive(Clone, Debug, Serialize, Default)]
pub struct TuneStats {
    pub notes: usize,
    pub notes_moved: usize,
    /// Mean absolute distance from the key's nearest note, cents, before / after.
    pub off_key_cents_before: f32,
    pub off_key_cents_after: f32,
}

/// Nearest pitch (fractional MIDI) of `m` that is in the scale.
pub fn snap_to_scale(m: f32, key_pc: u8, scale: &[u8]) -> f32 {
    let base = m.round() as i32;
    let mut best = (base as f32, f32::INFINITY);
    for d in -2..=2 {
        let c = base + d;
        let pc = (c - key_pc as i32).rem_euclid(12) as u8;
        if scale.contains(&pc) {
            let dist = (c as f32 - m).abs();
            if dist < best.1 {
                best = (c as f32, dist);
            }
        }
    }
    best.0
}

fn off_key_cents(notes: &[VNote], key_pc: u8, scale: &[u8]) -> f32 {
    let (mut s, mut w) = (0.0, 0.0);
    for n in notes {
        s += (n.midi - snap_to_scale(n.midi, key_pc, scale)).abs() * 100.0 * n.dur();
        w += n.dur();
    }
    if w > 0.0 { s / w } else { 0.0 }
}

/// Pitch-correct a sung take to a key (TD-PSOLA, formants kept). Each sung
/// note is moved to the nearest scale note: `amount` (0..1) scales the move,
/// `hard` (0..1) also flattens the note's own wobble/vibrato toward the
/// target (1 = the hard, robotic effect), `speed_ms` smooths how fast the
/// correction follows (glides between notes stay natural).
/// TD-PSOLA pitch shift: `corr` is the shift in semitones per pitch frame
/// (HOP_S apart, aligned with `frames`); unvoiced parts pass through. The
/// ratio is clamped to [lo, hi] (formants are kept, so +-7 semitones is fine).
pub fn psola(x: &[f32], sr: f32, frames: &[PitchFrame], f: &[f32], lo: f32, hi: f32) -> Vec<f32> {
    let frame_at = |s: usize| -> usize { ((s as f32 / sr - 0.02) / HOP_S).max(0.0) as usize };
    let f0_at = |s: usize| -> f32 { frames.get(frame_at(s)).map(|fr| fr.hz).unwrap_or(0.0) };
    let ratio_at = |s: usize| -> f32 { 2f32.powf(f.get(frame_at(s)).copied().unwrap_or(0.0) / 12.0) };
    // analysis epochs: one per period in voiced parts (on the waveform's
    // local peak), every 5 ms elsewhere
    let mut epochs: Vec<(usize, usize)> = Vec::new(); // (position, period)
    let mut pos = 0usize;
    let mut prev_peak: Option<usize> = None;
    while pos < x.len() {
        let hz = f0_at(pos);
        if hz > 0.0 {
            let t = ((sr / hz) as usize).clamp(20, 1200);
            // snap to the local maximum within +-t/4, following the previous epoch
            let lo = pos.saturating_sub(t / 4);
            let hi = (pos + t / 4).min(x.len().saturating_sub(1));
            let mut best = pos;
            for k in lo..=hi {
                if x[k] > x[best] {
                    best = k;
                }
            }
            if let Some(pp) = prev_peak {
                if best <= pp + t / 2 {
                    best = pp + t;
                }
            }
            if best >= x.len() {
                break;
            }
            epochs.push((best, t));
            prev_peak = Some(best);
            pos = best + t;
        } else {
            let t = (sr * 0.005) as usize;
            epochs.push((pos, t));
            prev_peak = None;
            pos += t;
        }
    }
    let mut out = vec![0.0f32; x.len()];
    let mut wsum = vec![0.0f32; x.len()];
    if epochs.is_empty() {
        return x.to_vec();
    }
    // synthesis: output epochs at the corrected period, each takes the grain
    // of the nearest analysis epoch
    let mut o = epochs[0].0 as f32;
    let mut ai = 0usize;
    while (o as usize) < x.len() {
        let oi = o as usize;
        while ai + 1 < epochs.len() && (epochs[ai + 1].0 as f32 - o).abs() < (epochs[ai].0 as f32 - o).abs() {
            ai += 1;
        }
        let (ae, t) = epochs[ai];
        let voiced = f0_at(ae) > 0.0;
        let r = if voiced { ratio_at(ae).clamp(lo, hi) } else { 1.0 };
        let half = t;
        for k in 0..(2 * half) {
            let src = ae as i64 - half as i64 + k as i64;
            let dst = oi as i64 - half as i64 + k as i64;
            if src < 0 || dst < 0 || src as usize >= x.len() || dst as usize >= x.len() {
                continue;
            }
            let w = 0.5 - 0.5 * (std::f32::consts::TAU * k as f32 / (2 * half) as f32).cos();
            out[dst as usize] += x[src as usize] * w;
            wsum[dst as usize] += w;
        }
        o += t as f32 / r;
    }
    for (v, s) in out.iter_mut().zip(wsum.iter()) {
        if *s > 0.05 {
            *v /= *s;
        }
    }
    out
}

pub fn autotune(x: &[f32], sr: f32, key_pc: u8, scale: &[u8], amount: f32, hard: f32, speed_ms: f32) -> (Vec<f32>, TuneStats) {
    let frames = pitch_track(x, sr);
    // absolute pitch (A440): the beat is in concert tuning even if the singer is not
    let mut notes = segment_notes(&frames, 0.0);
    retune_and_merge(&mut notes);
    let before = off_key_cents(&notes, key_pc, scale);
    // per-frame correction in semitones
    let mut corr = vec![0.0f32; frames.len()];
    let mut moved = 0usize;
    for n in &notes {
        let target = snap_to_scale(n.midi, key_pc, scale);
        if (target - n.midi).abs() > 0.03 {
            moved += 1;
        }
        let i0 = ((n.start - 0.02) / HOP_S).max(0.0) as usize;
        let i1 = (((n.end - 0.02) / HOP_S) as usize).min(frames.len());
        for i in i0..i1 {
            let m = frames[i].midi;
            if m <= 0.0 {
                continue;
            }
            // the frame's own octave (the median was folded)
            let mut fm = m;
            while fm - n.midi > 6.0 { fm -= 12.0; }
            while n.midi - fm > 6.0 { fm += 12.0; }
            let soft = target - n.midi;
            let firm = (target - fm).clamp(-1.0, 1.0);
            corr[i] = amount.clamp(0.0, 1.0) * ((1.0 - hard) * soft + hard * firm);
        }
    }
    // smooth the correction (one-pole both ways, so it does not lag)
    let a = (-HOP_S / (speed_ms.max(1.0) / 1000.0)).exp();
    let mut f = corr.clone();
    for i in 1..f.len() {
        f[i] = a * f[i - 1] + (1.0 - a) * f[i];
    }
    for i in (0..f.len().saturating_sub(1)).rev() {
        f[i] = a * f[i + 1] + (1.0 - a) * f[i];
    }
    let out = psola(x, sr, &frames, &f, 0.7, 1.45);
    // measure what the listener gets
    let fr2 = pitch_track(&out, sr);
    let mut n2 = segment_notes(&fr2, 0.0);
    retune_and_merge(&mut n2);
    let after = off_key_cents(&n2, key_pc, scale);
    (
        out,
        TuneStats {
            notes: notes.len(),
            notes_moved: moved,
            off_key_cents_before: (before * 10.0).round() / 10.0,
            off_key_cents_after: (after * 10.0).round() / 10.0,
        },
    )
}

// ------------------------------------------------------------ lyrics

/// The lyrics helper, shipped inside the binary and written next to the
/// project on first use.
pub const TRANSCRIBE_PY: &str = include_str!("../scripts/transcribe_lyrics.py");

/// Transcribe sung lyrics with word timestamps. Uses `BEATBOX_PYTHON` (or
/// python3) with faster-whisper; `model` is tiny / base / small.
pub fn transcribe(audio: &Path, workdir: &Path, model: &str, language: Option<&str>, prompt: Option<&str>) -> Result<Lyrics> {
    let cache = audio.with_extension(format!("lyrics.{model}.json"));
    if prompt.is_none() {
        if let Ok(s) = std::fs::read_to_string(&cache) {
            if let Ok(l) = serde_json::from_str::<Lyrics>(&s) {
                return Ok(l);
            }
        }
    }
    let script = workdir.join(".beatbox_transcribe_lyrics.py");
    std::fs::write(&script, TRANSCRIBE_PY).context("writing the lyrics helper")?;
    let py = std::env::var("BEATBOX_PYTHON").unwrap_or_else(|_| "python3".into());
    let mut cmd = std::process::Command::new(&py);
    cmd.arg(&script).arg(audio).arg("--model").arg(model);
    if let Some(l) = language {
        cmd.arg("--language").arg(l);
    }
    if let Some(p) = prompt {
        cmd.arg("--prompt").arg(p);
    }
    let out = cmd
        .output()
        .with_context(|| format!("running {py} (set BEATBOX_PYTHON to a Python with faster-whisper)"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(stdout.trim()).map_err(|_| {
        anyhow!(
            "lyrics helper failed: {}",
            String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("no output")
        )
    })?;
    if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
        bail!("{e}");
    }
    let l: Lyrics = serde_json::from_value(v)?;
    if prompt.is_none() {
        let _ = std::fs::write(&cache, serde_json::to_string(&l)?);
    }
    Ok(l)
}

impl Lyrics {
    pub fn words(&self) -> Vec<Word> {
        self.segments.iter().flat_map(|s| s.words.iter().cloned()).collect()
    }
}

fn norm_words(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word-bag similarity of two lyric passages (0..1).
pub fn lyric_similarity(a: &str, b: &str) -> f32 {
    let wa = norm_words(a);
    let wb = norm_words(b);
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    let sa: std::collections::BTreeSet<&String> = wa.iter().collect();
    let sb: std::collections::BTreeSet<&String> = wb.iter().collect();
    let inter = sa.intersection(&sb).count() as f32;
    let uni = sa.union(&sb).count() as f32;
    inter / uni.max(1.0)
}

// ------------------------------------------------------------ harmony

/// Diatonic chord candidates of a key as roman numerals (minor keys also get
/// the major V and the bVII colour).
pub fn candidate_chords(key_pc: u8, scale: &str) -> Result<Vec<Chord>> {
    let romans: &[&str] = if scale == "major" {
        &["I", "ii", "iii", "IV", "V", "vi"]
    } else {
        &["i", "III", "iv", "v", "V", "VI", "VII"]
    };
    let sc = if scale == "major" { "major" } else { "minor" };
    romans.iter().map(|r| theory::parse_chord(r, key_pc, sc)).collect()
}

fn functional_bonus(from: &str, to: &str) -> f32 {
    let f = from.to_lowercase();
    let t = to.to_lowercase();
    match (f.as_str(), t.as_str()) {
        ("v", "i") | ("v", "vi") | ("iv", "v") | ("ii", "v") | ("iv", "i") | ("i", "iv") | ("i", "v")
        | ("vi", "iv") | ("vi", "ii") | ("i", "vi") | ("iii", "vi") | ("vii", "i") | ("vi", "v")
        | ("iii", "iv") | ("vi", "vii") | ("vii", "iii") | ("i", "vii") => 0.25,
        _ => 0.0,
    }
}

/// One chord per slot (`slot_beats` beats, e.g. 2 = half bars) chosen by a
/// Viterbi pass: sung chord tones on strong beats score, clashing tones cost,
/// functional moves and phrase-level holding are preferred.
/// `notes_beats`: (start_beat, len_beats, pitch) on the song grid, beat 0 = the
/// first slot.
pub fn harmonize(notes_beats: &[(f32, f32, u8)], n_slots: usize, slot_beats: f32, key_pc: u8, scale: &str) -> Result<Vec<Chord>> {
    let cands = candidate_chords(key_pc, scale)?;
    let scale_iv = theory::scale_intervals(if scale == "major" { "major" } else { "minor" })?;
    let in_scale = |pc: u8| scale_iv.iter().any(|i| (key_pc + i) % 12 == pc);
    let nc = cands.len();
    let mut emis = vec![vec![0.0f32; nc]; n_slots];
    for (s, row) in emis.iter_mut().enumerate() {
        let t0 = s as f32 * slot_beats;
        let t1 = t0 + slot_beats;
        let mut tot = 0.0;
        for &(st, len, p) in notes_beats {
            let a = st.max(t0);
            let b = (st + len).min(t1);
            if b <= a {
                continue;
            }
            let pos = st - t0;
            let strong = if st >= t0 && pos < 0.25 { 1.6 } else if (st.fract()) < 0.2 { 1.0 } else { 0.6 };
            let w = (b - a).min(2.0) * strong;
            tot += w;
            let pc = p % 12;
            for (ci, c) in cands.iter().enumerate() {
                let tones: Vec<u8> = c.intervals.iter().take(3).map(|i| (c.root_pc + i) % 12).collect();
                let primary = matches!(c.label.as_str(), "I" | "IV" | "V" | "i" | "iv");
                row[ci] += w * if tones.contains(&pc) {
                    (if pc == c.root_pc { 1.05 } else { 1.0 }) + if primary { 0.1 } else { 0.0 }
                } else if in_scale(pc) {
                    -0.35
                } else {
                    -0.9
                };
            }
        }
        if tot > 0.0 {
            for v in row.iter_mut() {
                *v /= tot;
            }
        }
    }
    let tonic = 0usize;
    let mut score = vec![vec![f32::NEG_INFINITY; nc]; n_slots];
    let mut back = vec![vec![0usize; nc]; n_slots];
    for c in 0..nc {
        score[0][c] = emis[0][c] + if c == tonic { 0.4 } else { 0.0 };
    }
    let slots_per_bar = (4.0 / slot_beats).round().max(1.0) as usize;
    for s in 1..n_slots {
        let mid_bar = s % slots_per_bar != 0;
        for c in 0..nc {
            let mut best = (f32::NEG_INFINITY, 0usize);
            for p in 0..nc {
                let mut tr = if p == c {
                    if mid_bar { 0.15 } else { -0.05 }
                } else {
                    functional_bonus(&cands[p].label, &cands[c].label) - if mid_bar { 0.3 } else { 0.0 }
                };
                // the minor v is a colour, the V a cadence
                if cands[c].label == "v" {
                    tr -= 0.1;
                }
                let v = score[s - 1][p] + tr;
                if v > best.0 {
                    best = (v, p);
                }
            }
            score[s][c] = best.0 + emis[s][c];
            back[s][c] = best.1;
        }
    }
    let mut path = vec![0usize; n_slots];
    if n_slots > 0 {
        let last = n_slots - 1;
        path[last] = (0..nc)
            .max_by(|&a, &b| score[last][a].partial_cmp(&score[last][b]).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(0);
        for s in (1..n_slots).rev() {
            path[s - 1] = back[s][path[s]];
        }
    }
    Ok(path.into_iter().map(|i| cands[i].clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::SR;

    /// A synthetic sung line: sine notes with vibrato and syllable gaps.
    fn sing(notes: &[(f32, f32, f32)], len: f32) -> Vec<f32> {
        let mut out = vec![0.0f32; (len * SR) as usize];
        for &(t, d, midi) in notes {
            let f0 = 440.0 * 2f32.powf((midi - 69.0) / 12.0);
            let s0 = (t * SR) as usize;
            let n = (d * SR) as usize;
            let mut ph = 0.0f32;
            for i in 0..n {
                let tt = i as f32 / SR;
                let f = f0 * (1.0 + 0.004 * (tt * 5.5 * std::f32::consts::TAU).sin());
                ph += f / SR;
                let env = (tt / 0.03).min(1.0) * ((d - tt) / 0.05).clamp(0.0, 1.0);
                let x = (ph * std::f32::consts::TAU).sin() + 0.4 * (2.0 * ph * std::f32::consts::TAU).sin() + 0.2 * (3.0 * ph * std::f32::consts::TAU).sin();
                if let Some(o) = out.get_mut(s0 + i) {
                    *o += 0.25 * env * x;
                }
            }
        }
        out
    }

    fn twinkle(bpm: f32, offset: f32) -> (Vec<(f32, f32, f32)>, f32) {
        // C C G G A A G- F F E E D D C-
        let mel = [60, 60, 67, 67, 69, 69, 67, 67, 65, 65, 64, 64, 62, 62, 60, 60];
        let beat = 60.0 / bpm;
        let mut v = Vec::new();
        let mut i = 0;
        for rep in 0..2 {
            for (k, &m) in mel.iter().enumerate() {
                if k % 8 == 7 {
                    continue; // the held half note
                }
                let held = k % 8 == 6;
                let t = offset + (rep * 16 + k) as f32 * beat;
                v.push((t, beat * if held { 1.85 } else { 0.85 }, m as f32));
                i += 1;
            }
        }
        let _ = i;
        (v, offset + 32.0 * beat + 1.0)
    }

    #[test]
    fn hears_notes_key_and_tempo_of_a_sung_line() {
        let (mel, len) = twinkle(100.0, 0.7);
        let x = sing(&mel, len);
        let a = analyze(&x, SR, &[], None);
        assert!(a.notes.len() >= 24 && a.notes.len() <= 34, "notes {}", a.notes.len());
        let pitches: Vec<u8> = a.notes.iter().map(|n| n.pitch).collect();
        assert!(pitches.contains(&60) && pitches.contains(&67) && pitches.contains(&69), "{pitches:?}");
        assert_eq!(a.key_root, "C", "key {} {}", a.key_root, a.scale);
        assert_eq!(a.scale, "major");
        assert!((a.tempo.bpm - 100.0).abs() < 2.0, "bpm {}", a.tempo.bpm);
        let period = 0.6;
        let d = ((a.tempo.downbeat - 0.7) / period).rem_euclid(4.0);
        assert!(d < 0.15 || d > 3.85, "downbeat {} phase {}", a.tempo.downbeat, a.tempo.phase);
    }

    #[test]
    fn harmonizes_twinkle_with_tonic_subdominant_dominant() {
        let mel = [60u8, 60, 67, 67, 69, 69, 67, 67, 65, 65, 64, 64, 62, 62, 60, 60];
        let nb: Vec<(f32, f32, u8)> = mel.iter().enumerate().filter(|(k, _)| k % 8 != 7).map(|(k, &p)| (k as f32, if k % 8 == 6 { 2.0 } else { 1.0 }, p)).collect();
        let ch = harmonize(&nb, 8, 2.0, 0, "major").unwrap();
        let labels: Vec<&str> = ch.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels[0], "I");
        assert_eq!(labels[2], "IV", "{labels:?}");
        assert_eq!(labels[6], "V", "{labels:?}");
        assert_eq!(labels[7], "I", "{labels:?}");
    }

    #[test]
    fn warp_pins_drifting_phrases_to_bar_lines() {
        // a singer who slows down: phrases 2 bars long at 100 BPM, drifting +3% per phrase
        let mut t = 0.5f32;
        let mut anchors = vec![];
        for i in 0..8 {
            anchors.push(t);
            t += 4.8 * (1.0 + 0.03 * i as f32 * 0.5);
        }
        let w = fit_warp(&anchors, 100.0);
        for (k, a) in w.anchors.iter().enumerate() {
            assert!((a.1 % 4.0).abs() < 1e-3, "anchor {k} at beat {}", a.1);
        }
        assert!(w.anchors.len() >= 7);
        let b = w.beat_at(anchors[3]);
        assert!((b - b.round()).abs() < 1e-3);
        assert!((w.time_at_beat(w.beat_at(7.7)) - 7.7).abs() < 1e-3);
    }

    #[test]
    fn wsola_keeps_pitch_and_hits_the_length() {
        let x: Vec<f32> = (0..(SR as usize * 2)).map(|i| (i as f32 * 330.0 / SR * std::f32::consts::TAU).sin() * 0.5).collect();
        let y = warp_audio(&x, SR, 2.4, |o| o / 1.2);
        assert_eq!(y.len(), (2.4 * SR) as usize);
        let f = crate::sc_dsp::yin(&y[40000..42048], SR, 60.0, 1000.0, 0.15).unwrap();
        assert!((f - 330.0).abs() < 3.0, "pitch {f}");
        let rms = (y[20000..80000].iter().map(|v| v * v).sum::<f32>() / 60000.0).sqrt();
        assert!(rms > 0.3, "no holes: rms {rms}");
    }

    #[test]
    fn autotune_pulls_a_flat_singer_into_key() {
        // C major line sung 40 cents flat
        let mel: Vec<(f32, f32, f32)> = [60.0, 62.0, 64.0, 65.0, 67.0, 65.0, 64.0, 62.0]
            .iter()
            .enumerate()
            .map(|(i, m)| (0.3 + i as f32 * 0.5, 0.42, m - 0.4))
            .collect();
        let x = sing(&mel, 4.8);
        let sc = theory::scale_intervals("major").unwrap();
        let (y, st) = autotune(&x, SR, 0, sc, 1.0, 0.0, 30.0);
        assert_eq!(y.len(), x.len());
        assert!(st.off_key_cents_before > 30.0, "{st:?}");
        assert!(st.off_key_cents_after < 12.0, "{st:?}");
        // no dropouts: the tuned take keeps its level
        let rms = |v: &[f32]| (v.iter().map(|s| s * s).sum::<f32>() / v.len() as f32).sqrt();
        assert!(rms(&y) > rms(&x) * 0.8);
    }

    #[test]
    fn lyric_similarity_finds_the_repeat() {
        assert!(lyric_similarity("Twinkle twinkle little star", "twinkle, twinkle little star!") > 0.9);
        assert!(lyric_similarity("Up above the world so high", "When the blazing sun is gone") < 0.2);
    }
}
