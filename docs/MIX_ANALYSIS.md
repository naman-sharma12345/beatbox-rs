# Mix analysis primitives: analyze_loudness and carve_frequencies

Two MCP actions (`analyze_loudness` in category `analyze`, `carve_frequencies`
in category `mixer`), backed by `src/dsp/loudness_meter.rs`,
`src/dsp/true_peak.rs` and `src/dsp/carve_eq.rs`. Opt-in: no existing render
or analysis path calls them; `analysis::loudness_at` and every existing
report are unchanged (only `analysis::k_weighting` became `pub(crate)` so the
new meter reuses it instead of duplicating it).

## analyze_loudness (clean-room: ITU-R BS.1770-4, EBU R128, Tech 3341, Tech 3342)

- K-weighting (BS.1770, any sample rate), channel weights 1.0 (L/R/C), 1.41 (surrounds).
- Momentary: 400 ms, short-term: 3 s, both every 100 ms.
- Integrated: 400 ms blocks, 75 % overlap, gates -70 LUFS absolute, -10 LU relative.
- LRA (Tech 3342): short-term values, gates -70 LUFS absolute and -20 LU relative,
  95th minus 10th percentile (nearest rank).
- True peak: BS.1770-4 Annex 2 4x interpolator plus the sample peak.
- A file is measured at its own sample rate; the mix at 44.1 kHz.

## carve_frequencies (sidechain spectral ducking)

A key (the vocal) drives a peaking cut in one band of a target only while the key
has energy in that band: key bandpass -> 10 ms RMS -> cut = (level - threshold)
(1 - 1/ratio), clamped to max_cut_db -> attack/release -> RBJ peaking EQ at the
band's geometric centre, Q = centre / bandwidth (mode `bell`), or an RBJ shelf
(S = 1) with its corner at high_hz (`low_shelf`) or low_hz (`high_shelf`),
refreshed every 16 samples. Phase: minimum-phase IIR (poles and zeros inside the
unit circle, tested); the action reports the group delay at the band centre /
shelf corner for the deepest cut it applied.
Presets: `mud` (150-400 Hz), `presence` (2-4 kHz). With a quiet key the target
passes bit for bit. Zero latency; no allocation in `process`
(`tests/no_alloc_carve.rs`).

## Measured (unit tests, release build)

analyze_loudness
- Tech 3341 integrated (synthesized): -22.993 / -32.993 / -23.014 / -23.014 / -22.979 LUFS
  against -23 / -33 / -23 / -23 / -23 (all within +-0.1 LU).
- Momentary (400 ms) on its 3341 case: -22.961 .. -22.960 LUFS; short-term (3 s): -22.986 LUFS (expected -23.0).
- Tech 3342 LRA cases 1-4: 10.000 / 5.000 / 20.000 / 15.000 LU (expected 10 / 5 / 20 / 15).
- K-weighting at 48 kHz against the BS.1770-4 printed coefficients: 0.00000 dB difference at
  100 Hz (-1.1335 dB), 1 kHz (+0.6977), 2 kHz (+3.0713), 10 kHz (+4.0419).
- Gates: a -72 LUFS block is dropped by the -70 absolute gate; with a -25.74 LUFS ungated level a -36 block is
  dropped and a -35 block kept by the -10 LU relative gate.
- True peak, Tech 3341 cases 15-18 (fs/4 sine, -6 dBFS peak, tolerance +0.2 / -0.4): phase 0 -5.911,
  45 deg -5.917, 60 deg -5.754, 67.5 deg -5.858 dBTP. Case 17 (60 deg) is OUTSIDE the +0.2 tolerance by
  0.046 dB. Cause: the BS.1770-4 Annex 2 interpolator's middle branches have +0.22 dB gain at exactly fs/4
  (12 kHz at 48 kHz), plus onset overshoot of the abruptly started test tone; with 20 ms fades the reading is
  about -5.796 (still +0.004 over). The filter is kept as published (it is the same file the true-peak
  limiter is gated on); this is a documented miss, not hidden.

carve_frequencies (minimum-phase RBJ biquads, coefficients refreshed every 16 samples from a one-pole
smoothed cut in dB)
- Null with the key below threshold: max |dry - out| = 0 (bit-exact) for bell, low shelf and high shelf.
- Error against the analog prototype at 16 kHz, 12 dB max cut: bell 150-400 Hz +0.003 dB, bell 2-4 kHz
  +0.189 dB, low shelf 0.000 dB, high shelf -0.004 dB; at the band centre / corner every mode is within 0.0001 dB.
- Phase and group delay at max cut (12 dB): bell 150-400 Hz 0.00 deg and -83.91 samples (-1.903 ms) at 245 Hz;
  bell 2-4 kHz 0.00 deg and -10.78 samples (-0.244 ms) at 2828 Hz; low shelf +52.97 deg at 400 Hz;
  high shelf -52.97 deg at 2000 Hz (shelf group delay at the corner ~0). All poles and zeros inside the unit circle.
- Dynamic sweep (key ramping on/off every 100 ms with 5 ms ramps, attack 5 ms, release 50 ms, 12 dB max cut on a
  250 Hz + 4 kHz target): cut moved 5.56 .. 12.00 dB, at most 0.651 dB per 16-sample refresh; max sample-to-sample
  step 0.14546 against 0.14359 for the static 12 dB run (x1.013) and 0.15072 dry; max second difference 0.07958
  (static 0.07928, dry 0.07999); group delay at 245 Hz stayed within the static max-cut value (|gd| <= 83.907
  samples) and changed by at most 4.24 samples per refresh (no jumps).
- Allocation: 0 heap allocations in `Carver::process` (counting allocator in its own test binary,
  `tests/no_alloc_carve.rs`, with a working positive control and a check that the key actually carved).
