//! Channel tools ported from SoundCraft: EQ 7-Band (HPF, low shelf, three
//! peaks, high shelf, LPF), EQ 1-Band, Expander/Gate with hold, a dynamic-EQ
//! De-Esser and the Channel Strip (trim -> gate -> compressor -> EQ with
//! HPF -> output), plus `vocal_clean_chain`, a gentle clean-up preset built
//! from them (no bandwidth extension, no spectral noise reduction).
//!
//! Adapted from SoundCraft (https://github.com/storytold/soundcraft):
//! `crates/dsp/src/biquad.rs` (`Coeffs::design`, `Biquad::process_sample`,
//! `butterworth_qs`), `crates/dsp/src/plugins/eq.rs` (`pass_filter`,
//! `design_eq7`, `design_eq1`), `crates/dsp/src/plugins/dynamics.rs`
//! (`ExpanderGate::tick`, `DeEsser::process`) and
//! `crates/dsp/src/plugins/strip.rs` (`ChannelStrip` routing).
//! Copyright (c) 2026 ArtCraft Team and the SoundCraft contributors,
//! MIT OR Apache-2.0 (used here under MIT). Changes: edition 2024 -> 2021,
//! stateful block plugins rewritten as whole-buffer mono functions over a
//! sample, parameter smoothing dropped (parameters are static per call),
//! automatic thresholds added for the gate, de-esser and compressor, tests
//! added. See THIRD_PARTY_NOTICES.md.

use crate::dsp::SR;
use crate::engine::Engine;
use crate::tools::{b_or, f_opt, f_or, obj, s_opt, s_req, Tool};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::f64::consts::PI;

// ------------------------------------------------------------------ biquad

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterType {
    LowPass,
    HighPass,
    BandPass,
    Notch,
    Peak,
    LowShelf,
    HighShelf,
    LowPass1,
    HighPass1,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl Coeffs {
    pub const IDENTITY: Coeffs = Coeffs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// RBJ cookbook design, computed in f64.
    pub fn design(kind: FilterType, freq: f32, q: f32, gain_db: f32, sample_rate: f32) -> Coeffs {
        let sr = f64::from(sample_rate.max(1000.0));
        let f = if freq.is_finite() {
            f64::from(freq).clamp(1.0, sr * 0.49)
        } else {
            1000.0
        };
        let q = if q.is_finite() {
            f64::from(q).clamp(0.025, 100.0)
        } else {
            std::f64::consts::FRAC_1_SQRT_2
        };
        let g = if gain_db.is_finite() {
            f64::from(gain_db).clamp(-48.0, 48.0)
        } else {
            0.0
        };
        let w0 = 2.0 * PI * f / sr;
        let (sw, cw) = w0.sin_cos();
        let alpha = sw / (2.0 * q);
        let a = 10f64.powf(g / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            FilterType::LowPass => (
                (1.0 - cw) / 2.0,
                1.0 - cw,
                (1.0 - cw) / 2.0,
                1.0 + alpha,
                -2.0 * cw,
                1.0 - alpha,
            ),
            FilterType::HighPass => (
                (1.0 + cw) / 2.0,
                -(1.0 + cw),
                (1.0 + cw) / 2.0,
                1.0 + alpha,
                -2.0 * cw,
                1.0 - alpha,
            ),
            FilterType::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            FilterType::Notch => (1.0, -2.0 * cw, 1.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            FilterType::Peak => (
                1.0 + alpha * a,
                -2.0 * cw,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cw,
                1.0 - alpha / a,
            ),
            FilterType::LowShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cw + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
                    a * ((a + 1.0) - (a - 1.0) * cw - s),
                    (a + 1.0) + (a - 1.0) * cw + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cw),
                    (a + 1.0) + (a - 1.0) * cw - s,
                )
            }
            FilterType::HighShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cw + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
                    a * ((a + 1.0) + (a - 1.0) * cw - s),
                    (a + 1.0) - (a - 1.0) * cw + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cw),
                    (a + 1.0) - (a - 1.0) * cw - s,
                )
            }
            FilterType::LowPass1 | FilterType::HighPass1 => {
                // bilinear transform of 1/(s+1) with a prewarped cutoff
                let k = (w0 / 2.0).tan();
                let norm = 1.0 / (1.0 + k);
                let a1 = (k - 1.0) * norm;
                return if kind == FilterType::LowPass1 {
                    Coeffs {
                        b0: (k * norm) as f32,
                        b1: (k * norm) as f32,
                        b2: 0.0,
                        a1: a1 as f32,
                        a2: 0.0,
                    }
                } else {
                    Coeffs {
                        b0: norm as f32,
                        b1: -norm as f32,
                        b2: 0.0,
                        a1: a1 as f32,
                        a2: 0.0,
                    }
                };
            }
        };
        let c = Coeffs {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
        };
        if [c.b0, c.b1, c.b2, c.a1, c.a2].iter().all(|v| v.is_finite()) {
            c
        } else {
            Coeffs::IDENTITY
        }
    }

    /// Magnitude response in dB at `freq`.
    pub fn magnitude_db(&self, freq: f32, sample_rate: f32) -> f32 {
        let sr = f64::from(sample_rate.max(1000.0));
        let w = 2.0 * PI * f64::from(freq).clamp(0.0, sr * 0.5) / sr;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let (b0, b1, b2, a1, a2) = (
            f64::from(self.b0),
            f64::from(self.b1),
            f64::from(self.b2),
            f64::from(self.a1),
            f64::from(self.a2),
        );
        let nr = b0 + b1 * c1 + b2 * c2;
        let ni = -(b1 * s1 + b2 * s2);
        let dr = 1.0 + a1 * c1 + a2 * c2;
        let di = -(a1 * s1 + a2 * s2);
        let num = nr * nr + ni * ni;
        let den = (dr * dr + di * di).max(1e-30);
        (10.0 * (num / den).max(1e-30).log10()).max(-300.0) as f32
    }
}

