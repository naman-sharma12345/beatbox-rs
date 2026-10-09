//! Sound engines added in sprint 4: a band-limited morphing wavetable synth,
//! a granular sampler (with a built-in texture when no sample is set) and
//! instrument layering with key / velocity splits.

use crate::dsp::*;
use crate::instruments::*;
use crate::samples::SampleBank;
use std::f32::consts::PI;

const TABLE: usize = 2048;
/// Morph resolution: tables precomputed per note along the position axis.
const FRAMES: usize = 24;

/// (cos, sin) Fourier coefficients of harmonic `k` (1-based) of a table at
/// morph position `x` (0..1) for fundamental `f0`.
fn coeff(table: WtTable, x: f32, k: usize, f0: f32) -> (f32, f32) {
    let kf = k as f32;
    let sgn = |odd_even: bool| if odd_even { 1.0 } else { -1.0 };
    match table {
        WtTable::Basic => {
            // four shapes: sine, triangle, saw, square (sine-phase coefficients)
            let shape = |s: usize| -> f32 {
                match s {
                    0 => {
                        if k == 1 {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    1 => {
                        if k % 2 == 1 {
                            8.0 / (PI * PI) / (kf * kf) * sgn((k - 1) / 2 % 2 == 0)
                        } else {
                            0.0
                        }
                    }
                    2 => 2.0 / PI / kf * sgn(k % 2 == 1),
                    _ => {
                        if k % 2 == 1 {
                            4.0 / PI / kf
                        } else {
                            0.0
                        }
                    }
                }
            };
            let pos = x.clamp(0.0, 1.0) * 3.0;
            let i = (pos.floor() as usize).min(2);
            let f = pos - i as f32;
            (0.0, shape(i) * (1.0 - f) + shape(i + 1) * f)
        }
        WtTable::Harmonic => {
            let n = 1.0 + x.clamp(0.0, 1.0) * 48.0;
            let fade = (n - kf + 1.0).clamp(0.0, 1.0);
            (0.0, fade / kf.powf(0.8))
        }
        WtTable::Pwm => {
            let d = 0.5 - 0.45 * x.clamp(0.0, 1.0);
            ((2.0 / (PI * kf)) * (PI * kf * d).sin(), 0.0)
        }
        WtTable::Vocal => {
            // vowel formants (F1, F2, F3) a -> e -> i -> o -> u
            const V: [[f32; 3]; 5] = [
                [800.0, 1150.0, 2900.0],
                [400.0, 1600.0, 2700.0],
                [300.0, 2300.0, 3000.0],
                [450.0, 800.0, 2830.0],
                [325.0, 700.0, 2530.0],
            ];
            let pos = x.clamp(0.0, 1.0) * 4.0;
            let i = (pos.floor() as usize).min(3);
            let f = pos - i as f32;
            let fr = kf * f0.max(20.0);
            let mut a = 0.0;
            for j in 0..3 {
                let fm = V[i][j] * (1.0 - f) + V[i + 1][j] * f;
                let bw = 80.0 + fm * 0.08;
                a += [1.0, 0.6, 0.25][j] * (-((fr - fm) / bw).powi(2)).exp();
            }
            (0.0, (a + 0.02) / kf.sqrt())
        }
        WtTable::Digital => {
            let m = 2 + (x.clamp(0.0, 1.0) * 13.0) as usize;
            let on = (k * 7 + k / m) % m < m / 2 + 1 || k == 1;
            (0.0, if on { 1.0 / kf.powf(0.7) } else { 0.0 })
        }
        WtTable::Organ => {
            // drawbars 16', 8', 5 1/3', 4', 2 2/3', 2' registrations morphing
            const REG: [[f32; 6]; 3] = [
                [0.0, 1.0, 0.0, 0.6, 0.0, 0.3],
                [0.8, 1.0, 0.8, 0.5, 0.4, 0.3],
                [0.5, 1.0, 1.0, 1.0, 0.8, 1.0],
            ];
            let pos = x.clamp(0.0, 1.0) * 2.0;
            let i = (pos.floor() as usize).min(1);
            let f = pos - i as f32;
            // harmonics relative to a sub-octave fundamental: 16' = h1 of f/2,
            // we render at f so map 8'->1, 4'->2, 5 1/3'->1.5 (skip), 2 2/3'->3, 2'->4
            let w = |r: &[f32; 6]| match k {
                1 => r[1] + r[0] * 0.5,
                2 => r[3],
                3 => r[4] + r[2] * 0.5,
                4 => r[5],
                6 => r[5] * 0.4,
                8 => r[5] * 0.25,
                _ => 0.0,
            };
            (0.0, w(&REG[i]) * (1.0 - f) + w(&REG[i + 1]) * f)
        }
    }
}

/// Single-cycle table at morph `x`, band-limited for fundamental `f0`.
pub fn build_table(table: WtTable, x: f32, f0: f32, fft: &Fft) -> Vec<f32> {
    let max_h = ((SR * 0.45 / f0.max(1.0)) as usize).clamp(1, TABLE / 2 - 1);
    let mut re = vec![0.0f32; TABLE];
    let mut im = vec![0.0f32; TABLE];
    for k in 1..=max_h {
        let (a, b) = coeff(table, x, k, f0);
        // x(t) = a cos + b sin  ->  X[k] = N/2 (a - i b), X[N-k] = conj
        re[k] = a * TABLE as f32 / 2.0;
        im[k] = -b * TABLE as f32 / 2.0;
        re[TABLE - k] = re[k];
        im[TABLE - k] = -im[k];
    }
    fft.inverse(&mut re, &mut im);
    // normalize peak so morphing doesn't jump in level
    let peak = re.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    re.iter_mut().for_each(|v| *v /= peak);
    re
}

fn table_read(t: &[f32], phase: f32) -> f32 {
    let pos = phase * TABLE as f32;
    let i = pos as usize % TABLE;
    let f = pos - pos.floor();
    t[i] * (1.0 - f) + t[(i + 1) % TABLE] * f
}

pub fn render_wavetable(
    p: &WavetableParams,
    pitch: f32,
    vel: f32,
    gate: f32,
    rng: &mut Rng,
) -> Vec<f32> {
    let total = p.amp_env.total(gate).min(12.0);
    let n = (total * SR) as usize;
    let mut out = vec![0.0f32; n];
    let f = midi_to_hz(pitch);
    let voices = p.unison.clamp(1, 7) as usize;
    let fft = Fft::new(TABLE);
    let fmax = f * 2f32.powf(p.unison_spread_cents.abs() / 1200.0) * 1.03;
    let tables: Vec<Vec<f32>> = (0..FRAMES)
        .map(|i| build_table(p.table, i as f32 / (FRAMES - 1) as f32, fmax, &fft))
        .collect();
    let det: Vec<f32> = (0..voices)
        .map(|v| {
            if voices == 1 {
                1.0
            } else {
                let c = (v as f32 / (voices - 1) as f32 - 0.5) * 2.0 * p.unison_spread_cents;
                2f32.powf(c / 1200.0)
            }
        })
        .collect();
    let mut ph: Vec<f32> = (0..voices).map(|_| rng.f32()).collect();
    let mut sub = 0.0f32;
    let norm = 1.0 / (voices as f32).sqrt();
    let mut filt = Svf::default();
    let lfo0 = rng.f32();
    let drive = 1.0 + p.drive.max(0.0) * 8.0;
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let pos = (p.position
            + p.position_env_amount * p.position_env.level(t, gate)
            + p.position_lfo_depth * (2.0 * PI * (p.position_lfo_rate * t + lfo0)).sin())
        .clamp(0.0, 1.0);
        let fp = pos * (FRAMES - 1) as f32;
        let k = (fp as usize).min(FRAMES - 2);
        let fr = fp - k as f32;
        let mut x = 0.0;
        for v in 0..voices {
            let a = table_read(&tables[k], ph[v]);
            let b = table_read(&tables[k + 1], ph[v]);
            x += a + (b - a) * fr;
            ph[v] = (ph[v] + f * det[v] / SR) % 1.0;
        }
        x *= norm;
        if p.sub_level > 0.0 {
            sub = (sub + f * 0.5 / SR) % 1.0;
            x += (2.0 * PI * sub).sin() * p.sub_level;
        }
        let cutoff = p.cutoff * 2f32.powf(p.filter_env_amount * p.filter_env.level(t, gate));
        let mut y = filt.process(x, cutoff, p.resonance, p.filter_mode);
        if drive > 1.01 {
            y = (y * drive).tanh() / drive.tanh().max(0.5);
        }
        *s = y * p.amp_env.level(t, gate) * vel * p.gain;
    }
    out
}

/// The built-in granular source: 2 s of a soft detuned C4 chord.
fn builtin_texture() -> Vec<f32> {
    let n = (2.0 * SR) as usize;
    let mut rng = Rng::new(42);
    let notes = [60.0f32, 67.0, 72.0, 64.0];
    let mut ph: Vec<f32> = (0..8).map(|_| rng.f32()).collect();
    let mut f = Svf::default();
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let mut x = 0.0;
            for (j, m) in notes.iter().enumerate() {
                for d in 0..2 {
                    let idx = j * 2 + d;
                    let hz = midi_to_hz(*m) * if d == 0 { 0.997 } else { 1.004 };
                    ph[idx] = (ph[idx] + hz / SR) % 1.0;
                    x += (2.0 * ph[idx] - 1.0) * 0.12 + (2.0 * PI * ph[idx]).sin() * 0.1;
                }
            }
            let cut = 1800.0 + 1200.0 * (2.0 * PI * 0.5 * t).sin();
            f.process(x + rng.bipolar() * 0.01, cut, 0.2, FilterMode::Lowpass)
        })
        .collect()
}

