//! Cover/song pipeline: a finished song around one sung vocal take, in minutes.
//!
//! The steps (each also its own tool in `tools_cover`):
//! 1. `identify_song`: transcribe the take, search the lyric lines, agree on a
//!    title; `fetch_reference` downloads that record for ANALYSIS ONLY (it is
//!    never placed in a project, never committed, and deleted afterwards).
//! 2. `analyze_song`: beat grid, downbeats, key, chords per half bar, sections,
//!    the drum pattern per section (kick / snare / hat per 16th, from the
//!    percussive part), bass and comping rhythm, per-role band balance.
//! 3. take timing: did the singer sing along to the record (constant offset,
//!    found by chroma + envelope cross-correlation)? Then the record's grid is
//!    used as is. Otherwise the grid is fitted to the singer: his tempo and bar
//!    lines, his key, the record's chord loops transposed to his key. His take
//!    is never stretched or tuned.
//! 4. arrangement on the grid, real recorded instruments per role (installed
//!    catalog first, zones pruned to the notes used), the raw lead chain, a
//!    narrow duck on the music only while he sings, levels matched from one
//!    stems render, master to ~-10 LUFS / -1 dBTP, then an automatic QA report.
//!
//! Lessons from the Sweater Weather cover that are code here: the beat must sit
//! under the voice (presence >= +8 dB at 1-4 kHz inside phrases, by level and a
//! dynamic duck, never by broad static cuts that bury the guitar), real
//! instruments over synths, and the lead stays raw (HPF, light de-ess, 2.5:1).

use crate::dsp::{Fft, SR};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

pub const NFFT: usize = 2048;
pub const HOP: usize = 512;

pub fn fps() -> f32 {
    SR / HOP as f32
}

fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos()).collect()
}

fn bin_of(hz: f32) -> usize {
    ((hz * NFFT as f32 / SR).round() as usize).min(NFFT / 2)
}

fn db(x: f32) -> f32 {
    10.0 * x.max(1e-12).log10()
}

fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

fn r2(x: f32) -> f32 {
    (x * 100.0).round() / 100.0
}

/// Band summaries of a magnitude spectrogram split into harmonic and
/// percussive parts (median filtering across time / across frequency).
pub struct Features {
    pub frames: usize,
    /// percussive energy per frame: kick 30-120, snare 250-400 + 700-2k, hat 6-15k, full
    pub p_kick: Vec<f32>,
    pub p_snare: Vec<f32>,
    pub p_hat: Vec<f32>,
    pub p_full: Vec<f32>,
    /// harmonic energy per frame: bass 30-200, comp 250-5k, voice 300-3k, full
    pub h_bass: Vec<f32>,
    pub h_comp: Vec<f32>,
    pub h_full: Vec<f32>,
    /// total energy per frame
    pub total: Vec<f32>,
    /// harmonic chroma (60 Hz-5 kHz) and bass chroma (30-200 Hz) per frame
    pub chroma: Vec<[f32; 12]>,
    pub bass_chroma: Vec<[f32; 12]>,
    /// spectral-flux onset strength over all bins
    pub onset: Vec<f32>,
    /// energy per octave band (TONE_EDGES) per frame: the tonal balance a mix is matched on
    pub tone: Vec<[f32; 9]>,
}

/// Octave band edges (Hz) of `Features::tone`.
pub const TONE_EDGES: [f32; 10] = [40.0, 80.0, 160.0, 320.0, 640.0, 1250.0, 2500.0, 5000.0, 10000.0, 16000.0];

/// Magnitude STFT (mono) -> harmonic/percussive band features. Memory is one
/// magnitude matrix plus its two median-filtered copies, built bin-band by
/// bin-band.
pub fn features(x: &[f32]) -> Features {
    let bins = NFFT / 2 + 1;
    let frames = x.len() / HOP + 1;
    let w = hann(NFFT);
    let fft = Fft::new(NFFT);
    let mut mag = vec![0.0f32; frames * bins];
    let mut re = vec![0.0f32; NFFT];
    let mut im = vec![0.0f32; NFFT];
    for f in 0..frames {
        for k in 0..NFFT {
            let i = (f * HOP + k) as isize - (NFFT / 2) as isize;
            re[k] = if i >= 0 && (i as usize) < x.len() { x[i as usize] * w[k] } else { 0.0 };
            im[k] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        let row = &mut mag[f * bins..(f + 1) * bins];
        for k in 0..bins {
            row[k] = (re[k] * re[k] + im[k] * im[k]).sqrt();
        }
    }
    // onset: log flux
    let mut onset = vec![0.0f32; frames];
    for f in 1..frames {
        let (a, b) = (&mag[(f - 1) * bins..f * bins], &mag[f * bins..(f + 1) * bins]);
        let mut s = 0.0;
        for k in 2..bins.min(bin_of(12000.0)) {
            let d = (1.0 + 100.0 * b[k]).ln() - (1.0 + 100.0 * a[k]).ln();
            if d > 0.0 {
                s += d;
            }
        }
        onset[f] = s;
    }
    // harmonic estimate: median over 17 frames per bin; percussive: median over 17 bins per frame
    const MT: usize = 17;
    const MF: usize = 17;
    let hi_bin = bin_of(15000.0).min(bins - 1);
    let mut harm = vec![0.0f32; frames * bins];
    let mut win: Vec<f32> = Vec::with_capacity(MT);
    for k in 0..=hi_bin {
        for f in 0..frames {
            win.clear();
            let a = f.saturating_sub(MT / 2);
            let b = (f + MT / 2 + 1).min(frames);
            for g in a..b {
                win.push(mag[g * bins + k]);
            }
            let m = win.len() / 2;
            win.select_nth_unstable_by(m, |p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
            harm[f * bins + k] = win[m];
        }
    }
    let b30 = bin_of(30.0).max(1);
    let (b120, b150, b200, b250, b300) = (bin_of(120.0), bin_of(150.0), bin_of(200.0), bin_of(250.0), bin_of(300.0));
    let (b1500, b5000, b6000) = (bin_of(1500.0), bin_of(5000.0), bin_of(6000.0));
    let (b400, b700, b2000) = (bin_of(400.0), bin_of(700.0), bin_of(2000.0));
    let _ = (b150, b300, b1500);
    let mut fe = Features {
        frames,
        p_kick: vec![0.0; frames],
        p_snare: vec![0.0; frames],
        p_hat: vec![0.0; frames],
        p_full: vec![0.0; frames],
        h_bass: vec![0.0; frames],
        h_comp: vec![0.0; frames],
        h_full: vec![0.0; frames],
        total: vec![0.0; frames],
        chroma: vec![[0.0; 12]; frames],
        bass_chroma: vec![[0.0; 12]; frames],
        onset,
        tone: vec![[0.0; 9]; frames],
    };
    let tone_band: Vec<i32> = (0..bins)
        .map(|k| {
            let hz = k as f32 * SR / NFFT as f32;
            (0..9).find(|&i| hz >= TONE_EDGES[i] && hz < TONE_EDGES[i + 1]).map(|i| i as i32).unwrap_or(-1)
        })
        .collect();
    let mut perc = vec![0.0f32; bins];
    for f in 0..frames {
        let row = &mag[f * bins..(f + 1) * bins];
        for k in 0..bins {
            let tb = tone_band[k];
            if tb >= 0 {
                fe.tone[f][tb as usize] += row[k] * row[k];
            }
        }
        for k in 0..=hi_bin {
            win.clear();
            let a = k.saturating_sub(MF / 2);
            let b = (k + MF / 2 + 1).min(hi_bin + 1);
            for j in a..b {
                win.push(row[j]);
            }
            let m = win.len() / 2;
            win.select_nth_unstable_by(m, |p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));
            perc[k] = win[m];
        }
        let hrow = &harm[f * bins..(f + 1) * bins];
        for k in b30..=hi_bin {
            let e = row[k] * row[k];
            let (h, p) = (hrow[k] * hrow[k], perc[k] * perc[k]);
            let hm = h / (h + p + 1e-12);
            let he = e * hm;
            let pe = e - he;
            fe.total[f] += e;
            fe.h_full[f] += he;
            fe.p_full[f] += pe;
            if k < b120 {
                fe.p_kick[f] += pe;
            }
            // snare body + crack bands, between the kick's low end and the
            // guitar's pick noise (measured on Sweater Weather's record: the old
            // 150-300 + 1.5-5k band also lit up on every kick and strum)
            if (k >= b250 && k < b400) || (k >= b700 && k < b2000) {
                fe.p_snare[f] += pe;
            }
            if k >= b6000 {
                fe.p_hat[f] += pe;
            }
            if k < b200 {
                fe.h_bass[f] += he;
            }
            if k >= b250 && k < b5000 {
                fe.h_comp[f] += he;
            }
        }
    }
    // chroma from a long window (5.4 Hz bins): a 2048 FFT's low bins are
    // semitones wide and pull every key toward C/F/A
    {
        const N2: usize = 8192;
        let w2 = hann(N2);
        let fft2 = Fft::new(N2);
        let mut re2 = vec![0.0f32; N2];
        let mut im2 = vec![0.0f32; N2];
        let hz_bin = |hz: f32| (hz * N2 as f32 / SR) as usize;
        let (c_lo, c_hi, b_lo, b_hi) = (hz_bin(100.0), hz_bin(5000.0), hz_bin(45.0), hz_bin(250.0));
        let pc2: Vec<i32> = (0..N2 / 2)
            .map(|k| {
                let hz = k as f32 * SR / N2 as f32;
                if hz < 40.0 { -1 } else { ((69.0 + 12.0 * (hz / 440.0).log2()).round() as i32).rem_euclid(12) }
            })
            .collect();
        let g_frames = frames / 4 + 1;
        for g in 0..g_frames {
            let centre = (g * 4 * HOP + 2 * HOP) as isize;
            for k in 0..N2 {
                let i = centre + k as isize - (N2 / 2) as isize;
                re2[k] = if i >= 0 && (i as usize) < x.len() { x[i as usize] * w2[k] } else { 0.0 };
                im2[k] = 0.0;
            }
            fft2.forward(&mut re2, &mut im2);
            let mut cc = [0.0f32; 12];
            let mut bc = [0.0f32; 12];
            for k in b_lo..c_hi {
                let pc = pc2[k];
                if pc < 0 {
                    continue;
                }
                let m = (re2[k] * re2[k] + im2[k] * im2[k]).sqrt();
                if k >= c_lo {
                    cc[pc as usize] += m;
                }
                if k < b_hi {
                    bc[pc as usize] += m;
                }
            }
            for f in g * 4..(g * 4 + 4).min(frames) {
                fe.chroma[f] = cc;
                fe.bass_chroma[f] = bc;
            }
        }
    }
    fe
}

/// Positive log-difference (flux) of a band energy curve.
fn flux(e: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0f32; e.len()];
    let floor = {
        let mut v: Vec<f32> = e.iter().copied().filter(|x| *x > 0.0).collect();
        if v.is_empty() {
            1e-9
        } else {
            let m = v.len() / 2;
            v.select_nth_unstable_by(m, |a, b| a.partial_cmp(b).unwrap());
            v[m].max(1e-12) * 0.05
        }
    };
    for i in 1..e.len() {
        let d = (e[i] + floor).ln() - (e[i - 1] + floor).ln();
        out[i] = d.max(0.0);
    }
    out
}

