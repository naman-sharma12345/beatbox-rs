# Oversampled ADAA: alias floor and cost

`src/dsp/adaa_oversampled.rs` runs any ADAA waveshaper (hard clip, tanh,
cubic, quintic; naive, first or second order) inside the half-band
polyphase oversampler from `src/oversample.rs`:

```rust
use beatbox::dsp::adaa::{AdaaOrder, Tanh};
use beatbox::dsp::adaa_oversampled::{OversampledShaper, Oversampling};

let mut sat = OversampledShaper::new(Tanh, AdaaOrder::Second, 4.0); // 2x
let mut strict = OversampledShaper::with_oversampling(Tanh, AdaaOrder::Second, 4.0, Oversampling::X4);
sat.prepare(512);            // optional; off the audio thread
sat.process_block(&mut block); // never allocates
let dry_delay = sat.latency(); // 48.5 base-rate samples
```

- **2x is the default; 4x is opt-in** for strict alias requirements. 1x is
  the plain shaper, bit for bit.
- **Latency is exact**, in base-rate samples (naive / ADAA-1 / ADAA-2):
  1x 0 / 0.5 / 1, 2x 48 / 48.25 / 48.5, 4x 56 / 56.125 / 56.25. A unit test
  measures the phase delay and checks it against `latency()` to 2e-3 samples.
- **No heap allocation while processing.** Buffers are reserved at
  construction (2048 samples) or by `prepare(max_block)`; longer blocks run
  in pieces of that size (same output). A test-only counting allocator
  checks it.
- The half-band filter taps are the oversampler's own, unchanged.
- Opt-in only: nothing in the engine uses it yet, so no render changes.

## The transition floor (read this before quoting a number)

The first 2x stage, which 2x and 4x share, passes up to 0.43 fs and stops
from 0.57 fs (fs = base rate): **20.6 to 27.4 kHz at 48 kHz, 19.0 to
25.1 kHz at the engine's 44.1 kHz**. Harmonics the shaper creates inside
that transition band are only partly removed before decimation and fold
back to just under Nyquist (about 21 kHz at 44.1 kHz). For heavily driven
low notes those leftovers, not the shaper, set the total alias power at 2x
and at 4x. That is filter leakage, not ADAA error. The taps are not
steepened, on purpose (latency and cost).

So there are two honest measures, and the tables give both:

- **Total alias power up to Nyquist.** On a 1 kHz sine, ADAA-2 at 2x is
  about 24 to 27 dB below the naive 1x shaper (hard clip), and **4x does not
  fix that**: it lands 1.4 to 1.6 dB *worse* than 2x, because the floor
  comes from the stage they share. The >= 30 dB bar on this measure is not
  met at 1 kHz by 2x or by 4x.
- **Alias power below 20 kHz only.** The same 2x ADAA-2 path is 43 to 64 dB
  below naive 1x across the four curves (hard clip 43.0 to 43.9), and 4x
  adds more (hard clip 70 to 75).

4x does pay off on total power for high notes, where the harmonics that
matter sit well above the transition band (the 5 kHz table below).

Notes on the numbers:
- tanh at +6 dB makes almost no harmonics: its naive 1x alias level is
  already about -141 dBc, at the f32 rounding floor, so its suppression
  figures (some negative) only compare rounding noise. Likewise tanh +12 dB
  at 2x/4x below 20 kHz (~59 dB for every order) is that floor.
- Cost is the median of 9 runs on a shared 4-CPU Linux box, scalar code;
  expect +-20 % run to run. At 2x and 4x it is dominated by the oversampler
  itself (about 70 and 105 to 120 ns per input sample with the naive
  shaper), not by ADAA. The 1x figures here use noise at drive 4, which
  defeats branch prediction in the clipping curves, so they read higher
  than the sine-input figures quoted for the 1x ADAA shapers earlier.

Reproduce: `cargo run --release --example adaa_oversampled_bench` (about
80 s). The unit test `alias_figures_match_the_bench` pins seven of the
alias figures below to within 0.3 dB.

---

## Bench output: 1 kHz (1000.6 Hz) unit sine at 44.1 kHz, 65536-point FFT

