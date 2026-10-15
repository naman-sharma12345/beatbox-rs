//! Streaming true-peak lookahead limiter.
//!
//! Clean-room design from the standard lookahead-limiter construction
//! (a gain computer fed by a true-peak detector, a sliding minimum over the
//! lookahead window and a moving-average smoother of the same length, with
//! the audio delayed so the gain is already down when the peak arrives) and
//! the ITU-R BS.1770-4 Annex 2 true-peak interpolator (`super::true_peak`).
//!
//! Signal flow, per sample, all channels linked:
//! 1. `d[m]`: the true peak of the interval [m, m+1) over every channel
//!    (the 4x interpolator plus the sample itself, 6 samples of delay).
//! 2. `g_req[m] = min(1, ceiling / d[m])`.
//! 3. Program-dependent dual release, in dB of gain reduction:
//!    `GR_req = 20 log10(d / ceiling)` when over, else 0. A slow envelope
//!    `e_s` charges towards `GR_req` with a 150 ms time constant and decays
//!    with `release_slow_ms`; a fast envelope relaxes towards `e_s` with
//!    `release_fast_ms` and is never below the request:
//!    `e_f = max(GR_req, e_s + (e_f - e_s) r_f)`. A lone transient barely
//!    charges `e_s`, so it releases fast; sustained limiting charges it, so
//!    the release slows down and the gain does not pump.
//!    `g_rel = 10^(-e_f / 20) <= ceiling / d`.
//! 4. Hold: `h[m] = min(g_rel[m - W + 1 ..= m])` with `W = L + 11`.
//! 5. Smooth: `s[m] = mean(h[m - L + 1 ..= m])`, a box filter of length `L`
//!    (the lookahead), so the gain moves as a straight-line ramp.
//! 6. Output `y[t] = x[t] * s[t + L + 4]`: the audio is delayed by
//!    `latency() = L + 10` samples.
//!
//! Why it holds the ceiling: every `h` inside the box window of each of the
//! 12 output samples that the meter's interpolator reads around the
//! interval m (x[m-5] ..= x[m+6]) includes `g_rel[m] <= g_req[m]` in its
//! hold window (that is what W = L + 11 buys), so each of those 12 gains is
//! at most `ceiling / d[m]`. The sample peak is held exactly; an
//! interpolated point is held up to the small effect of the gain varying
//! across the 12 taps (measured in the tests, reported in the docs).
//!
//! Real-time contract: all buffers are sized in `new`; `process*` never
//! allocates, never locks and is O(1) per sample (amortized for the
//! monotonic min queue). Output does not depend on how the stream is cut
//! into blocks. Below the ceiling the gain is exactly 1.0, so a quiet
//! signal comes out bit-identical, delayed by exactly `latency()` samples.

use super::true_peak::TruePeakDetector;

/// Settings of a [`TruePeakLimiter`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TruePeakLimiterParams {
    /// Output ceiling in dBTP (BS.1770-4 4x meter).
    pub ceiling_dbtp: f32,
    /// Lookahead and attack ramp length in ms.
    pub lookahead_ms: f32,
    /// Fast release time constant in ms (transients).
    pub release_fast_ms: f32,
    /// Slow release time constant in ms (sustained limiting).
    pub release_slow_ms: f32,
}

/// Time constant with which sustained gain reduction charges the slow
/// release envelope.
pub const SLOW_CHARGE_MS: f32 = 150.0;

impl Default for TruePeakLimiterParams {
    fn default() -> Self {
        TruePeakLimiterParams { ceiling_dbtp: -0.1, lookahead_ms: 3.0, release_fast_ms: 40.0, release_slow_ms: 400.0 }
    }
}

/// Streaming linked multi-channel true-peak limiter (see the module docs).
pub struct TruePeakLimiter {
    channels: usize,
    ceiling: f32,
    look: usize,
    hold_len: u64,
    latency: usize,
    rel_fast: f32,
    rel_slow: f32,
    charge: f32,
    e_fast: f32,
    e_slow: f32,
    det: Vec<TruePeakDetector>,
    // per-channel delay rings, channel c at [c * dlen, (c + 1) * dlen)
    delay: Vec<f32>,
    dlen: usize,
    dpos: usize,
    // monotonic (non-decreasing from head) queue of (index, gain) for the sliding minimum
    q_idx: Vec<u64>,
    q_val: Vec<f32>,
    q_head: usize,
    q_len: usize,
    // box smoother
    box_buf: Vec<f64>,
    box_pos: usize,
    box_sum: f64,
    t: u64,
    min_gain: f32,
}

