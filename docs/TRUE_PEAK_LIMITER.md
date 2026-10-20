# True-peak lookahead limiter

`src/dsp/tp_limiter.rs` (`TruePeakLimiter`, `limit_offline`) and the MCP action
`true_peak_limit` (category `export`). Opt-in: no existing render path calls it.

## Design (clean-room)

- Detector: ITU-R BS.1770-4 Annex 2 4x polyphase interpolator (48 taps, 12 per
  phase, `src/dsp/true_peak.rs`) plus the sample itself, max over channels (linked).
- Gain computer: `GR_req = 20 log10(peak / ceiling)` when over.
- Program-dependent dual release: a slow envelope charges towards the request
  (150 ms) and releases with `release_slow_ms` (default 400); a fast envelope
  relaxes towards it with `release_fast_ms` (default 40) and never sits below
  the request. Lone transients release fast; sustained limiting releases slowly.
- Sliding minimum over `L + 11` samples, then a box (moving-average) smoother of
  length `L` (the lookahead): a straight-line attack ramp that is fully down
  when the peak arrives.
- Latency: exactly `L + 10` samples (`L` = lookahead, plus 6 for the
  interpolator and 4 for alignment). Default lookahead 3 ms: L = 132 at
  44.1 kHz (latency 142), L = 144 at 48 kHz (latency 154).
- Real-time: buffers sized in `new`; `process` never allocates; output does not
  depend on block size; below the ceiling the gain is exactly 1.0.

## Measured (unit tests, `cargo test --release`)

Ceiling on the BS.1770-4 4x meter, 7 worst-case signals (fs/4 sine at 45 deg
+6 dBTP, naive 1 kHz square, +12 dB white noise, impulse doublets, 18 kHz bursts,
hot drum loop, step into a +9 dB 5 kHz tone):

| ceiling | worst output (4x BS.1770) | worst output (16x long-sinc reference) |
|---|---|---|
| -1.0 dBTP | -1.000 dBTP | +0.62 dB (white noise) |
| -0.1 dBTP | -0.100 dBTP | +1.52 dB (white noise) |

+3 dBFS inter-sample overshoot (samples at 0 dBFS, fs/4 sine at 45 deg, +3.04 dBTP
in): output -0.1009 dBTP, never above -0.1.

Honest limit: the limiter holds the ceiling as BS.1770-4's 4x meter reads it.
A 16x reference with a long Kaiser sinc reads up to +1.6 dB higher on
full-band white noise and about +0.5 dB higher on hot drums and 18 kHz bursts
(tonal and mid-band signals read at or below the ceiling). That is the 4x
filter's own blind spot near Nyquist, inherited by any BS.1770 4x detector.

Dual release: back within 0.5 dB of unity 124 ms after a 1 ms click and
1052 ms after 400 ms of sustained 9 dB limiting.

Sub-threshold audio: bit-exact, delayed by exactly `latency()` (tested at
0.5/1.5/5 ms lookahead). Zero heap allocations while processing:
`tests/no_alloc_tp_limiter.rs`, a counting global allocator in its own test
binary, with a positive control.

References: ITU-R BS.1770-4 (2015) Annex 2; EBU R128 / Tech 3341.
