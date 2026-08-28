# Durable Goal events and recovery

## Problem and scope

Phase 12 loses Goal state when `dsh` exits. Phase 13 makes accepted Goal
mutations and admitted automatic rounds reconstructible from the same Session
log that already owns turns, messages, tools, approvals, and compaction.

In scope are typed `goal/change` events, strict replay, durable local-command
and tool mutations, full Goal message attribution, and disarmed recovery.
Images, background execution, more than one current Goal, configurable caps,
and cross-session Goal transfer remain out of scope.

## Upstream basis

The baseline remains DeepSeek Harness commit
`47f943859bef60e4160492346772ded9b24f765a`:

- `packages/goal/goal/src/{types,domain,fold,index}.ts` defines the version-1
  event, full snapshots, clear tombstones, compare-and-set transitions,
  replayed rounds, and process-local activation;
- `packages/goal/goal/tests/{goal.spec.ts,goal.e2e.ts}` fixes strict decoding,
  revision/identity/phase rules, event-only state, replay, and disarmed restart;
- `packages/goal/tool-goal/src/{index,authority}.ts` and
  `tests/tool-goal.spec.ts` fix tool mutation authority and durable mutation
  before the correlated tool result;
- `packages/goal/goal-round-driver/src/{index,prompt}.ts` fixes the attributed
  round source and sequential continuation.

Latest `master` at `cd5ef8148158c3a752a658978873241fdf8e2bbc`
adds image attachments but does not change the event-sourced recovery rule.

## Durable vocabulary

A non-clear event carries the complete post-mutation snapshot:

```text
goal/change v1
  operation: create | edit | pause | resume | complete | block
  goal: id, revision, objective, phase, optional blockedReason, maxGoalRounds
  roundsStarted, createdAt, updatedAt
```

`clear` instead carries the next `{id, revision}` tombstone and `clearedAt`.
Goal IDs are opaque `goal-<uuid-v4>` strings. Objectives remain trimmed and
bounded to 4 KiB; the Rust cap remains 32 rounds as the documented Phase 12
product limit.

An automatic user message source is exactly `{kind:"goal", goalId, revision,
round}`. Replay accepts only the next positive round for the current active
Goal, matching its ID/revision and staying within its cap.

## Ownership and commit order

Session projection is the durable source of truth. `GoalRuntime` is a
process-local cache plus activation flag and at most one prepared mutation.
Preparation validates a compare-and-set transition but does not change the
visible Goal. While a mutation is prepared, competing local/tool mutations
fail as busy.

Local command order is:

```text
prepare mutation -> append goal/change -> commit runtime cache -> show notice
```

Goal tool order inside an Agent step is:

```text
tool/call -> prepare mutation -> append goal/change -> commit runtime cache
          -> append correlated tool/result -> next Provider request
```

The Agent owns both appends. A sealed prepared Goal-mutation carrier lets the
built-in registry request this order; plugin and external executors cannot
construct it. If the Goal event cannot commit, the runtime preparation is
aborted and the turn stops with the existing Session-storage failure rather
than claiming a mutation.

## Recovery and cancellation

Assembly initializes the runtime from Session projection after replay. Durable
phase, ID, revision, objective, cap, timestamps, blocker, and rounds are
restored; activation is always `disarmed`. No model request is generated until
a direct `/goal resume` mutation commits. A completed Goal may be replaced;
paused, active, or blocked Goals must be resumed, completed, or cleared.

Cancellation before a Goal change append aborts the prepared mutation.
Cancellation after the change commits cannot rewrite history; the correlated
tool result is still settled using the existing durable-result rules. A
cancelled automatic round pauses the Goal through another durable mutation
after the turn closes. Storage failure stops the interactive driver and never
arms work from an uncommitted state.

## Replay and safety rules

- Known malformed `goal/change` payloads fail import; they are not downgraded
  to ignorable events.
- Create requires a fresh ID, revision 1, active phase, zero rounds, and no
  current non-complete Goal.
- Every later mutation preserves ID, advances exactly one revision, and keeps
  created/updated times and round counters monotonic.
- Edit changes only objective; pause/resume/complete/block use closed phase
  transitions; block requires a bounded normalized reason.
- Clear advances the current ref once and leaves a tombstone preventing ID
  reuse.
- Goal changes never enter the model-visible surface. The generated round user
  message does, so its full source is recorded before the Provider sees it.
- No Goal event grants filesystem, Shell, plugin, approval, or background-task
  authority.

## Intentional differences

Rust keeps the established 32-round cap rather than upstream's default 256 and
does not expose per-Goal cap editing yet. Image attachments remain unsupported.
The event vocabulary and disarmed restart semantics otherwise target the fixed
upstream behavior. Compatibility remains `partial` after the local event and
resume evidence because caller-sensitive tool authority, configurable caps,
and attachments are still missing.

## Verification

Tests cover exact JSON, malformed fields, stale revisions, ID reuse, phase
transitions, round attribution, clear, replay, Session resume, failure without
cache mutation, tool event ordering, cancellation pause, and a real PTY
create/exit/`--resume`/show/rearm/continue journey. All traffic uses loopback
fixtures and fake credentials.