Latency (base-rate samples, naive / ADAA-1 / ADAA-2):
- 1x: 0 / 0.5 / 1
- 2x: 48 / 48.25 / 48.5
- 4x: 56 / 56.125 / 56.25

## hard clip, 5 kHz sine at +20 dB

Alias suppression vs naive 1x (-11.1 / -11.3 dBc), dB (total / below 20 kHz).

| rate | naive | ADAA-1 | ADAA-2 |
|---|---|---|---|
| 1x | 0.0 / 0.0 | 8.0 / 7.9 | 15.0 / 14.9 |
| 2x | 12.5 / 12.3 | 30.4 / 30.5 | 47.7 / 48.7 |
| 4x | 31.2 / 31.1 | 55.1 / 55.0 | 76.7 / 76.6 |

## hard clip

Alias suppression vs naive 1x, dB (total to Nyquist / below 20 kHz). Higher is better.

| drive | rate | naive | ADAA-1 | ADAA-2 |
|---|---|---|---|---|
| +6 dB | 1x | 0.0 / 0.0 | 7.7 / 8.3 | 13.8 / 14.7 |
| +6 dB | 2x | 14.2 / 14.2 | 24.8 / 30.3 | 27.4 / 43.7 |
| +6 dB | 4x | 23.2 / 26.9 | 25.5 / 50.1 | 25.8 / 70.4 |
| | naive 1x level | -46.5 / -47.1 dBc | | |
| +12 dB | 1x | 0.0 / 0.0 | 8.6 / 9.5 | 15.2 / 17.2 |
| +12 dB | 2x | 13.3 / 13.6 | 24.5 / 29.7 | 27.5 / 43.0 |
| +12 dB | 4x | 23.0 / 26.4 | 25.8 / 50.1 | 26.1 / 70.9 |
| | naive 1x level | -40.5 / -41.2 dBc | | |
| +24 dB | 1x | 0.0 / 0.0 | 6.2 / 7.2 | 11.6 / 13.5 |
| +24 dB | 2x | 15.0 / 14.9 | 22.1 / 30.5 | 23.7 / 43.9 |
| +24 dB | 4x | 21.1 / 27.9 | 22.0 / 53.2 | 22.3 / 74.6 |
| | naive 1x level | -25.1 / -26.5 dBc | | |

Cost, ns per input sample (median of 9, blocks of 512, noise at drive 4):

| rate | naive | ADAA-1 | ADAA-2 |
|---|---|---|---|
| 1x | 0.7 | 9.9 | 11.4 |
| 2x | 71.7 | 85.6 | 87.9 |
| 4x | 102.4 | 124.9 | 130.6 |

## tanh

Alias suppression vs naive 1x, dB (total to Nyquist / below 20 kHz). Higher is better.

| drive | rate | naive | ADAA-1 | ADAA-2 |
|---|---|---|---|---|
| +6 dB | 1x | 0.0 / 0.0 | 4.2 / 2.2 | 8.0 / 3.1 |
| +6 dB | 2x | 2.6 / -6.6 | 2.7 / -6.6 | 2.8 / -6.5 |
| +6 dB | 4x | 1.7 / -7.5 | 1.8 / -7.4 | 1.8 / -7.4 |
| | naive 1x level | -140.9 / -150.6 dBc | | |
| +12 dB | 1x | 0.0 / 0.0 | 4.7 / 5.6 | 9.5 / 11.4 |
| +12 dB | 2x | 17.4 / 58.9 | 18.4 / 59.0 | 19.4 / 59.0 |
| +12 dB | 4x | 17.4 / 58.0 | 17.6 / 58.1 | 17.9 / 58.0 |
| | naive 1x level | -78.3 / -85.0 dBc | | |
| +24 dB | 1x | 0.0 / 0.0 | 5.9 / 7.0 | 11.2 / 13.2 |
| +24 dB | 2x | 21.0 / 37.9 | 22.1 / 51.5 | 23.1 / 64.4 |
| +24 dB | 4x | 21.1 / 94.1 | 21.4 / 94.1 | 21.6 / 94.1 |
| | naive 1x level | -28.9 / -30.7 dBc | | |

