# Batch calls: policy and transaction boundary

`batch` executes at most 500 members in order. It accepts `tool`/`args` or
`name`/`arguments` for a member. Successful ordinary project edits are grouped
into one undo step and one outer revision change.

## Effective-action policy

The following six actions cannot execute inside a batch, in either atomic mode:

- `batch`
- `undo`
- `redo`
- `load_project`
- `new_project`
- `produce_track`

This rule applies to the effective terminal action, not just its spelling.
Direct calls, category calls (flat or `params`), cross-category routing, and
`call_tool` forwarding share a side-effect-free invocation normalizer. An
explicit terminal result is not looked up as a category again: legacy
`history {}` and `transport {"action":"status"}` still terminate normally.
Category help is an answer, including help about one of the forbidden actions.

All syntactically resolvable members are checked for forbidden effective actions
before any member executes. A forbidden member rejects the whole request even
with `atomic:false`; the error names its original index/spelling and effective
action. Ordinary unknown tools, malformed wrapper arguments, and other ordinary
validation failures retain their per-call execution behavior. In particular,
with `atomic:false` they do not globally reject unrelated successful members.
No IDs, seeds, handlers, files, or live Project state are resolved or touched by
normalization/preflight. Stable IDs are resolved during execution, so a later
member can reference an effect that an earlier member created.

A request may peel at most 32 `call_tool` wrappers, including wrappers reached
through categories. Category resolution itself is not recursive terminal
redispatch. The existing direct `call_tool` self-target rejection remains.
Wrapper parsing preserves existing required-name, unknown-key, target lookup,
and default-arguments behavior; it does not introduce broader schema/type
validation. Wrapper hops do not add execution depth or choose random seeds.
Terminal validation retains the ordinary fresh/given seed rules and depth
behavior. Routing notes retain inner-result precedence.

An engine-local, owned scope guard keeps the policy active through internal
`call_from`, `call_nested`, and validated dispatch, regardless of diagnostic
source labels. The final dispatch check uses the actual registry tool, not a
caller-supplied display name. The guard unwinds on success, errors, and panics;
a separate Engine does not inherit another Engine's batch policy. This does not
make all internal composite calls share transaction ownership or classify every
registered action's effects. Direct handler function-pointer invocation outside
validated dispatch is not a supported policy boundary.

Outside batches, category/wrapper Undo, Redo, New, and Load remain available.
Normalization dispatches the terminal tool under its ordinary mutation metadata,
not an artificial wrapper transaction. Terminal logs/history labels name the
actual action; peeled wrappers no longer add redundant wrapper log entries or
wrapper-only revision/undo bookkeeping. Routing notes are still attached.
Terminal top-level hooks also apply: a wrapped render now reaches the terminal
render's automatic judging hook. This is a lifecycle compatibility change, not
byte-for-byte preservation of the former wrapper behavior.

## `atomic:true` is Project rollback, not an all-effects transaction

Default `atomic:true` stops at the first failing member and restores project
edits to the pre-batch Project. The error identifies the failing member. Existing
ordinary tool safeguards still apply. This is **not** a filesystem, transport,
MIDI session, checkpoint, sample-bank, cache, or activity-log transaction.

Examples of permitted effects that are not undone by a later failure:

- `snapshot` creates or overwrites an in-memory checkpoint. The checkpoint can
  retain an intermediate Project even when the live Project rolls back.
- `save_project` writes the main JSON and writes or removes its snapshot
  companion. Overwritten/deleted files do not roll back. The save also copies
  live MIDI setup into `project.midi`; restoring the Project can restore that
  field without restoring the files or changing the live MIDI session.
- `transport` commands remain queued for an attached studio to apply later.
- Render/comparison tools can create directories, WAVs and related files.
- Activity log entries and cache/bank effects are not erased on failure.

`atomic:false` keeps successful members and reports ordered per-member
`ok`/`error` results. A failed mutating member restores its own Project snapshot,
not external/session effects. Retained ordinary project edits still produce one
outer undo step; if no Project edit survives, no undo step is added.

The six-action prohibition closes existing alias/forwarding bypasses. It is not
a new general effects allowlist, a safe-save redesign, or a guarantee that every
composite handler has one-undo semantics. Broader transaction guarantees require
explicit effects classification and separate session/file staging design.

## Regression coverage and validation limits

`tests/batch_policy.rs` covers the six-action alias matrix in both atomic modes
and both member-key spellings, exact observable state/file invariants, standalone
aliases/help, hop limits, partial failures, one undo, runtime-created effect IDs,
fresh/given seeds, and Project-only checkpoint/save/transport boundaries. Private
engine tests cover inherited internal forwarding and panic-safe scope cleanup.
Fixtures use isolated temporary directories and no audio or MIDI hardware.

These new tests were written but **not executed** in this constrained worktree.
No Cargo build, test, clippy, installation, render, GUI launch or hardware check
was performed. Compilation and execution remain for the parent on an adequately
provisioned runner; no passing-test or whole-repository safety claim is made.
