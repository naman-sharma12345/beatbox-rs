//! Production-grade drum and 808 voices (sprint 9, sound palette).
//!
//! The first-generation voices were one sine plus one noise each: thin,
//! static, and identical on every hit. These are built the way a sound
//! designer layers a one-shot:
//!
//! * kick: beater click (band-passed noise + a short high blip) over a
//!   two-stage pitch-swept sine body, soft-clipped for knock;
//! * snare: stick crack, two tuned shell modes, and a band-passed wire tail;
//! * clap: four jittered bursts and a room tail;
//! * hats / cymbals: six band-limited square oscillators at the 808 metal
//!   ratios through a band-pass + high-pass, with velocity-dependent tone;
//! * 808: two-stage pitch envelope, asymmetric drive (even harmonics) with a
//!   tone filter, a clean sub layer under it, a click layer, and
//!   raised-cosine release so it never ends on a step.
//!
//! Every hit draws a small, deterministic round-robin variation (tune,
//! decay, level, tone) from the note's seed, so a 16th-note pattern does
//! not machine-gun. All voices start at zero and end at zero.

use crate::dsp::*;
use std::f32::consts::PI;

/// Round-robin micro-variation for one hit.
#[derive(Clone, Copy, Debug)]
pub struct Variation {
    /// Semitones.
    pub tune: f32,
    /// Decay multiplier.
    pub decay: f32,
    /// Linear gain.
    pub level: f32,
    /// Octaves of filter / brightness shift.
    pub tone: f32,
}

impl Variation {
    pub const NONE: Variation = Variation {
        tune: 0.0,
        decay: 1.0,
        level: 1.0,
        tone: 0.0,
    };

    /// Small, musical spread: +-0.12 st, +-6 % decay, +-0.7 dB, +-0.12 oct.
    pub fn draw(rng: &mut Rng, amount: f32) -> Variation {
        let a = amount.clamp(0.0, 2.0);
        Variation {
            tune: rng.bipolar() * 0.12 * a,
            decay: 1.0 + rng.bipolar() * 0.06 * a,
            level: db_to_gain(rng.bipolar() * 0.7 * a),
            tone: rng.bipolar() * 0.12 * a,
        }
    }
}

fn st(semis: f32) -> f32 {
    2f32.powf(semis / 12.0)
}

/// Raised-cosine fade-in over `n` samples and fade-out over `m` samples.
pub fn fade_edges(x: &mut [f32], n_in: usize, n_out: usize) {
    let len = x.len();
    let n_in = n_in.min(len / 4);
    for (i, s) in x.iter_mut().take(n_in).enumerate() {
        let g = 0.5 - 0.5 * (PI * i as f32 / n_in as f32).cos();
        *s *= g;
    }
    let n_out = n_out.min(len / 2).max(1);
    for j in 0..n_out {
        let i = len - 1 - j;
        let g = 0.5 - 0.5 * (PI * j as f32 / n_out as f32).cos();
        x[i] *= g;
    }
}

/// One-pole low-pass (TPT), cheap tone control.
#[derive(Clone, Copy, Default)]
struct OnePole {
    s: f32,
}

impl OnePole {
    fn lp(&mut self, x: f32, hz: f32) -> f32 {
        let g = (PI * hz.clamp(10.0, SR * 0.45) / SR).tan();
        let g = g / (1.0 + g);
        let v = (x - self.s) * g;
        let y = v + self.s;
        self.s = y + v;
        y
    }
}

/// Very gentle DC blocker (~3.5 Hz) that leaves a 30 Hz sub untouched.
#[derive(Clone, Copy, Default, Debug)]
pub struct SubDc {
    x1: f32,
    y1: f32,
}

impl SubDc {
    pub fn process(&mut self, x: f32) -> f32 {
        let y = x - self.x1 + 0.9995 * self.y1;
        self.x1 = x;
        self.y1 = y;
        y
    }
}