Cost, ns per input sample (median of 9, blocks of 512, noise at drive 4):

| rate | naive | ADAA-1 | ADAA-2 |
|---|---|---|---|
| 1x | 22.6 | 29.8 | 36.2 |
| 2x | 113.6 | 128.2 | 144.6 |
| 4x | 188.7 | 210.0 | 233.6 |

## cubic (x - x^3/3)

Alias suppression vs naive 1x, dB (total to Nyquist / below 20 kHz). Higher is better.

| drive | rate | naive | ADAA-1 | ADAA-2 |
|---|---|---|---|---|
| +6 dB | 1x | 0.0 / 0.0 | 5.6 / 7.1 | 10.5 / 13.4 |
| +6 dB | 2x | 18.7 / 23.4 | 20.6 / 39.7 | 21.7 / 53.6 |
| +6 dB | 4x | 19.7 / 42.5 | 20.0 / 65.6 | 20.2 / 79.5 |
| | naive 1x level | -60.0 / -62.6 dBc | | |
| +12 dB | 1x | 0.0 / 0.0 | 5.4 / 6.5 | 10.3 / 12.4 |
| +12 dB | 2x | 18.9 / 24.0 | 20.7 / 40.0 | 21.7 / 54.0 |
| +12 dB | 4x | 19.7 / 43.0 | 20.0 / 65.7 | 20.2 / 84.7 |
| | naive 1x level | -45.5 / -48.1 dBc | | |
| +24 dB | 1x | 0.0 / 0.0 | 6.8 / 7.9 | 12.3 / 14.4 |
| +24 dB | 2x | 21.4 / 26.0 | 23.8 / 46.9 | 24.8 / 64.1 |
| +24 dB | 4x | 22.8 / 44.5 | 23.1 / 65.7 | 23.3 / 85.3 |
| | naive 1x level | -22.6 / -23.7 dBc | | |

Cost, ns per input sample (median of 9, blocks of 512, noise at drive 4):

| rate | naive | ADAA-1 | ADAA-2 |
|---|---|---|---|
| 1x | 4.7 | 10.8 | 14.1 |
| 2x | 80.6 | 84.6 | 93.0 |
| 4x | 122.6 | 130.2 | 142.8 |

## quintic (C2)

Alias suppression vs naive 1x, dB (total to Nyquist / below 20 kHz). Higher is better.

| drive | rate | naive | ADAA-1 | ADAA-2 |
|---|---|---|---|---|
| +6 dB | 1x | 0.0 / 0.0 | 6.1 / 6.5 | 12.0 / 12.4 |
| +6 dB | 2x | 22.5 / 31.7 | 24.0 / 46.2 | 25.3 / 59.0 |
| +6 dB | 4x | 22.9 / 57.7 | 23.2 / 69.6 | 23.5 / 70.0 |
| | naive 1x level | -71.8 / -72.9 dBc | | |
| +12 dB | 1x | 0.0 / 0.0 | 6.9 / 6.9 | 13.2 / 13.3 |
| +12 dB | 2x | 30.8 / 32.1 | 44.2 / 46.8 | 55.4 / 59.7 |
| +12 dB | 4x | 56.9 / 58.4 | 65.8 / 81.1 | 71.5 / 90.4 |
| | naive 1x level | -52.1 / -52.1 dBc | | |
| +24 dB | 1x | 0.0 / 0.0 | 7.3 / 8.4 | 12.9 / 14.9 |
| +24 dB | 2x | 20.6 / 23.9 | 24.2 / 37.5 | 25.5 / 50.5 |
| +24 dB | 4x | 23.5 / 49.6 | 23.7 / 72.9 | 24.0 / 91.6 |
| | naive 1x level | -21.4 / -22.3 dBc | | |

Cost, ns per input sample (median of 9, blocks of 512, noise at drive 4):

| rate | naive | ADAA-1 | ADAA-2 |
|---|---|---|---|
| 1x | 1.6 | 11.4 | 14.7 |
| 2x | 70.6 | 83.8 | 90.4 |
| 4x | 105.5 | 125.4 | 139.7 |
