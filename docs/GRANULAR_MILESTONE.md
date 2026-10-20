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

## Latency target (decided)

The effect itself adds 0 samples of latency. The playback path runs at
block 512 by default (about 11.6 ms at 44.1 kHz) with block 256 (about
5.8 ms) as a runtime option. This is the project owner's standing call.
Ceiling note only: playback-only monitoring should stay under about
20 ms. The streamed arm is per-sample, so it holds at any block size.

## Monitoring shape (working default, reversible)

Not picked by the project owner yet. Working default: option 3.

| Option | Behaviour |
|---|---|
| 1 | granular insert is part of the live monitoring chain (heard while playing) |
| 2 | granular is rendered offline only and auditioned after the render |
| 3 | live monitoring with a switch: streamed arm while playing, offline for the final render |

Option 3 is the default because the streamed and offline arms are
bit-identical, so the switch cannot change what is bounced. Nothing built
so far blocks option 1 or option 2: both arms exist and either can be
used alone. Switching the default is a one-line change.
