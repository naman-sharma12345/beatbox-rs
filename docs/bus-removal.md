# Bus removal: shared routing impact and confirmation

Bus removal now plans its dependencies before changing the project. The same
walker supplies the read-only preview, the rack confirmation, and the actual
cleanup. This is a routing-integrity change, not a DSP or gain-law change.

## Preview through the mixer category

```json
{"action":"preview_remove_bus","params":{"name":"mix"}}
```

The category is `mixer`. The fine-grained `preview_remove_bus` action also works
directly and is available through normal search/describe/help discovery. It
accepts the bus name or stable ID, including case aliases. A successful result
contains:

* `bus`: current display name.
* `bus_id`: stable identity to use for the later deletion.
* `revision`: the engine revision of this preview.
* `impact`: the exact dependencies that the removal will change:
  * `track_outputs`: track `{id, name}` entries returning to master.
  * `bus_outputs`: bus `{id, name}` entries returning to master.
  * `sends`: `{id, track_id, track}` entries to remove.
  * `automation`: `{id, target, param}` lanes to remove.
  * `automation_clips`: `{id, name, target, param}` clips to remove.
  * `macro_targets`: `{macro, target, param}` mappings to remove. Macros have
    no stable target IDs in the existing project model; entries retain order
    and multiplicity. The macro knob and its unrelated mappings survive.

Preview does not modify the project, normalize IDs, change the revision, affect
undo/redo or checkpoints, or access project files. Like other read actions,
calling it through the engine adds a normal call-log entry. Opening the rack's
preview uses the same helper on its snapshot without even an engine call.

## Commit after review

Use the actual `bus_id` and `revision` returned by the preview, not the example
values below:

```json
{"action":"remove_bus","params":{"name":"bus7","expected_revision":42}}
```

The engine checks `expected_revision` before walking or mutating. A mismatch
returns an error containing `StaleRevision`; refresh the preview and review the
new impact before retrying. Invalid revision values also fail without changes.
No generic revision or persisted preview-token system was added.

For compatibility, direct `remove_bus` calls may omit `expected_revision` and
perform the existing immediate deletion. Clients relying on a reviewed preview
must supply the guard and use the stable ID. Reusing a deleted bus's display
name does not restore the old identity or its routes/controls.

Commit keeps the existing `removed` and `rerouted_to_master` result fields,
adds `rerouted_buses_to_master`, and returns the same `impact` lists as the
matching preview. The engine adds its usual resulting revision/change summary.
The successful deletion is one undo step, including all dependency cleanup.

`rack_apply` already delegates bus deletion to this same engine action. A rack
client can pass the preview's revision to the existing outer guard:

```json
{"commands":[{"op":"remove_bus","strip":"bus7"}],"expected_revision":42}
```

Rack command result envelopes remain unchanged. No second routing model or
separate rack-only deletion implementation was added.

## Rack interaction

Each bus strip has a **Remove...** button. It opens **Review bus removal** with
the affected track/bus outputs, sends, automation lanes/clips, and macro
controls. Owners and send destinations are shown by readable names rather than
stable IDs where resolvable. Insert controls use one-based insert numbers.

**Cancel** makes no edit. **Refresh impact** recomputes the shared preview.
**Remove bus** commits only a current preview. A changed project disables that
button until refresh/review. A missing bus disables both refresh and commit;
cancel and select another bus instead. Refresh never falls back from the old
stable ID to a replacement with the same display name. The engine rechecks the
revision inside the edit call, protecting the interval after the frame snapshot
was acquired. The GUI stays manual, with no new production or chat controls.

## Dependency scope

Names and stable bus IDs are recognized in incoming track outputs, bus
outputs, and send destinations. Actual `bus_index` resolution wins; cleanup
uses case-insensitive IDs only as a fallback when that resolver finds nothing.
This cleanup recognition does not guarantee global playback support for every
stored case/ID alias. No playback resolver was changed.

Controls owned by the deleted bus are removed. Incoming send controls use the
owning track's concrete sends: each send matches its stored destination or
stable send ID case-insensitively, with the existing `send`/`sends` spellings.
A surviving send match takes precedence over dangling bus-alias cleanup, so a
bus named after an unrelated surviving send ID cannot steal that send's lane,
clip or macro mapping. A send alias matching both removed and surviving sends
fails preview and commit with `AmbiguousBusRemoval`, without edits. Owner aliases
shared by the removed bus and a surviving track, bus or master also reject
conservatively. Resolve those ambiguous stored references before retrying.

A send ID matches only controls on its owning track. Unrelated routes, sends,
clips, lanes, and macro mappings retain their order and identity. Cleanup uses
precomputed indices in reverse order where elements are removed.

## Validation status

This routing fix was prepared under a 2 GiB cgroup. No Cargo build, Rust test,
clippy run, audio render or native GUI launch was performed for this fix.
Formatting/parser and source checks are not compilation or runtime evidence.
The parent separately reported nine isolated render-helper tests passing via
direct rustc at 19:35 UTC on 2026-10-28; that is not a full-crate or routing-test
pass, and was not rerun for this fix.

On an adequately provisioned runner, execute the existing
`tests/bus_remove_routing.rs`, new `tests/bus_remove_preview.rs`, existing
send/channel cleanup tests, rack command tests, and the rack-view tests under
the GUI feature. The rack-view tests drive panel/removal controls with egui pointer events,
not the complete dialog lifecycle in Studio. Guarded commit is tested separately
through Engine. The manual wording scan covers 17 source files, including
`render_job.rs`. Collision regressions cover case-variant surviving send IDs,
all three control kinds, ambiguity rejection, actual bus resolution and undo. Native layout at compact,
large and high-DPI sizes still needs visual verification; no screenshot or
audible-quality claim is made here.
