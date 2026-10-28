# Try a rhythm, compare in the mix

This is a manual bridge between the piano Compose controls and the existing
Browser matched A/B comparison. It adds no rhythm algorithm, rendering pipeline,
player, or saved settings.

## Workflow

1. Select a track and source pattern in the piano. Open **Compose**. The menu
   shows names and stable identities, hits, subdivisions, rotation, and
   replace/layer scope. **Write notes** remains the ordinary edit.
2. Click **Try & compare**. The controls and current project context are checked
   before anything is captured. The bridge captures a unique **whole-project**
   checkpoint, then calls the existing Euclidean edit once for this track and
   whole pattern. It is not limited to selected notes. Pattern-source edits can
   affect every arranged occurrence. The checkpoint is not an undo transaction.
3. The trial panel identifies the before checkpoint. Browser opens on
   **Effects & MIDI**. Scroll to **MATCHED A/B**, or use **Open matched A/B** in
   the trial panel to return there. This bounded bridge does not extract or
   duplicate those controls. It does not scroll directly to their row.
4. Check the visible elapsed-second range. Start is prefilled from transport,
   clamped to 0 through 86400 seconds; length starts at 6 seconds. Existing
   Browser controls allow 1 through 20 seconds. This is an elapsed-time
   comparison, not a bar-aligned or pattern-local audition. No arrangement
   position is inferred. Choose an interval where the pattern is audible.
   Unarranged or muted material may not be heard; this bridge does not prove
   that a selected pattern occurs in the chosen excerpt.
5. Explicitly click **PREPARE BEFORE / NOW**, then **PLAY BEFORE / NOW**. The
   existing browser state owns the background job, completion/error reporting,
   ready audio, and invalidation. It compares the selected checkpoint against
   the current whole project at preparation, including any later edits.
6. **Keep change** only dismisses the trial panel. The edit is already applied.
   **Undo trial** invokes one ordinary Undo while it is still exactly the latest
   project edit in the same loaded project and target context. Otherwise it is
   disabled. Use ordinary History to navigate other edits; the trial never
   automatically restores a whole-project checkpoint.

## Listening and side effects

- Preparation captures both versions without restoring the live project or
  starting playback. It renders full mixes in the background and writes the
  existing comparison audio files under the engine work directory. Long songs
  can take time even for short excerpts.
- Playback is before, a one-second gap, then current-at-preparation. Matching is
  attenuation-only, with shared peak safety. Silence/below-gate sides are not
  matched. Different rendered lengths are reported by the existing controls.
- Play stops song transport. Stop preview does not resume song playback.
- No automatic winner, per-pattern solo, or automatic Prepare/Play is added.
- Before checkpoints intentionally remain after Keep, Undo, or a failed edit.
  Save uses the existing checkpoint companion file behavior. No new persistence
  or crash-safety guarantee is made.

## Freshness and failures

The operation holds the engine lock across validation, unique capture, and edit.
A stale frame, invalid target, or invalid control values fails before capture.
Stable identities and names are displayed and validated. Compiled `pl:` playlist
copies are rejected; edit their source pattern instead. If the edit itself
fails after capture, the UI names the retained checkpoint and does not open
comparison. Failure feedback also remains in Compose after the toast expires.

Undo is gated by revision, project-load generation, target identity/name, and
exact post-edit project content. Its availability latches off after an observed
intervening change. A no-op does not gain access to an older undo entry. The
Undo result is checked; a mismatch reports an error, never a fallback restore.

The trial's before reference compares actual checkpoint project content, not
only revision or name. Overwrite/removal or a project load disables the trial
comparison link and clears its selected checkpoint reference. The existing
Browser comparison independently checks captured checkpoint contents, range,
revision, and load generation at preparation completion and playback. No third
comparison job/result state is introduced.

The trial is session-local, not saved UI state. Its metadata holds before/after
project copies for identity and content guards. Keep does not stop an already
requested preview, cancel a comparison job, delete checkpoints, or edit history.
Existing Stop controls remain responsible for preview playback.

## Validation status

Added state regression tests cover scope, layer/replace, unique checkpoint
capture, one-step exact Undo, stale frames, validation failure, rename/deletion,
load-generation mismatch, content changes without revision changes, snapshot
replacement, no-op behavior, and transport non-use. Pointer-event tests exercise
the exact trial action row at 1x and 2x scale, including disabled Undo/Compare and Keep behavior,
without constructing a Studio or opening an audio device.

These new Rust tests are **not executed** in the implementation environment.
There is no Rust toolchain available there; no Cargo, install, audio render, or
GUI launch was attempted. Static source/wording checks and diff review do not
prove compilation, high-DPI layout, playback, or audible usefulness. Existing
Browser freshness, render-isolation, playback and readout tests remain unchanged.
The new tests do not exercise comparison job completion/error/disconnect states,
the post-capture edit-failure branch, or end-to-end Compose pointer navigation. Parent review and adequately provisioned CI are required
before publishing.
