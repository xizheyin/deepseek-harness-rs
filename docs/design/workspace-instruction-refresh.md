# Tool-driven workspace-instruction refresh

## Scope and upstream basis

Phase 26 adds the active-turn behavior deliberately deferred by Phase 25. The
fixed upstream basis is commit `47f943859bef60e4160492346772ded9b24f765a`,
especially `packages/context/agent-instructions/src/{index,files,state}.ts` and
the dynamic nested workspace-context tests in
`packages/context/agent-instructions/tests/agent-instructions.spec.ts`. Latest
inspected master `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains the same
observable boundary.

This phase refreshes instructions after successful built-in file operations.
It does not add file watching, infer paths from Shell text, make instruction
candidate names configurable, walk above the opened workspace, follow
instruction symlinks, or grant instruction text any tool authority.

## Trusted touch and ownership

`ToolExecutionResult` may carry one crate-private workspace touch. Public tool
constructors cannot set it. The built-in registry adds it only after a real
`read` succeeds; the built-in patch commit adds it only to outcomes whose
durable disposition is `Committed`. The Agent retains the fact only when the
preferred corresponding `tool/result` is actually recorded. Plugin results,
including a plugin tool named `read`, cannot set the field.

The Agent owns a cloneable workspace-instruction runtime constructed from the
same `WorkspaceAuthority` used by the tools. The Session remains the only
durable state: current baseline and nested digests are folded from visible
`agent-instructions` messages whenever refresh runs. The runtime retains at
most 256 already discovered directory names so the same process can re-arm a
nested scope after compaction. Those names contain no instruction content or
digest and are not restart state. Private pending touches exist only to carry
a completed tool result across a cancelled turn; they cannot cause a side
effect and are safe to recompute after restart from a later real touch.

## State and event order

For every tool step:

1. the usual assistant message and `tool/call` are recorded;
2. the built-in tool runs and its truthful `tool/result` is recorded;
3. the current `step/end` is recorded;
4. successful touch paths are deduplicated and their ancestor directories,
   from workspace root to the touched file's parent, are inspected;
5. root/global and all already-visible nested scopes are reconciled too;
6. at most one context message is claimed in the next `step/start` after any
   direct pending input and before that step's Provider request.

Multiple touches in one step are batched deterministically. A refresh that
finds no visible state change adds no event. Touches are cleared only after a
successful no-change reconciliation or after the resulting instruction
message commits. If cancellation closes the turn between `step/end` and the
next step, touches remain pending and the next non-empty turn retries the
read-only reconciliation before its Provider request.

Each refresh also reconciles the visible baseline without requiring a touch.
This lets the same process re-arm instructions after compaction removes their
context message from the current surface. Historical events are never edited.

## Discovery, bounds, and failure behavior

Touched paths are normalized as workspace-relative paths. Absolute paths,
parent traversal, empty paths, and paths that do not identify a descendant
directory contribute no nested scope. Every applicable directory checks the
same ordered candidates as Phase 25: `AGENTS.md`, `CLAUDE.md`,
`AGENTS.local.md`, then `CLAUDE.local.md`. Same-directory trimmed-content
dedup, framing escape, most-specific-first render budgeting, unavailable-source
retention, 1 MiB source limits, and the 65,536-byte message limit are reused.

Refresh is a bounded blocking read protected by the turn cancellation token.
Cancellation stops discovery and truthfully ends the turn; pending touches are
not consumed. Missing files are normal, while unreadable, invalid, oversized,
or symlinked candidates remain unavailable and cannot silently revoke visible
instructions. A task or message-construction failure is an Agent error before
the next Provider request. The refresh itself performs no write and requests
no approval.

## Verification

Focused unit and Agent fixtures cover nested order/dedup, changed and removed
instructions, successful built-in read, committed patch, failed read, rejected
or uncommitted patch, cancellation across the step boundary, plugin
non-forgeability, event order, and post-compaction rearming. One offline real
CLI journey must show that a nested instruction discovered by `read` enters
the next DeepSeek-shaped request. Local formatting, check, focused tests, full
tests, Clippy, and diff checks remain the completion gate; no remote platform
matrix is required for this phase.
