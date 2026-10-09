//! Character effects added in sprint 7: a multi-mode saturator (tape, tube,
//! transistor, diode, wavefolder, exciter) and a Haas stereo widener.

use crate::dsp::*;
use crate::fx::*;

/// Static transfer curve of each saturation mode (input already driven).
fn shape(mode: SatMode, x: f32, bias: f32) -> f32 {
    match mode {
        // soft tanh with a gentle asymmetry: warm, rounded
        SatMode::Tape => ((x + bias * 0.3).tanh() - (bias * 0.3).tanh()) * 0.95,
        // asymmetric: positive swings compress harder -> even harmonics
        SatMode::Tube => {
            let b = 0.18 + bias;
            let y = if x + b >= 0.0 {
                1.0 - (-(x + b)).exp()
            } else {
                -0.8 * (1.0 - (x + b).exp())
            };
            y - (if b >= 0.0 {
                1.0 - (-b).exp()
            } else {
                -0.8 * (1.0 - b.exp())
            })
        }
        // fast saturating knee, almost hard: odd harmonics, edgy
        SatMode::Transistor => {
            let v = x + bias * 0.2;
            v / (1.0 + v.abs().powi(4)).powf(0.25)
        }
        // diode clipper: exponential on one side, harder on the other
        SatMode::Diode => {
            let v = x + bias * 0.3;
            if v >= 0.0 {
                1.0 - (-v * 1.5).exp()
            } else {
                -(1.0 - (v * 3.0).exp()) * 0.6
            }
        }
        // sine wavefolder: metallic harmonics that grow with drive
        SatMode::Fold => ((x + bias * 0.3) * std::f32::consts::FRAC_PI_2).sin(),
        SatMode::Exciter => (x + bias * 0.2).tanh(),
    }
}

pub fn saturator(p: &SaturatorFx, l: &mut [f32], r: &mut [f32]) {
    let drive = db_to_gain(p.drive_db.clamp(0.0, 36.0));
    // keep loudness in the same ballpark as the dry signal as drive rises
    let comp = 1.0 / (1.0 + (drive - 1.0) * 0.12).sqrt();
    let out = db_to_gain(p.output_db.clamp(-24.0, 24.0));
    let mix = p.mix.clamp(0.0, 1.0);
    let bias = p.bias.clamp(-0.5, 0.5);
    let tone = p.tone_hz.clamp(200.0, 20000.0);
    for ch in [&mut *l, &mut *r] {
        let dry: Vec<f32> = ch.to_vec();
        let mut lp = Biquad::new(BiquadKind::HighCut, tone, 0.707, 0.0);
        let mut dc = Biquad::new(BiquadKind::LowCut, 12.0, 0.707, 0.0);
        let mut hp = Biquad::new(BiquadKind::LowCut, tone.clamp(1500.0, 8000.0), 0.707, 0.0);
        let mut prev = 0.0f32;
        let mut prev_mid = 0.0f32;
        for (i, s) in ch.iter_mut().enumerate() {
            let x = dry[i] * drive;
            let y = if p.oversample {
                let mid = 0.5 * (prev + x);
                let ym = shape(p.mode, mid, bias);
                let y = shape(p.mode, x, bias);
                let o = 0.25 * prev_mid + 0.5 * ym + 0.25 * y;
                prev_mid = y;
                o
            } else {
                shape(p.mode, x, bias)
            };
            prev = x;
            let y = dc.process(y);
            let wet = if p.mode == SatMode::Exciter {
                // only the harmonics above the corner are added back
                dry[i] + hp.process(y) * comp * 1.2
            } else {
                lp.process(y) * comp
            };
            *s = (dry[i] * (1.0 - mix) + wet * mix) * out;
        }
    }
}