impl TruePeakLimiter {
    /// A limiter for `channels` linked channels at sample rate `sr`.
    /// Allocates; call it off the audio thread.
    pub fn new(sr: f32, channels: usize, p: TruePeakLimiterParams) -> Self {
        let channels = channels.max(1);
        let look = ((p.lookahead_ms.max(0.0) * 0.001 * sr).round() as usize).max(1);
        let latency = look + 10;
        let hold = look + 11;
        let dlen = latency + 1;
        let mut l = TruePeakLimiter {
            channels,
            ceiling: 1.0,
            look,
            hold_len: hold as u64,
            latency,
            rel_fast: 0.0,
            rel_slow: 0.0,
            charge: 1.0 - (-1.0 / (SLOW_CHARGE_MS * 0.001 * sr.max(1.0))).exp(),
            e_fast: 0.0,
            e_slow: 0.0,
            det: vec![TruePeakDetector::new(); channels],
            delay: vec![0.0; channels * dlen],
            dlen,
            dpos: 0,
            q_idx: vec![0; hold + 1],
            q_val: vec![1.0; hold + 1],
            q_head: 0,
            q_len: 0,
            box_buf: vec![1.0; look],
            box_pos: 0,
            box_sum: look as f64,
            t: 0,
            min_gain: 1.0,
        };
        l.set_ceiling_dbtp(p.ceiling_dbtp);
        l.set_release(sr, p.release_fast_ms, p.release_slow_ms);
        l
    }

    /// Samples of delay between input and output. Exact.
    pub fn latency(&self) -> usize {
        self.latency
    }