#[inline]
fn flush(x: f32) -> f32 {
    if x.is_finite() && x.abs() > 1.0e-25 {
        x
    } else {
        0.0
    }
}

/// One biquad section with state (transposed direct form II).
#[derive(Clone, Copy, Debug)]
pub struct Bq {
    pub c: Coeffs,
    z1: f32,
    z2: f32,
}

impl Bq {
    pub fn new(c: Coeffs) -> Bq {
        Bq {
            c,
            z1: 0.0,
            z2: 0.0,
        }
    }
    #[inline]
    pub fn run(&mut self, x: f32) -> f32 {
        let c = self.c;
        let y = c.b0 * x + self.z1;
        self.z1 = flush(c.b1 * x - c.a1 * y + self.z2);
        self.z2 = flush(c.b2 * x - c.a2 * y);
        if y.is_finite() {
            y
        } else {
            self.z1 = 0.0;
            self.z2 = 0.0;
            0.0
        }
    }
}

fn butterworth_qs4() -> [f32; 2] {
    [0.541_196_1, 1.306_563]
}

/// 1-2 cascaded sections for an HPF/LPF; slope in dB/oct (6, 12, 18, 24).
pub fn pass_filter(slope_db: u32, high: bool, freq: f32) -> [Coeffs; 2] {
    let (k1, k2) = if high {
        (FilterType::HighPass1, FilterType::HighPass)
    } else {
        (FilterType::LowPass1, FilterType::LowPass)
    };
    if slope_db <= 6 {
        [Coeffs::design(k1, freq, 0.707, 0.0, SR), Coeffs::IDENTITY]
    } else if slope_db >= 19 {
        let q = butterworth_qs4();
        [
            Coeffs::design(k2, freq, q[0], 0.0, SR),
            Coeffs::design(k2, freq, q[1], 0.0, SR),
        ]
    } else if slope_db >= 13 {
        [
            Coeffs::design(k1, freq, 0.707, 0.0, SR),
            Coeffs::design(k2, freq, 1.0, 0.0, SR),
        ]
    } else {
        [
            Coeffs::design(k2, freq, std::f32::consts::FRAC_1_SQRT_2, 0.0, SR),
            Coeffs::IDENTITY,
        ]
    }
}

fn cascade(x: &mut [f32], cs: &[Coeffs], gain: f32) {
    let mut f: Vec<Bq> = cs
        .iter()
        .filter(|c| **c != Coeffs::IDENTITY)
        .map(|c| Bq::new(*c))
        .collect();
    for v in x.iter_mut() {
        let mut y = *v;
        for b in f.iter_mut() {
            y = b.run(y);
        }
        *v = y * gain;
    }
}

// ------------------------------------------------------------------ EQ 7

#[derive(Clone, Debug)]
pub struct Eq7 {
    pub input_db: f32,
    pub hpf: Option<(f32, u32)>,
    pub low_shelf: (f32, f32, f32),
    pub low_mid: (f32, f32, f32),
    pub mid: (f32, f32, f32),
    pub high_mid: (f32, f32, f32),
    pub high_shelf: (f32, f32, f32),
    pub lpf: Option<(f32, u32)>,
    pub output_db: f32,
}

impl Default for Eq7 {
    fn default() -> Self {
        Eq7 {
            input_db: 0.0,
            hpf: None,
            low_shelf: (100.0, 0.0, 0.707),
            low_mid: (250.0, 0.0, 1.0),
            mid: (1000.0, 0.0, 1.0),
            high_mid: (4000.0, 0.0, 1.0),
            high_shelf: (8000.0, 0.0, 0.707),
            lpf: None,
            output_db: 0.0,
        }
    }
}

impl Eq7 {
    pub fn coeffs(&self) -> Vec<Coeffs> {
        let mut c = Vec::new();
        if let Some((f, s)) = self.hpf {
            c.extend(pass_filter(s, true, f));
        }
        let bands = [
            (self.low_shelf, FilterType::LowShelf),
            (self.low_mid, FilterType::Peak),
            (self.mid, FilterType::Peak),
            (self.high_mid, FilterType::Peak),
            (self.high_shelf, FilterType::HighShelf),
        ];
        for ((f, g, q), kind) in bands {
            if g.abs() > 1e-4 {
                c.push(Coeffs::design(kind, f, q, g, SR));
            }
        }
        if let Some((f, s)) = self.lpf {
            c.extend(pass_filter(s, false, f));
        }
        c.retain(|x| *x != Coeffs::IDENTITY);
        c
    }
    pub fn process(&self, x: &mut [f32]) {
        cascade(
            x,
            &self.coeffs(),
            10f32.powf((self.input_db + self.output_db) / 20.0),
        );
    }
    pub fn response_db(&self, f: f32) -> f32 {
        self.input_db
            + self.output_db
            + self
                .coeffs()
                .iter()
                .map(|c| c.magnitude_db(f, SR))
                .sum::<f32>()
    }
}

/// EQ 1-Band: one filter of any type.
pub fn eq1(
    x: &mut [f32],
    kind: FilterType,
    freq: f32,
    gain_db: f32,
    q: f32,
    output_db: f32,
) -> Coeffs {
    let c = if kind == FilterType::Peak && gain_db.abs() < 1e-4 {
        Coeffs::IDENTITY
    } else {
        Coeffs::design(kind, freq, q, gain_db, SR)
    };
    cascade(x, &[c], 10f32.powf(output_db / 20.0));
    c
}

pub fn filter_type(s: &str) -> Result<FilterType> {
    Ok(
        match s.trim().to_lowercase().replace([' ', '-'], "_").as_str() {
            "peak" | "bell" => FilterType::Peak,
            "low_shelf" | "lowshelf" => FilterType::LowShelf,
            "high_shelf" | "highshelf" => FilterType::HighShelf,
            "high_pass" | "highpass" | "hpf" | "low_cut" => FilterType::HighPass,
            "low_pass" | "lowpass" | "lpf" | "high_cut" => FilterType::LowPass,
            "notch" => FilterType::Notch,
            "band_pass" | "bandpass" => FilterType::BandPass,
            o => bail!(
                "type '{o}': peak, low_shelf, high_shelf, high_pass, low_pass, notch, band_pass"
            ),
        },
    )
}

