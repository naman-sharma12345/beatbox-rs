//! Channel vocoder (FL's Vocodex / Fruity Vocoder idea, self-contained): the
//! track's own audio is the modulator; the carrier is a built-in saw chord on
//! `notes` (plus a little noise for consonants). Each of `bands` log-spaced
//! band-pass filters measures the voice's energy and opens the same band of
//! the carrier, so the chord "speaks" the words: robot voices, talk-box
//! leads, vocoded hooks.

use crate::dsp::{FilterMode, Svf, SR};
use crate::fx::VocoderFx;

/// MIDI notes from "C3 Eb3 G3" / "48,51,55".
pub fn parse_notes(s: &str) -> Result<Vec<f32>, String> {
    let mut out = Vec::new();
    for tok in s.split([',', ' ', ';']).filter(|t| !t.trim().is_empty()) {
        let t = tok.trim();
        if let Ok(n) = t.parse::<f32>() {
            out.push(n.clamp(12.0, 108.0));
            continue;
        }
        let b = t.as_bytes();
        let pc = match b[0].to_ascii_uppercase() {
            b'C' => 0, b'D' => 2, b'E' => 4, b'F' => 5, b'G' => 7, b'A' => 9, b'B' => 11,
            _ => return Err(format!("bad note '{t}' (use C3, Eb3, F#4 or MIDI numbers)")),
        };
        let mut i = 1;
        let mut acc = 0i32;
        while i < b.len() && (b[i] == b'#' || b[i] == b'b') {
            acc += if b[i] == b'#' { 1 } else { -1 };
            i += 1;
        }
        let oct: i32 = t[i..].parse().map_err(|_| format!("bad octave in '{t}'"))?;
        out.push(((oct + 1) * 12 + pc + acc) as f32);
    }
    if out.is_empty() {
        return Err("no carrier notes".into());
    }
    if out.len() > 8 {
        return Err("at most 8 carrier notes".into());
    }
    Ok(out)
}

pub fn check(p: &VocoderFx) -> Result<(), String> {
    parse_notes(&p.notes)?;
    if !(4..=40).contains(&p.bands) {
        return Err(format!("bands {} outside 4..40", p.bands));
    }
    Ok(())
}

pub fn vocoder(p: &VocoderFx, l: &mut [f32], r: &mut [f32]) {
    let Ok(notes) = parse_notes(&p.notes) else { return };
    let mix = p.mix.clamp(0.0, 1.0);
    if mix <= 0.0 || l.is_empty() {
        return;
    }
    let n = l.len();
    let bands = p.bands.clamp(4, 40) as usize;
    let (lo, hi) = (p.low_hz.clamp(50.0, 1000.0), p.high_hz.clamp(2000.0, 16000.0));
    let freqs: Vec<f32> = (0..bands).map(|i| lo * (hi / lo).powf(i as f32 / (bands - 1) as f32)).collect();
    // narrower bands as there are more of them
    let res = (0.35 + bands as f32 / 60.0).min(0.9);
    let att = (-1.0 / (p.attack_ms.max(0.5) * 0.001 * SR)).exp();
    let rel = (-1.0 / (p.release_ms.max(1.0) * 0.001 * SR)).exp();
    // carrier: detuned saws on each note (+ a noise share for consonants)
    let mut phases: Vec<[f32; 2]> = notes.iter().map(|_| [0.0, 0.37]).collect();
    let incs: Vec<[f32; 2]> = notes
        .iter()
        .map(|m| {
            let f = 440.0 * 2f32.powf((m - 69.0) / 12.0);
            [f / SR, f * 1.004 / SR]
        })
        .collect();
    let mut seed = 0x2545F491u32;
    let mut mod_f: Vec<Svf> = vec![Svf::default(); bands];
    let mut car_fl: Vec<Svf> = vec![Svf::default(); bands];
    let mut car_fr: Vec<Svf> = vec![Svf::default(); bands];
    let mut env = vec![0.0f32; bands];
    let mut sib_hp = Svf::default();
    let noise_amt = p.noise.clamp(0.0, 1.0);
    let norm = 1.0 / (notes.len() as f32).sqrt();
    for i in 0..n {
        let m = 0.5 * (l[i] + r[i]);
        // carrier sample (L/R decorrelated by the detune pair)
        let (mut cl, mut cr) = (0.0f32, 0.0f32);
        for (ph, inc) in phases.iter_mut().zip(&incs) {
            for k in 0..2 {
                ph[k] += inc[k];
                if ph[k] >= 1.0 {
                    ph[k] -= 1.0;
                }
            }
            cl += 2.0 * ph[0] - 1.0;
            cr += 2.0 * ph[1] - 1.0;
        }
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let nz = (seed as f32 / u32::MAX as f32) * 2.0 - 1.0;
        cl = cl * norm * (1.0 - noise_amt) + nz * noise_amt;
        cr = cr * norm * (1.0 - noise_amt) + nz * noise_amt;
        let (mut ol, mut or) = (0.0f32, 0.0f32);
        for b in 0..bands {
            let x = mod_f[b].process(m, freqs[b], res, FilterMode::Bandpass).abs();
            let c = if x > env[b] { att } else { rel };
            env[b] = x + (env[b] - x) * c;
            ol += car_fl[b].process(cl, freqs[b], res, FilterMode::Bandpass) * env[b];
            or += car_fr[b].process(cr, freqs[b], res, FilterMode::Bandpass) * env[b];
        }
        // consonants: the voice's own top end passes through
        let s = sib_hp.process(m, 5000.0, 0.1, FilterMode::Highpass) * p.sibilance.clamp(0.0, 1.0);
        let g = 4.0 * p.gain.max(0.0) * (bands as f32 / 16.0).sqrt();
        let (wl, wr) = (ol * g + s, or * g + s);
        l[i] = l[i] * (1.0 - mix) + wl * mix;
        r[i] = r[i] * (1.0 - mix) + wr * mix;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocoder_speaks_on_the_chord() {
        assert_eq!(parse_notes("C3 Eb3 G3").unwrap(), vec![48.0, 51.0, 55.0]);
        assert_eq!(parse_notes("60,F#4").unwrap(), vec![60.0, 66.0]);
        assert!(parse_notes("H3").is_err());
        // a modulator that is on for 0.25 s then silent: the output follows it
        let n = (SR * 0.5) as usize;
        let mut seed = 1u32;
        let mut l: Vec<f32> = (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                if i < n / 2 { (seed as f32 / u32::MAX as f32 - 0.5) * 0.6 } else { 0.0 }
            })
            .collect();
        let mut r = l.clone();
        let p = VocoderFx::default();
        vocoder(&p, &mut l, &mut r);
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let on = rms(&l[n / 8..n / 2]);
        let off = rms(&l[n * 3 / 4..]);
        assert!(on > 0.01, "vocoder output while the voice speaks: {on}");
        assert!(off < on * 0.05, "silent when the voice stops: {off} vs {on}");
        assert!(l.iter().all(|x| x.is_finite()));
    }
}