/// Max of `v` within +-`w` frames of `pos`.
fn peak_near(v: &[f32], pos: f32, w: usize) -> f32 {
    if pos < 0.0 {
        return 0.0;
    }
    let c = pos.round() as isize;
    let mut m = 0.0f32;
    for i in (c - w as isize)..=(c + w as isize) {
        if i >= 0 && (i as usize) < v.len() {
            m = m.max(v[i as usize]);
        }
    }
    m
}

/// Constant-tempo beat grid from an onset curve: (bpm, first beat time s, confidence).
pub fn beat_grid(onset: &[f32], bpm_lo: f32, bpm_hi: f32, prior_bpm: f32) -> (f32, f32, f32) {
    let fps = fps();
    let n = onset.len();
    if n < 200 {
        return (120.0, 0.0, 0.0);
    }
    let mean = onset.iter().sum::<f32>() / n as f32;
    let e: Vec<f32> = onset.iter().map(|v| (v - mean).max(0.0)).collect();
    let ac = |lag: f32| -> f32 {
        let l0 = lag.floor() as usize;
        let f = lag - l0 as f32;
        let mut s = 0.0;
        for i in 0..n.saturating_sub(l0 + 1) {
            s += e[i] * (e[i + l0] * (1.0 - f) + e[i + l0 + 1] * f);
        }
        s
    };
    let lag_for = |bpm: f32| 60.0 * fps / bpm;
    let mut cands: Vec<(f32, f32)> = Vec::new();
    let mut bpm = bpm_lo;
    while bpm <= bpm_hi {
        let s = ac(lag_for(bpm)) + 0.5 * ac(lag_for(bpm / 2.0)) + 0.5 * ac(2.0 * lag_for(bpm)) + 0.25 * ac(lag_for(bpm * 2.0));
        let w = (-0.5 * ((bpm / prior_bpm).log2() / 0.8).powi(2)).exp();
        cands.push((bpm, s * w));
        bpm += 0.5;
    }
    let mean_sc = cands.iter().map(|c| c.1).sum::<f32>() / cands.len() as f32;
    let best = cands.iter().cloned().fold((120.0, f32::MIN), |a, b| if b.1 > a.1 { b } else { a });
    let conf = ((best.1 - mean_sc) / best.1.abs().max(1e-9)).clamp(0.0, 1.0);
    // fine tempo + phase: comb sum over the whole curve
    let mut top = (best.0, 0.0f32, f32::MIN);
    let mut b = best.0 - 1.0;
    while b <= best.0 + 1.0 {
        let period = lag_for(b);
        let steps = 48usize;
        for ph in 0..steps {
            let phase = period * ph as f32 / steps as f32;
            let mut s = 0.0;
            let mut pos = phase;
            while pos < n as f32 {
                s += peak_near(&e, pos, 1);
                pos += period;
            }
            if s > top.2 {
                top = (b, phase, s);
            }
        }
        b += 0.05;
    }
    (top.0, top.1 / fps, conf)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DrumPattern {
    /// strength 0..1 per 16th step of one bar
    pub kick: Vec<f32>,
    pub snare: Vec<f32>,
    pub hat: Vec<f32>,
    /// how present the drums are in this section (0 = none, 1 = full)
    pub presence: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefSection {
    pub name: String,
    pub kind: String,
    /// bars from the first downbeat
    pub bar0: usize,
    pub bars: usize,
    pub energy_db: f32,
    pub drums: DrumPattern,
    pub bass_steps: Vec<f32>,
    pub comp_steps: Vec<f32>,
    /// comping hits per bar
    pub comp_density: f32,
    /// band levels (dB re the section's total): percussive, harmonic bass, harmonic comp
    pub perc_db: f32,
    pub bass_db: f32,
    pub comp_db: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SongAnalysis {
    pub source: String,
    pub duration_s: f32,
    pub bpm: f32,
    pub tempo_confidence: f32,
    /// time of the first downbeat (s) and of each bar line
    pub downbeat_s: f32,
    pub bar_times: Vec<f32>,
    pub key_root: String,
    pub scale: String,
    pub key_confidence: f32,
    /// two chord names per bar (first half, second half), "N" = none
    pub chords: Vec<[String; 2]>,
    pub sections: Vec<RefSection>,
    /// "guitar" | "piano": what the comping sounds most like
    pub comp_timbre: String,
    pub comp_centroid_hz: f32,
    pub comp_pluck: f32,
}

const PCN: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

pub fn pc_name(pc: u8) -> &'static str {
    PCN[(pc % 12) as usize]
}

fn cos12(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let mut d = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..12 {
        d += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    d / (na * nb).sqrt().max(1e-9)
}

fn sum_chroma(c: &[[f32; 12]], a: usize, b: usize) -> [f32; 12] {
    let mut s = [0.0f32; 12];
    for f in a..b.min(c.len()) {
        // per-frame normalisation so loud frames cannot outvote the rest
        let m = c[f].iter().cloned().fold(0.0f32, f32::max);
        if m > 1e-6 {
            for i in 0..12 {
                s[i] += c[f][i] / m;
            }
        }
    }
    s
}

/// Best triad for a chroma vector: (root pc, minor?, score).
pub fn best_triad(c: &[f32; 12], bass: &[f32; 12], key: Option<(u8, bool)>) -> (u8, bool, f32) {
    let bm = bass.iter().cloned().fold(0.0f32, f32::max).max(1e-9);
    let mut best = (0u8, false, f32::MIN);
    for r in 0..12u8 {
        for minor in [false, true] {
            let mut t = [0.0f32; 12];
            t[r as usize] = 1.0;
            t[((r + if minor { 3 } else { 4 }) % 12) as usize] = 0.8;
            t[((r + 7) % 12) as usize] = 0.8;
            let mut s = cos12(c, &t) + 0.15 * bass[r as usize] / bm;
            if let Some((kpc, kmin)) = key {
                // diatonic triads of the key (relative major)
                let rel = if kmin { (kpc + 3) % 12 } else { kpc };
                let deg = (r + 12 - rel) % 12;
                let diatonic = matches!((deg, minor), (0, false) | (5, false) | (7, false) | (2, true) | (4, true) | (9, true));
                if diatonic {
                    s += 0.06;
                }
            }
            if s > best.2 {
                best = (r, minor, s);
            }
        }
    }
    best
}

pub fn chord_name(root: u8, minor: bool) -> String {
    format!("{}{}", pc_name(root), if minor { "m" } else { "" })
}

/// Parse "D#m" / "Eb" / "N" -> Some((root, minor)).
pub fn parse_chord_name(s: &str) -> Option<(u8, bool)> {
    if s == "N" || s.is_empty() {
        return None;
    }
    let minor = s.ends_with('m');
    let root = s.trim_end_matches('m');
    crate::theory::pitch_class(root).ok().map(|pc| (pc, minor))
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let m = v.len() / 2;
    v.select_nth_unstable_by(m, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[m]
}

fn mean(v: &[f32]) -> f32 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f32>() / v.len() as f32
    }
}

/// Analyse a reference recording (mono samples at SR).
pub fn analyze_song(x: &[f32], source: &str) -> Result<SongAnalysis> {
    if x.len() < (SR * 10.0) as usize {
        bail!("the reference is shorter than 10 seconds");
    }
    let fe = features(x);
    let fps = fps();
    let dur = x.len() as f32 / SR;
    let (bpm, beat0, tconf) = beat_grid(&fe.onset, 70.0, 180.0, 110.0);
    let beat_s = 60.0 / bpm;
    let nbeats = ((dur - beat0) / beat_s).floor().max(0.0) as usize;
    let beat_t = |k: usize| beat0 + k as f32 * beat_s;
    let kflux = flux(&fe.p_kick);
    let sflux = flux(&fe.p_snare);
    let hflux = flux(&fe.p_hat);
    let bflux = flux(&fe.h_bass);
    let cflux = flux(&fe.h_comp);
    // downbeat phase: chord changes and kicks on beat 1, snares on 2 and 4
    let mut chg = vec![0.0f32; nbeats];
    for k in 1..nbeats {
        let a = sum_chroma(&fe.chroma, (beat_t(k - 1) * fps) as usize, (beat_t(k) * fps) as usize);
        let b = sum_chroma(&fe.chroma, (beat_t(k) * fps) as usize, (beat_t(k + 1) * fps) as usize);
        chg[k] = 1.0 - cos12(&a, &b);
    }
    let norm = |v: Vec<f32>| -> Vec<f32> {
        let m = mean(&v).max(1e-9);
        v.iter().map(|x| x / m).collect()
    };
    let kb = norm((0..nbeats).map(|k| peak_near(&kflux, beat_t(k) * fps, 2)).collect());
    let sb = norm((0..nbeats).map(|k| peak_near(&sflux, beat_t(k) * fps, 2)).collect());
    let cb = norm(chg);
    let mut best_o = (0usize, f32::MIN);
    for o in 0..4usize {
        let mut s = 0.0;
        for k in 0..nbeats {
            let pos = (k + 4 - o) % 4;
            s += match pos {
                0 => cb[k] + 0.6 * kb[k] - 0.3 * sb[k],
                2 => 0.3 * kb[k] - 0.3 * sb[k] + 0.3 * cb[k],
                _ => 0.3 * sb[k],
            };
        }
        if s > best_o.1 {
            best_o = (o, s);
        }
    }
    let bar_s = 4.0 * beat_s;
    let mut first_db = beat_t(best_o.0);
    if first_db - bar_s >= -0.05 {
        first_db -= bar_s;
    }
    let first_db = first_db.max(0.0);
    // phase refinement: the comb lands within a few frames; the record's own
    // strong kick/snare hits say where its beats really are (median offset of
    // the hits within 70 ms of a beat line). Measured on Sweater Weather: the
    // comb grid sat ~40 ms after the drums, so every hit of ours was late.
    let first_db = {
        let c: Vec<f32> = kflux.iter().zip(sflux.iter()).map(|(k, s)| k + s).collect();
        let w = (0.07 * fps) as isize;
        let mut cand: Vec<(f32, f32)> = Vec::new();
        let mut t = first_db;
        while t < dur {
            let ci = (t * fps).round() as isize;
            let mut best = (0isize, 0.0f32);
            for i in (ci - w).max(1)..=(ci + w).min(c.len() as isize - 1) {
                if c[i as usize] > best.1 {
                    best = (i, c[i as usize]);
                }
            }
            if best.1 > 0.0 {
                cand.push((best.0 as f32 / fps - t, best.1));
            }
            t += beat_s;
        }
        let mut vals: Vec<f32> = cand.iter().map(|x| x.1).collect();
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let thr = vals.get(vals.len() * 7 / 10).copied().unwrap_or(f32::MAX);
        let errs: Vec<f32> = cand.iter().filter(|x| x.1 >= thr).map(|x| x.0).collect();
        if errs.len() >= 16 {
            let d = median(errs).clamp(-0.06, 0.06);
            let f = first_db + d;
            if f < 0.0 { f + bar_s } else { f }
        } else {
            first_db
        }
    };
    let nbars = ((dur - first_db) / bar_s).floor().max(1.0) as usize;
    let bar_times: Vec<f32> = (0..nbars).map(|b| first_db + b as f32 * bar_s).collect();
    let fr = |t: f32| ((t * fps).round().max(0.0) as usize).min(fe.frames);
    // key
    let mut tot = [0.0f32; 12];
    let all = sum_chroma(&fe.chroma, 0, fe.frames);
    for i in 0..12 {
        tot[i] = all[i];
    }
    let keys = crate::midi_ops::key_from_chroma(&tot);
    let (kpc, kscale, kconf) = keys.first().copied().unwrap_or((0, "major", 0.0));
    let kmin = kscale == "minor";
    // chords per half bar
    let mut chords: Vec<[String; 2]> = Vec::with_capacity(nbars);
    let hmean = mean(&fe.h_full).max(1e-12);
    for b in 0..nbars {
        let t0 = bar_times[b];
        let mut pair = [String::from("N"), String::from("N")];
        for h in 0..2 {
            let a = fr(t0 + h as f32 * bar_s / 2.0);
            let z = fr(t0 + (h + 1) as f32 * bar_s / 2.0);
            let en = mean(&fe.h_full[a..z.max(a + 1).min(fe.frames)]);
            if en < hmean * 0.02 {
                continue;
            }
            let c = sum_chroma(&fe.chroma, a, z);
            let bc = sum_chroma(&fe.bass_chroma, a, z);
            let (r, m, _) = best_triad(&c, &bc, Some((kpc, kmin)));
            pair[h] = chord_name(r, m);
        }
        chords.push(pair);
    }
    // per bar levels
    let bar_db = |v: &[f32], b: usize| -> f32 {
        let a = fr(bar_times[b]);
        let z = fr(bar_times[b] + bar_s).max(a + 1).min(v.len());
        db(mean(&v[a..z]))
    };
    // step strengths per bar for a flux curve: 16 values
    let step_vals = |v: &[f32], b: usize| -> Vec<f32> {
        (0..16)
            .map(|s| peak_near(v, (bar_times[b] + s as f32 * bar_s / 16.0) * fps, 2))
            .collect()
    };
    // 4-bar blocks merged into sections of similar level and harmony
    #[derive(Clone)]
    struct Blk {
        b0: usize,
        n: usize,
        tot: f32,
        perc: f32,
        sig: Vec<String>,
    }
    let mut blocks: Vec<Blk> = Vec::new();
    let mut b = 0;
    while b < nbars {
        let n = 4.min(nbars - b);
        let tot = mean(&(b..b + n).map(|i| bar_db(&fe.total, i)).collect::<Vec<_>>());
        let perc = mean(&(b..b + n).map(|i| bar_db(&fe.p_full, i)).collect::<Vec<_>>());
        let sig = (b..b + n).map(|i| chords[i][0].clone()).collect();
        blocks.push(Blk { b0: b, n, tot, perc, sig });
        b += n;
    }
    let sim = |a: &Vec<String>, b: &Vec<String>| -> f32 {
        let n = a.len().min(b.len()).max(1);
        a.iter().zip(b.iter()).filter(|(x, y)| x == y).count() as f32 / n as f32
    };
    let mut groups: Vec<Vec<Blk>> = Vec::new();
    for bl in blocks {
        let join = match groups.last() {
            Some(g) => {
                let last = g.last().unwrap();
                let len: usize = g.iter().map(|x| x.n).sum();
                (bl.tot - last.tot).abs() < 2.5 && (bl.perc - last.perc).abs() < 3.0 && (sim(&bl.sig, &last.sig) >= 0.5 || bl.n < 4) && len < 16
            }
            None => false,
        };
        if join {
            groups.last_mut().unwrap().push(bl);
        } else {
            groups.push(vec![bl]);
        }
    }
    let max_tot = groups.iter().map(|g| mean(&g.iter().map(|x| x.tot).collect::<Vec<_>>())).fold(f32::MIN, f32::max);
    let max_perc = groups.iter().map(|g| mean(&g.iter().map(|x| x.perc).collect::<Vec<_>>())).fold(f32::MIN, f32::max);
    let tots: Vec<f32> = groups.iter().map(|g| mean(&g.iter().map(|x| x.tot).collect::<Vec<_>>())).collect();
    let med_tot = median(tots.clone());
    let ng = groups.len();
    let mut sections: Vec<RefSection> = Vec::new();
    let mut counts = std::collections::BTreeMap::new();
    for (gi, g) in groups.iter().enumerate() {
        let b0 = g[0].b0;
        let bars: usize = g.iter().map(|x| x.n).sum();
        let tot = tots[gi];
        let perc = mean(&g.iter().map(|x| x.perc).collect::<Vec<_>>());
        let drums_on = perc > max_perc - 12.0;
        let kind = if !drums_on || tot < max_tot - 9.0 {
            if gi == 0 {
                "intro"
            } else if gi + 1 == ng {
                "outro"
            } else {
                "break"
            }
        } else if tot >= med_tot + 1.0 || tot >= max_tot - 1.5 {
            "chorus"
        } else {
            "verse"
        };
        let c = counts.entry(kind).or_insert(0);
        *c += 1;
        // patterns: mean step strength over the section's bars, normalised per instrument
        let avg = |v: &[f32]| -> Vec<f32> {
            let mut s = vec![0.0f32; 16];
            for bb in b0..b0 + bars {
                for (i, x) in step_vals(v, bb).iter().enumerate() {
                    s[i] += x / bars as f32;
                }
            }
            s
        };
        let normed = |s: Vec<f32>, global: f32| -> Vec<f32> {
            let m = s.iter().cloned().fold(0.0f32, f32::max);
            // too weak against the whole song: absent here
            if m < global * 0.35 {
                return vec![0.0; 16];
            }
            s.iter().map(|x| r2(x / m.max(1e-9))).collect()
        };
        let gc = global_step_peak(&cflux, &bar_times, bar_s, fps);
        let gb = global_step_peak(&bflux, &bar_times, bar_s, fps);
        let presence = if drums_on { ((perc - (max_perc - 12.0)) / 12.0).clamp(0.2, 1.0) } else { 0.0 };
        // the record's own pattern: per 16th, the mean onset strength over the
        // section's bars, as a z-score against the section's other steps; a step
        // plays when it stands out (no genre priors). Snare and hat are read
        // with the kick (and snare) bleed taken off.
        let (mut kick, mut snare, mut hat) = if drums_on {
            let k = avg(&kflux);
            let s0 = avg(&sflux);
            let h0 = avg(&hflux);
            let s: Vec<f32> = s0.iter().zip(k.iter()).map(|(a, b)| a - 0.5 * b).collect();
            let h: Vec<f32> = h0.iter().zip(s.iter()).map(|(a, b)| a - 0.3 * b.max(0.0)).collect();
            (zhits(&k, 1.0), zhits(&s, 0.7), zhits(&h, 0.45))
        } else {
            (vec![0.0; 16], vec![0.0; 16], vec![0.0; 16])
        };
        if drums_on {
            // a backbeat on the wrong beat means the bar phase is a beat off: turn it
            let sa = snare[4] + snare[12] + kick[0] + kick[8];
            let sb = snare[0] + snare[8] + kick[4] + kick[12];
            if sb > 1.3 * sa && sb > 0.0 {
                for v in [&mut kick, &mut snare, &mut hat] {
                    v.rotate_left(4);
                }
            }
        }
        let drums = DrumPattern { kick, snare, hat, presence: r2(presence) };
        let comp_steps = normed(avg(&cflux), gc);
        let comp_density = comp_steps.iter().filter(|v| **v >= 0.5).count() as f32;
        let lvl = |v: &[f32]| mean(&(b0..b0 + bars).map(|i| bar_db(v, i)).collect::<Vec<_>>()) - tot;
        sections.push(RefSection {
            name: format!("{}{}", kind, c),
            kind: kind.to_string(),
            bar0: b0,
            bars,
            energy_db: r1(tot),
            drums,
            bass_steps: { let b = zhits(&avg(&bflux), 0.8); if normed(avg(&bflux), gb).iter().all(|v| *v == 0.0) { vec![0.0; 16] } else { b } },
            comp_steps,
            comp_density,
            perc_db: r1(lvl(&fe.p_full)),
            bass_db: r1(lvl(&fe.h_bass)),
            comp_db: r1(lvl(&fe.h_comp)),
        });
    }
    // comping timbre: centroid of the harmonic 250-5k band is not kept per bin,
    // so use the pluck ratio (comp-band flux vs level) and the comp/bass balance
    let pluck = mean(&cflux) / (mean(&fe.h_comp.iter().map(|v| v.sqrt()).collect::<Vec<_>>()).max(1e-9)).max(1e-9);
    let pluck = r2(pluck.min(99.0));
    let centroid = 0.0;
    let comp_timbre = "guitar".to_string();
    Ok(SongAnalysis {
        source: source.to_string(),
        duration_s: r2(dur),
        bpm: r2(bpm),
        tempo_confidence: r2(tconf),
        downbeat_s: (first_db * 1000.0).round() / 1000.0,
        bar_times: bar_times.iter().map(|t| (t * 1000.0).round() / 1000.0).collect(),
        key_root: pc_name(kpc).to_string(),
        scale: kscale.to_string(),
        key_confidence: r2(kconf),
        chords,
        sections,
        comp_timbre,
        comp_centroid_hz: centroid,
        comp_pluck: pluck,
    })
}

/// Steps of a 16-step strength profile that stand out: z-score against the
/// profile's median/spread; a hit's value is 0.5..1 by how far it stands out.
pub fn zhits(m: &[f32], thr: f32) -> Vec<f32> {
    let n = m.len().max(1) as f32;
    let mut s = m.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med = s[s.len() / 2];
    let mu = m.iter().sum::<f32>() / n;
    let sd = (m.iter().map(|x| (x - mu) * (x - mu)).sum::<f32>() / n).sqrt().max(1e-9);
    m.iter().map(|x| { let z = (x - med) / sd; if z >= thr { r2((0.5 + (z - thr) / 4.0).min(1.0)) } else { 0.0 } }).collect()
}

#[allow(dead_code)]
/// Musical prior on a transcribed drum line: the expected hits need modest
/// evidence (and are lifted to a clear hit), anything else needs strong evidence.
fn drum_prior(v: &[f32], expected: &[usize], other_thr: f32) -> Vec<f32> {
    v.iter()
        .enumerate()
        .map(|(i, x)| {
            if expected.contains(&i) {
                if *x >= 0.4 { x.max(0.65) } else { 0.0 }
            } else if *x >= other_thr {
                *x
            } else {
                0.0
            }
        })
        .collect()
}

fn global_step_peak(v: &[f32], bar_times: &[f32], bar_s: f32, fps: f32) -> f32 {
    let mut vals = Vec::new();
    for &t in bar_times {
        for s in 0..16 {
            vals.push(peak_near(v, (t + s as f32 * bar_s / 16.0) * fps, 2));
        }
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if vals.is_empty() {
        return 0.0;
    }
    // the typical strong hit: 90th percentile of all grid steps
    vals[(vals.len() * 9 / 10).min(vals.len() - 1)]
}

// ------------------------------------------------------------------ take vs record

#[derive(Clone, Debug, Serialize)]
pub struct Alignment {
    /// record time (s) at which the take's t=0 plays
    pub lag_s: f32,
    /// how far the best lag stands out (z-score of the similarity curve)
    pub z: f32,
    /// both halves of the take point at the same lag
    pub halves_agree: bool,
    pub synced: bool,
}

fn coarse_chroma(c: &[[f32; 12]], d: usize) -> Vec<[f32; 12]> {
    let n = c.len() / d;
    (0..n).map(|i| sum_chroma(c, i * d, i * d + d)).collect()
}

/// Did the singer sing along to the record? Cross-correlate the take's
/// chroma (weighted by where he sings) against the record's harmonic chroma
/// over every lag, coarse then fine.
pub fn align_take(take: &Features, rec: &Features) -> Alignment {
    const D: usize = 4;
    let ct = coarse_chroma(&take.chroma, D);
    let cr = coarse_chroma(&rec.chroma, D);
    let et: Vec<f32> = (0..ct.len()).map(|i| mean(&take.h_full[i * D..(i * D + D).min(take.frames)])).collect();
    let thr = median(et.clone()) * 1.0 + 1e-12;
    let w: Vec<f32> = et.iter().map(|e| if *e > thr { 1.0 } else { 0.0 }).collect();
    let nt = ct.len() as isize;
    let nr = cr.len() as isize;
    let score = |lag: isize, a: isize, b: isize| -> f32 {
        let mut s = 0.0;
        let mut n = 0.0;
        for i in a..b {
            let j = i + lag;
            if j < 0 || j >= nr || w[i as usize] == 0.0 {
                continue;
            }
            s += cos12(&ct[i as usize], &cr[j as usize]);
            n += 1.0;
        }
        if n < 20.0 {
            f32::NAN
        } else {
            s / n
        }
    };
    let mut curve: Vec<(isize, f32)> = Vec::new();
    let lo = -(nt / 2);
    let hi = nr - nt / 4;
    let mut lag = lo;
    while lag < hi {
        let s = score(lag, 0, nt);
        if s.is_finite() {
            curve.push((lag, s));
        }
        lag += 1;
    }
    if curve.len() < 10 {
        return Alignment { lag_s: 0.0, z: 0.0, halves_agree: false, synced: false };
    }
    let vals: Vec<f32> = curve.iter().map(|c| c.1).collect();
    let m = mean(&vals);
    let sd = (vals.iter().map(|v| (v - m).powi(2)).sum::<f32>() / vals.len() as f32).sqrt().max(1e-6);
    let best = curve.iter().cloned().fold((0isize, f32::MIN), |a, b| if b.1 > a.1 { b } else { a });
    let z = (best.1 - m) / sd;
    // halves: best lag of each half within +-40 coarse frames of the global one
    let half_best = |a: isize, b: isize| -> isize {
        let mut hb = (best.0, f32::MIN);
        for l in (best.0 - 40)..=(best.0 + 40) {
            let s = score(l, a, b);
            if s.is_finite() && s > hb.1 {
                hb = (l, s);
            }
        }
        hb.0
    };
    let h1 = half_best(0, nt / 2);
    let h2 = half_best(nt / 2, nt);
    let agree = (h1 - best.0).abs() <= 2 && (h2 - best.0).abs() <= 2;
    // fine: full-resolution frames around the coarse lag
    let fine_score = |lag: isize| -> f32 {
        let mut s = 0.0;
        let mut n = 0.0;
        let mut i = 0isize;
        while (i as usize) < take.frames {
            let j = i + lag;
            if j >= 0 && (j as usize) < rec.frames && take.h_full[i as usize] > thr {
                s += cos12(&take.chroma[i as usize], &rec.chroma[j as usize]);
                n += 1.0;
            }
            i += 2;
        }
        if n > 0.0 { s / n } else { f32::MIN }
    };
    let c = best.0 * D as isize;
    let mut fb = (c, f32::MIN);
    for l in (c - 8)..=(c + 8) {
        let s = fine_score(l);
        if s > fb.1 {
            fb = (l, s);
        }
    }
    let lag_s = fb.0 as f32 / fps();
    Alignment { lag_s: (lag_s * 1000.0).round() / 1000.0, z: r2(z), halves_agree: agree, synced: z >= 4.5 && agree }
}

/// Seconds of the take where he actually sings: (first, last) above -40 dB of its loudest frames.
pub fn sung_span(take: &Features) -> (f32, f32) {
    let mut v = take.total.clone();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let loud = v[(v.len() * 98 / 100).min(v.len() - 1)];
    let thr = loud * 1e-4;
    let first = take.total.iter().position(|e| *e > thr).unwrap_or(0);
    let last = take.total.iter().rposition(|e| *e > thr).unwrap_or(take.frames.saturating_sub(1));
    (first as f32 / fps(), last as f32 / fps())
}

// ------------------------------------------------------------------ plan

pub type Ch = Option<(u8, bool)>;

#[derive(Clone, Debug, Serialize)]
pub struct PlanSection {
    pub name: String,
    pub kind: String,
    pub bars: usize,
    /// two chords per bar
    pub chords: Vec<[Ch; 2]>,
    pub drums: DrumPattern,
    pub bass: Vec<f32>,
    pub comp: Vec<f32>,
    pub comp_arp: bool,
    /// velocity scale 0..1 from the section's energy
    pub energy: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub mode: String,
    pub bpm: f32,
    pub key_root: String,
    pub scale: String,
    pub sections: Vec<PlanSection>,
    /// where the take sits: song beat of the clip start and seconds skipped into the take
    pub vocal_start_beat: f32,
    pub vocal_offset_s: f32,
    pub comp_role: String,
    /// sync mode: the record's bar where the song starts
    pub rec_bar0: Option<usize>,
    /// lyric sync: the take placed phrase by phrase, [song beat, take offset s, length s]
    pub vocal_clips: Vec<[f32; 3]>,
    /// lyric sync: which stretches of the record the song plays, [song s, record s, length s]
    pub rec_map: Vec<[f32; 3]>,
    pub notes: Vec<String>,
}

fn steps_from(v: &[usize]) -> Vec<f32> {
    let mut s = vec![0.0f32; 16];
    for &i in v {
        s[i % 16] = 1.0;
    }
    s
}

/// Drum / bass / comp defaults for a style word (no reference).
pub fn style_pattern(style: &str, kind: &str) -> (DrumPattern, Vec<f32>, Vec<f32>, bool) {
    let s = style.to_lowercase();
    let quiet = matches!(kind, "intro" | "outro" | "break");
    let (k, sn, h, bass, comp, arp): (Vec<usize>, Vec<usize>, Vec<usize>, Vec<usize>, Vec<usize>, bool) = if s.contains("hip") || s.contains("boom") || s.contains("rap") || s.contains("trap") {
        (vec![0, 7, 10], vec![4, 12], vec![0, 2, 4, 6, 8, 10, 12, 14], vec![0, 7, 10], vec![0, 8], false)
    } else if s.contains("lofi") || s.contains("lo-fi") || s.contains("chill") {
        (vec![0, 10], vec![4, 12], vec![0, 2, 4, 6, 8, 10, 12, 14], vec![0, 10], vec![0, 6, 8], false)
    } else if s.contains("acoustic") || s.contains("ballad") || s.contains("slow") || s.contains("sad") || s.contains("romantic") {
        (vec![0, 10], vec![8], vec![0, 4, 8, 12], vec![0, 8], vec![0, 3, 6, 8, 11, 14], true)
    } else if s.contains("dance") || s.contains("house") || s.contains("edm") {
        (vec![0, 4, 8, 12], vec![4, 12], vec![2, 6, 10, 14], vec![2, 6, 10, 14], vec![0, 3, 6, 10], false)
    } else {
        // pop / rock default
        (vec![0, 8, 10], vec![4, 12], vec![0, 2, 4, 6, 8, 10, 12, 14], vec![0, 6, 8, 14], vec![0, 3, 6, 8, 11, 14], true)
    };
    let drums = if quiet {
        DrumPattern { kick: steps_from(&[0]), snare: vec![0.0; 16], hat: steps_from(&[0, 4, 8, 12]), presence: 0.5 }
    } else {
        DrumPattern { kick: steps_from(&k), snare: steps_from(&sn), hat: steps_from(&h), presence: if kind == "hook" || kind == "chorus" { 1.0 } else { 0.8 } }
    };
    (drums, if quiet { steps_from(&[0]) } else { steps_from(&bass) }, steps_from(&comp), arp)
}

/// The smallest repeating chord loop (in half-bar slots) of a bar range.
pub fn chord_loop(chords: &[[String; 2]], b0: usize, bars: usize) -> Vec<Ch> {
    let slots: Vec<Ch> = (b0..(b0 + bars).min(chords.len())).flat_map(|b| [parse_chord_name(&chords[b][0]), parse_chord_name(&chords[b][1])]).collect();
    if slots.is_empty() {
        return vec![None];
    }
    for p in [2usize, 4, 8, 16] {
        if p > slots.len() {
            break;
        }
        let ok = (0..slots.len()).filter(|&i| slots[i] == slots[i % p]).count() as f32 / slots.len() as f32;
        if ok >= 0.75 {
            return slots[..p].to_vec();
        }
    }
    slots[..slots.len().min(8)].to_vec()
}

pub fn transpose_ch(c: Ch, shift: i32) -> Ch {
    c.map(|(r, m)| (((r as i32 + shift).rem_euclid(12)) as u8, m))
}

fn rel_major(pc: u8, scale: &str) -> u8 {
    if scale == "minor" { (pc + 3) % 12 } else { pc }
}

/// Semitones to move a record's chords into the singer's key (-5..=6).
pub fn key_shift(rec_root: &str, rec_scale: &str, his_root: &str, his_scale: &str) -> i32 {
    let a = crate::theory::pitch_class(rec_root).unwrap_or(0);
    let b = crate::theory::pitch_class(his_root).unwrap_or(0);
    let d = (rel_major(b, his_scale) as i32 - rel_major(a, rec_scale) as i32).rem_euclid(12);
    if d > 6 { d - 12 } else { d }
}

fn chord_has(c: Ch, pc: u8) -> bool {
    match c {
        Some((r, m)) => {
            let iv = (pc + 12 - r) % 12;
            iv == 0 || iv == 7 || iv == if m { 3 } else { 4 }
        }
        None => false,
    }
}

/// Lay a loop over `bars` bars, rotated to fit the sung notes (beats from the section start).
pub fn fit_loop(lp: &[Ch], bars: usize, sung: &[(f32, f32, u8)]) -> Vec<[Ch; 2]> {
    let n = lp.len().max(1);
    let mut best = (0usize, f32::MIN);
    for rot in 0..n {
        let mut s = 0.0;
        for &(b, len, p) in sung {
            let slot = (b / 2.0).floor().max(0.0) as usize;
            let c = lp[(slot + rot) % n];
            s += if chord_has(c, p % 12) { len.min(2.0) } else { -0.5 * len.min(2.0) };
        }
        if s > best.1 {
            best = (rot, s);
        }
    }
    (0..bars).map(|b| [lp[(2 * b + best.0) % n], lp[(2 * b + 1 + best.0) % n]]).collect()
}

// ------------------------------------------------------------------ notes

#[derive(Clone, Debug, Serialize)]
pub struct NoteOut {
    pub start: f32,
    pub len: f32,
    pub pitch: u8,
    pub vel: f32,
}

fn voicing(c: (u8, bool), prev_center: f32) -> Vec<u8> {
    let (r, m) = c;
    let pcs = [r, (r + if m { 3 } else { 4 }) % 12, (r + 7) % 12];
    let mut best: (Vec<u8>, f32) = (vec![], f32::MAX);
    for inv in 0..3 {
        for base in [48u8, 60u8] {
            let mut v: Vec<u8> = Vec::new();
            let mut last = 0u8;
            for i in 0..3 {
                let pc = pcs[(i + inv) % 3];
                let mut p = base + pc;
                while p <= last {
                    p += 12;
                }
                v.push(p);
                last = p;
            }
            if v[0] < 52 || v[2] > 72 {
                continue;
            }
            let center = v.iter().map(|x| *x as f32).sum::<f32>() / 3.0;
            let d = (center - prev_center).abs();
            if d < best.1 {
                best = (v, d);
            }
        }
    }
    if best.0.is_empty() {
        vec![60 + pcs[0], 60 + pcs[1] + if pcs[1] < pcs[0] { 12 } else { 0 }, 60 + pcs[2] + if pcs[2] < pcs[0] { 12 } else { 0 }]
    } else {
        best.0
    }
}

fn hits(steps: &[f32], thr: f32) -> Vec<usize> {
    (0..16).filter(|&i| steps.get(i).copied().unwrap_or(0.0) >= thr).collect()
}

/// Notes for one section: (drums, bass, comp), steps within the section.
pub fn section_notes(s: &PlanSection) -> (Vec<NoteOut>, Vec<NoteOut>, Vec<NoteOut>) {
    let mut drums = Vec::new();
    let mut bass = Vec::new();
    let mut comp = Vec::new();
    let e = s.energy.clamp(0.5, 1.0);
    let pres = s.drums.presence;
    if pres > 0.0 {
        for bar in 0..s.bars {
            let o = (bar * 16) as f32;
            for (steps, pitch, thr, base) in [(&s.drums.kick, 36u8, 0.5f32, 0.62f32), (&s.drums.snare, 38u8, 0.5, 0.62), (&s.drums.hat, 42u8, 0.4, 0.4)] {
                for i in hits(steps, thr) {
                    let v = (base + 0.38 * steps[i]) * e * (0.75 + 0.25 * pres);
                    drums.push(NoteOut { start: o + i as f32, len: 1.0, pitch, vel: v.clamp(0.2, 1.0) });
                }
            }
        }
        if s.kind == "chorus" || s.kind == "hook" {
            drums.push(NoteOut { start: 0.0, len: 4.0, pitch: 49, vel: 0.8 * e });
        }
    }
    let mut center = 62.0f32;
    let bh = {
        let h = hits(&s.bass, 0.5);
        if h.is_empty() { vec![0usize, 8] } else { h }
    };
    let ch = {
        let h = hits(&s.comp, 0.5);
        if h.is_empty() { vec![0usize, 8] } else { h }
    };
    // a half bar the chroma could not name keeps the chord around it (an unnamed
    // half bar left the guitar silent: 0.6 s holes in Sweater Weather's intro)
    let first_ch: Ch = s.chords.iter().flat_map(|c| c.iter().copied()).flatten().next();
    let mut last_ch: Ch = first_ch;
    for bar in 0..s.bars {
        let o = (bar * 16) as f32;
        let chord_at = |step: usize| -> Ch { s.chords.get(bar).map(|c| c[if step < 8 { 0 } else { 1 }]).flatten() };
        // bass: chord root, low register, held to the next hit
        for (k, &i) in bh.iter().enumerate() {
            if let Some(c) = chord_at(i) {
                last_ch = Some(c);
            }
            if let Some((r, _)) = chord_at(i).or(last_ch) {
                let mut p = 36 + r;
                if p > 44 {
                    p -= 12;
                }
                let next = bh.get(k + 1).copied().unwrap_or(16);
                let len = ((next - i) as f32 - 0.25).max(0.75);
                bass.push(NoteOut { start: o + i as f32, len, pitch: p, vel: (0.82 * e).clamp(0.3, 1.0) });
            }
        }
        // comping: arpeggio over the hits, or block chords held to the next hit
        let mut arp_i = 0usize;
        for (k, &i) in ch.iter().enumerate() {
            let c = match chord_at(i).or(last_ch) {
                Some(c) => c,
                None => continue,
            };
            last_ch = Some(c);
            let v = voicing(c, center);
            center = v.iter().map(|x| *x as f32).sum::<f32>() / 3.0;
            let next = ch.get(k + 1).copied().unwrap_or(16);
            let next = if i < 8 && next > 8 && s.chords.get(bar).map(|c| c[0] != c[1]).unwrap_or(false) { 8 } else { next };
            let len = ((next - i) as f32 - 0.1).max(0.5);
            if s.comp_arp {
                let order = [v[0], v[1], v[2], v[0] + 12, v[2], v[1]];
                let p = order[arp_i % order.len()];
                arp_i += 1;
                comp.push(NoteOut { start: o + i as f32, len: len.max(1.5), pitch: p, vel: (0.7 * e).clamp(0.3, 1.0) });
            } else {
                for (j, p) in v.iter().enumerate() {
                    comp.push(NoteOut { start: o + i as f32 + 0.04 * j as f32, len, pitch: *p, vel: (0.66 * e).clamp(0.3, 1.0) });
                }
            }
        }
    }
    (drums, bass, comp)
}

/// Word recall of a sung take in a mix transcript: share of the take's words
/// (multiset) that the mix transcript also contains.
pub fn word_recall(take: &str, mix: &str) -> f32 {
    let norm = |s: &str| -> Vec<String> {
        s.split_whitespace()
            .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
            .filter(|w| !w.is_empty())
            .collect()
    };
    let a = norm(take);
    let b = norm(mix);
    if a.is_empty() {
        return 0.0;
    }
    let mut bag: std::collections::HashMap<String, i32> = std::collections::HashMap::new();
    for w in b {
        *bag.entry(w).or_insert(0) += 1;
    }
    let mut hit = 0;
    for w in &a {
        if let Some(c) = bag.get_mut(w) {
            if *c > 0 {
                *c -= 1;
                hit += 1;
            }
        }
    }
    hit as f32 / a.len() as f32
}

/// Normalised "artist - title" key of a video title, for agreeing across searches.
pub fn title_key(t: &str) -> String {
    let mut s = t.to_lowercase();
    for cut in ["(", "[", "|", " ft.", " feat", " official", " lyrics", " lyric", " audio", " video", " full song", " hd"] {
        if let Some(i) = s.find(cut) {
            if i > 3 {
                s.truncate(i);
            }
        }
    }
    s.chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-').collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click_track(bpm: f32, secs: f32) -> Vec<f32> {
        let n = (secs * SR) as usize;
        let mut x = vec![0.0f32; n];
        let beat = 60.0 / bpm;
        let mut k = 0usize;
        loop {
            let t = 0.25 + k as f32 * beat;
            let i = (t * SR) as usize;
            if i + 2000 >= n {
                break;
            }
            // kick on 1 and 3, snare-ish noise burst on 2 and 4, plus a C-major / A-minor pad
            for j in 0..2000 {
                let env = (-(j as f32) / 300.0).exp();
                let v = if k % 2 == 0 { (2.0 * PI * 60.0 * j as f32 / SR).sin() } else { ((j * 7919 % 101) as f32 / 50.0 - 1.0) * 0.6 };
                x[i + j] += 0.6 * env * v;
            }
            k += 1;
        }
        for i in 0..n {
            let t = i as f32 / SR;
            let bar = ((t - 0.25) / (4.0 * beat)).floor() as i64;
            let f: [f32; 3] = if bar.rem_euclid(2) == 0 { [261.6, 329.6, 392.0] } else { [220.0, 261.6, 329.6] };
            for hz in f {
                x[i] += 0.05 * (2.0 * PI * hz * t).sin();
            }
        }
        x
    }

    #[test]
    fn grid_key_chords_from_a_synthetic_record() {
        let x = click_track(120.0, 30.0);
        let a = analyze_song(&x, "synthetic").unwrap();
        assert!((a.bpm - 120.0).abs() < 1.5, "bpm {}", a.bpm);
        let names: Vec<String> = a.chords.iter().map(|c| c[0].clone()).collect();
        assert!(names.iter().filter(|c| *c == "C" || *c == "Am").count() >= names.len() / 2, "{names:?}");
        assert!(!a.sections.is_empty());
        let s = &a.sections[a.sections.len() / 2];
        assert!(s.drums.kick.iter().any(|v| *v > 0.5), "{:?}", s.drums);
    }

    #[test]
    fn loops_transpose_and_fit() {
        let ch: Vec<[String; 2]> = (0..8).map(|b| { let n = ["D#", "Cm", "Gm", "A#"][b % 4]; [n.to_string(), n.to_string()] }).collect();
        let lp = chord_loop(&ch, 0, 8);
        assert_eq!(lp.len(), 8);
        assert_eq!(key_shift("D#", "major", "C", "minor"), 0);
        assert_eq!(key_shift("D#", "major", "F", "major"), 2);
        let t = transpose_ch(lp[0], 2);
        assert_eq!(t, Some((5, false)));
        // a melody on G over beats 0-2 fits Cm/Eb/Gm, the rotation must start on a chord holding G
        let fitted = fit_loop(&lp, 4, &[(0.0, 2.0, 67), (2.0, 2.0, 67)]);
        assert!(chord_has(fitted[0][0], 7));
    }

    #[test]
    fn notes_for_a_section() {
        let (d, b, c, arp) = style_pattern("pop", "chorus");
        let s = PlanSection { name: "chorus1".into(), kind: "chorus".into(), bars: 2, chords: vec![[Some((0, false)), Some((9, true))]; 2], drums: d, bass: b, comp: c, comp_arp: arp, energy: 1.0 };
        let (dn, bn, cn) = section_notes(&s);
        assert!(dn.iter().any(|n| n.pitch == 36) && dn.iter().any(|n| n.pitch == 38));
        assert!(bn.iter().all(|n| n.pitch >= 32 && n.pitch <= 44), "{bn:?}");
        assert!(cn.iter().all(|n| n.pitch >= 48 && n.pitch <= 86));
        assert!(word_recall("one love two mouth", "one love two mouths") >= 0.74);
        assert_eq!(title_key("The Neighbourhood - Sweater Weather (Official Video)"), "the neighbourhood - sweater weather");
    }
}


// ------------------------------------------------------------------ snare check

/// Snare onset times (s) inside [t0, t1): peaks of the snare-band flux (kick
/// bleed taken off) that reach `rel` of the window's 90th-percentile peak.
pub fn snare_onsets(fe: &Features, t0: f32, t1: f32, rel: f32) -> Vec<f32> {
    let fps = fps();
    let kf = flux(&fe.p_kick);
    let sf = flux(&fe.p_snare);
    let sc: Vec<f32> = sf.iter().zip(kf.iter()).map(|(s, k)| (s - 0.5 * k).max(0.0)).collect();
    let a = ((t0 * fps).max(0.0) as usize).min(sc.len());
    let b = ((t1 * fps).max(0.0) as usize).min(sc.len());
    let mut pk: Vec<(usize, f32)> = Vec::new();
    for i in (a + 3)..b.saturating_sub(3) {
        let v = sc[i];
        if v > 0.0 && (i - 3..=i + 3).all(|j| sc[j] <= v) && (i - 3..i).all(|j| sc[j] < v) {
            pk.push((i, v));
        }
    }
    if pk.is_empty() {
        return vec![];
    }
    let mut vals: Vec<f32> = pk.iter().map(|p| p.1).collect();
    vals.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let thr = rel * vals[(vals.len() * 9 / 10).min(vals.len() - 1)];
    pk.into_iter().filter(|p| p.1 >= thr).map(|p| p.0 as f32 / fps).collect()
}

/// Share of `want` times that have a `got` time within `tol` s.
pub fn onset_recall(want: &[f32], got: &[f32], tol: f32) -> f32 {
    if want.is_empty() {
        return 0.0;
    }
    let hit = want.iter().filter(|w| got.iter().any(|g| (g - *w).abs() <= tol)).count();
    hit as f32 / want.len() as f32
}

/// Percussive share of a stretch (dB, percussive energy vs total): the
/// drums-band level the same way it is read on the record.
pub fn perc_share_db(fe: &Features, t0: f32, t1: f32) -> f32 {
    let fps = fps();
    let a = ((t0 * fps).max(0.0) as usize).min(fe.frames);
    let b = ((t1 * fps).max(0.0) as usize).min(fe.frames).max(a + 1).min(fe.frames);
    if b <= a {
        return 0.0;
    }
    db(mean(&fe.p_full[a..b])) - db(mean(&fe.total[a..b]))
}

/// Tonal balance of a stretch: mean energy per octave band (dB), relative to
/// the stretch's loudest band.
pub fn tone_db(fe: &Features, t0: f32, t1: f32) -> [f32; 9] {
    let fps = fps();
    let a = ((t0 * fps).max(0.0) as usize).min(fe.frames);
    let b = ((t1 * fps).max(0.0) as usize).min(fe.frames);
    let mut out = [0.0f32; 9];
    if b <= a {
        return out;
    }
    for i in 0..9 {
        let s: f32 = fe.tone[a..b].iter().map(|t| t[i]).sum();
        out[i] = db(s / (b - a) as f32);
    }
    let mx = out.iter().cloned().fold(f32::MIN, f32::max);
    for v in out.iter_mut() {
        *v -= mx;
    }
    out
}


/// One phrase of a take and where it lands on the reference's clock.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PhraseMap {
    pub from_s: f32,
    pub to_s: f32,
    /// reference time (s) where this phrase's first sample plays
    pub at_s: f32,
    /// match quality 0..1 (melody chroma x syllable-rhythm agreement)
    pub score: f32,
    /// true when no confident match was found and the phrase follows the previous one at his own spacing
    pub followed: bool,
}

/// Phrase-level alignment of a sung take against the reference's vocal stem,
/// using audio only (no words): the take is split at its pauses, every phrase
/// is matched over the whole reference on melody (chroma) and syllable rhythm
/// (where each side is voiced), and a monotonic path through the best
/// candidates picks one position per phrase, so a singer who skipped an
/// instrumental gap or drifted against the record lands each line where the
/// record sings it.
pub fn align_phrases(take: &Features, refv: &Features, min_pause_s: f32, max_phrase_s: f32) -> Vec<PhraseMap> {
    const D: usize = 2;
    let fr = SR / HOP as f32;
    let cfr = fr / D as f32;
    let voiced = |e: &[f32]| -> Vec<bool> {
        let db: Vec<f32> = e.iter().map(|v| 10.0 * (v + 1e-12).log10()).collect();
        let mut s = db.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p95 = s[((s.len() as f32 * 0.95) as usize).min(s.len().saturating_sub(1))];
        let p20 = s[((s.len() as f32 * 0.20) as usize).min(s.len().saturating_sub(1))];
        let thr = (p95 - 28.0).max(p20 + 6.0);
        db.iter().map(|d| *d > thr).collect()
    };
    let coarse_e = |e: &[f32]| -> Vec<f32> { (0..e.len() / D).map(|i| mean(&e[i * D..i * D + D])).collect() };
    let et = coarse_e(&take.h_full);
    let er = coarse_e(&refv.h_full);
    let vt = voiced(&et);
    let vr = voiced(&er);
    let ct = coarse_chroma(&take.chroma, D);
    let cr = coarse_chroma(&refv.chroma, D);
    let nt = ct.len().min(vt.len());
    let nr = cr.len().min(vr.len());
    if nt < 20 || nr < 20 {
        return Vec::new();
    }
    // phrases: voiced runs separated by >= min_pause of silence
    let gap = (min_pause_s * cfr).round() as usize;
    let mut phrases: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < nt {
        if !vt[i] { i += 1; continue; }
        let s = i;
        let mut last = i;
        let mut k = i;
        while k < nt && k - last <= gap {
            if vt[k] { last = k; }
            k += 1;
        }
        phrases.push((s, last + 1));
        i = last + 1;
    }
    // drop blips, split long phrases at their quietest point
    phrases.retain(|p| (p.1 - p.0) as f32 / cfr >= 0.25);
    let maxf = (max_phrase_s * cfr) as usize;
    let edge = (1.5 * cfr) as usize;
    let mut q = 0;
    while q < phrases.len() {
        let (a, b) = phrases[q];
        if b - a > maxf && b - a > 2 * edge + 2 {
            let w = (0.15 * cfr).max(1.0) as usize;
            let mut best = (a + edge, f32::MAX);
            for c in (a + edge)..(b - edge) {
                let lo = c.saturating_sub(w);
                let hi = (c + w).min(nt);
                let m = mean(&et[lo..hi]);
                if m < best.1 { best = (c, m); }
            }
            phrases[q] = (a, best.0);
            phrases.insert(q + 1, (best.0, b));
            continue;
        }
        q += 1;
    }
    if phrases.is_empty() {
        return Vec::new();
    }
    // candidate positions per phrase: (ref start frame, score)
    let score_at = |p: (usize, usize), r0: isize| -> f32 {
        let mut s = 0.0;
        let mut n = 0.0;
        let mut mis = 0.0;
        for i in p.0..p.1 {
            let j = r0 + (i - p.0) as isize;
            if j < 0 || j as usize >= nr { mis += 1.0; continue; }
            let j = j as usize;
            if vt[i] != vr[j] { mis += 1.0; }
            if vt[i] {
                n += 1.0;
                if vr[j] { s += cos12(&ct[i], &cr[j]); }
            }
        }
        let len = (p.1 - p.0) as f32;
        if n < 1.0 { return 0.0; }
        (s / n) * (1.0 - mis / len).max(0.0)
    };
    let k_best = 8usize;
    let sup = (1.0 * cfr) as isize;
    let mut cands: Vec<Vec<(isize, f32)>> = Vec::new();
    for p in &phrases {
        let len = (p.1 - p.0) as isize;
        let mut curve: Vec<(isize, f32)> = Vec::new();
        let mut r = 0isize;
        while r + len / 2 < nr as isize {
            curve.push((r, score_at(*p, r)));
            r += 1;
        }
        curve.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut picked: Vec<(isize, f32)> = Vec::new();
        for c in curve {
            if picked.len() >= k_best { break; }
            if picked.iter().all(|x| (x.0 - c.0).abs() > sup) { picked.push(c); }
        }
        cands.push(picked);
    }
    // monotonic path: maximise total score, mild preference for his own spacing
    let np = phrases.len();
    let lam = 0.12f32;
    // state: candidate index, or k_best = "follow previous at his spacing"
    let mut best: Vec<Vec<(f32, usize, isize)>> = Vec::new(); // (total, back pointer, ref start)
    for pi in 0..np {
        let mut row: Vec<(f32, usize, isize)> = Vec::new();
        let opts: Vec<(isize, f32, bool)> = cands[pi].iter().map(|c| (c.0, c.1, false)).collect();
        if pi == 0 {
            for o in &opts { row.push((o.1, usize::MAX, o.0)); }
        } else {
            let prev = &best[pi - 1];
            let (ta, tb) = (phrases[pi - 1], phrases[pi]);
            let my_gap = (tb.0 as isize) - (ta.0 as isize);
            for o in &opts {
                let mut bb = (f32::MIN, usize::MAX);
                for (k, pr) in prev.iter().enumerate() {
                    if pr.0 == f32::MIN { continue; }
                    let need = ((ta.1 - ta.0) as f32 * 0.85) as isize;
                    if o.0 < pr.2 + need { continue; }
                    let dev = ((o.0 - pr.2 - my_gap).abs() as f32 / cfr).min(8.0) / 8.0;
                    let v = pr.0 + o.1 - lam * dev;
                    if v > bb.0 { bb = (v, k); }
                }
                row.push((bb.0, bb.1, o.0));
            }
            // follow option from every previous state
            let mut bf = (f32::MIN, usize::MAX, 0isize);
            for (k, pr) in prev.iter().enumerate() {
                if pr.0 == f32::MIN { continue; }
                let r0 = pr.2 + my_gap;
                let v = pr.0 + 0.5 * score_at(tb, r0);
                if v > bf.0 { bf = (v, k, r0); }
            }
            row.push(bf);
        }
        if row.iter().all(|x| x.0 == f32::MIN) {
            // nothing reachable: restart the path at this phrase's best candidate
            row = opts.iter().map(|o| (o.1, usize::MAX, o.0)).collect();
            if pi > 0 { row.push((f32::MIN, usize::MAX, 0)); }
        }
        best.push(row);
    }
    let mut out: Vec<PhraseMap> = vec![PhraseMap { from_s: 0.0, to_s: 0.0, at_s: 0.0, score: 0.0, followed: false }; np];
    let last = &best[np - 1];
    let mut k = (0..last.len()).fold(0usize, |a, b| if last[b].0 > last[a].0 { b } else { a });
    for pi in (0..np).rev() {
        let st = best[pi][k];
        let followed = pi > 0 && k == best[pi].len() - 1 && best[pi].len() > cands[pi].len();
        let p = phrases[pi];
        let sc = score_at(p, st.2);
        out[pi] = PhraseMap { from_s: p.0 as f32 / cfr, to_s: p.1 as f32 / cfr, at_s: st.2 as f32 / cfr, score: (sc * 1000.0).round() / 1000.0, followed };
        if st.1 == usize::MAX { if pi > 0 { k = (0..best[pi - 1].len()).fold(0usize, |a, b| if best[pi - 1][b].0 > best[pi - 1][a].0 { b } else { a }); } } else { k = st.1; }
    }
    out
}

#[cfg(test)]
mod align_phrases_tests {
    use super::*;
    fn tone_phrases(spec: &[(f32, f32, f32)], total: f32) -> Vec<f32> {
        // (start s, len s, hz): a sung-like tone with 4 Hz syllable pulses
        let n = (total * SR) as usize;
        let mut x = vec![0.0f32; n];
        for &(s, l, hz) in spec {
            let a = (s * SR) as usize;
            let b = ((s + l) * SR) as usize;
            for i in a..b.min(n) {
                let t = (i - a) as f32 / SR;
                let env = (0.5 + 0.5 * (2.0 * std::f32::consts::PI * 4.0 * t).sin()).powf(0.5);
                x[i] = 0.3 * env * ((2.0 * std::f32::consts::PI * hz * t).sin() + 0.4 * (2.0 * std::f32::consts::PI * 2.0 * hz * t).sin());
            }
        }
        x
    }
    #[test]
    fn phrases_land_where_the_record_sings_them() {
        // the take sings three lines back to back; the record spreads them out with an instrumental gap
        let take = tone_phrases(&[(0.5, 2.0, 220.0), (3.2, 2.0, 330.0), (5.9, 2.0, 262.0)], 9.0);
        let rec = tone_phrases(&[(2.0, 2.0, 220.0), (4.6, 2.0, 330.0), (12.0, 2.0, 262.0)], 16.0);
        let ph = align_phrases(&features(&take), &features(&rec), 0.35, 6.0);
        assert_eq!(ph.len(), 3, "{ph:?}");
        let want = [2.0, 4.6, 12.0];
        for (p, w) in ph.iter().zip(want) {
            assert!((p.at_s - w).abs() < 0.15, "{ph:?}");
        }
    }
}
