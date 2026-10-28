# Audio check

## Command contract

`detect_artifacts` (or the existing `analyze` category action, including
`call_tool` wrappers) is a nonmutating diagnostic. It does not repair audio,
change DSP, play or stop transport, or write audio/image files. It may decode
source audio and populate render caches. Logs can record the invocation.

Choose at most one source: `track`, `sample`, or `path`. Omit all three for the
whole mix. Choose at most one window coordinate system:

* Mix/track: `section`, a `start_beat`/`end_beat` pair, or
  `start_s`/`end_s`. Omitted start means zero; omitted end means available end.
  Beat windows follow the project's tempo curve.
* File/sample: elapsed `start_s`/`end_s` only. Timeline sections or beats are
  errors, not silently ignored. Multiple source or coordinate selectors are
  errors, even when they appear to describe the same region.

Bounds must be numeric, finite, nonnegative, ordered and nonempty, and fit the
existing f32 timeline representation. End past available audio clamps to the
available end. Start at/past available end, empty source, reversed/equal bounds,
or a window containing no samples returns an ordinary error before slicing.
The available stereo length is the shorter channel. Seconds windows use sample
indices at the engine analysis rate, not the input file's original sample rate.
Returned bounds describe those effective sample coordinates without display
rounding. `section` must be a nonempty name or nonnegative integer index.

`tail_s` is **only the outro-analysis horizon**, default 6 seconds. It must be
finite and nonnegative; it never extends an artifact window. Passing an explicit
default and omitting it produce the same window. The separate `render_preview`
command retains its existing `tail_s` ring-out extension and preview length cap.

`max_events` is an integer from **1 to 256**, default 20. Zero, negative,
fractional and larger values are errors. This is the existing per-detector
budget, not a global count of all kinds: click candidates are expanded by eight
for existing drum masking, then reduced to the requested click budget. DC
findings and source-ending/noise findings retain their existing accounting.
The validated budget is bounded before conversion and checked multiplication.
Detector thresholds and producer-facing detector API/defaults are unchanged.

### Additive result metadata and compatibility

The pre-existing `window` **string label stays a string**. A proposed object
under the same key would break that public field. GUI consumers should use the
new additive `window_bounds` object:

```json
{
  "window": "1-2 s",
  "window_bounds": {
    "start_s": 1.0,
    "end_s": 2.0,
    "source_end_s": 10.0,
    "source_end_checked": false
  },
  "scope_note": "Excerpt boundary: source ending not checked. Clean applies only to this window, not the whole source."
}
```

Existing fields (`source`, `clean`, `fail`, `warn`, drum masking count, `events`,
`noise_bed`, `culprits`) retain their shapes. Existing events keep absolute
source-time offsets for the primary scan. Existing per-track culprit event
coordinates are unchanged by this patch.

`source_end_checked` is true exactly when the effective right edge reaches the
available source end. When false, only `truncated_tail` and `end_level_jump`
findings are suppressed because an interior cut is not a song ending. Click,
DC and noise checks still run. `clean` describes only the checked window; it is
not a whole-song clean claim or musical-quality verdict. Even when the source
ending is checked, an excerpt does not check earlier omitted audio. `scope_note`
states this limitation in returned JSON. Per-track culprit scans use their own
available stereo end for this context and still report their existing
click/noise-only subset.

### Verification boundary

Focused integration fixtures live in `tests/artifact_windows.rs`; pure resolver
and endpoint-filter tests live in `src/tools_ears.rs`. Adequate CI must execute
them and the existing ears, dense-trap, and tempo-window regressions. A bounded
source implementation and lightweight helper harness do not establish a full
crate test pass, GUI/native usability, decoded render parity or human listening
approval. No audio output or threshold tuning is intended by this correction.
