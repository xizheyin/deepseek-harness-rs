# Official Goal tool contract

## Scope

Phase 15 replaces the temporary Phase 12 Goal-tool arguments and result shape
with the fixed upstream contract. It also makes the existing durable
`maxGoalRounds` and blocked reason fields reachable from the model tools.

In scope are closed argument schemas, exact Goal ID plus revision checks,
optional create/edit caps, model-reported blocker text, and the canonical
`{goal, activation}` result. Autonomous wrap-up context, image attachments,
subagents, background work, and changing the Rust default cap remain out of
scope.

## Upstream basis

At commit `47f943859bef60e4160492346772ded9b24f765a`:

- `packages/goal/tool-goal/src/index.ts` defines `create_goal {objective,
  max_goal_rounds?}` and `update_goal {goal_id,revision,action,objective?,
  max_goal_rounds?,blocked_reason?}` plus conditional-field validation;
- `packages/goal/goal/src/index.ts` trims objectives and blocker text, requires
  positive safe-integer caps, lets edit replace objective and/or cap without
  changing phase/block reason, and records blocker code `model-reported`;
- `packages/goal/goal/src/fold.ts` permits cap changes only on edit and keeps
  every other definition field stable;
- `packages/goal/tool-goal/tests/tool-goal.spec.ts` fixes the compact output,
  stale ref behavior, empty filler handling, cap-only edits, blocker validation,
  and caller-sensitive block threshold.

## Contract

`get_goal` returns `{ "goal": null }` when absent. Otherwise all three tools
return:

```text
{
  goal: { id, revision, objective, phase, roundsStarted, maxGoalRounds,
          blockedReason? },
  activation: "armed" | "disarmed"
}
```

Updates require both exact `goal_id` and `revision`. Edit requires at least one
meaningful objective/cap replacement. Pause/resume/complete reject replacement
fields. Block requires non-whitespace `blocked_reason`, trims it, and persists
`{code:"model-reported", message}`. Empty string/zero optional fillers are
ignored only where the official tool does so.

Rust continues to default new Goals to 32 rounds instead of upstream's 256,
and stores caps as positive `u32` values. This is a documented bounded-product
difference; the chosen cap is durable and can be raised or lowered through an
edit.

## Safety and ordering

Parsing and conditional validation remain side-effect-free and precede the
Phase 13 prepared mutation. A wrong ID/revision, invalid cap, invalid blocker,
or forbidden field returns a correlated error and appends no `goal/change`.
Accepted calls retain `tool/call -> goal/change -> tool/result`; caller
authority remains Phase 14's sealed fact.

## Verification

Focused tests cover schema names/required fields, canonical outputs, exact ID
and revision, create cap, objective-only/cap-only edit, empty fillers,
conditional rejection, blocker text, resume capacity, and unchanged state on
failure. Real PTY Goal completion and recovery fixtures use the new contract.
Only local check, focused tests, format, Clippy, and whitespace checks are
required for this fast checkpoint.
