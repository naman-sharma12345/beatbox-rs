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
            b'C' => 0,
            b'D' => 2,
            b'E' => 4,
            b'F' => 5,
            b'G' => 7,
            b'A' => 9,
            b'B' => 11,
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
    vocoder_at(p, l, r, 0)
}

/// `vocoder` for a buffer that starts `offset` samples into the song: the
/// carrier (saws, noise and the carrier band filters, none of which hear the
/// audio) is run through the first `offset` samples, so it is the whole-song
/// pass's carrier; the modulator side settles from the buffer's audio.
pub fn vocoder_at(p: &VocoderFx, l: &mut [f32], r: &mut [f32], offset: usize) {
    let mix = p.mix.clamp(0.0, 1.0);
    if mix <= 0.0 || l.is_empty() {
        return;
    }
    let Some(mut core) = VocoderCore::new(p) else {
        return;
    };
    // The whole per-sample machine lives in VocoderCore: the offline pass
    // and the RT StreamFx arm run the same code, so a chunked render is
    // the full render bit for bit by construction.
    core.warm_carrier(offset);
    for i in 0..l.len() {
        let (a, b) = core.step(l[i], r[i]);
        l[i] = a;
        r[i] = b;
    }
}

/// The vocoder's per-sample state machine, shared by the offline pass
/// (`vocoder_at`, which pre-warms the carrier for `offset` samples) and
/// the real-time `StreamFx::Vocoder` arm (offset 0, no pre-warm). Every
/// float op mirrors the original inline `vocoder_at` loop in the same
/// order, so both paths agree bit for bit.
pub struct VocoderCore {
    phases: Vec<[f32; 2]>,
    incs: Vec<[f32; 2]>,
    seed: u32,
    mod_f: Vec<Svf>,
    car_fl: Vec<Svf>,
    car_fr: Vec<Svf>,
    env: Vec<f32>,
    sib_hp: Svf,
    freqs: Vec<f32>,
    res: f32,
    att: f32,
    rel: f32,
    noise_amt: f32,
    norm: f32,
    sibilance: f32,
    g: f32,
    mix: f32,
}

impl VocoderCore {
    /// None when `notes` fails to parse: the offline pass then leaves the
    /// buffer untouched, and the RT chain must use a bit-exact bypass.
    pub fn new(p: &VocoderFx) -> Option<Self> {
        let notes = parse_notes(&p.notes).ok()?;
        let bands = p.bands.clamp(4, 40) as usize;
        let (lo, hi) = (
            p.low_hz.clamp(50.0, 1000.0),
            p.high_hz.clamp(2000.0, 16000.0),
        );
        let freqs: Vec<f32> = (0..bands)
            .map(|i| lo * (hi / lo).powf(i as f32 / (bands - 1) as f32))
            .collect();
        // narrower bands as there are more of them
        let res = (0.35 + bands as f32 / 60.0).min(0.9);
        Some(VocoderCore {
            phases: notes.iter().map(|_| [0.0, 0.37]).collect(),
            incs: notes
                .iter()
                .map(|m| {
                    let f = 440.0 * 2f32.powf((m - 69.0) / 12.0);
                    [f / SR, f * 1.004 / SR]
                })
                .collect(),
            seed: 0x2545F491,
            mod_f: vec![Svf::default(); bands],
            car_fl: vec![Svf::default(); bands],
            car_fr: vec![Svf::default(); bands],
            env: vec![0.0; bands],
            sib_hp: Svf::default(),
            freqs,
            res,
            att: (-1.0 / (p.attack_ms.max(0.5) * 0.001 * SR)).exp(),
            rel: (-1.0 / (p.release_ms.max(1.0) * 0.001 * SR)).exp(),
            noise_amt: p.noise.clamp(0.0, 1.0),
            norm: 1.0 / (notes.len() as f32).sqrt(),
            sibilance: p.sibilance.clamp(0.0, 1.0),
            g: 4.0 * p.gain.max(0.0) * (bands as f32 / 16.0).sqrt(),
            mix: p.mix.clamp(0.0, 1.0),
        })
    }

    /// Advance one carrier sample (detuned saw pair per note, the shared
    /// xorshift noise, per-note norm and noise mix). No filtering, no
    /// output.
    #[inline]
    fn carrier(&mut self) -> (f32, f32) {
        let (mut cl, mut cr) = (0.0f32, 0.0f32);
        for (ph, inc) in self.phases.iter_mut().zip(&self.incs) {
            for k in 0..2 {
                ph[k] += inc[k];
                if ph[k] >= 1.0 {
                    ph[k] -= 1.0;
                }
            }
            cl += 2.0 * ph[0] - 1.0;
            cr += 2.0 * ph[1] - 1.0;
        }
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        let nz = (self.seed as f32 / u32::MAX as f32) * 2.0 - 1.0;
        cl = cl * self.norm * (1.0 - self.noise_amt) + nz * self.noise_amt;
        cr = cr * self.norm * (1.0 - self.noise_amt) + nz * self.noise_amt;
        (cl, cr)
    }

    /// Pre-offset warmup: the carrier runs through its band filters for
    /// `n` samples with no envelopes or output, exactly the offset loop
    /// of the original inline `vocoder_at`.
    pub fn warm_carrier(&mut self, n: usize) {
        for _ in 0..n {
            let (cl, cr) = self.carrier();
            for b in 0..self.freqs.len() {
                self.car_fl[b].process(cl, self.freqs[b], self.res, FilterMode::Bandpass);
                self.car_fr[b].process(cr, self.freqs[b], self.res, FilterMode::Bandpass);
            }
        }
    }

    /// One output sample. State advances even when `mix` is 0; the mix
    /// law `x * (1.0 - mix) + wet * mix` then returns the input.
    #[inline]
    pub fn step(&mut self, xl: f32, xr: f32) -> (f32, f32) {
        let m = 0.5 * (xl + xr);
        let (cl, cr) = self.carrier();
        let (mut ol, mut or) = (0.0f32, 0.0f32);
        for b in 0..self.freqs.len() {
            let x = self.mod_f[b]
                .process(m, self.freqs[b], self.res, FilterMode::Bandpass)
                .abs();
            let c = if x > self.env[b] { self.att } else { self.rel };
            self.env[b] = x + (self.env[b] - x) * c;
            ol += self.car_fl[b].process(cl, self.freqs[b], self.res, FilterMode::Bandpass)
                * self.env[b];
            or += self.car_fr[b].process(cr, self.freqs[b], self.res, FilterMode::Bandpass)
                * self.env[b];
        }
        // consonants: the voice's own top end passes through
        let s = self.sib_hp.process(m, 5000.0, 0.1, FilterMode::Highpass) * self.sibilance;
        let (wl, wr) = (ol * self.g + s, or * self.g + s);
        (
            xl * (1.0 - self.mix) + wl * self.mix,
            xr * (1.0 - self.mix) + wr * self.mix,
        )
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
                if i < n / 2 {
                    (seed as f32 / u32::MAX as f32 - 0.5) * 0.6
                } else {
                    0.0
                }
            })
            .collect();
        let mut r = l.clone();
        let p = VocoderFx::default();
        vocoder(&p, &mut l, &mut r);
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        let on = rms(&l[n / 8..n / 2]);
        let off = rms(&l[n * 3 / 4..]);
        assert!(on > 0.01, "vocoder output while the voice speaks: {on}");
        assert!(
            off < on * 0.05,
            "silent when the voice stops: {off} vs {on}"
        );
        assert!(l.iter().all(|x| x.is_finite()));
    }
}
