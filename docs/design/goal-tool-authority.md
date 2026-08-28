# Goal tool caller authority

## Problem and scope

Phase 13 persists Goal mutations but lets any model turn call every Goal
mutation. Phase 14 carries the current turn's already validated input origin
through the Agent's sealed tool-dispatch identity and enforces the fixed
upstream caller matrix at execution time.

This phase does not add subagents, steering, a public impersonation field,
image attachments, configurable round caps, or the newer official tool
argument names. It changes only which existing Goal mutations a model call may
prepare.

## Upstream basis

DeepSeek Harness commit
`47f943859bef60e4160492346772ded9b24f765a` is authoritative:

- `packages/goal/tool-goal/src/authority.ts` authenticates the live root Agent,
  treats any accepted `source.kind === "user"` in the open turn as direct-human
  authority, and otherwise accepts only the current exact Goal round;
- `packages/goal/tool-goal/src/index.ts` requires direct-human authority for
  create/edit/pause/resume, permits complete/block from direct human or the
  exact Goal round, and applies the three-round block threshold only to a Goal
  round;
- `packages/goal/tool-goal/tests/tool-goal.spec.ts` covers forged non-human
  input, mixed human/Goal turns, exact rounds, early block rejection, and
  direct-human early block.

## Rust ownership

Before moving `TurnProposal` into the loop, Agent classifies its validated user
messages as `direct-human`, `goal-round`, or `untrusted`. Direct human wins a
mixed turn. A Goal-round classification is useful only after Session accepts
the exact Goal source; malformed ID/revision/round values already fail before
Provider dispatch.

The classification is stored inside the per-call sealed
`ToolDispatchBinding`. It is not added to model arguments and cannot be chosen
by a plugin or external executor. `LocalToolRegistry` reads it only for Goal
tools:

| Operation | Direct human | Exact Goal round | Other |
| --- | --- | --- | --- |
| `get_goal` | allow | allow | allow inside the Agent driver |
| `create_goal` | allow | reject | reject |
| edit/pause/resume | allow | reject | reject |
| complete/block | allow | allow | reject |

For block, the three-started-round threshold is a Goal-round tool policy, not a
Session event invariant. Direct-human block may therefore commit earlier while
an automatic Goal round still fails closed before round three.

## Failure and safety

Authority rejection occurs during side-effect-free preparation after the
durable `tool/call`; it returns a correlated model-visible error and appends no
`goal/change`. Cancellation and storage behavior remain Phase 13 behavior.
The classification grants no filesystem, Shell, plugin, approval, Session, or
background authority.

## Verification

Focused tests cover classification precedence, a Goal round rejecting create
and edit, Goal-round complete, early Goal-round block rejection, direct-human
early block, and no `goal/change` on rejection. The existing real Goal PTY
journeys prove ordinary completion and recovery remain reachable. The local
gate is format, all-target compilation, focused tests, Clippy with warnings
denied, and `git diff --check`.
