# Latency, monitoring and speech_song clamp bounds

Notes only. No code changed. Figures marked "computed" come from arithmetic
on constants in the source, not from a measurement on a device.

## Block size and latency

| Setting | Samples | At 44.1 kHz | Status |
|---|---|---|---|
| Default block (`DEFAULT_BLOCK`, src/rt.rs:10) | 512 | 11.61 ms | computed, not measured |
| Runtime option | 256 | 5.80 ms | computed, not measured |

Block size is a runtime parameter. 512 is the default and 256 is the
runtime option; that is the standing choice.

The numbers above are one block, the least the engine can hold back. They
are not an end-to-end figure. Device buffers, the driver and the output
path add to them, and none of that was measured.

### freq_splitter adds its own delay

The linear-phase FIR delays the signal by `(taps - 1) / 2` samples.

| taps | Delay (samples) | At 44.1 kHz | Status |
|---|---|---|---|
| 513 | 256 | 5.80 ms | computed, not measured |
| 65537 (maximum) | 32768 | 743 ms | computed, not measured |

The streaming arm reports this latency, as the cab_ir arm does for its own.

### Proposal, not a decision

A 20 ms target for playback-only monitoring. Nothing here sets it. It is
written down so it can be accepted, changed or dropped. With 256-sample
blocks one block is 5.80 ms, so about 14 ms would remain for the device and
for any latency-reporting effects (for example freq_splitter at 513 taps,
5.80 ms). Whether that holds on real hardware is unverified.

## speech_song.rs clamp bounds (read from the code; no change)

| Line | Expression | What it bounds | Note |
|---|---|---|---|
| :103 | `.clamp(floor, 1.0)` | per-bin noise gate gain | `floor` = 10^(floor_db/20), set by the caller |
| :324 | `.clamp(1.0, 2.0)` | line stretch factor k | Was `if sing { 2.0 } else { 2.0 }`; both branches were identical and clippy collapsed them. The intended sing vs non-sing bounds are unconfirmed and the question is open with Naman. Do not change them without him. |
| :346 | `.clamp(0.6, if sing { max_sing.max(1.0) } else { 1.35 })` | per-word stretch | `max_sing` = min((src+0.6)/max(src,0.05), 2.6) |
| :724 | `.clamp(-9.0, 9.0)` | pitch correction in semitones | scaled by `tune_hard` before the clamp |