pub fn render_granular(
    p: &GranularParams,
    pitch: f32,
    vel: f32,
    gate: f32,
    bank: &SampleBank,
    rng: &mut Rng,
) -> Vec<f32> {
    let owned;
    let src: &[f32] = match bank.get(&p.sample).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => {
            owned = builtin_texture();
            &owned
        }
    };
    let total = p.amp_env.total(gate).min(12.0);
    let n = (total * SR) as usize;
    let mut out = vec![0.0f32; n];
    let glen = ((p.grain_ms.clamp(5.0, 500.0) * 0.001 * SR) as usize).max(16);
    let density = p.density.clamp(1.0, 400.0);
    let base_rate = 2f32.powf((pitch - p.root as f32) / 12.0);
    let mut t = 0.0f32;
    let overlap = (density * glen as f32 / SR).max(1.0);
    let norm = 1.0 / overlap.sqrt();
    let len = src.len() as f32;
    while (t * SR) < n as f32 {
        let start = (t * SR) as usize;
        let pos = (p.position + p.scan * t + p.spray * rng.bipolar() * 0.5).rem_euclid(1.0);
        let cents = p.pitch_spread_cents * rng.bipolar();
        let rate = base_rate * 2f32.powf(cents / 1200.0);
        let rev = rng.chance(p.reverse_prob);
        let mut read = pos * len;
        let step = if rev { -rate } else { rate };
        for k in 0..glen {
            let o = start + k;
            if o >= n {
                break;
            }
            let w = 0.5 - 0.5 * (2.0 * PI * k as f32 / glen as f32).cos();
            let rp = read.rem_euclid(len - 1.0);
            out[o] += lerp_read(src, rp) * w * norm;
            read += step;
        }
        // jittered grain clock
        t += (1.0 + 0.3 * rng.bipolar()) / density;
    }
    let mut f = Svf::default();
    for (i, s) in out.iter_mut().enumerate() {
        let tt = i as f32 / SR;
        let y = if p.cutoff < 19000.0 {
            f.process(*s, p.cutoff, 0.1, FilterMode::Lowpass)
        } else {
            *s
        };
        *s = y * p.amp_env.level(tt, gate) * vel * p.gain;
    }
    out
}

