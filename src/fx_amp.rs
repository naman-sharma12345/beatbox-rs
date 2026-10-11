//! Guitar/bass amp simulator (preamp waveshaper, tone stack, power amp,
//! modelled speaker cabinet) and IR loading for cabinets and real-room
//! convolution reverbs.

use crate::dsp::{db_to_gain, SR};
use crate::fx::{AmpSimFx, CabIrFx};
use crate::sc_strip::{Bq, Coeffs, FilterType};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

fn bq(kind: FilterType, f: f32, q: f32, g: f32, sr: f32) -> Bq {
    Bq::new(Coeffs::design(kind, f.min(sr * 0.45), q, g, sr))
}

/// Cabinet + mic response as a filter chain (minimum phase).
pub fn cab_chain(cab: &str, mic: &str) -> Vec<Bq> {
    use FilterType::*;
    let room = mic.eq_ignore_ascii_case("room");
    let lp = |f: f32| if room { f * 0.85 } else { f };
    let v: Vec<(FilterType, f32, f32, f32)> = match cab {
        "1x12" | "1x12_open" => vec![(HighPass, 80.0, 0.9, 0.0), (Peak, 120.0, 1.2, 3.0), (Peak, 2200.0, 1.5, 3.0), (Peak, 3600.0, 3.0, -3.0), (LowPass, lp(5500.0), 0.7, 0.0), (LowPass, lp(5500.0), 0.7, 0.0), (LowPass, 9000.0, 0.7, 0.0)],
        "2x12" => vec![(HighPass, 75.0, 0.9, 0.0), (Peak, 110.0, 1.3, 3.5), (Peak, 400.0, 1.0, -1.5), (Peak, 2500.0, 1.4, 2.5), (LowPass, lp(5000.0), 0.7, 0.0), (LowPass, lp(5000.0), 0.7, 0.0), (LowPass, 8500.0, 0.7, 0.0)],
        "4x12" => vec![(HighPass, 70.0, 1.0, 0.0), (Peak, 100.0, 1.4, 4.0), (Peak, 500.0, 1.0, -2.0), (Peak, 1800.0, 1.2, 2.0), (Peak, 3000.0, 2.0, 1.5), (LowPass, lp(4500.0), 0.7, 0.0), (LowPass, lp(4500.0), 0.7, 0.0), (LowPass, 8000.0, 0.7, 0.0)],
        "bass_4x10" | "bass" => vec![(HighPass, 40.0, 0.8, 0.0), (Peak, 80.0, 1.0, 3.0), (Peak, 700.0, 1.0, -2.0), (Peak, 2500.0, 1.5, 3.0), (LowPass, lp(4000.0), 0.7, 0.0), (LowPass, lp(4000.0), 0.7, 0.0)],
        _ => vec![],
    };
    v.into_iter().map(|(k, f, q, g)| bq(k, f, q, g, SR)).collect()
}

fn shaper(x: f32, bias: f32) -> f32 {
    // asymmetric soft clip: even harmonics from the bias, DC removed
    (x + bias).tanh() - bias.tanh()
}

