# Granular insert: milestone and open decisions

Status: offline arm and streamed arm implemented on rt-ci2 (see the commit
log). CI is the source of truth for whether they pass; this file records
the design and the two choices that are still open.

## What exists

- Effect type `granular` (raw primitive, reachable through add_effect,
  tweak_effect, describe_effect).
- Parameters: `grain_ms` 5..500, `density_hz` 0.5..200,
  `pitch_semitones` -24..24, `position_jitter_ms` 0..500, `spread` 0..1,
  `freeze_at_ms` (negative = off), `mix` 0..1, `seed`.
- One per-sample state machine (`fx_extra::GranularCore`) runs both the
  offline pass and the streamed arm, so they agree bit for bit at any
  block size. The effect is causal: a grain only reads input at or before
  the sample being produced.
- All buffers (history ring, frozen slice, grain pool of at most 512) are
  allocated when the chain is built; the audio path does not allocate.
- `mix` 0 leaves the audio untouched (bit-exact). NaN, infinity and
  denormal input are cleaned to zero inside the core.

## Open decision 1: latency target

The effect itself adds 0 samples of latency. The question is what the
monitoring path around it is allowed to cost. Options:

| Option | Budget | Notes |
|---|---|---|
| A | playback block 256 (about 5.8 ms at 44.1 kHz) | tightest; needs the 256 runtime option |
| B | playback block 512 (about 11.6 ms), default | matches the current default block |
| C | up to 20 ms playback-only | headroom for a lookahead-free chain plus one block |

Proposal: B as the default, A as a runtime option, C as the stated
ceiling. Latency is a parameter of the playback path, not of this effect.

## Open decision 2: monitoring shape

| Option | Behaviour |
|---|---|
| 1 | granular insert is part of the live monitoring chain (heard while playing) |
| 2 | granular is rendered offline only and auditioned after the render |
| 3 | live monitoring with a switch: streamed arm while playing, offline for the final render |

Proposal: 3. The streamed and offline arms are bit-identical, so the
switch cannot change what is bounced.

## Not decided here

The 20 ms figure and the monitoring shape are proposals. Neither is
decided until the project owner picks one.
