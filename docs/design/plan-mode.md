# Durable terminal Plan Mode

## Scope and upstream basis

Phase 23 implements the terminal-relevant Plan Mode loop from fixed upstream
commit `47f943859bef60e4160492346772ded9b24f765a`:

- `packages/plan/plan-mode/src/{index,types}.ts` and its tests define the
  `plan/mode` fold, system-prompt section, `/plan` command, stable
  `exit_plan_mode` tool, reviewed exit, and next-step boundary;
- `packages/interaction/user-questions/src/{index,types}.ts` validates the
  `plan-review` intent;
- `packages/client/ui-user-questions/src/client/{PlanReviewPanel,contract/slots}.tsx`
  defines when a single binary review may receive specialized presentation.

Latest inspected master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` keeps the same core contract and
adds command-settlement and image support. Rust does not add images, Web UI,
subagents, arbitrary named modes, or a Cordis service container.

## State, inputs, and event order

`plan/mode { active }` is a versionless, log-only whole-value Session event;
the last value wins and absence means inactive. One `PlanModeRuntime`, restored
from the Session projection, is shared by CLI command handling, Agent request
assembly, and the exit tool.

Idle `/plan` prepares and commits `plan/mode { active: true }`; `/plan off`
commits false. `/plan <message>` first commits entry and then submits the
trimmed suffix as the ordinary user request. Repeating the current state is an
idempotent local notice. Active-turn mode mutation remains fail-closed in this
terminal slice because the current TUI queue is next-turn, not upstream
same-turn steering; it must not silently apply a different ordering.

The `exit_plan_mode` schema is always declared. Its call must:

1. verify committed Plan Mode is active;
2. validate one bounded markdown `plan` beginning with `# `;
3. ask one internal `plan-review` question carrying the exact plan, Approve,
   and Keep planning;
4. treat only one exact Approve selection with no custom text as consent;
5. durably record the tool result, then arm a silent pending exit;
6. at the next accepted step, append `plan/mode { active: false }` before
   request assembly and emit a changed request header with no plan guidance.

Other tools in the same assistant batch still run under the old Plan Mode.
Keep planning or feedback produces a failed correlated tool result and leaves
the mode active. Dismissing the review produces the official user-takeover
failure. Turn cancellation produces the ordinary abort failure and never arms
an exit.

## Prompt, ownership, and recovery

The Agent owns the effective system prompt. It combines the existing base
prompt with one fixed bounded Plan policy only while the committed runtime is
active. The request header records the exact effective prompt, so every model
request remains reconstructible and resume restores the mode solely from
Session facts. Tool schemas never change across transitions.

The Session owns durable state; `PlanModeRuntime` owns only a pending in-process
boundary mutation. The tool cannot append Session events directly. The Agent
commits idle command mutations and next-step exit mutations, then installs the
matching runtime state. A failed append leaves the mutation retryable and
cannot create a phantom mode switch.

## Safety, limits, and side effects

Plan Mode is soft guidance, exactly as upstream documents. It does not grant or
deny filesystem, Shell, or plugin authority and does not bypass approval. The
review performs no external side effect. The submitted plan is capped at 16
KiB UTF-8, rejects controls other than newline/tab, and is shown as untrusted
model text. Existing question, turn, tool-result, and Session budgets still
apply.

## Verification and intentional differences

Deterministic tests cover event codec/fold, resume, idempotent commands,
inactive/invalid plan calls, approve/keep-planning/feedback/dismiss/cancel,
tool-result-before-mode-change order, stable schemas, changed system headers,
and no approval bypass. Real enhanced and linear PTY journeys cover entry,
review, approval, continuation, and manual exit.

The Rust terminal uses its existing question and markdown-safe presentation
rather than the Web decision card, and does not support image-bearing `/plan`
commands or active-turn steering. These are visible, tested differences; the
model-visible exit-tool schema/result and durable mode/request ordering remain
the compatibility target.