/// One channel through the amp, 2x oversampled around the nonlinear stages.
fn amp_channel(p: &AmpSimFx, x: &mut [f32]) {
    use FilterType::*;
    let model = p.model.to_lowercase();
    let (stages, drive, bias, hpf, bright) = match model.as_str() {
        "crunch" => (2, 4.0, 0.2, 90.0, 2.0),
        "lead" => (3, 12.0, 0.25, 110.0, 1.0),
        "bass" => (1, 1.5, 0.1, 35.0, 0.0),
        _ => (1, 0.8, 0.08, 80.0, 2.5),
    };
    let sr2 = SR * 2.0;
    let k = db_to_gain(p.gain_db) * drive;
    let mut pre_hp = bq(HighPass, hpf, 0.7, 0.0, SR);
    let mut pre_bright = bq(HighShelf, 2500.0, 0.7, bright, SR);
    // tone stack (Fender-ish: bass shelf, scooped mid, treble shelf)
    let mut ts = [
        bq(LowShelf, 120.0, 0.7, (p.bass - 0.5) * 18.0, SR),
        bq(Peak, 650.0, 0.8, (p.mid - 0.5) * 14.0 - 2.0, SR),
        bq(HighShelf, 3000.0, 0.7, (p.treble - 0.5) * 16.0, SR),
        bq(HighShelf, 4500.0, 0.7, (p.presence - 0.5) * 10.0, SR),
    ];
    let mut aa = [bq(LowPass, 20000.0, 0.54, 0.0, sr2), bq(LowPass, 20000.0, 1.31, 0.0, sr2)];
    let mut aa2 = [bq(LowPass, 20000.0, 0.54, 0.0, sr2), bq(LowPass, 20000.0, 1.31, 0.0, sr2)];
    let mut dc = (0.0f32, 0.0f32);
    let mut prev = 0.0f32;
    let lowsplit = model == "bass";
    let mut lo = bq(LowPass, 150.0, 0.7, 0.0, SR);
    for s in x.iter_mut() {
        let dry = *s;
        let v = pre_bright.run(pre_hp.run(dry));
        // 2x oversample: midpoint + sample
        let mut outs = [0.0f32; 2];
        for (j, u) in [0.5 * (prev + v), v].iter().enumerate() {
            let t0 = aa[0].run(*u * 2.0);
            let mut y = aa[1].run(t0) * 0.5;
            for st in 0..stages {
                y = shaper(y * if st == 0 { k } else { 2.5 }, bias) / if st == 0 { k.max(1.0).sqrt() } else { 1.6 };
            }
            let t1 = aa2[0].run(y);
            outs[j] = aa2[1].run(t1);
        }
        prev = v;
        let mut y = outs[1];
        // DC blocker
        let o = y - dc.0 + 0.995 * dc.1;
        dc = (y, o);
        y = o;
        for b in ts.iter_mut() {
            y = b.run(y);
        }
        // power amp: gentle saturation
        y = (y * 1.2).tanh() / 1.2;
        if lowsplit {
            y += lo.run(dry) * 0.8;
        }
        *s = y;
    }
    let mut cab = cab_chain(&p.cab.to_lowercase(), &p.mic);
    if !cab.is_empty() {
        for s in x.iter_mut() {
            let mut y = *s;
            for b in cab.iter_mut() {
                y = b.run(y);
            }
            *s = y;
        }
    }
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

pub fn amp_sim(p: &AmpSimFx, l: &mut [f32], r: &mut [f32]) {
    let mix = p.mix.clamp(0.0, 1.0);
    let in_rms = (rms(l) + rms(r)) * 0.5;
    let (dl, dr) = (l.to_vec(), r.to_vec());
    amp_channel(p, l);
    amp_channel(p, r);
    // level-matched to the input, then master
    let out_rms = (rms(l) + rms(r)) * 0.5;
    let g = if out_rms > 1e-9 && in_rms > 1e-9 { in_rms / out_rms } else { 1.0 } * db_to_gain(p.master_db);
    for i in 0..l.len() {
        l[i] = l[i] * g * mix + dl[i] * (1.0 - mix);
        r[i] = r[i] * g * mix + dr[i] * (1.0 - mix);
    }
}

// ------------------------------------------------------------ impulse responses

fn ir_cache() -> &'static Mutex<HashMap<PathBuf, Arc<(Vec<f32>, Vec<f32>)>>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Arc<(Vec<f32>, Vec<f32>)>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Load an impulse response (any decodable audio file; a library id/path is
/// resolved by the caller), resampled to SR, trimmed to max_s, energy
/// normalised so the wet level does not depend on the file.
pub fn load_ir(path: &str, max_s: f32) -> Option<Arc<(Vec<f32>, Vec<f32>)>> {
    let pb = crate::sound_catalog::resolve_file(path).unwrap_or_else(|| PathBuf::from(path));
    if let Some(v) = ir_cache().lock().unwrap().get(&pb) {
        return Some(v.clone());
    }
    let (mut l, mut r) = crate::samples::decode_stereo(&pb).ok()?;
    let n = ((max_s.clamp(0.01, 12.0)) * SR) as usize;
    l.truncate(n);
    r.truncate(n);
    let e = (l.iter().chain(r.iter()).map(|v| v * v).sum::<f32>() * 0.5).sqrt().max(1e-9);
    l.iter_mut().for_each(|v| *v /= e);
    r.iter_mut().for_each(|v| *v /= e);
    let a = Arc::new((l, r));
    let mut c = ir_cache().lock().unwrap();
    if c.len() > 16 {
        c.clear();
    }
    c.insert(pb, a.clone());
    Some(a)
}

/// Cabinet IR (or any short IR) as an insert: convolve each side, wet/dry.
pub fn cab_ir(p: &CabIrFx, l: &mut [f32], r: &mut [f32]) {
    let Some(ir) = load_ir(&p.path, p.max_ms / 1000.0) else { return };
    let mix = p.mix.clamp(0.0, 1.0);
    let g = db_to_gain(p.gain_db);
    let wl = crate::fx_extra::convolve(l, &ir.0);
    let wr = crate::fx_extra::convolve(r, if ir.1.is_empty() { &ir.0 } else { &ir.1 });
    for i in 0..l.len() {
        l[i] = (wl[i] * g) * mix + l[i] * (1.0 - mix);
        r[i] = (wr[i] * g) * mix + r[i] * (1.0 - mix);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f32, s: f32, a: f32) -> Vec<f32> {
        (0..(s * SR) as usize).map(|i| a * (2.0 * std::f32::consts::PI * f * i as f32 / SR).sin()).collect()
    }

    fn harmonics(x: &[f32], f: f32) -> f32 {
        // energy at 2f..5f relative to f (Goertzel)
        let g = |fr: f32| {
            let w = 2.0 * std::f32::consts::PI * fr / SR;
            let (mut s1, mut s2) = (0.0f32, 0.0f32);
            for v in &x[x.len() / 2..] {
                let s0 = v + 2.0 * w.cos() * s1 - s2;
                s2 = s1;
                s1 = s0;
            }
            (s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2).sqrt()
        };
        (2..6).map(|h| g(f * h as f32)).sum::<f32>() / g(f).max(1e-9)
    }

    #[test]
    fn crunch_adds_harmonics_and_clean_stays_cleaner_at_matched_level() {
        let x = sine(220.0, 0.5, 0.5);
        let mut cl = (x.clone(), x.clone());
        let mut cr = (x.clone(), x.clone());
        amp_sim(&AmpSimFx { cab: "none".into(), ..Default::default() }, &mut cl.0, &mut cl.1);
        amp_sim(&AmpSimFx { model: "crunch".into(), gain_db: 12.0, cab: "none".into(), ..Default::default() }, &mut cr.0, &mut cr.1);
        let (hc, hr) = (harmonics(&cl.0, 220.0), harmonics(&cr.0, 220.0));
        assert!(hr > hc * 1.5, "clean {hc} crunch {hr}");
        assert!((rms(&cl.0) - rms(&x)).abs() < 0.05, "level matched");
        assert!(cl.0.iter().all(|v| v.is_finite() && v.abs() < 2.0));
    }

    #[test]
    fn cabinet_rolls_off_the_fizz() {
        let mut hi = sine(9000.0, 0.3, 0.3);
        let mut mid = sine(1000.0, 0.3, 0.3);
        for x in [&mut hi, &mut mid] {
            let mut c = cab_chain("2x12", "close");
            for s in x.iter_mut() {
                let mut y = *s;
                for b in c.iter_mut() {
                    y = b.run(y);
                }
                *s = y;
            }
        }
        assert!(rms(&hi[4000..]) < rms(&mid[4000..]) * 0.2);
    }
}