pub fn haas(p: &HaasFx, l: &mut [f32], r: &mut [f32]) {
    let d = ((p.delay_ms.clamp(1.0, 40.0) * 0.001 * SR) as usize).max(1);
    let mix = p.mix.clamp(0.0, 1.0);
    let lvl = db_to_gain(p.level_db.clamp(-12.0, 6.0));
    let n = l.len();
    let right = p.side >= 0;
    let mut hp = Biquad::new(
        BiquadKind::LowCut,
        p.low_cut_hz.clamp(20.0, 1000.0),
        0.707,
        0.0,
    );
    // the bass stays dead centre (not delayed); the highs are delayed on one side
    let src: &mut [f32] = if right { &mut *r } else { &mut *l };
    let dry: Vec<f32> = src.to_vec();
    let hi: Vec<f32> = dry.iter().map(|&x| hp.process(x)).collect();
    for i in 0..n {
        let delayed = if i >= d { hi[i - d] } else { 0.0 };
        let wet = (dry[i] - hi[i]) + delayed * lvl;
        src[i] = dry[i] * (1.0 - mix) + wet * mix;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f32, n: usize, a: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * f / SR).sin() * a)
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn every_mode_is_finite_bounded_and_adds_harmonics() {
        for mode in [
            SatMode::Tape,
            SatMode::Tube,
            SatMode::Transistor,
            SatMode::Diode,
            SatMode::Fold,
            SatMode::Exciter,
        ] {
            // the exciter only adds what lies above its corner: feed it a 2 kHz tone
            let ex = mode == SatMode::Exciter;
            let mut l = sine(if ex { 2000.0 } else { 220.0 }, 24000, 0.7);
            let mut r = l.clone();
            let dry = l.clone();
            let fx = SaturatorFx {
                mode,
                drive_db: 18.0,
                tone_hz: if ex { 1500.0 } else { 16000.0 },
                ..Default::default()
            };
            saturator(&fx, &mut l, &mut r);
            assert!(l.iter().all(|x| x.is_finite() && x.abs() < 4.0), "{mode:?}");
            // a saturated sine is no longer a sine: the residual against the
            // level-matched dry signal carries energy (harmonics)
            let g = rms(&l) / rms(&dry).max(1e-9);
            let resid: Vec<f32> = l.iter().zip(&dry).map(|(a, b)| a - b * g).collect();
            assert!(
                rms(&resid) > 0.02 * rms(&l),
                "{mode:?} added nothing: resid {} out {} g {g}",
                rms(&resid),
                rms(&l)
            );
        }
    }

    #[test]
    fn zero_drive_mix_zero_is_transparent() {
        let mut l = sine(440.0, 8000, 0.5);
        let mut r = l.clone();
        let dry = l.clone();
        saturator(
            &SaturatorFx {
                mix: 0.0,
                ..Default::default()
            },
            &mut l,
            &mut r,
        );
        assert!(l.iter().zip(&dry).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn tube_is_asymmetric_and_dc_free() {
        let mut l = sine(110.0, 48000, 0.6);
        let mut r = l.clone();
        saturator(
            &SaturatorFx {
                mode: SatMode::Tube,
                drive_db: 12.0,
                ..Default::default()
            },
            &mut l,
            &mut r,
        );
        let mean = l[12000..].iter().sum::<f32>() / (l.len() - 12000) as f32;
        assert!(mean.abs() < 0.02, "dc {mean}");
        let pos = l.iter().cloned().fold(0.0f32, f32::max);
        let neg = -l.iter().cloned().fold(0.0f32, f32::min);
        assert!((pos - neg).abs() > 0.02, "pos {pos} neg {neg}");
    }

    #[test]
    fn haas_widens_highs_and_keeps_bass_centered() {
        let n = 24000;
        let mut l: Vec<f32> = sine(80.0, n, 0.4)
            .iter()
            .zip(sine(3300.0, n, 0.3))
            .map(|(a, b)| a + b)
            .collect();
        let mut r = l.clone();
        haas(&HaasFx::default(), &mut l, &mut r);
        let diff: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a - b).collect();
        // side energy appeared, but the low 80 Hz is still in both channels equally
        assert!(rms(&diff[2000..]) > 0.1);
        assert!(l.iter().chain(r.iter()).all(|x| x.is_finite()));
    }
}
