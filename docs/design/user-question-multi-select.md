# Bounded multi-select user questions

## Scope

Phase 20 extends `ask_user_question` with the official `multi_select: true`
shape. A multi-select question retains zero through four toggled option indices
while the human edits it, then submits one or more selected labels, optional
custom text, or both. The existing one-to-three question batch, zero-or-two-to-
four option bound, and 4,096-byte custom cap remain unchanged.

Skip, backward/forward page navigation, plan-review presentation, product
subagents, background interaction, and the latest Cordis answerer waterfall
remain out of scope. The feature changes no file, Shell, plugin, Goal,
approval, or Session authority.

## Official behavior and terminal mapping

The fixed evidence is
`deepseek-ai/deepseek-harness@47f943859bef60e4160492346772ded9b24f765a`:

- `packages/interaction/tool-ask-user/src/index.ts` accepts `multi_select` and
  projects all returned labels plus optional custom text unchanged;
- `packages/interaction/tool-ask-user/tests/tool-ask-user.spec.ts` fixes a
  multi-select answer containing multiple labels and custom text in one compact
  result;
- `packages/interaction/user-questions/src/types.ts` defines `multiSelect` and
  permits selected labels alongside custom text;
- `packages/client/ui-user-questions/src/client/QuestionComposer.tsx` toggles
  labels, preserves toggle order, requires at least one label or nonblank custom
  text, and keeps selected labels when multi-select custom text changes;
- `packages/client/ui-user-questions/tests/user-questions-composer.client.spec.tsx`
  fixes toggle-off/on, multi-label plus custom, and one final batch submit.

Latest `master` at
`cd5ef8148158c3a752a658978873241fdf8e2bbc` keeps that model contract.

The Rust terminal maps the browser controls to bounded keys:

- enhanced mode: digits 1–4 toggle choices, Enter submits, and the extra custom
  digit opens the Composer; the Dock shows the currently selected numbers;
- linear mode: one number plus Enter toggles a choice, and an empty line submits;
- Escape cancels the whole batch in either mode.

This key mapping is an intentional terminal presentation difference. It does
not change the answer object.

## Types, ownership, and result

`UserQuestionItem` owns the boolean `multi_select`. A response owns an ordered
vector of unique option indices plus optional custom text. The broker validates
the response against the exact original question:

- single-select without custom has exactly one valid index;
- single-select custom has no selected index;
- multi-select has at least one valid unique index, nonblank custom text, or
  both;
- all indices are in range, and custom text is trimmed and within its existing
  byte/control limits.

Indices become exact displayed labels in response order. For example:

```json
{"answers":[{"id":"targets","selected":["tests","docs"],"custom":"release notes"}]}
```

The UI owns only the current question's bounded selected indices. Completed
response items remain local until the final question settles the capacity-one
broker. No partial choice becomes model or Session context.

## State, cancellation, recovery, and safety

A multi-select question starts with no choices. Toggling an unselected index
appends it; toggling a selected index removes it. Re-selecting therefore moves
that label to the end, matching the upstream UI's array behavior. Enter with no
selection and no custom answer retries without settling.

Enhanced mode borrows the existing Composer overlay for the whole multi-select
interaction, keeping any next-turn draft hidden and protected. Opening custom
text reuses the same overlay. Selected-only submit, custom submit, Escape, EOF,
turn cancellation, failure, and panic cleanup all restore the original draft.

The broker rejects duplicate/out-of-range indices and illegal single-select
combinations even if a UI is buggy. A crash may leave the ordinary tool intent
unresolved, and recovery never replays it. Human waiting remains bounded by the
existing turn deadline and creates no external side effect.

## Verification and status

Focused tests cover schema parsing, toggle order/removal, empty-submit retry,
single-versus-multi response validation, selected-plus-custom projection,
batch atomicity, draft restoration, cancellation, and exact compact JSON. Real
fake-Provider PTY journeys cover selected-only and selected-plus-custom multi-
select answers, including toggle-off/on order.

The compatibility row remains `partial`: skip, plan-review presentation,
officially unbounded question counts/text, product subagent routing, and the
latest answerer waterfall remain absent. The Phase 20 gate is local focused
tests, all-target compilation, format, Clippy with warnings denied, and a
whitespace check; remote and cross-platform reruns are intentionally omitted.