// ------------------------------------------------------------------ dynamics

fn time_coef(ms: f32) -> f32 {
    let n = ms * 0.001 * SR;
    if n < 1.0e-3 {
        0.0
    } else {
        (-1.0 / n).exp()
    }
}

fn to_db(g: f32) -> f32 {
    (20.0 * g.abs().max(1e-8).log10()).max(-144.0)
}

fn from_db(d: f32) -> f32 {
    10f32.powf(d / 20.0)
}

#[derive(Clone, Copy, Debug)]
pub struct Gate {
    pub threshold_db: f32,
    pub ratio: f32,
    pub range_db: f32,
    pub attack_ms: f32,
    pub hold_ms: f32,
    pub release_ms: f32,
}

impl Default for Gate {
    fn default() -> Self {
        Gate {
            threshold_db: -40.0,
            ratio: 100.0,
            range_db: 80.0,
            attack_ms: 0.5,
            hold_ms: 50.0,
            release_ms: 100.0,
        }
    }
}

/// Downward expander / gate with hold. Returns (max gain reduction dB,
/// share of the time it reduced more than 3 dB).
pub fn expander_gate(x: &mut [f32], p: &Gate) -> (f32, f32) {
    let att = time_coef(p.attack_ms.clamp(0.01, 100.0));
    let rel = time_coef(p.release_ms.clamp(1.0, 4000.0));
    let env_rel = time_coef(5.0);
    let hold = (p.hold_ms.clamp(0.0, 2000.0) * 0.001 * SR) as usize;
    let (mut env, mut gr, mut hold_left) = (0.0f32, 0.0f32, 0usize);
    let (mut max_gr, mut closed) = (0.0f32, 0usize);
    for v in x.iter_mut() {
        let level = if v.is_finite() { v.abs() } else { 0.0 };
        env = level.max(env * env_rel);
        let ldb = to_db(env);
        let mut target = if ldb < p.threshold_db {
            ((p.threshold_db - ldb) * (p.ratio.max(1.0) - 1.0)).min(p.range_db.max(0.0))
        } else {
            0.0
        };
        if target <= 0.0 {
            hold_left = hold;
        } else if target > gr && hold_left > 0 {
            hold_left -= 1;
            target = gr;
        }
        // opening (less reduction) uses attack, closing uses release
        let c = if target < gr { att } else { rel };
        gr = target + (gr - target) * c;
        if !gr.is_finite() {
            gr = 0.0;
        }
        max_gr = max_gr.max(gr);
        if gr > 3.0 {
            closed += 1;
        }
        *v *= from_db(-gr);
    }
    (max_gr, closed as f32 / x.len().max(1) as f32)
}

#[derive(Clone, Copy, Debug)]
pub struct DeEss {
    pub freq: f32,
    pub range_db: f32,
    pub threshold_db: f32,
}

/// Dynamic-EQ de-esser: a high-passed sidechain detects sibilance and a high
/// shelf at 0.8 x freq dips by up to `range_db` while it is over the
/// threshold. Returns (max reduction dB, share of time reducing > 1 dB).
pub fn de_ess(x: &mut [f32], p: &DeEss) -> (f32, f32) {
    const CHUNK: usize = 16;
    let att = time_coef(0.5);
    let rel = time_coef(60.0);
    let mut hp = Bq::new(Coeffs::design(
        FilterType::HighPass,
        p.freq,
        std::f32::consts::FRAC_1_SQRT_2,
        0.0,
        SR,
    ));
    let mut shelf = Bq::new(Coeffs::IDENTITY);
    let mut shelf_gr = 0.0f32;
    let mut env = 0.0f32;
    let (mut max_gr, mut active) = (0.0f32, 0usize);
    let n = x.len();
    let mut i0 = 0;
    while i0 < n {
        let len = CHUNK.min(n - i0);
        for v in &x[i0..i0 + len] {
            let level = hp.run(*v).abs();
            let c = if level > env { att } else { rel };
            env = level + (env - level) * c;
            if !env.is_finite() {
                env = 0.0;
            }
        }
        let gr = (to_db(env) - p.threshold_db).clamp(0.0, p.range_db.max(0.0));
        max_gr = max_gr.max(gr);
        if gr > 1.0 {
            active += len;
        }
        if (gr - shelf_gr).abs() > 0.05 || (gr == 0.0 && shelf_gr != 0.0) {
            shelf_gr = gr;
            shelf.c = if gr < 0.01 {
                Coeffs::IDENTITY
            } else {
                Coeffs::design(FilterType::HighShelf, p.freq * 0.8, 0.8, -gr, SR)
            };
        }
        for v in &mut x[i0..i0 + len] {
            *v = shelf.run(*v);
        }
        i0 += len;
    }
    (max_gr, active as f32 / n.max(1) as f32)
}

/// 10 ms RMS levels (dB) of the frames.
fn rms_db_frames(x: &[f32]) -> Vec<f32> {
    let w = (0.01 * SR) as usize;
    x.chunks(w.max(1))
        .map(|c| to_db((c.iter().map(|v| v * v).sum::<f32>() / c.len().max(1) as f32).sqrt()))
        .collect()
}

