# Manual Session title rename

## Problem and scope

Automatically generated titles are useful but sometimes too vague. Phase 48
lets a person rename the current Session with `/rename <TITLE>`. The result is
durable and visible everywhere that already reads the latest Session title.
This phase does not add explicit refresh/unpin, remote rename, a title index or
renaming another running process's Session.

## Upstream basis

The semantic baseline remains
`47f943859bef60e4160492346772ded9b24f765a`. The focused sources are
`packages/session/session-title/src/index.ts` and
`packages/session/session-title/tests/rename.spec.ts`. Fresh `origin/master` at
`cd5ef8148158c3a752a658978873241fdf8e2bbc` keeps the same rename tests and
observable rules.

## Input, output and event order

The terminal accepts `/rename <TITLE>` only while idle. The raw suffix is
normalized with the existing title normalizer: terminal controls and invisible
directional characters are removed, whitespace is collapsed, and the result
is capped at 80 UTF-8 bytes. An empty result is rejected.

For a valid input the Agent first ensures the Session journal exists, then
supersedes its one owned automatic-title task, and finally appends exactly one
log-only `session/title` event:

```text
title = normalized input
messageSeqs = []
source.kind = user
```

The command returns the accepted normalized title. The latest-title projection
and durable replay need no new state or schema.

## Ownership, failure, cancellation and timeout

`AgentLoop` remains the sole owner of the Session and exposes the idle rename
operation. `SessionTitleRuntime` owns automatic-title cancellation. If a title
task is running, rename cancels it and waits up to the existing one-second
shutdown grace before aborting and joining it. This prevents a late provider
result from appending after the user title.

An invalid title changes nothing. A journal materialization or append failure
is returned as an Agent error and no success is shown. The operation has no
turn, step, approval or tool event. During an active turn the enhanced terminal
reports that rename must wait; it does not queue a hidden metadata mutation.

## Side effects and safety

The only side effect is one bounded append to the current Session journal.
There is no model request, filesystem path supplied by the model, tool
execution or network write. Normalization prevents terminal escape injection,
and the existing event validator enforces the same canonical 80-byte limit on
replay.

## Recovery and pinning

The user-sourced event is the latest-title source of truth after restart.
Constructing `SessionTitleRuntime` from any Session that already has a title
leaves automatic generation done, so later prompts cannot replace the manual
title. This matches the official pin behavior. Official `refresh()` is the
deliberate unpin and remains outside this phase.

## Tests and compatibility

The source-attributed fixture records normalization, empty rejection, event
shape, in-flight supersession and pinning. Rust tests cover the runtime event,
provider cancellation, a later prompt, parsing, command completion and both
terminal paths. Local formatting, compilation, test and lint gates provide the
release evidence.

## Intentional differences

Official code exposes a synchronous service method for any exact live Session
object. Rust exposes one async command on the current idle Agent because the
journal has a single writer and automatic provider work must be joined. The
user-visible result and append-only fact are the same; Rust may wait up to one
second for cleanup. Explicit refresh/unpin is not implemented, so a manual
title stays pinned for the rest of the Session.