    /// Lookahead (= attack ramp) in samples.
    pub fn lookahead(&self) -> usize {
        self.look
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Change the ceiling (no allocation; safe on the audio thread).
    pub fn set_ceiling_dbtp(&mut self, db: f32) {
        // 1e-4 (0.0009 dB) of margin so rounding can never land a hair over
        self.ceiling = 10f32.powf(db.min(24.0) / 20.0) * (1.0 - 1e-4);
    }

    /// Change the fast and slow release (no allocation; safe on the audio thread).
    pub fn set_release(&mut self, sr: f32, fast_ms: f32, slow_ms: f32) {
        self.rel_fast = (-1.0 / (fast_ms.max(0.1) * 0.001 * sr)).exp();
        self.rel_slow = (-1.0 / (slow_ms.max(0.1) * 0.001 * sr)).exp();
    }

    /// Lowest gain applied since the last call (as dB of reduction, >= 0),
    /// then resets the meter.
    pub fn take_max_gain_reduction_db(&mut self) -> f32 {
        let g = self.min_gain;
        self.min_gain = 1.0;
        -20.0 * g.max(1e-12).log10()
    }

    /// Back to silence (no allocation).
    pub fn reset(&mut self) {
        for d in self.det.iter_mut() {
            d.reset();
        }
        self.delay.iter_mut().for_each(|v| *v = 0.0);
        self.dpos = 0;
        self.e_fast = 0.0;
        self.e_slow = 0.0;
        self.q_head = 0;
        self.q_len = 0;
        self.box_buf.iter_mut().for_each(|v| *v = 1.0);
        self.box_pos = 0;
        self.box_sum = self.look as f64;
        self.t = 0;
        self.min_gain = 1.0;
    }

    /// One frame through the gain computer; returns the smoothed gain for
    /// the output frame `latency()` samples ago.
    #[inline]
    fn gain_step(&mut self, d: f32) -> f32 {
        let gr_req = if d > self.ceiling { 20.0 * (d / self.ceiling).log10() } else { 0.0 };
        if gr_req > self.e_slow {
            self.e_slow += self.charge * (gr_req - self.e_slow);
        } else {
            self.e_slow *= self.rel_slow;
        }
        let mut e = (self.e_slow + (self.e_fast - self.e_slow) * self.rel_fast).max(gr_req);
        if e < 1e-7 {
            e = 0.0;
            if self.e_slow < 1e-7 {
                self.e_slow = 0.0;
            }
        }
        self.e_fast = e;
        let g = if e == 0.0 {
            1.0
        } else {
            // never above the request, whatever exp/log rounding does
            let g = (-e * (std::f32::consts::LN_10 / 20.0)).exp();
            if gr_req > 0.0 {
                g.min(self.ceiling / d)
            } else {
                g
            }
        };
        // sliding minimum over the last `hold_len` values
        let cap = self.q_idx.len();
        while self.q_len > 0 {
            let tail = (self.q_head + self.q_len - 1) % cap;
            if self.q_val[tail] >= g {
                self.q_len -= 1;
            } else {
                break;
            }
        }
        let tail = (self.q_head + self.q_len) % cap;
        self.q_idx[tail] = self.t;
        self.q_val[tail] = g;
        self.q_len += 1;
        while self.q_idx[self.q_head] + self.hold_len <= self.t {
            self.q_head = (self.q_head + 1) % cap;
            self.q_len -= 1;
        }
        let h = self.q_val[self.q_head];
        self.t += 1;
        // box smoother, re-summed exactly once per period so rounding never drifts
        self.box_sum += h as f64 - self.box_buf[self.box_pos];
        self.box_buf[self.box_pos] = h as f64;
        self.box_pos += 1;
        if self.box_pos == self.look {
            self.box_pos = 0;
            self.box_sum = self.box_buf.iter().sum();
        }
        let s = ((self.box_sum / self.look as f64) as f32).min(1.0);
        if s < self.min_gain {
            self.min_gain = s;
        }
        s
    }

    /// Process linked channels in place. Every slice should have the same
    /// length and there should be `channels()` of them; missing channels are
    /// treated as silence and extra ones are left untouched.
    pub fn process(&mut self, chans: &mut [&mut [f32]]) {
        let nch = chans.len().min(self.channels);
        let n = chans[..nch].iter().map(|c| c.len()).min().unwrap_or(0);
        let dlen = self.dlen;
        for i in 0..n {
            let mut d = 0.0f32;
            for c in 0..self.channels {
                let x = if c < nch { chans[c][i] } else { 0.0 };
                d = d.max(self.det[c].push(x));
                self.delay[c * dlen + self.dpos] = x;
            }
            let s = self.gain_step(d);
            let rd = if self.dpos + 1 == dlen { 0 } else { self.dpos + 1 };
            for (c, ch) in chans.iter_mut().enumerate().take(nch) {
                ch[i] = self.delay[c * dlen + rd] * s;
            }
            self.dpos = rd;
        }
    }

    /// Stereo convenience wrapper around [`process`](Self::process).
    pub fn process_stereo(&mut self, l: &mut [f32], r: &mut [f32]) {
        self.process(&mut [l, r]);
    }
}

/// What an offline [`limit_offline`] pass did.
#[derive(Clone, Copy, Debug)]
pub struct LimitReport {
    pub latency: usize,
    pub lookahead: usize,
    pub max_gain_reduction_db: f32,
}

/// Limit whole buffers offline with the streaming limiter, compensating its
/// latency so the output lines up with the input sample for sample and keeps
/// its length. Allocates (it is not for the audio thread).
pub fn limit_offline(chans: &mut [Vec<f32>], sr: f32, p: TruePeakLimiterParams) -> LimitReport {
    let mut lim = TruePeakLimiter::new(sr, chans.len(), p);
    let lat = lim.latency();
    let n = chans.iter().map(|c| c.len()).min().unwrap_or(0);
    for c in chans.iter_mut() {
        c.truncate(n);
        c.resize(n + lat, 0.0);
    }
    {
        let mut refs: Vec<&mut [f32]> = chans.iter_mut().map(|c| c.as_mut_slice()).collect();
        lim.process(&mut refs);
    }
    for c in chans.iter_mut() {
        c.drain(..lat);
    }
    LimitReport { latency: lat, lookahead: lim.lookahead(), max_gain_reduction_db: lim.take_max_gain_reduction_db() }
}

#[cfg(test)]
mod tests {
    use super::super::true_peak::{tests::true_peak_ref16, true_peak};
    use super::*;
    use std::f32::consts::PI;