fn pct(v: &[f32], p: f32) -> f32 {
    if v.is_empty() {
        return -144.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    s[((s.len() - 1) as f32 * p.clamp(0.0, 1.0)).round() as usize]
}

/// Gate threshold from the take: halfway (in dB) between the noise floor
/// (10th percentile of 10 ms RMS) and the voice (70th), never closer than
/// 6 dB to the floor.
pub fn auto_gate_threshold(x: &[f32]) -> f32 {
    let f = rms_db_frames(x);
    let floor = pct(&f, 0.1);
    let voice = pct(&f, 0.7);
    (floor + 0.5 * (voice - floor))
        .max(floor + 6.0)
        .min(voice - 3.0)
}

/// De-esser threshold from the take: the 90th percentile of the
/// high-passed peak envelope, so only the loudest tenth (the esses) dips.
pub fn auto_deess_threshold(x: &[f32], freq: f32) -> f32 {
    let mut hp = Bq::new(Coeffs::design(
        FilterType::HighPass,
        freq,
        std::f32::consts::FRAC_1_SQRT_2,
        0.0,
        SR,
    ));
    let h: Vec<f32> = x.iter().map(|v| hp.run(*v)).collect();
    let w = (0.005 * SR) as usize;
    let pk: Vec<f32> = h
        .chunks(w.max(1))
        .map(|c| to_db(c.iter().fold(0.0f32, |m, v| m.max(v.abs()))))
        .filter(|d| *d > -90.0)
        .collect();
    pct(&pk, 0.9)
}

/// Compressor threshold from the take: the 80th percentile of the voiced
/// 10 ms RMS levels plus 3 dB (peaks get caught, the body breathes).
pub fn auto_comp_threshold(x: &[f32]) -> f32 {
    let f = rms_db_frames(x);
    let floor = pct(&f, 0.1);
    let voiced: Vec<f32> = f.into_iter().filter(|d| *d > floor + 10.0).collect();
    pct(&voiced, 0.8) - 3.0
}

fn comp(
    x: &mut [f32],
    threshold_db: f32,
    ratio: f32,
    attack_ms: f32,
    release_ms: f32,
    makeup_db: f32,
) -> f32 {
    let mut r = x.to_vec();
    let p = crate::sc_dsp::CompParams {
        threshold_db,
        ratio,
        attack_ms,
        release_ms,
        knee_db: 6.0,
        makeup_db,
        sc_hpf_hz: 0.0,
        lookahead_ms: 0.0,
        mix: 1.0,
    };
    crate::sc_dsp::compress(&p, x, &mut r)
}

// ------------------------------------------------------------------ strip

#[derive(Clone, Debug)]
pub struct Strip {
    pub input_db: f32,
    pub gate: Option<Gate>,
    pub comp: Option<(f32, f32, f32, f32, f32)>,
    pub eq: Option<Eq7>,
    pub output_db: f32,
}

/// Channel strip: input trim -> gate -> compressor -> EQ -> output.
/// Returns (gate max GR, comp max GR).
pub fn channel_strip(x: &mut [f32], s: &Strip) -> (f32, f32) {
    let g = from_db(s.input_db);
    x.iter_mut().for_each(|v| *v *= g);
    let gate_gr = s
        .gate
        .as_ref()
        .map(|p| expander_gate(x, p).0)
        .unwrap_or(0.0);
    let comp_gr = s
        .comp
        .map(|(t, r, a, rl, m)| comp(x, t, r, a, rl, m))
        .unwrap_or(0.0);
    if let Some(eq) = &s.eq {
        eq.process(x);
    }
    let o = from_db(s.output_db);
    x.iter_mut().for_each(|v| *v *= o);
    (gate_gr, comp_gr)
}

// ------------------------------------------------------------------ tools

pub(crate) fn r1(x: f32) -> f32 {
    (x * 10.0).round() / 10.0
}

fn save(
    e: &mut Engine,
    base: &crate::samples::SampleInfo,
    nn: &str,
    y: &[f32],
    what: &str,
) -> Result<String> {
    let mut y = y.to_vec();
    let pk = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if pk > 0.98 {
        let g = 0.98 / pk;
        y.iter_mut().for_each(|v| *v *= g);
    }
    let name = crate::samples::sample_name(nn);
    std::fs::create_dir_all(e.samples_dir())?;
    let path = e.samples_dir().join(format!("{name}.wav"));
    crate::render::write_wav(&path, &y, &y)?;
    let info = crate::samples::SampleInfo {
        name,
        path: path.to_string_lossy().into(),
        source: format!("{} ({what})", base.source),
        license: base.license.clone(),
        author: base.author.clone(),
        duration: 0.0,
    };
    let r = crate::tools::register_sample(e, info)?;
    e.revision += 1;
    Ok(r["sample"].as_str().unwrap_or(nn).to_string())
}

/// Point the clips of `old` (on `track`, or on every track when None) at `new`.
fn retarget(e: &mut Engine, old: &str, new: &str, track: Option<&str>) -> usize {
    let mut n = 0;
    for c in e.project.audio_clips.iter_mut() {
        if c.sample.eq_ignore_ascii_case(old)
            && track.is_none_or(|t| c.track.eq_ignore_ascii_case(t))
        {
            c.sample = new.to_string();
            n += 1;
        }
    }
    n
}

pub(crate) fn finish(
    e: &mut Engine,
    a: &Value,
    info: &crate::samples::SampleInfo,
    x: &[f32],
    y: &[f32],
    suffix: &str,
    what: &str,
    mut extra: Value,
) -> Result<Value> {
    let nn = s_opt(a, "new_name").unwrap_or_else(|| format!("{}_{suffix}", info.name));
    let s = save(e, info, &nn, y, what)?;
    let mut moved = 0;
    if b_or(a, "replace_clips", false) || s_opt(a, "track").is_some() {
        let t = s_opt(a, "track");
        moved = retarget(e, &info.name, &s, t.as_deref());
    }
    let (f0, p0) = crate::vocal_repair::floor_and_peak_db(x);
    let (f1, p1) = crate::vocal_repair::floor_and_peak_db(y);
    if let Value::Object(m) = &mut extra {
        m.insert("sample".into(), json!(s));
        m.insert("clips_switched".into(), json!(moved));
        m.insert("measured".into(), json!({"noise_floor_db": {"before": r1(f0), "after": r1(f1)}, "loud_parts_db": {"before": r1(p0), "after": r1(p1)}}));
    }
    Ok(extra)
}

fn slope(a: &Value, k: &str) -> u32 {
    (f_or(a, k, 12.0).round() as u32).clamp(6, 24)
}

fn eq7_from(a: &Value) -> Eq7 {
    let d = Eq7::default();
    let band = |pre: &str, def: (f32, f32, f32)| -> (f32, f32, f32) {
        (
            f_or(a, &format!("{pre}_freq"), def.0),
            f_or(a, &format!("{pre}_gain"), def.1).clamp(-24.0, 24.0),
            f_or(a, &format!("{pre}_q"), def.2).clamp(0.1, 10.0),
        )
    };
    Eq7 {
        input_db: f_or(a, "input_gain", 0.0).clamp(-24.0, 24.0),
        hpf: f_opt(a, "hpf_freq")
            .filter(|_| b_or(a, "hpf_on", true))
            .map(|f| (f.clamp(10.0, 2000.0), slope(a, "hpf_slope"))),
        low_shelf: band("low_shelf", d.low_shelf),
        low_mid: band("low_mid", d.low_mid),
        mid: band("mid", d.mid),
        high_mid: band("high_mid", d.high_mid),
        high_shelf: band("high_shelf", d.high_shelf),
        lpf: f_opt(a, "lpf_freq")
            .filter(|_| b_or(a, "lpf_on", true))
            .map(|f| (f.clamp(1000.0, 20000.0), slope(a, "lpf_slope"))),
        output_db: f_or(a, "output_gain", 0.0).clamp(-24.0, 24.0),
    }
}

fn eq7_curve(eq: &Eq7) -> Value {
    let fs = [
        50.0f32, 100.0, 200.0, 300.0, 500.0, 1000.0, 2000.0, 3000.0, 5000.0, 8000.0, 12000.0,
        16000.0,
    ];
    Value::Object(
        fs.iter()
            .map(|f| (format!("{f:.0}"), json!(r1(eq.response_db(*f)))))
            .collect(),
    )
}

fn gate_from(a: &Value, x: &[f32]) -> Gate {
    let d = Gate::default();
    Gate {
        threshold_db: f_opt(a, "threshold")
            .unwrap_or_else(|| auto_gate_threshold(x))
            .clamp(-80.0, 0.0),
        ratio: f_or(a, "ratio", d.ratio).clamp(1.0, 100.0),
        range_db: f_or(a, "range", d.range_db).clamp(0.0, 80.0),
        attack_ms: f_or(a, "attack_ms", d.attack_ms),
        hold_ms: f_or(a, "hold_ms", d.hold_ms),
        release_ms: f_or(a, "release_ms", d.release_ms),
    }
}

fn tone_pair(x: &[f32], y: &[f32]) -> Value {
    json!({"before": crate::vocal_clarity::tone(x), "after": crate::vocal_clarity::tone(y)})
}

const EQ7_KEYS: &str = "input_gain, hpf_freq (+hpf_slope 6/12/18/24 dB/oct), low_shelf_freq/gain/q, low_mid_freq/gain/q, mid_freq/gain/q, high_mid_freq/gain/q, high_shelf_freq/gain/q, lpf_freq (+lpf_slope), output_gain";

fn eq7_schema() -> Value {
    let mut m = serde_json::Map::new();
    m.insert("sample".into(), json!({"type": "string"}));
    for k in [
        "input_gain",
        "hpf_freq",
        "hpf_slope",
        "lpf_freq",
        "lpf_slope",
        "output_gain",
    ] {
        m.insert(k.into(), json!({"type": "number"}));
    }
    for b in ["low_shelf", "low_mid", "mid", "high_mid", "high_shelf"] {
        for s in ["freq", "gain", "q"] {
            m.insert(format!("{b}_{s}"), json!({"type": "number"}));
        }
    }
    m.insert("hpf_on".into(), json!({"type": "boolean"}));
    m.insert("lpf_on".into(), json!({"type": "boolean"}));
    m.insert("new_name".into(), json!({"type": "string"}));
    m.insert("track".into(), json!({"type": "string", "description": "switch this track's clips of the sample to the result"}));
    m.insert(
        "replace_clips".into(),
        json!({"type": "boolean", "description": "switch every clip of the sample to the result"}),
    );
    obj(Value::Object(m), &["sample"])
}

pub(crate) fn common(extra: Value) -> Value {
    let mut m = match extra {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    m.insert("sample".into(), json!({"type": "string"}));
    m.insert("new_name".into(), json!({"type": "string"}));
    m.insert("track".into(), json!({"type": "string", "description": "switch this track's clips of the sample to the result"}));
    m.insert("replace_clips".into(), json!({"type": "boolean"}));
    obj(Value::Object(m), &["sample"])
}

pub fn vocal_clean(x: &[f32], a: &Value) -> (Vec<f32>, Value) {
    let mut y = x.to_vec();
    // 1. EQ: HPF 90 (18 dB/oct), box cut ~300 Hz, presence +3 dB at 3 kHz
    let eq = Eq7 {
        hpf: Some((f_or(a, "hpf_hz", 90.0).clamp(20.0, 400.0), 18)),
        low_mid: (
            f_or(a, "box_hz", 300.0).clamp(150.0, 800.0),
            f_or(a, "box_db", -3.0).clamp(-12.0, 0.0),
            1.2,
        ),
        high_mid: (
            f_or(a, "presence_hz", 3000.0).clamp(1500.0, 8000.0),
            f_or(a, "presence_db", 3.0).clamp(0.0, 8.0),
            0.9,
        ),
        ..Eq7::default()
    };
    eq.process(&mut y);
    // 2. de-ess (auto threshold: only the loudest tenth of the esses dips)
    let df = f_or(a, "deess_hz", 6500.0).clamp(3000.0, 12000.0);
    let dt = f_opt(a, "deess_threshold").unwrap_or_else(|| auto_deess_threshold(&y, df));
    let (ds_gr, ds_t) = if b_or(a, "deess", true) {
        de_ess(
            &mut y,
            &DeEss {
                freq: df,
                range_db: f_or(a, "deess_range", 6.0).clamp(0.0, 20.0),
                threshold_db: dt,
            },
        )
    } else {
        (0.0, 0.0)
    };
    // 3. gentle gate: a 2:1 expander, at most 10 dB down, slow release
    let gt = f_opt(a, "gate_threshold").unwrap_or_else(|| auto_gate_threshold(&y));
    let (g_gr, g_t) = if b_or(a, "gate", true) {
        expander_gate(
            &mut y,
            &Gate {
                threshold_db: gt,
                ratio: 2.0,
                range_db: f_or(a, "gate_range", 10.0).clamp(0.0, 40.0),
                attack_ms: 1.0,
                hold_ms: 60.0,
                release_ms: 160.0,
            },
        )
    } else {
        (0.0, 0.0)
    };
    // 4. comp 3:1, 10/120 ms, soft knee, half the reduction made up
    let ct = f_opt(a, "comp_threshold").unwrap_or_else(|| auto_comp_threshold(&y));
    let ratio = f_or(a, "comp_ratio", 3.0).clamp(1.0, 10.0);
    let c_gr = if b_or(a, "comp", true) {
        comp(&mut y, ct, ratio, 10.0, 120.0, 0.0)
    } else {
        0.0
    };
    // level: match the input's voiced loudness (no surprise jumps on the track)
    let f_in = rms_db_frames(x);
    let f_out = rms_db_frames(&y);
    let fl = pct(&f_in, 0.1);
    let voiced = |f: &[f32]| -> f32 {
        let v: Vec<f32> = f.iter().copied().filter(|d| *d > fl + 10.0).collect();
        pct(&v, 0.6)
    };
    let gain = (voiced(&f_in) - voiced(&f_out)).clamp(-6.0, 12.0);
    let g = from_db(gain);
    y.iter_mut().for_each(|v| *v *= g);
    let rep = json!({
        "eq": {"hpf_hz": eq.hpf.map(|h| h.0), "box": {"hz": eq.low_mid.0, "db": eq.low_mid.1}, "presence": {"hz": eq.high_mid.0, "db": eq.high_mid.1}},
        "de_ess": {"freq_hz": df, "threshold_db": r1(dt), "max_reduction_db": r1(ds_gr), "active_pct": r1(ds_t * 100.0)},
        "gate": {"threshold_db": r1(gt), "max_reduction_db": r1(g_gr), "reducing_pct": r1(g_t * 100.0)},
        "comp": {"threshold_db": r1(ct), "ratio": ratio, "max_reduction_db": r1(c_gr)},
        "level_match_db": r1(gain),
    });
    (y, rep)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "eq_7band",
            description: "EQ 7-Band on a sample (writes a new one): HPF (6/12/18/24 dB/oct), low shelf, three peaks (low-mid, mid, high-mid), high shelf, LPF, input/output gain. Bands with 0 gain and filters without a freq are off. Keys: input_gain, hpf_freq (+hpf_slope 6/12/18/24 dB/oct), low_shelf_freq/gain/q, low_mid_freq/gain/q, mid_freq/gain/q, high_mid_freq/gain/q, high_shelf_freq/gain/q, lpf_freq (+lpf_slope), output_gain. Reports the curve (dB at 50 Hz-16 kHz) and tone before/after. track or replace_clips switches the clips to the result.",
            mutates: true,
            schema: eq7_schema,
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let eq = eq7_from(a);
                let mut y = x.clone();
                eq.process(&mut y);
                let curve = eq7_curve(&eq);
                let t = tone_pair(&x, &y);
                finish(e, a, &info, &x, &y, "eq7", "EQ 7-Band", json!({"curve_db": curve, "tone_db": t, "keys": EQ7_KEYS}))
            },
        },
        Tool {
            name: "eq_1band",
            description: "EQ 1-Band on a sample (writes a new one): type peak, low_shelf, high_shelf, high_pass, low_pass, notch or band_pass; freq (default 1000), gain dB (peak/shelves), q (default 1), output_gain. Reports the curve and tone before/after.",
            mutates: true,
            schema: || common(json!({"type": {"type": "string", "enum": ["peak", "low_shelf", "high_shelf", "high_pass", "low_pass", "notch", "band_pass"]}, "freq": {"type": "number"}, "gain": {"type": "number"}, "q": {"type": "number"}, "output_gain": {"type": "number"}})),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let kind = filter_type(&s_opt(a, "type").unwrap_or_else(|| "peak".into()))?;
                let mut y = x.clone();
                let c = eq1(&mut y, kind, f_or(a, "freq", 1000.0).clamp(20.0, 20000.0), f_or(a, "gain", 0.0).clamp(-24.0, 24.0), f_or(a, "q", 1.0).clamp(0.1, 10.0), f_or(a, "output_gain", 0.0).clamp(-24.0, 24.0));
                let fs = [100.0f32, 300.0, 1000.0, 3000.0, 6000.0, 10000.0];
                let curve: serde_json::Map<String, Value> = fs.iter().map(|f| (format!("{f:.0}"), json!(r1(c.magnitude_db(*f, SR))))).collect();
                let t = tone_pair(&x, &y);
                finish(e, a, &info, &x, &y, "eq1", "EQ 1-Band", json!({"curve_db": curve, "tone_db": t}))
            },
        },
        Tool {
            name: "expander_gate",
            description: "Expander/Gate with hold on a sample (writes a new one): threshold dB (default: halfway between the take's noise floor and its voice), ratio (default 100 = gate; 2-4 = gentle expander), range dB (max reduction, default 80), attack_ms 0.5, hold_ms 50, release_ms 100. Reports max reduction and how much of the time it was closing.",
            mutates: true,
            schema: || common(json!({"threshold": {"type": "number"}, "ratio": {"type": "number"}, "range": {"type": "number"}, "attack_ms": {"type": "number"}, "hold_ms": {"type": "number"}, "release_ms": {"type": "number"}})),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let g = gate_from(a, &x);
                let mut y = x.clone();
                let (gr, t) = expander_gate(&mut y, &g);
                finish(e, a, &info, &x, &y, "gate", "expander/gate", json!({"threshold_db": r1(g.threshold_db), "ratio": g.ratio, "range_db": g.range_db, "max_reduction_db": r1(gr), "reducing_pct": r1(t * 100.0)}))
            },
        },
        Tool {
            name: "de_ess",
            description: "De-esser on a sample (writes a new one): a dynamic high shelf at 0.8 x freq dips by up to range dB while the high-passed sidechain is over the threshold, and is flat at rest. freq default 6500 Hz (5-8k for most voices), range default 6 dB, threshold dB default = the take's own 90th-percentile sibilance level (only the loudest esses dip). Reports max reduction and active time.",
            mutates: true,
            schema: || common(json!({"freq": {"type": "number"}, "range": {"type": "number"}, "threshold": {"type": "number"}})),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let f = f_or(a, "freq", 6500.0).clamp(2000.0, 16000.0);
                let thr = f_opt(a, "threshold").unwrap_or_else(|| auto_deess_threshold(&x, f));
                let mut y = x.clone();
                let (gr, t) = de_ess(&mut y, &DeEss { freq: f, range_db: f_or(a, "range", 6.0).clamp(0.0, 40.0), threshold_db: thr });
                let tp = tone_pair(&x, &y);
                finish(e, a, &info, &x, &y, "deess", "de-esser", json!({"freq_hz": f, "threshold_db": r1(thr), "max_reduction_db": r1(gr), "active_pct": r1(t * 100.0), "tone_db": tp}))
            },
        },
        Tool {
            name: "channel_strip",
            description: "Channel strip on a sample (writes a new one): input trim -> gate -> compressor -> EQ (HPF + low shelf, mid bell, high shelf) -> output. gate_on (threshold default auto, range 40, release 150), comp_on (threshold default auto, ratio 3, attack 10, release 120, makeup 0, 6 dB knee), eq_on (default true): hpf_freq, low_gain/low_freq (100), mid_gain/mid_freq (1000)/mid_q, high_gain/high_freq (8000); input_gain, output_gain.",
            mutates: true,
            schema: || common(json!({"input_gain": {"type": "number"}, "gate_on": {"type": "boolean"}, "gate_threshold": {"type": "number"}, "gate_range": {"type": "number"}, "gate_release": {"type": "number"}, "comp_on": {"type": "boolean"}, "comp_threshold": {"type": "number"}, "comp_ratio": {"type": "number"}, "comp_attack": {"type": "number"}, "comp_release": {"type": "number"}, "comp_makeup": {"type": "number"}, "eq_on": {"type": "boolean"}, "hpf_freq": {"type": "number"}, "low_gain": {"type": "number"}, "low_freq": {"type": "number"}, "mid_gain": {"type": "number"}, "mid_freq": {"type": "number"}, "mid_q": {"type": "number"}, "high_gain": {"type": "number"}, "high_freq": {"type": "number"}, "output_gain": {"type": "number"}})),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                let gate = if b_or(a, "gate_on", false) {
                    Some(Gate { threshold_db: f_opt(a, "gate_threshold").unwrap_or_else(|| auto_gate_threshold(&x)), ratio: 100.0, range_db: f_or(a, "gate_range", 40.0), attack_ms: 0.5, hold_ms: 50.0, release_ms: f_or(a, "gate_release", 150.0) })
                } else {
                    None
                };
                let comp = if b_or(a, "comp_on", false) {
                    Some((f_opt(a, "comp_threshold").unwrap_or_else(|| auto_comp_threshold(&x)), f_or(a, "comp_ratio", 3.0).clamp(1.0, 100.0), f_or(a, "comp_attack", 10.0), f_or(a, "comp_release", 120.0), f_or(a, "comp_makeup", 0.0)))
                } else {
                    None
                };
                let eq = if b_or(a, "eq_on", true) {
                    Some(Eq7 {
                        hpf: f_opt(a, "hpf_freq").map(|f| (f.clamp(10.0, 2000.0), 12)),
                        low_shelf: (f_or(a, "low_freq", 100.0), f_or(a, "low_gain", 0.0), 0.707),
                        mid: (f_or(a, "mid_freq", 1000.0), f_or(a, "mid_gain", 0.0), f_or(a, "mid_q", 1.0)),
                        high_shelf: (f_or(a, "high_freq", 8000.0), f_or(a, "high_gain", 0.0), 0.707),
                        ..Eq7::default()
                    })
                } else {
                    None
                };
                let s = Strip { input_db: f_or(a, "input_gain", 0.0), gate, comp, eq, output_db: f_or(a, "output_gain", 0.0) };
                let mut y = x.clone();
                let (ggr, cgr) = channel_strip(&mut y, &s);
                let t = tone_pair(&x, &y);
                finish(e, a, &info, &x, &y, "strip", "channel strip", json!({"gate_max_reduction_db": r1(ggr), "comp_max_reduction_db": r1(cgr), "tone_db": t}))
            },
        },
        Tool {
            name: "vocal_clean_chain",
            description: "Gentle vocal clean-up in one call (writes a new sample): HPF 90 Hz (18 dB/oct), box cut -3 dB at ~300 Hz, presence +3 dB at 3 kHz, de-ess (6.5 kHz, at most 6 dB, only the loudest esses), a gentle 2:1 expander (at most 10 dB down between phrases), a 3:1 soft-knee compressor, then the voiced level matched to the input. No bandwidth extension and no spectral noise reduction, so nothing smears or dulls the voice. Every stage has a knob (hpf_hz, box_hz/box_db, presence_hz/presence_db, deess/deess_hz/deess_range/deess_threshold, gate/gate_threshold/gate_range, comp/comp_threshold/comp_ratio). track: switch that track's clips to the result. Reports each stage and the tone before/after.",
            mutates: true,
            schema: || common(json!({"hpf_hz": {"type": "number"}, "box_hz": {"type": "number"}, "box_db": {"type": "number"}, "presence_hz": {"type": "number"}, "presence_db": {"type": "number"}, "deess": {"type": "boolean"}, "deess_hz": {"type": "number"}, "deess_range": {"type": "number"}, "deess_threshold": {"type": "number"}, "gate": {"type": "boolean"}, "gate_threshold": {"type": "number"}, "gate_range": {"type": "number"}, "comp": {"type": "boolean"}, "comp_threshold": {"type": "number"}, "comp_ratio": {"type": "number"}})),
            run: |e, a| {
                let (info, x) = crate::tools_sound::sample_data(e, &s_req(a, "sample")?)?;
                if x.len() < (0.2 * SR) as usize {
                    bail!("the sample is too short (under 0.2 s)");
                }
                let (y, rep) = vocal_clean(&x, a);
                let t = tone_pair(&x, &y);
                finish(e, a, &info, &x, &y, "clean", "vocal clean chain", json!({"stages": rep, "tone_db": t}))
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f32, secs: f32, amp: f32) -> Vec<f32> {
        (0..(secs * SR) as usize)
            .map(|i| amp * (std::f32::consts::TAU * f * i as f32 / SR).sin())
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn eq7_curve_matches_its_bands() {
        let eq = Eq7 {
            hpf: Some((90.0, 18)),
            low_mid: (300.0, -3.0, 1.2),
            high_mid: (3000.0, 3.0, 0.9),
            ..Eq7::default()
        };
        assert!(
            (eq.response_db(300.0) + 3.0).abs() < 0.6,
            "{}",
            eq.response_db(300.0)
        );
        assert!(
            (eq.response_db(3000.0) - 3.0).abs() < 0.6,
            "{}",
            eq.response_db(3000.0)
        );
        assert!(eq.response_db(30.0) < -20.0);
        assert!(eq.response_db(1000.0).abs() < 1.5);
        // the processed signal follows the curve
        let mut x = sine(3000.0, 0.5, 0.3);
        let r0 = rms(&x[4800..]);
        eq.process(&mut x);
        let d = 20.0 * (rms(&x[4800..]) / r0).log10();
        assert!((d - 3.0).abs() < 0.7, "{d}");
        // 24 dB/oct LPF is steeper than 12
        let a = Eq7 {
            lpf: Some((2000.0, 24)),
            ..Eq7::default()
        }
        .response_db(8000.0);
        let b = Eq7 {
            lpf: Some((2000.0, 12)),
            ..Eq7::default()
        }
        .response_db(8000.0);
        assert!(a < b - 10.0, "{a} {b}");
    }

    #[test]
    fn gate_ducks_quiet_parts_and_deesser_dips_esses() {
        let mut x = sine(220.0, 0.5, 0.5);
        x.extend(sine(220.0, 0.5, 0.005));
        let thr = auto_gate_threshold(&x);
        assert!(thr < -10.0 && thr > -50.0, "{thr}");
        let mut y = x.clone();
        let (gr, _) = expander_gate(
            &mut y,
            &Gate {
                threshold_db: -30.0,
                ..Gate::default()
            },
        );
        assert!(gr > 20.0);
        assert!(rms(&y[(0.8 * SR) as usize..]) < rms(&x[(0.8 * SR) as usize..]) * 0.2);
        assert!(
            (rms(&y[..(0.4 * SR) as usize]) / rms(&x[..(0.4 * SR) as usize]) - 1.0).abs() < 0.05
        );
        // de-esser: an 8 kHz burst is dipped, a 300 Hz tone is untouched
        let mut s = sine(300.0, 0.4, 0.3);
        let burst = sine(8000.0, 0.2, 0.4);
        s.extend(burst.iter());
        let mut z = s.clone();
        let (dgr, _) = de_ess(
            &mut z,
            &DeEss {
                freq: 6000.0,
                range_db: 10.0,
                threshold_db: -30.0,
            },
        );
        assert!(dgr > 5.0, "{dgr}");
        let n = (0.4 * SR) as usize;
        assert!(rms(&z[n + 2000..]) < rms(&s[n + 2000..]) * 0.7);
        assert!((rms(&z[2000..n]) / rms(&s[2000..n]) - 1.0).abs() < 0.03);
    }

    #[test]
    fn clean_chain_keeps_level_and_brightness() {
        // a voice-like buzz with breaths of noise between phrases
        let mut rng = crate::dsp::Rng::new(5);
        let mut x = Vec::new();
        for p in 0..4 {
            for i in 0..(0.6 * SR) as usize {
                let t = i as f32 / SR;
                let f = 160.0 + 10.0 * p as f32;
                let mut v = 0.0;
                for h in 1..30 {
                    v += (std::f32::consts::TAU * f * h as f32 * t).sin() / h as f32;
                }
                x.push(0.2 * v + 0.004 * rng.bipolar());
            }
            for _ in 0..(0.3 * SR) as usize {
                x.push(0.004 * rng.bipolar());
            }
        }
        let (y, rep) = vocal_clean(&x, &json!({}));
        assert_eq!(y.len(), x.len());
        assert!(y.iter().all(|v| v.is_finite()));
        let t0 = crate::vocal_clarity::tone(&x);
        let t1 = crate::vocal_clarity::tone(&y);
        // presence is not lost (the chain must never dull the voice)
        let p0 = t0["presence_2k_5k"].as_f64().unwrap();
        let p1 = t1["presence_2k_5k"].as_f64().unwrap();
        assert!(p1 >= p0 - 0.5, "presence {p0} -> {p1} ({rep})");
        // the gaps sit further under the voice (a gentle 2:1 expander, not a hard gate)
        let (f0, p0) = crate::vocal_repair::floor_and_peak_db(&x);
        let (f1, p1) = crate::vocal_repair::floor_and_peak_db(&y);
        assert!(
            (p1 - f1) > (p0 - f0) + 2.5,
            "voice-to-gap {} -> {} ({rep})",
            p0 - f0,
            p1 - f1
        );
        assert!(rep["level_match_db"].as_f64().unwrap().abs() <= 12.0);
    }
}