/// Kick: click + swept body + knock. `tune` is a pitch ratio, `d` a decay
/// multiplier, `tone` -1 (soft, round) .. 1 (hard, clicky).
pub fn kick(tune: f32, d: f32, vel: f32, tone: f32, v: Variation, rng: &mut Rng) -> Vec<f32> {
    let dd = d * v.decay;
    let len = 0.7 * dd + 0.05;
    let mut out = vec![0.0f32; (len * SR) as usize];
    let f_end = 47.0 * tune * st(v.tune);
    let mut ph = 0.0f32;
    let mut bp = Svf::default();
    let mut lp = OnePole::default();
    let bright = 2f32.powf(tone * 0.8 + v.tone);
    let click_amt = (0.35 + 0.3 * tone.max(-0.8)) * (0.35 + 0.65 * vel * vel);
    let knock = 0.8 + 0.3 * vel + 0.2 * tone.max(0.0);
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        // two-stage sweep: a fast drop (the "tick" of pitch) and a slower
        // settle (the "boom"), landing on the tuned fundamental
        let f = f_end * (1.0 + 3.4 * (-t / 0.011).exp() + 0.55 * (-t / 0.055).exp());
        ph = (ph + f / SR) % 1.0;
        let amp = 0.72 * (-t * 6.0 / dd).exp() + 0.28 * (-t * 25.0).exp();
        let body = (2.0 * PI * ph).sin() * amp;
        // beater: band-passed noise burst + a 3 ms high blip
        let n = bp.process(rng.bipolar(), 3800.0 * bright, 0.25, FilterMode::Bandpass);
        let blip = (2.0 * PI * 1700.0 * bright * t).sin() * (-t * 700.0).exp();
        let click = lp.lp((n * 1.4 + blip * 0.6) * (-t * 260.0).exp(), 9000.0 * bright);
        let x = body + click * click_amt;
        *s = (knock * x).tanh() / knock.tanh();
    }
    fade_edges(&mut out, 8, (0.04 * SR) as usize);
    out
}

/// Snare: crack + shell modes + wires.
pub fn snare(tune: f32, d: f32, vel: f32, tone: f32, v: Variation, rng: &mut Rng) -> Vec<f32> {
    let dd = d * v.decay;
    let mut out = vec![0.0f32; ((0.36 * dd + 0.06) * SR) as usize];
    let tr = tune * st(v.tune);
    let bright = 2f32.powf(tone * 0.6 + v.tone + (vel - 0.8) * 0.5);
    let (mut p1, mut p2) = (0.0f32, 0.0f32);
    let mut crack_hp = Svf::default();
    let mut wire_bp = Svf::default();
    let mut wire_hp = Svf::default();
    let mut wire_lp = OnePole::default();
    let wire_amt = 0.55 + 0.35 * vel;
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let drop = 1.0 + 0.45 * (-t * 45.0).exp();
        p1 = (p1 + 185.0 * tr * drop / SR) % 1.0;
        p2 = (p2 + 332.0 * tr * drop / SR) % 1.0;
        let shell = ((2.0 * PI * p1).sin() * 0.62 + (2.0 * PI * p2).sin() * 0.38)
            * (0.75 * (-t * 26.0).exp() + 0.25 * (-t * 9.0 / dd).exp());
        let noise = rng.bipolar();
        let crack = crack_hp.process(noise, 2400.0 * bright, 0.15, FilterMode::Highpass)
            * (-t * 220.0).exp();
        let w = wire_bp.process(noise, 5200.0 * bright, 0.12, FilterMode::Bandpass) * 0.9
            + wire_hp.process(noise, 1900.0 * bright, 0.05, FilterMode::Highpass) * 0.45;
        let w = wire_lp.lp(w, 11_000.0 * bright);
        let wires = w * (0.6 * (-t * 13.0 / dd).exp() + 0.4 * (-t * 32.0).exp());
        let x = shell * 0.85 + crack * 0.5 + wires * wire_amt;
        *s = (1.3 * x).tanh() / 1.3f32.tanh();
    }
    fade_edges(&mut out, 6, (0.008 * SR) as usize);
    out
}

