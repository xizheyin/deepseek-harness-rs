# Goal automation design

## Scope

Phase 12 closes the most visible usability gap with official DeepSeek Harness:
an interactive user can give `dsh` a longer-running objective and let it keep
making bounded progress without manually typing “continue” after every turn.

The first Rust slice provides:

- one process-local Goal with objective, revision, phase, activation, round,
  and consecutive-block counters;
- `/goal`, `/goal <objective>`, `/goal edit <objective>`, `/goal pause`,
  `/goal resume`, and `/goal clear`;
- model tools `get_goal`, `create_goal`, and `update_goal`;
- sequential automatic Goal rounds while the Goal is active and armed;
- cancellation that pauses/disarms the Goal and a fixed maximum round count.

Durable Goal recovery, image attachments, multiple Goals, background work,
subagents, and a general scheduler are outside this slice.

## Upstream basis

The semantic baseline remains commit
`47f943859bef60e4160492346772ded9b24f765a`. The researched packages are:

- `packages/goal/command-goal/{README.md,src/index.ts,src/invariant.ts}` and
  `tests/command-goal.spec.ts`;
- `packages/goal/goal/{README.md,src/types.ts,src/fold.ts}`;
- `packages/goal/goal-round-driver/{README.md,src/index.ts,src/prompt.ts}`;
- `packages/goal/tool-goal/{README.md,src/index.ts}`.

Latest `master` was also inspected at
`cd5ef8148158c3a752a658978873241fdf8e2bbc`. Its material Goal addition is
image attachment support for create/edit. Rust has no interactive image input,
so that extension remains deferred without moving the fixed baseline.

## State and ownership

`GoalRuntime` is the single owner and is shared by the interactive driver and
the local tool registry through a mutex. A snapshot contains one non-empty
objective, monotonically increasing revision, `active`/`paused`/`blocked`/
`complete` phase, armed/disarmed activation, completed automatic round count,
and consecutive blocked-round count. Objectives are at most 4 KiB and generated
round prompts are bounded by the existing Provider request limit.

The interactive command path may create, edit, pause, resume, or clear a Goal.
The model may read, create, edit, pause, resume, complete, or block it through
closed schemas; blocking succeeds only after reports from three distinct
consecutive autonomous rounds. Every update checks the expected revision so a
stale tool call cannot settle a newer user edit.

## Round sequence

1. An accepted `/goal <objective>` creates an active armed Goal.
2. Once the driver is idle and no queued human prompt has priority, it builds a
   `<goal_round>` user message containing the JSON-quoted objective and
   `Round: n/max`.
3. The ordinary Agent Loop records and runs that message. All existing model,
   tool, approval, timeout, cancellation, and cleanup rules remain unchanged.
4. A successful turn returns to the driver. If the Goal is still active and
   armed, the next round is eligible; `update_goal` completion/blocking stops it.
5. `Ctrl+C`, a failed turn, or the round cap pauses/disarms automatic work. No
   failed round is silently retried.

Human queued input wins before another generated Goal round. Goal commands and
their local notices are not model-visible; generated Goal-round prompts are
model-visible and therefore use the ordinary recorded user-message path.

## Failure, cancellation, and safety

- Invalid, unknown, oversized, stale-revision, or illegal-phase mutations fail
  before state changes.
- Goal tools have no approval bypass and no external side effect beyond this
  bounded process-local state.
- Only one model/tool turn runs at a time. There is no background task or
  unowned queue.
- Cancelling a running Goal round first cancels the ordinary Agent turn, waits
  for its cleanup, then pauses/disarms the Goal.
- The fixed round cap prevents an unbounded automatic loop.
- Process termination loses the Goal. Session resume starts with no active
  Goal, so uncertain old work is never automatically replayed.

## Intentional differences

Official Goal changes are durable `goal/change` events, activation is separately
process-local, the default cap is 256, and current `master` can attach images.
This fast Rust slice keeps all Goal state process-local and uses a smaller cap.
That means a restart cannot show or resume the old Goal, but it also cannot
silently restart uncertain work. Durable typed events and recovery are the next
compatibility slice.

## Verification

Focused tests freeze command parsing, objective/revision/phase invariants,
three-round blocking, cap behavior, exact tool schemas/results, automatic
prompt order, human-input priority, and cancellation pause. One loopback PTY
journey must create a Goal, observe at least two generated rounds, let the model
complete it, and return to idle without another request. Local format, compile,
Clippy, and whitespace checks are the acceptance gate selected by the user.