pub fn render_layer(
    p: &LayerParams,
    pitch: f32,
    vel: f32,
    gate: f32,
    bank: &SampleBank,
    seed: u64,
) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::new();
    for (i, l) in p.layers.iter().enumerate() {
        let key = pitch.round() as i32;
        if key < l.key_min as i32 || key > l.key_max as i32 || vel < l.vel_min || vel > l.vel_max {
            continue;
        }
        // nested layers are flattened one level deep to bound recursion
        if matches!(l.instrument, Instrument::Layer(_)) {
            continue;
        }
        let buf = render_note(
            &l.instrument,
            pitch + l.transpose,
            vel,
            gate,
            bank,
            seed.wrapping_add(i as u64 * 7919),
        );
        let off = (l.delay_ms.max(0.0) * 0.001 * SR) as usize;
        let g = db_to_gain(l.gain_db);
        if out.len() < buf.len() + off {
            out.resize(buf.len() + off, 0.0);
        }
        for (k, v) in buf.iter().enumerate() {
            out[off + k] += v * g;
        }
    }
    out
}

// ---------------- modelled piano ----------------

/// Additive piano: inharmonic partials (stiff strings), 1-3 detuned strings
/// per key, hammer-position comb, velocity-dependent brightness, two-stage
/// (prompt + aftersound) decay, damper release and a hammer thump.
pub fn render_piano(p: &PianoParams, pitch: f32, vel: f32, gate: f32, rng: &mut Rng) -> Vec<f32> {
    let f0 = midi_to_hz(pitch);
    let key = pitch.clamp(21.0, 108.0);
    // decay shortens up the keyboard
    let base_decay = p.decay.max(0.1) * (12.0 * (-(key - 21.0) / 30.0).exp() + 0.8);
    let hold = if p.sustain_pedal {
        gate.max(base_decay)
    } else {
        gate
    };
    let total = (hold + p.release.max(0.02) * 3.0)
        .min(base_decay * 1.4 + 0.5)
        .min(14.0);
    let n = (total * SR) as usize;
    let mut out = vec![0.0f32; n];
    let b = 0.00008 * 2f32.powf((key - 60.0) / 14.0); // inharmonicity
    let strings = if key < 34.0 {
        1
    } else if key < 48.0 {
        2
    } else {
        3
    };
    let bright = (0.35 + 0.65 * vel) * p.brightness.clamp(0.0, 2.0);
    let max_k = ((SR * 0.45 / f0) as usize).clamp(1, 48);
    let strike = 1.0 / 7.5;
    let rel_c = (-1.0 / (p.release.max(0.02) * SR)).exp();
    for k in 1..=max_k {
        let kf = k as f32;
        let fk = f0 * kf * (1.0 + b * kf * kf).sqrt();
        if fk > SR * 0.45 {
            break;
        }
        // hammer spectrum: comb from strike position * soft lowpass by velocity
        let comb = (PI * kf * strike).sin().abs().max(0.08);
        let tilt = (-(kf - 1.0) / (2.0 + 10.0 * bright)).exp();
        let felt = if p.felt > 0.0 {
            (-(fk / (1200.0 + 3000.0 * vel)) * p.felt * 2.0).exp()
        } else {
            1.0
        };
        let amp = comb * tilt * felt / kf.powf(0.6);
        if amp < 1e-4 {
            continue;
        }
        // partial decay: higher partials die faster
        let tau1 = (base_decay * 0.18) / (1.0 + 0.15 * kf);
        let tau2 = base_decay / (1.0 + 0.04 * kf * kf.sqrt());
        for s in 0..strings {
            let det = (s as f32 - (strings - 1) as f32 / 2.0) * p.detune_cents;
            let fr = fk * 2f32.powf(det / 1200.0);
            let w = 2.0 * PI * fr / SR;
            let (mut y1, mut y2) = (0.0f32, -(w).sin());
            let c = 2.0 * w.cos();
            let d1 = (-1.0 / (tau1 * SR)).exp();
            let d2 = (-1.0 / (tau2 * SR)).exp();
            let (mut e1, mut e2) = (0.55f32, 0.45f32);
            let mut damp = 1.0f32;
            let gate_n = (hold * SR) as usize;
            let a = amp / strings as f32;
            for (i, o) in out.iter_mut().enumerate() {
                // sine via recurrence
                let y = c * y1 - y2;
                y2 = y1;
                y1 = y;
                e1 *= d1;
                e2 *= d2;
                if i > gate_n {
                    damp *= rel_c;
                    if damp < 1e-4 {
                        break;
                    }
                }
                *o += y * (e1 + e2) * damp * a;
            }
        }
    }
    // hammer thump + key noise
    let thump_n = ((0.012 + 0.01 * (1.0 - vel)) * SR) as usize;
    let mut lp = Svf::default();
    for i in 0..thump_n.min(n) {
        let t = i as f32 / thump_n as f32;
        let x = rng.bipolar() * (1.0 - t).powi(3) * p.hammer * 0.35 * vel;
        out[i] += lp.process(x, 900.0 + 2500.0 * vel, 0.1, FilterMode::Lowpass);
    }
    // soundboard: gentle body resonance
    if p.body > 0.0 {
        let mut b1 = Biquad::new(BiquadKind::Bell, 180.0, 1.2, 3.0 * p.body);
        let mut b2 = Biquad::new(BiquadKind::Bell, 2600.0, 1.5, -2.0 * p.body);
        for v in out.iter_mut() {
            *v = b2.process(b1.process(*v));
        }
    }
    let g = vel.powf(0.7) * p.gain;
    let fade = (0.003 * SR) as usize;
    let len = out.len();
    for (i, v) in out.iter_mut().enumerate() {
        let a = (i as f32 / 20.0).min(1.0);
        let e = ((len - i) as f32 / fade as f32).min(1.0);
        *v *= g * a * e;
    }
    out
}

