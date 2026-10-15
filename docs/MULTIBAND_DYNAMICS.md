# Multiband dynamics

`src/dsp/crossover.rs` (LR4 crossover tree), `src/dsp/multiband.rs`
(`MultibandDynamics`, `process_offline`) and the MCP action `multiband_dynamics`
(category `effects`). Opt-in: no existing render path calls it. The older
`fx_extra::multiband` (Butterworth lowpass + complementary subtraction) is left
exactly as it was.

## Crossover (clean-room: Linkwitz-Riley; Zolzer, DAFX)

- LR4 = two identical 2nd-order Butterworth sections (Q = 1/sqrt 2, bilinear,
  prewarped), lowpass and highpass. LP4 + HP4 = the 2nd-order allpass with the
  same poles; LP4 and HP4 are in phase at every frequency.
- N bands from up to 4 ascending crossovers (default 150 Hz and 2.5 kHz). Band i
  is LP4(f_i) after the highpasses below it, then the allpass of every higher
  crossover, so every band carries the same phase and the sum is
  AP(f_0)...AP(f_n) x.
- f64 state, zero latency, no allocation in `process`.

## Compressor (clean-room: Giannoulis, Massberg & Reiss, JAES 2012)

Peak level in dB (channels linked), soft-knee static curve (T, R, W), gain
reduction smoothed by the paper's log-domain smooth decoupled peak detector
(release then attack), makeup. Per band: threshold, ratio, attack, release,
knee, makeup, solo, bypass.

## Measured (unit tests, `cargo test --release`)

- Band sum flatness, 20 Hz..20 kHz, 400 log points: max deviation 2.1e-12 dB
  (150/2500 @ 44.1k), 1.3e-11 dB (150/2500 @ 48k), 7.9e-12 dB (4 crossovers),
  2.1e-13 dB (1 kHz @ 96k). Target was < 0.01 dB.
- Phase alignment: worst adjacent-band phase difference 2.9e-10 degrees where both
  bands are above -30 dB.
- Slopes: -6.02 dB at the crossover, about -48 dB two octaves away (24 dB/oct).
- Null test: every threshold at 0 dBFS (signal at -12 dBFS): no gain reduction in
  any band; output vs an independent AP(150) * AP(2500) of the input:
  max error 1.5e-8 (-139.7 dB re signal RMS, the f32 output rounding).
- A -3 dBFS 60 Hz tone with -30 dBFS 1 kHz / 8 kHz: low band 12.41 dB of
  reduction (curve: 12.7), mid and high 0.00 dB.
- Zero heap allocations while processing: `tests/no_alloc_multiband.rs`
  (counting global allocator in its own test binary, positive control).
