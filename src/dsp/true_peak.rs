//! True-peak measurement after ITU-R BS.1770-4 Annex 2: 4x oversampling
//! with the recommendation's 48-tap polyphase interpolator (12 taps per
//! phase), absolute value, maximum.
//!
//! Clean-room from the published recommendation. The coefficient table is
//! the one printed in BS.1770-4 Annex 2 (transcribed by hand). The filter is
//! linear phase (h[i] = h[47 - i]), so it delays by 23.5 samples at the 4x
//! rate: phase k of the output computed when sample x[n] arrives sits at
//! n - 6 + 1/8 + k/4 input samples. None of the four phases lands on an
//! input sample, so the detector also takes |x[n - 6]| itself (the sample
//! peak can never exceed the true peak, so this only removes an under-read).
//!
//! `TruePeakDetector` is a streaming, allocation-free, per-channel detector
//! (6 samples of delay); `true_peak` is the one-shot meter over a buffer.

/// The four polyphase branches of the BS.1770-4 Annex 2 interpolator.
/// Branch k, tap j multiplies x[n - j].
pub const BS1770_PHASES: [[f32; 12]; 4] = [
    [
        0.001_708_984_375, 0.010_986_328_125, -0.019_653_320_312_5, 0.033_203_125, -0.059_448_242_187_5, 0.137_329_101_562_5,
        0.972_167_968_75, -0.102_294_921_875, 0.047_607_421_875, -0.026_611_328_125, 0.014_892_578_125, -0.008_300_781_25,
    ],
    [
        -0.029_174_804_687_5, 0.029_296_875, -0.051_757_812_5, 0.089_111_328_125, -0.166_503_906_25, 0.465_087_890_625,
        0.779_785_156_25, -0.200_317_382_812_5, 0.101_562_5, -0.058_227_539_062_5, 0.033_081_054_687_5, -0.018_920_898_437_5,
    ],
    [
        -0.018_920_898_437_5, 0.033_081_054_687_5, -0.058_227_539_062_5, 0.101_562_5, -0.200_317_382_812_5, 0.779_785_156_25,
        0.465_087_890_625, -0.166_503_906_25, 0.089_111_328_125, -0.051_757_812_5, 0.029_296_875, -0.029_174_804_687_5,
    ],
    [
        -0.008_300_781_25, 0.014_892_578_125, -0.026_611_328_125, 0.047_607_421_875, -0.102_294_921_875, 0.972_167_968_75,
        0.137_329_101_562_5, -0.059_448_242_187_5, 0.033_203_125, -0.019_653_320_312_5, 0.010_986_328_125, 0.001_708_984_375,
    ],
];

/// Taps per polyphase branch.
pub const TAPS: usize = 12;

/// Streaming true-peak detector for one channel. No allocation, ever.
#[derive(Clone, Debug)]
pub struct TruePeakDetector {
    /// The last 12 inputs, written twice (at `pos` and `pos + 12`) so the
    /// newest-first window is always one contiguous slice.
    buf: [f32; 2 * TAPS],
    pos: usize,
}

impl Default for TruePeakDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl TruePeakDetector {
    /// Input samples between an input and the detector value that covers it.
    pub const DELAY: usize = 6;

    pub fn new() -> Self {
        TruePeakDetector { buf: [0.0; 2 * TAPS], pos: 0 }
    }

    pub fn reset(&mut self) {
        self.buf = [0.0; 2 * TAPS];
        self.pos = 0;
    }

    /// Push x[n]. Returns the true-peak magnitude of the interval
    /// [n - 6, n - 5): max(|x[n - 6]|, |four interpolated points|).
    #[inline]
    pub fn push(&mut self, x: f32) -> f32 {
        // newest-first window: buf[pos..pos + 12] holds x[n], x[n-1], ...
        self.pos = if self.pos == 0 { TAPS - 1 } else { self.pos - 1 };
        self.buf[self.pos] = x;
        self.buf[self.pos + TAPS] = x;
        let w = &self.buf[self.pos..self.pos + TAPS];
        let mut m = w[Self::DELAY].abs();
        for ph in BS1770_PHASES.iter() {
            let mut acc = 0.0f32;
            for j in 0..TAPS {
                acc += ph[j] * w[j];
            }
            m = m.max(acc.abs());
        }
        m
    }
}

/// True peak (linear magnitude) of one channel: the BS.1770-4 4x
/// interpolator plus the sample peak, flushed with zeros at the end.
pub fn true_peak(x: &[f32]) -> f32 {
    let mut d = TruePeakDetector::new();
    let mut m = 0.0f32;
    for &v in x {
        m = m.max(d.push(v));
    }
    for _ in 0..TAPS {
        m = m.max(d.push(0.0));
    }
    m
}