/// Clap: four jittered bursts and a short room tail.
pub fn clap(tune: f32, d: f32, vel: f32, tone: f32, v: Variation, rng: &mut Rng) -> Vec<f32> {
    let dd = d * v.decay;
    let mut out = vec![0.0f32; ((0.42 * dd + 0.06) * SR) as usize];
    let mut offs = [0.0f32; 4];
    let mut acc = 0.0;
    for (k, o) in offs.iter_mut().enumerate() {
        *o = acc;
        acc += if k == 0 { 0.008 } else { 0.009 } + rng.f32() * 0.004;
    }
    let tail_at = offs[3];
    let bright = 2f32.powf(tone * 0.5 + v.tone + (vel - 0.8) * 0.4);
    let mut bp = Svf::default();
    let mut bp2 = Svf::default();
    let mut lp = OnePole::default();
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let noise = rng.bipolar();
        let n = bp.process(noise, 1250.0 * tune * bright, 0.3, FilterMode::Bandpass)
            + 0.35 * bp2.process(noise, 2000.0 * tune * bright, 0.2, FilterMode::Bandpass);
        let n = lp.lp(n, 9000.0 * bright);
        let mut env = 0.0f32;
        for (k, o) in offs.iter().enumerate() {
            let dt = t - o;
            if dt >= 0.0 {
                let a = if k == 3 { 1.0 } else { 0.8 };
                env = env.max(a * (dt / 0.0008).min(1.0) * (-dt * 160.0).exp());
            }
        }
        let dt = t - tail_at;
        if dt >= 0.0 {
            env = env.max(0.62 * (-dt * 11.0 / dd).exp());
        }
        *s = (n * env * 2.4).tanh();
    }
    fade_edges(&mut out, 6, (0.01 * SR) as usize);
    out
}

/// 808 metal ratios (Hz at tune 1).
const METAL: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

/// Hats and cymbals: band-limited metal + noise. `kind`: 0 closed, 1 open,
/// 2 crash.
pub fn metal(
    kind: u8,
    tune: f32,
    d: f32,
    vel: f32,
    tone: f32,
    v: Variation,
    rng: &mut Rng,
) -> Vec<f32> {
    let dd = d * v.decay;
    let len = match kind {
        0 => 0.075 * dd + 0.03,
        1 => 0.6 * dd + 0.05,
        _ => 2.2 * dd + 0.1,
    };
    let mut out = vec![0.0f32; (len * SR) as usize];
    let tr = tune * st(v.tune);
    // velocity-dependent tone: soft hits are darker and less metallic
    let bright = 2f32.powf(tone * 0.5 + v.tone + (vel - 0.85) * 0.7);
    let mut phases: [f32; 6] = [0.0; 6];
    for p in phases.iter_mut() {
        *p = rng.f32();
    }
    let mut bp = Svf::default();
    let mut hp = Svf::default();
    let mut nhp = Svf::default();
    let mut lp = OnePole::default();
    let (hp_hz, metal_amt, noise_amt) = match kind {
        0 => (7600.0, 0.55, 0.45),
        1 => (6800.0, 0.6, 0.4),
        _ => (4800.0, 0.35, 0.65),
    };
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let mut m = 0.0;
        for (k, r) in METAL.iter().enumerate() {
            let dt = r * 1.75 * tr / SR;
            m += osc(Wave::Square, phases[k], dt, rng);
            phases[k] = (phases[k] + dt) % 1.0;
        }
        let m = bp.process(m / 6.0, 10_000.0 * bright, 0.35, FilterMode::Bandpass);
        let n = nhp.process(rng.bipolar(), 8000.0 * bright, 0.1, FilterMode::Highpass);
        let x = hp.process(
            m * metal_amt * 2.2 + n * noise_amt,
            hp_hz * bright.sqrt(),
            0.2,
            FilterMode::Highpass,
        );
        let x = lp.lp(x, 15_000.0 * bright.min(1.2));
        let env = match kind {
            0 => (-t * 58.0 / dd).exp(),
            1 => 0.3 * (-t * 30.0).exp() + 0.7 * (-t * 5.0 / dd).exp(),
            _ => 0.35 * (-t * 14.0).exp() + 0.65 * (-t * 1.7 / dd).exp(),
        };
        *s = x * env * 1.9;
    }
    fade_edges(&mut out, 4, (0.006 * SR) as usize);
    out
}

/// Parameters of the 808 voice (mirrors `Bass808Params`).
pub struct Bass808 {
    pub decay: f32,
    pub punch: f32,
    pub drive: f32,
    pub sustain: bool,
    pub gain: f32,
    pub glide_s: f32,
    pub click: f32,
    pub sub: f32,
    pub tone: f32,
}