// ---------------- ensembles: strings / choir / brass ----------------

fn formant_gain(kind: EnsembleKind, hz: f32, vowel: f32) -> f32 {
    let peaks: &[(f32, f32, f32)] = match kind {
        // violin-family body resonances (air, wood, bridge hill)
        EnsembleKind::Strings => &[
            (280.0, 120.0, 1.0),
            (460.0, 200.0, 0.8),
            (1100.0, 500.0, 0.6),
            (2600.0, 900.0, 0.9),
            (4200.0, 1500.0, 0.35),
        ],
        EnsembleKind::Brass => &[
            (500.0, 300.0, 0.6),
            (1200.0, 600.0, 1.0),
            (2500.0, 1200.0, 0.7),
        ],
        EnsembleKind::Choir => &[],
    };
    if kind == EnsembleKind::Choir {
        // "oo" (0) -> "ah" (1)
        let oo = [(320.0, 0.9), (800.0, 0.35), (2500.0, 0.08)];
        let ah = [(700.0, 1.0), (1150.0, 0.6), (2800.0, 0.2)];
        let mut g = 0.02;
        for j in 0..3 {
            let f = oo[j].0 + (ah[j].0 - oo[j].0) * vowel;
            let a = oo[j].1 + (ah[j].1 - oo[j].1) * vowel;
            let bw = 70.0 + f * 0.1;
            g += a * (-((hz - f) / bw).powi(2)).exp();
        }
        return g;
    }
    let mut g = 0.08;
    for (f, bw, a) in peaks {
        g += a * (-((hz - f) / bw).powi(2)).exp();
    }
    g
}