/// True peak of several channels in dBTP (-200 for silence).
pub fn true_peak_dbtp(chans: &[&[f32]]) -> f32 {
    let m = chans.iter().fold(0.0f32, |m, c| m.max(true_peak(c)));
    if m > 0.0 {
        20.0 * m.log10()
    } else {
        -200.0
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Test-only reference: 16x oversampled true peak with a long Kaiser
    /// windowed sinc (64 zero crossings each side, beta 10) in f64. Much
    /// finer than the 4x meter; used to report how far 4x can under-read.
    pub fn true_peak_ref16(x: &[f32]) -> f64 {
        const UP: usize = 16;
        const ZC: isize = 64;
        let beta = 10.0f64;
        let i0 = |v: f64| {
            let (mut s, mut t) = (1.0f64, 1.0f64);
            for k in 1..60 {
                t *= (v / 2.0) / k as f64;
                s += t * t;
            }
            s
        };
        let norm = i0(beta);
        // table[ph][k]: weight of x[i0 - ZC + 1 + k] for the point i0 + ph/UP
        let table: Vec<Vec<f64>> = (0..UP)
            .map(|ph| {
                (0..2 * ZC as usize)
                    .map(|k| {
                        let d = ph as f64 / UP as f64 + (ZC - 1 - k as isize) as f64;
                        let r = d / ZC as f64;
                        if r.abs() >= 1.0 {
                            return 0.0;
                        }
                        let w = i0(beta * (1.0 - r * r).sqrt()) / norm;
                        let s = if d.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * d).sin() / (std::f64::consts::PI * d) };
                        s * w
                    })
                    .collect()
            })
            .collect();
        let n = x.len() as isize;
        let mut peak = x.iter().fold(0.0f64, |m, v| m.max(v.abs() as f64));
        for i in -ZC..n + ZC {
            for tab in table.iter().skip(1) {
                let mut acc = 0.0f64;
                for (k, w) in tab.iter().enumerate() {
                    let j = i - ZC + 1 + k as isize;
                    if j >= 0 && j < n {
                        acc += x[j as usize] as f64 * w;
                    }
                }
                peak = peak.max(acc.abs());
            }
        }
        peak
    }

    fn fade(x: &mut [f32], n: usize) {
        let len = x.len();
        for i in 0..n {
            let g = 0.5 - 0.5 * (std::f32::consts::PI * i as f32 / n as f32).cos();
            x[i] *= g;
            x[len - 1 - i] *= g;
        }
    }

    #[test]
    fn the_interpolator_is_linear_phase_and_unity_ish_at_dc() {
        for k in 0..4 {
            for j in 0..TAPS {
                assert_eq!(BS1770_PHASES[k][j], BS1770_PHASES[3 - k][TAPS - 1 - j]);
            }
            let s: f32 = BS1770_PHASES[k].iter().sum();
            // the recommendation's table: branches 0/3 sum to 1.0016, 1/2 to 0.9730
            assert!((s - 1.0).abs() < 0.03, "phase {k} dc gain {s}");
        }
    }

    #[test]
    fn a_quarter_rate_sine_at_45_degrees_reads_its_true_peak() {
        // samples are +-0.7071, the waveform peaks at 1.0 between them (+3.01 dB ISP)
        let mut x: Vec<f32> = (0..4800)
            .map(|i| (std::f32::consts::FRAC_PI_2 * i as f32 + std::f32::consts::FRAC_PI_4).sin())
            .collect();
        // fades: an abrupt start or end rings (Gibbs) and truly over-reads
        fade(&mut x, 256);
        let sp = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let tp = true_peak(&x);
        assert!((20.0 * sp.log10() + 3.01).abs() < 0.01, "{sp}");
        // the 4x grid sits 1/8 sample off the inputs (cos(pi/16) = -0.17 dB) and
        // the recommendation's branches 1/2 gain +0.22 dB at fs/4: +0.045 dB net
        let db = 20.0 * tp.log10();
        assert!(db < 0.1 && db > -0.2, "4x tp {db} dB");
    }

    #[test]
    fn a_1k_sine_reads_its_peak_within_a_tenth_of_a_db() {
        for (amp, ph) in [(0.5f32, 0.3f32), (1.0, 1.1), (0.25, 2.0)] {
            let mut x: Vec<f32> = (0..44100)
                .map(|i| amp * (2.0 * std::f32::consts::PI * 997.0 * i as f32 / 44100.0 + ph).sin())
                .collect();
            fade(&mut x, 256);
            let tp = true_peak(&x);
            assert!((20.0 * (tp / amp).log10()).abs() < 0.1, "{amp} {tp}");
        }
    }

    #[test]
    fn the_streaming_detector_matches_the_one_shot_meter() {
        let mut s = 12345u32;
        let x: Vec<f32> = (0..3000)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect();
        let mut d = TruePeakDetector::new();
        let mut m = 0.0f32;
        for &v in x.iter().chain(std::iter::repeat(&0.0).take(TAPS)) {
            m = m.max(d.push(v));
        }
        assert_eq!(m, true_peak(&x));
        assert!(m >= x.iter().fold(0.0f32, |a, v| a.max(v.abs())));
    }
}
