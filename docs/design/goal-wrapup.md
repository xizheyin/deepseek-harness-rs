# Autonomous Goal wrap-up context

## Scope and upstream basis

When an exact automatic Goal round successfully marks the Goal complete or
blocked, the fixed Harness does not end the turn immediately. In
`packages/goal/tool-goal/src/{index,wrapup}.ts` and the wrap-up cases in
`tests/tool-goal.spec.ts`, it defers one model-visible user context after tool
settlement. That context tells the model to address the user with a grounded
closing summary and forbids further tools in that run.

Phase 16 adds this terminal context only for successful complete/block calls
from `GoalToolCaller::GoalRound`. Direct-human terminal updates remain
interactive and inject nothing. Images, background work, and generic public
tool context injection remain out of scope.

## Event order and ownership

The prepared Goal mutation exposes a bounded detached wrap-up snapshot
(objective and optional blocker). The Agent retains at most one pending wrap-up
while settling the model's already-declared tool batch. It appends the context
only after all correlated results, preserving:

```text
assistant/message -> tool/call -> goal/change -> tool/result
                  -> any later declared tool results -> user/message wrap-up
                  -> next Provider request
```

The message source is plugin notice `tool-goal`; complete uses
`<goal_complete>`, blocked uses `<goal_blocked>`. Objective and blocker are JSON
quoted. The instruction says to use only established Session facts, address
the user directly, and call no more tools. A bounded summary is retained in the
source metadata.

## Failure and safety

The context is prepared without side effects. If its event cannot commit, the
turn stops through the existing Session failure path rather than sending a
Provider request that omitted required terminal guidance. Cancellation cannot
undo a committed Goal change; the context is still recorded after already
declared tool results, although a cancelled turn may end before another model
request. No new tool, filesystem, process, approval, or background authority is
granted.

## Verification

Focused tests check exact complete/blocked tags, quoting, source metadata,
Goal-round-only selection, and batch order. Real Goal PTY completion and resume
journeys assert that the post-tool Provider request sees the terminal context.
The fast gate remains local check, focused tests, format, Clippy, and whitespace.