/// Section ensemble: `players` independent voices, each with its own
/// vibrato rate/depth, slow pitch drift and onset jitter, through a
/// body/formant-shaped band-limited spectrum, plus bow/breath noise.
pub fn render_ensemble(
    p: &EnsembleParams,
    pitch: f32,
    vel: f32,
    gate: f32,
    rng: &mut Rng,
) -> Vec<f32> {
    let f0 = midi_to_hz(pitch);
    let total = p.amp_env.total(gate).min(14.0);
    let n = (total * SR) as usize;
    let mut out = vec![0.0f32; n];
    // spectrum -> one band-limited single cycle per note
    let fft = Fft::new(TABLE);
    let max_h = ((SR * 0.45 / (f0 * 1.02)) as usize).clamp(1, TABLE / 2 - 1);
    let mut re = vec![0.0f32; TABLE];
    let mut im = vec![0.0f32; TABLE];
    let bright = p.brightness.clamp(0.0, 2.0) * (0.6 + 0.4 * vel);
    for k in 1..=max_h {
        let kf = k as f32;
        let hz = f0 * kf;
        let src = match p.kind {
            EnsembleKind::Strings => 1.0 / kf, // sawtooth-like bowed string
            EnsembleKind::Brass => 1.0 / kf.powf(0.8 - 0.4 * vel.min(1.0)),
            EnsembleKind::Choir => 1.0 / kf.powf(1.2),
        };
        let roll = (-(hz / (3000.0 + 6000.0 * bright))).exp();
        let a = src * formant_gain(p.kind, hz, p.vowel) * roll;
        let phase = rng.f32() * 2.0 * PI;
        re[k] = a * phase.cos() * TABLE as f32 / 2.0;
        im[k] = -a * phase.sin() * TABLE as f32 / 2.0;
        re[TABLE - k] = re[k];
        im[TABLE - k] = -im[k];
    }
    fft.inverse(&mut re, &mut im);
    let pk = re.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    let table: Vec<f32> = re.iter().map(|v| v / pk).collect();
    let players = p.players.clamp(1, 12) as usize;
    let norm = 1.0 / (players as f32).sqrt();
    for _ in 0..players {
        let vib_rate = 4.6 + rng.f32() * 1.6;
        let vib_depth = p.vibrato * (0.6 + 0.8 * rng.f32()) * 0.18; // semitones
        let drift_rate = 0.15 + rng.f32() * 0.3;
        let drift = (rng.bipolar() * p.detune_cents) / 100.0;
        let onset = (rng.f32() * p.attack_jitter_ms * 0.001 * SR) as usize;
        let mut ph = rng.f32();
        let ph_v = rng.f32();
        let ph_d = rng.f32();
        // vibrato fades in after the attack, like real players
        let vib_delay = 0.25f32;
        for i in onset..n {
            let t = (i - onset) as f32 / SR;
            let vfade = ((t - vib_delay) / 0.4).clamp(0.0, 1.0);
            let semis = drift * (1.0 + 0.3 * (2.0 * PI * (drift_rate * t + ph_d)).sin())
                + vib_depth * vfade * (2.0 * PI * (vib_rate * t + ph_v)).sin();
            let f = f0 * 2f32.powf(semis / 12.0);
            ph = (ph + f / SR) % 1.0;
            out[i] += table_read(&table, ph) * norm;
        }
    }
    // bow / breath noise, band-limited around the formants
    if p.noise > 0.0 {
        let mut bp = Svf::default();
        let center = match p.kind {
            EnsembleKind::Strings => 2500.0,
            EnsembleKind::Brass => 1500.0,
            EnsembleKind::Choir => 3200.0,
        };
        for v in out.iter_mut() {
            *v += bp.process(rng.bipolar(), center, 0.3, FilterMode::Bandpass) * p.noise * 0.08;
        }
    }
    let mut lp = Svf::default();
    let cutoff = (2500.0 + 9000.0 * bright).min(18000.0);
    for (i, v) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let y = lp.process(*v, cutoff, 0.05, FilterMode::Lowpass);
        *v = y * p.amp_env.level(t, gate) * (0.35 + 0.65 * vel) * p.gain;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zero_crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count()
    }

    #[test]
    fn tables_are_bandlimited_and_morph() {
        let fft = Fft::new(TABLE);
        let sine = build_table(WtTable::Basic, 0.0, 100.0, &fft);
        let square = build_table(WtTable::Basic, 1.0, 100.0, &fft);
        assert_eq!(zero_crossings(&sine), 1);
        // square spends most time near +-1, sine doesn't
        let near = |t: &[f32]| t.iter().filter(|v| v.abs() > 0.8).count();
        assert!(near(&square) > near(&sine));
        // high note: few harmonics -> almost sine
        let hi = build_table(WtTable::Basic, 0.66, 8000.0, &fft);
        assert!(zero_crossings(&hi) <= 2);
    }

    #[test]
    fn every_table_renders_pitched_audio() {
        for t in [
            WtTable::Basic,
            WtTable::Harmonic,
            WtTable::Pwm,
            WtTable::Vocal,
            WtTable::Digital,
            WtTable::Organ,
        ] {
            let p = WavetableParams {
                table: t,
                cutoff: 18000.0,
                ..Default::default()
            };
            let buf = render_wavetable(&p, 57.0, 0.9, 0.5, &mut Rng::new(1));
            assert!(buf.iter().all(|v| v.is_finite()));
            let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(peak > 0.05 && peak < 2.0, "{t:?} {peak}");
        }
    }

    #[test]
    fn granular_and_layers_render() {
        let bank = SampleBank::default();
        let g = render_granular(
            &GranularParams::default(),
            60.0,
            0.8,
            1.0,
            &bank,
            &mut Rng::new(3),
        );
        assert!(g.iter().all(|v| v.is_finite()));
        assert!(g.iter().fold(0.0f32, |m, v| m.max(v.abs())) > 0.02);
        let l = preset("layered_kick").unwrap();
        let k = render_note(&l, 60.0, 1.0, 0.2, &bank, 1);
        let single = render_note(&preset("kick").unwrap(), 60.0, 1.0, 0.2, &bank, 1);
        assert!(k.len() >= single.len());
        // key split: layer out of range is silent
        let split = LayerParams {
            layers: vec![Layer {
                key_max: 50,
                ..Layer::of(preset("sub_bass").unwrap())
            }],
        };
        assert!(render_layer(&split, 72.0, 0.8, 0.3, &bank, 1).is_empty());
        assert!(!render_layer(&split, 40.0, 0.8, 0.3, &bank, 1).is_empty());
    }

    #[test]
    fn piano_and_ensembles_render_pitched_decaying_audio() {
        let p = render_piano(&PianoParams::default(), 60.0, 0.8, 0.5, &mut Rng::new(1));
        assert!(p.iter().all(|v| v.is_finite()));
        let peak = p.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.05 && peak < 2.0, "{peak}");
        // decays: last 10% much quieter than the first 10%
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let k = p.len() / 10;
        assert!(rms(&p[p.len() - k..]) < rms(&p[..k]) * 0.3);
        for kind in [
            EnsembleKind::Strings,
            EnsembleKind::Choir,
            EnsembleKind::Brass,
        ] {
            let e = render_ensemble(
                &EnsembleParams {
                    kind,
                    ..Default::default()
                },
                57.0,
                0.7,
                1.0,
                &mut Rng::new(2),
            );
            assert!(e.iter().all(|v| v.is_finite()));
            let pk = e.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(pk > 0.05 && pk < 2.0, "{kind:?} {pk}");
        }
    }
}
