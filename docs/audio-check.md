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

### Scheduled percussion source policy

Click masking uses bounded scheduled source membership, not full acoustic
attribution. Whole-mix masks include percussion-role tracks satisfying the
renderer's `!mute && (!any_solo || solo)` predicate. `any_solo` counts all
tracks, including muted solos. Explicit track scans resolve the same project
track index as the solo renderer and use only that track's scheduled percussion
attacks, including its selected-unmute override. Tonal tracks contribute none.
Each per-track culprit pass uses its own track mask, never the primary mix mask;
the existing unmuted-track iteration and culprit time coordinates are unchanged.
File and registered-sample scans receive no project-timeline attack masks.

Existing instrument/name role heuristics, schedule probability and tempo mapping,
sorted/deduplicated coordinates, masking margins, detector thresholds and budgets
are unchanged. `drum_attack_transients_ignored` still counts only clicks actually
removed from the primary report. The public legacy `drum_onsets` helper used by
producer-facing listening remains unchanged, including its non-solo policy.

This does not determine full-path audibility: bus routing/mutes, sends, faders,
automation, sidechains and master/effect processing can change an attack's
acoustic contribution. A scheduled coincidence is not proof of causation, and
neither fewer findings nor `clean` is a musical-quality verdict. No renderer,
DSP or audio-output behavior is changed. Focused authored source controls live
in `tests/artifact_source_scope.rs`; execution and audio parity require actual
runner evidence, not just authored assertions or unchanged source-file hashes.

### Verification boundary

Focused integration fixtures live in `tests/artifact_windows.rs`; pure resolver
and endpoint-filter tests live in `src/tools_ears.rs`. Adequate CI must execute
them and the existing ears, dense-trap, and tempo-window regressions. A bounded
source implementation and lightweight helper harness do not establish a full
crate test pass, GUI/native usability, decoded render parity or human listening
approval. No audio output or threshold tuning is intended by this correction.

## Bounded native screenshot scenarios

The existing `studio --screenshot` path accepts **screenshot-only environment
selectors**, not a new command, command schema, workflow or control endpoint:

* `BEATBOX_AUDIO_SCENARIO=populated|interior|invalid-range|retry|prepared|stale|long`
* `BEATBOX_NO_LISTEN=1` is mandatory when selecting a scenario. Studio honors
  this guard before attempting to bind its existing control server.
* `BEATBOX_AUDIO_SCALE=1|2`, default 1; other values fail before opening Studio.
* Existing `BEATBOX_NARROW_SHOT=1` selects the 1100 by 700 logical-size target;
  omit it for 1480 by 920. At 2x the intended physical PNGs are 2200 by 1400
  and 2960 by 1840 respectively, not those pixel sizes squeezed into half the
  logical space. Native results record both logical viewport and pixel size;
  CI checks exact physical size and scale. Actual platform behavior is still
  subject to the unrun native acceptance gate below.

Without `BEATBOX_AUDIO_SCENARIO`, ordinary screenshot view, dimensions, timing
and initial cursor seek remain unchanged. The old screenshot autoplay is removed
for **all** captures, including the original ten, to honor the no-autoplay safety
requirement; this is the one intentional ordinary-capture behavior change.
A selected scenario forces
Mixer, opens the existing Mix analysis header through its existing initial-open
condition, and removes the empty header opener on the next frame. It never
inserts an audio report or a completed job. Scenario capture does **not** run
ordinary screenshot autoplay, call Play, queue an audition, or claim listening.

### Real actions and capture conditions

The adapter calls the same `request_check` and `request_prepare` methods as the
pane buttons. Those methods launch the real private snapshot workers, invoking
`detect_artifacts` and `render_preview`. Only `pump` accepts their actual
completion/error. The adapter never supplies `Output`, parses a fabricated
report, or changes a pending worker into a ready one. This is handler-adapter
coverage, **not** native pointer/keyboard interaction coverage.

Each scenario first waits for the ordinary song render to be ready. It then
waits for its own genuine analysis/preparation result, and for five consecutive
frames with its required state-specific rectangles inside the actual pane clip:

| Selector | Required terminal state |
| --- | --- |
| `populated` | Current nonempty detector report; summary, ending notice and first event heading visible. |
| `interior` | Real check of 0 to 1 second; structured metadata confirms available source continues beyond that boundary and ending is not checked; report and notice visible. |
| `invalid-range` | Successful underlying song render, then an ordinary worker error from a 60000 to 60001 second analysis request, not initial song-render failure or local DragValue validation. Error visible. |
| `retry` | The same genuine range error recorded in actions; parameters corrected to full mix; a second real check finishes with a populated current report. |
| `prepared` | First real finding prepared through `render_preview`, actual written WAV decoded; prepared label and enabled Play control visible. Play is never activated. |
| `stale` | Real check and preparation, then `set_tempo` via `call_from` on the exact shared Studio Engine; pump invalidates; stale notice and actually disabled Play control visible. |
| `long` | Actual detector returns at least 21 findings; real pane scroll reaches the final finding and end marker. Fewer findings fail instead of injecting rows. |