    const SR: f32 = 44_100.0;

    fn rng(seed: u32) -> impl FnMut() -> f32 {
        let mut s = seed.max(1);
        move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s as f32 / u32::MAX as f32) * 2.0 - 1.0
        }
    }

    fn fade(x: &mut [f32], n: usize) {
        let len = x.len();
        for i in 0..n.min(len) {
            let g = i as f32 / n as f32;
            x[i] *= g;
            x[len - 1 - i] *= g;
        }
    }

    /// The worst-case test signals, each a stereo pair, all well over 0 dBTP.
    pub(crate) fn torture_signals() -> Vec<(&'static str, Vec<f32>, Vec<f32>)> {
        let n = 22050;
        let mut v = Vec::new();
        // quarter-rate sine at 45 degrees: samples +-1.414, true peak 2.0 (+6 dBTP)
        let q: Vec<f32> = (0..n).map(|i| 2.0 * (PI / 2.0 * i as f32 + PI / 4.0).sin()).collect();
        v.push(("fs/4 sine 45deg +6dBTP", q.clone(), q));
        // naive (aliased) full-scale square wave at 1 kHz, +3 dB
        let sq: Vec<f32> = (0..n).map(|i| if (i as f32 * 1000.0 / SR).fract() < 0.5 { 1.41 } else { -1.41 }).collect();
        v.push(("1k naive square +3dB", sq.clone(), sq));
        // +12 dB white noise, independent channels
        let mut r = rng(7);
        let a: Vec<f32> = (0..n).map(|_| 4.0 * r()).collect();
        let b: Vec<f32> = (0..n).map(|_| 4.0 * r()).collect();
        v.push(("white noise +12dB", a, b));
        // isolated full-scale impulses of alternating sign every 1000 samples, 4.0
        let mut imp = vec![0.0f32; n];
        for (k, i) in (500..n - 500).step_by(1000).enumerate() {
            imp[i] = if k % 2 == 0 { 4.0 } else { -4.0 };
            imp[i + 1] = -imp[i];
        }
        v.push(("impulse doublets 4.0", imp.clone(), imp));
        // near-Nyquist bursts (18 kHz, random phase, +6 dB) over a quiet bed
        let mut r = rng(99);
        let mut hf: Vec<f32> = (0..n).map(|_| 0.05 * r()).collect();
        for b0 in (0..n - 2000).step_by(3000) {
            let ph = r() * PI;
            for i in 0..1500 {
                hf[b0 + i] += 2.0 * (2.0 * PI * 18000.0 * i as f32 / SR + ph).sin();
            }
        }
        v.push(("18k bursts +6dB", hf.clone(), hf));
        // a drum-like loop: decaying 60 Hz kicks + noise snaps + a 10.5 kHz hat, summed hot
        let mut r = rng(3);
        let mut dr = vec![0.0f32; n];
        for k in 0..8 {
            let b0 = k * 2700;
            for i in 0..2600.min(n - b0) {
                let t = i as f32 / SR;
                dr[b0 + i] += 2.5 * (-t * 18.0).exp() * (2.0 * PI * 60.0 * t * (1.0 + 2.0 * (-t * 40.0).exp())).sin();
                if k % 2 == 1 {
                    dr[b0 + i] += 1.5 * (-t * 30.0).exp() * r();
                }
                dr[b0 + i] += 0.6 * (2.0 * PI * 10500.0 * t).sin() * (-t * 60.0).exp();
            }
        }
        let dl = dr.clone();
        v.push(("drum loop hot", dl, dr));
        // a step from silence to a +9 dB 5 kHz tone (fast transient)
        let st: Vec<f32> = (0..n).map(|i| if i < n / 2 { 0.0 } else { 2.8 * (2.0 * PI * 5000.0 * i as f32 / SR + 0.7).sin() }).collect();
        v.push(("step into +9dB 5k", st.clone(), st));
        for (_, l, r) in v.iter_mut() {
            fade(l, 64);
            fade(r, 64);
        }
        v
    }

    #[test]
    fn the_ceiling_holds_on_4x_inter_sample_peaks() {
        let mut lines = Vec::new();
        for ceil in [-1.0f32, -0.1] {
            let mut worst = -200.0f32;
            let mut worst16 = -200.0f32;
            for (name, l0, r0) in torture_signals() {
                let mut ch = vec![l0.clone(), r0.clone()];
                let p = TruePeakLimiterParams { ceiling_dbtp: ceil, ..Default::default() };
                let rep = limit_offline(&mut ch, SR, p);
                let tp = 20.0 * true_peak(&ch[0]).max(true_peak(&ch[1])).log10();
                let tp_in = 20.0 * true_peak(&l0).max(true_peak(&r0)).log10();
                let r16 = 20.0 * true_peak_ref16(&ch[0]).max(true_peak_ref16(&ch[1])).log10() as f32;
                lines.push(format!(
                    "ceiling {ceil:+.1} | {name}: in {tp_in:+.2} dBTP -> out {tp:+.3} dBTP (4x BS.1770) / {r16:+.3} dB (16x ref), max GR {:.1} dB",
                    rep.max_gain_reduction_db
                ));
                worst = worst.max(tp);
                worst16 = worst16.max(r16);
                assert!(tp <= ceil + 0.1, "{name}: {tp} over {ceil}");
                assert!(tp >= ceil - 0.1, "{name}: over-limited to {tp} (ceiling {ceil})");
            }
            lines.push(format!("ceiling {ceil:+.1} | WORST out {worst:+.3} dBTP (4x), {worst16:+.3} dB (16x ref)"));
        }
        eprintln!("TP_LIMITER_CEILING\n{}", lines.join("\n"));
    }

    #[test]
    fn plus_3_dbfs_inter_sample_peaks_never_exceed_minus_0_1_dbtp() {
        // samples at exactly +-1.0 (0 dBFS) whose waveform peaks at +3.01 dBTP,
        // at the quarter rate and at other rational rates with 45-degree phase
        for (num, den) in [(1u32, 4u32), (1, 3), (3, 8), (1, 6), (2, 5)] {
            let w = 2.0 * PI * num as f32 / den as f32;
            let raw: Vec<f32> = (0..22050).map(|i| (w * i as f32 + PI / 4.0).sin()).collect();
            let sp = raw.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let mut x: Vec<f32> = raw.iter().map(|v| v / sp).collect();
            fade(&mut x, 64);
            let tp_in = 20.0 * true_peak(&x).log10();
            let mut ch = vec![x.clone(), x];
            limit_offline(&mut ch, SR, TruePeakLimiterParams::default());
            let tp = 20.0 * true_peak(&ch[0]).log10();
            eprintln!("TP_LIMITER_PLUS3 f = {num}/{den} fs: in {tp_in:+.3} dBTP (sample peak 0 dBFS) -> out {tp:+.4} dBTP");
            assert!(tp <= -0.1, "{num}/{den}: {tp}");
            assert!(tp > -0.2, "{num}/{den}: {tp}");
        }
    }

    #[test]
    fn latency_is_exact_and_quiet_audio_is_bit_identical() {
        for look_ms in [0.5f32, 1.5, 5.0] {
            let p = TruePeakLimiterParams { lookahead_ms: look_ms, ..Default::default() };
            let mut lim = TruePeakLimiter::new(SR, 2, p);
            let lat = lim.latency();
            assert_eq!(lat, (look_ms * 0.001 * SR).round() as usize + 10);
            let mut r = rng(11);
            let x: Vec<f32> = (0..4000).map(|_| 0.3 * r()).collect();
            let mut l = x.clone();
            l.extend(std::iter::repeat(0.0).take(lat));
            let mut rr = l.clone();
            lim.process_stereo(&mut l, &mut rr);
            assert!(l[..lat].iter().all(|v| *v == 0.0));
            for i in 0..x.len() {
                assert_eq!(l[i + lat].to_bits(), x[i].to_bits(), "look {look_ms} i {i}");
            }
            // a lone loud impulse: its (limited) peak comes out exactly `lat` later
            lim.reset();
            let mut imp = vec![0.0f32; 3 * lat + 64];
            imp[17] = 3.0;
            let mut im2 = imp.clone();
            lim.process_stereo(&mut imp, &mut im2);
            let arg = imp.iter().enumerate().fold((0, 0.0f32), |m, (i, v)| if v.abs() > m.1 { (i, v.abs()) } else { m }).0;
            assert_eq!(arg, 17 + lat);
        }
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        let (_, l0, r0) = torture_signals().swap_remove(5);
        let mut whole = TruePeakLimiter::new(SR, 2, TruePeakLimiterParams::default());
        let (mut wl, mut wr) = (l0.clone(), r0.clone());
        whole.process_stereo(&mut wl, &mut wr);
        let mut lim = TruePeakLimiter::new(SR, 2, TruePeakLimiterParams::default());
        let (mut bl, mut br) = (l0.clone(), r0.clone());
        let mut r = rng(5);
        let mut i = 0;
        while i < bl.len() {
            let len = (((r() + 1.0) * 300.0) as usize + 1).min(bl.len() - i);
            lim.process_stereo(&mut bl[i..i + len], &mut br[i..i + len]);
            i += len;
        }
        assert!(wl.iter().zip(&bl).all(|(a, b)| a.to_bits() == b.to_bits()));
        assert!(wr.iter().zip(&br).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn the_gain_ramps_without_steps() {
        // a sustained +6 dB tone burst starting and ending on zero crossings
        let n = 20000;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let s = (2.0 * PI * 200.0 * i as f32 / SR).sin();
                if (4410..12348).contains(&i) {
                    2.0 * s
                } else {
                    0.1 * s
                }
            })
            .collect();
        let mut ch = vec![x.clone(), x.clone()];
        let p = TruePeakLimiterParams { ceiling_dbtp: -1.0, ..Default::default() };
        let rep = limit_offline(&mut ch, SR, p);
        assert!((rep.max_gain_reduction_db - 7.0).abs() < 0.2, "{}", rep.max_gain_reduction_db);
        // the largest sample-to-sample jump stays a small step of a 200 Hz wave
        let jump = ch[0].windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(jump < 0.045, "{jump}");
    }

    /// Samples until the gain is back within 0.5 dB of unity after `x`
    /// (the signal is followed by a quiet DC probe at 0.1).
    fn recovery_samples(event: &[f32]) -> usize {
        let mut x = event.to_vec();
        let start = x.len();
        x.extend(std::iter::repeat(0.1f32).take(60_000));
        let mut ch = vec![x.clone()];
        let p = TruePeakLimiterParams { ceiling_dbtp: -1.0, ..Default::default() };
        limit_offline(&mut ch, SR, p);
        let ok = 0.1 * 10f32.powf(-0.5 / 20.0);
        (start + 200..x.len()).find(|&i| ch[0][i] >= ok).unwrap_or(x.len()) - start
    }

    #[test]
    fn the_dual_release_is_fast_after_a_transient_and_slow_after_sustained_limiting() {
        // the same 9 dB over: once as a 1 ms click, once held for 400 ms
        let click: Vec<f32> = (0..44).map(|i| 2.8 * (2.0 * PI * 1000.0 * i as f32 / SR).sin()).collect();
        let held: Vec<f32> = (0..17640).map(|i| 2.8 * (2.0 * PI * 1000.0 * i as f32 / SR).sin()).collect();
        let (rc, rh) = (recovery_samples(&click), recovery_samples(&held));
        eprintln!("TP_LIMITER_RELEASE click {:.1} ms, sustained {:.1} ms", rc as f32 / SR * 1000.0, rh as f32 / SR * 1000.0);
        assert!(rc < (0.15 * SR) as usize, "click recovery {rc}");
        assert!(rh > 3 * rc, "sustained {rh} vs click {rc}");
    }

    #[test]
    fn mono_and_eight_channels_link_and_stay_finite() {
        for nch in [1usize, 8] {
            let mut r = rng(21);
            let mut chans: Vec<Vec<f32>> = (0..nch).map(|c| (0..5000).map(|_| (c as f32 + 1.0) * r()).collect()).collect();
            limit_offline(&mut chans, 48_000.0, TruePeakLimiterParams { ceiling_dbtp: -0.3, ..Default::default() });
            let tp = chans.iter().fold(0.0f32, |m, c| m.max(true_peak(c)));
            assert!(20.0 * tp.log10() <= -0.3 + 0.1, "{nch}: {}", 20.0 * tp.log10());
            assert!(chans.iter().flatten().all(|v| v.is_finite()));
        }
    }
}