/// 808: sub + driven body + click, tuned glide, click-free release.
pub fn bass808(
    p: &Bass808,
    pitch: f32,
    vel: f32,
    gate: f32,
    slide_to: Option<f32>,
    rng: &mut Rng,
) -> Vec<f32> {
    let f = midi_to_hz(pitch);
    let sustain = p.sustain || slide_to.is_some();
    let rel = 0.06f32;
    let total = if sustain {
        gate + rel
    } else {
        p.decay.max(0.1) + 0.1
    }
    .min(8.0);
    let n = (total * SR) as usize;
    let mut out = vec![0.0f32; n];
    let drive = 1.0 + p.drive.clamp(0.0, 1.0) * 14.0;
    let bias = 0.32 * p.drive.clamp(0.0, 1.0);
    let sub = p.sub.clamp(0.0, 1.0);
    let tone_hz = 1500.0 * 2f32.powf(p.tone.clamp(-2.0, 2.0)) * (0.6 + 0.4 * vel);
    let click = p.click.clamp(0.0, 1.0) * (0.4 + 0.6 * vel);
    let mut ph = 0.0f32;
    let mut lp = OnePole::default();
    let mut lp2 = OnePole::default();
    let mut dc = SubDc::default();
    let mut cbp = Svf::default();
    let norm = drive.tanh();
    let rel_n = (rel * SR) as usize;
    let gate_n = (gate.max(0.0) * SR) as usize;
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let semis = p.punch * (0.7 * (-t / 0.012).exp() + 0.3 * (-t / 0.05).exp())
            + crate::instruments::glide_semis(t, pitch, slide_to, p.glide_s, gate);
        let fr = f * st(semis);
        ph = (ph + fr / SR) % 1.0;
        let y = (2.0 * PI * ph).sin();
        // asymmetric drive: odd + even harmonics that read on phone speakers
        let driven = ((y * drive + bias).tanh() - bias.tanh()) / norm;
        let driven = dc.process(lp2.lp(lp.lp(driven, tone_hz), tone_hz * 1.6));
        let body = y * sub + driven * (1.0 - sub);
        let c = cbp.process(rng.bipolar(), 1600.0, 0.3, FilterMode::Bandpass) * (-t * 500.0).exp()
            + (2.0 * PI * 900.0 * t).sin() * (-t * 350.0).exp() * 0.5;
        let env = if sustain {
            let held = 0.82 + 0.18 * (-t * 7.0).exp();
            let r = if i >= gate_n {
                let j = (i - gate_n) as f32 / rel_n.max(1) as f32;
                0.5 + 0.5 * (PI * j.min(1.0)).cos()
            } else {
                1.0
            };
            held * r
        } else {
            (-t * 4.5 / p.decay.max(0.1)).exp()
        };
        *s = (body + c * click * 0.35) * env * vel * p.gain;
    }
    fade_edges(&mut out, (0.0015 * SR) as usize, (0.012 * SR) as usize);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0, |m, v| m.max(v.abs()))
    }

    fn mean(x: &[f32]) -> f32 {
        x.iter().sum::<f32>() / x.len().max(1) as f32
    }

    fn all_voices(seed: u64) -> Vec<(&'static str, Vec<f32>)> {
        let mut r = Rng::new(seed);
        let v = Variation::draw(&mut Rng::new(seed ^ 77), 1.0);
        let p808 = Bass808 {
            decay: 1.2,
            punch: 12.0,
            drive: 0.6,
            sustain: false,
            gain: 0.85,
            glide_s: 0.09,
            click: 0.4,
            sub: 0.5,
            tone: 0.0,
        };
        vec![
            ("kick", kick(1.0, 1.0, 1.0, 0.0, v, &mut r)),
            ("kick_hard", kick(1.0, 1.0, 1.0, 1.0, v, &mut r)),
            ("snare", snare(1.0, 1.0, 1.0, 0.0, v, &mut r)),
            ("clap", clap(1.0, 1.0, 1.0, 0.0, v, &mut r)),
            ("hat", metal(0, 1.0, 1.0, 1.0, 0.0, v, &mut r)),
            ("open_hat", metal(1, 1.0, 1.0, 1.0, 0.0, v, &mut r)),
            ("crash", metal(2, 1.0, 1.0, 1.0, 0.0, v, &mut r)),
            ("808", bass808(&p808, 36.0, 1.0, 0.5, None, &mut r)),
            (
                "808_slide",
                bass808(&p808, 36.0, 1.0, 0.5, Some(43.0), &mut r),
            ),
        ]
    }

    #[test]
    fn voices_are_deterministic() {
        let a = all_voices(5);
        let b = all_voices(5);
        for ((n, x), (_, y)) in a.iter().zip(b.iter()) {
            assert_eq!(x, y, "{n} not deterministic");
        }
    }

    #[test]
    fn voices_no_clip_no_dc_no_boundary_clicks() {
        for seed in 1..4 {
            for (name, x) in all_voices(seed) {
                assert!(x.iter().all(|v| v.is_finite()), "{name}");
                let pk = peak(&x);
                assert!(pk > 0.05, "{name} silent {pk}");
                // raw layers; the instrument wrapper normalises drums
                assert!(pk < 2.5, "{name} runaway level {pk}");
                assert!(mean(&x).abs() < 0.01, "{name} DC {}", mean(&x));
                assert!(x[0].abs() < 1e-3, "{name} starts at {}", x[0]);
                assert!(
                    x[x.len() - 1].abs() < 1e-3,
                    "{name} ends at {}",
                    x[x.len() - 1]
                );
                let c = crate::ears::clicks(&x, 10);
                // drums may legitimately have a click at the onset (the
                // beater); nothing after the first 15 ms
                let late: Vec<_> = c
                    .iter()
                    .filter(|(i, _)| *i > (0.015 * SR) as usize)
                    .collect();
                assert!(late.is_empty(), "{name} clicks at {late:?}");
            }
        }
    }

    #[test]
    fn rendered_presets_have_headroom_and_clean_edges() {
        let bank = crate::samples::SampleBank::default();
        for name in [
            "kick", "snare", "clap", "hat", "open_hat", "crash", "kick_punchy", "kick_grit",
            "snare_dusty", "hat_crisp", "808", "808_grit", "808_slide", "808_clean",
        ] {
            let inst = crate::instruments::preset(name).unwrap();
            for seed in 0..4u64 {
                let x = crate::instruments::render_note(&inst, 60.0 - 24.0 * (name.starts_with("808") as u8 as f32), 1.0, 0.4, &bank, seed);
                let pk = peak(&x);
                assert!(pk <= 0.95, "{name} peak {pk}");
                assert!(x[0].abs() < 1e-3 && x[x.len() - 1].abs() < 1e-3, "{name} edges");
                assert!(mean(&x).abs() < 0.01, "{name} DC");
            }
        }
    }

    #[test]
    fn round_robin_varies_but_stays_close() {
        let mk = |s: u64| {
            let v = Variation::draw(&mut Rng::new(s), 1.0);
            snare(1.0, 1.0, 1.0, 0.0, v, &mut Rng::new(s))
        };
        let (a, b) = (mk(1), mk(2));
        assert_ne!(a, b);
        let ea: f32 = a.iter().map(|v| v * v).sum();
        let eb: f32 = b.iter().map(|v| v * v).sum();
        let ratio = 10.0 * (ea / eb).log10();
        assert!(ratio.abs() < 2.5, "rr level spread {ratio} dB");
    }

    #[test]
    fn kick_has_weight_and_808_has_harmonics() {
        let mut r = Rng::new(1);
        let k = kick(1.0, 1.0, 1.0, 0.0, Variation::NONE, &mut r);
        let low = band_share(&k, 20.0, 150.0);
        assert!(low > 0.5, "kick low share {low}");
        let p = Bass808 {
            decay: 1.2,
            punch: 12.0,
            drive: 0.6,
            sustain: false,
            gain: 0.85,
            glide_s: 0.09,
            click: 0.3,
            sub: 0.5,
            tone: 0.0,
        };
        let b = bass808(&p, 36.0, 1.0, 0.6, None, &mut r);
        let mid = band_share(&b, 120.0, 1200.0);
        assert!(mid > 0.02, "808 has no audible harmonics ({mid})");
        assert!(band_share(&b, 20.0, 120.0) > 0.6, "808 lost its sub");
    }

    #[test]
    fn hats_are_bright_and_velocity_darkens() {
        let mut r = Rng::new(1);
        let hard = metal(0, 1.0, 1.0, 1.0, 0.0, Variation::NONE, &mut r);
        let soft = metal(0, 1.0, 1.0, 0.4, 0.0, Variation::NONE, &mut Rng::new(1));
        let air_h = band_share(&hard, 8000.0, 22050.0);
        let air_s = band_share(&soft, 8000.0, 22050.0);
        assert!(air_h > 0.4, "hat air {air_h}");
        assert!(
            air_s < air_h,
            "soft hat should be darker ({air_s} vs {air_h})"
        );
        assert!(band_share(&hard, 2000.0, 5000.0) < 0.2, "hat clang");
    }

    /// Share of energy between lo and hi Hz (single FFT over the start).
    fn band_share(x: &[f32], lo: f32, hi: f32) -> f32 {
        let n = 8192;
        let fft = Fft::new(n);
        let mut re: Vec<f32> = (0..n)
            .map(|i| {
                let w = 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos();
                x.get(i).copied().unwrap_or(0.0) * w
            })
            .collect();
        let mut im = vec![0.0f32; n];
        fft.forward(&mut re, &mut im);
        let (mut a, mut tot) = (0.0f32, 0.0f32);
        for k in 1..n / 2 {
            let p = re[k] * re[k] + im[k] * im[k];
            let f = k as f32 * SR / n as f32;
            tot += p;
            if f >= lo && f < hi {
                a += p;
            }
        }
        a / tot.max(1e-20)
    }

    /// Energy away from the harmonic series of f0, dB relative to total.
    pub(crate) fn alias_db(x: &[f32], f0: f32) -> f32 {
        let n = 8192;
        let fft = Fft::new(n);
        let start = 2000.min(x.len().saturating_sub(n));
        let mut re: Vec<f32> = (0..n)
            .map(|i| {
                let z = 2.0 * PI * i as f32 / n as f32;
                let w = 0.42 - 0.5 * z.cos() + 0.08 * (2.0 * z).cos();
                x.get(start + i).copied().unwrap_or(0.0) * w
            })
            .collect();
        let mut im = vec![0.0f32; n];
        fft.forward(&mut re, &mut im);
        let df = SR / n as f32;
        let (mut off, mut tot) = (0.0f64, 0.0f64);
        for k in 1..n / 2 {
            let p = (re[k] * re[k] + im[k] * im[k]) as f64;
            let f = k as f32 * df;
            let h = (f / f0).round();
            tot += p;
            if (f - h * f0).abs() > 3.5 * df && f > 30.0 {
                off += p;
            }
        }
        (10.0 * (off / tot.max(1e-30)).log10()) as f32
    }

    #[test]
    fn oscillators_alias_below_threshold() {
        let mut rng = Rng::new(1);
        for f0 in [440.0f32, 1760.0, 3520.0] {
            let dt = f0 / SR;
            for wave in [Wave::Saw, Wave::Square, Wave::Triangle] {
                let mut ph = 0.0f32;
                let x: Vec<f32> = (0..12_000)
                    .map(|_| {
                        let y = osc(wave, ph, dt, &mut rng);
                        ph = (ph + dt) % 1.0;
                        y
                    })
                    .collect();
                let a = alias_db(&x, f0);
                assert!(a < -48.0, "{wave:?} at {f0} Hz aliases at {a} dB");
            }
            // and the mipmapped saw beats the old two-sample PolyBLEP
            let mut ph = 0.0f32;
            let old: Vec<f32> = (0..12_000)
                .map(|_| {
                    let y = saw_polyblep(ph, dt);
                    ph = (ph + dt) % 1.0;
                    y
                })
                .collect();
            let mut ph = 0.0f32;
            let new: Vec<f32> = (0..12_000)
                .map(|_| {
                    let y = osc(Wave::Saw, ph, dt, &mut rng);
                    ph = (ph + dt) % 1.0;
                    y
                })
                .collect();
            assert!(alias_db(&new, f0) < alias_db(&old, f0) - 10.0, "{f0}");
        }
    }
}