The `stale` call uses a source tag on a direct shared-Engine call. No external
MCP client, network server, cross-process edit, or external-MCP proof is claimed.
The native path deliberately disables listening. An enabled Play screenshot
proves preparation/current eligibility only, not playback or listening quality.
Pending screenshots are omitted: no artificial worker delay or synthetic
pending/completion state is introduced to make them easier to observe.

There is a 90-second total scenario deadline, including song render and every
worker/visibility phase. On failure Studio logs the error and closes without a
successful PNG/result pair. The workflow additionally bounds each subprocess
with `timeout 110s` and a 120-second supervising timeout. The existing job has a
45-minute overall deadline. Nothing dispatches that job automatically from a
local implementation/review action; its existing triggers are unchanged.

### Existing workflow, fixture and exact file contract

`.github/workflows/screenshots.yml` keeps the original ten rich-project captures
and its single existing artifact uploader. It requires all ten, not `ok > 0`.
For these new scenarios only, it creates one deterministic four-second stereo
PCM16 44.1 kHz WAV: a low-amplitude 220 Hz sine with 32 isolated impulses. It
uses existing `new_project`, `remove_effect`, `import_sample`, `add_track` and
`add_audio_clip` commands to author one neutral diagnostic project, then reuses
that project for every native case. These deliberately defective authored
signals are detector evidence, not examples of music or taste improvements.
No additional producer campaign, dependency, secret, permission, public
listener or publishing mechanism is added.

At 1x all seven selectors run at both sizes. At 2x `populated` and `long` run at
both sizes. All 18 exact stems are listed in `native-results-manifest.json`:
`audio_<selector>_<1100x700|1480x920>_<1|2>x`. Each requires a nonempty `.png`,
`.json` and `.log`. The workflow executes the **existing screenshot tool** under
Xvfb with a display large enough for each physical viewport; the child Studio
inherits the selectors. Missing states, dimensions, files, insufficient genuine
findings, or preview outputs fail the job. New screenshots are not satisfied by
an unrelated successful standard capture.

Example invocation on an adequately provisioned built checkout (not run as part
of the bounded source-only implementation):

```sh
BEATBOX_NO_LISTEN=1 BEATBOX_AUDIO_SCENARIO=stale BEATBOX_AUDIO_SCALE=1 \
BEATBOX_NARROW_SHOT=1 \
xvfb-run -a -s '-screen 0 1100x700x24' timeout 110s \
./target/release/beatbox call screenshot \
'{"view":"mixer","out":"/absolute/path/shots/audio_stale_1100x700_1x.png","inline":false}' \
--project /absolute/path/shots/native-fixture/project.json
```

The direct existing `studio <project> --screenshot <png> --view mixer` form
accepts the same environment selectors. The wrapper invocation above is the
workflow route; no new command-line parser behavior is added.

### Provenance, safety and remaining acceptance

The existing upload directory gains checked-out SHA / event / ref / run ID,
build SHA, active verbose `rustc`, executable SHA-256, fixture hashes and authoring
log, the fixture itself, exact expected-case manifest, per-case invocation and
logs, native viewport results and PNG hashes. The build embeds
`BEATBOX_BUILD_SHA`; native result files record that value and CI compares it
with the build/checkout record. Native results include request actions, raw
check data, captured Project, actual preview request/result metadata, decoded
frame count/duration, currentness and widget enablement, clip/row geometry and
terminal visibility. Prepared WAVs are copied from the declared renders
directory into the same existing artifact, with hashes and WAV frame/rate checks;
a stale preparation's output remains traceable in its pre-edit action record.
No audio is played automatically. Source project/sample hashes must stay
unchanged across all cases. All preview writes are declared effects.

CI validates PNG chunk CRCs, RGBA8 decompression and inverse row filters,
decoded-pixel hashes, exact dimensions and actual pixels-per-point. Native pixels, readability, exact 2x behavior and the final row
still require parent download and visual inspection. Artifact-provider digest,
download digest and safe ZIP entry verification belong to that later parent
inspection; the workflow does not claim them in advance. Its manifest says
`captured_uninspected`, never visual acceptance. This implementation has not
been Cargo-checked, Rust-test-executed, launched natively or run on CI. No native
capture, external MCP demonstration, decoded-audio parity or listening result is
claimed. Focused selector/gate, real range-error/retry and real authored-project
long-report tests are authored in `audio_check.rs`, not executed locally.

All new production code in `audio_check.rs` remains before its first test gate.
The inherited `gui/mod.rs` prefix wording scanner still omits production after
its early `save_state_tests`; this extension does not refactor that parser or
claim whole-module wording coverage. Lightweight static checks do not replace
adequately provisioned type checking, tests, CI captures or independent review.
