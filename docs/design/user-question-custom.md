# Bounded custom user-question answers

## Scope

Phase 19 extends the production `ask_user_question` path with two official
single-select forms:

- an option-free question that directly collects free text;
- an option question with two through four choices plus an explicit custom
  answer entry.

One call still contains one through three sequential questions. Multi-select,
skip, plan-review presentation, background interaction, product subagents, and
the latest Cordis answerer waterfall remain out of scope. This phase grants no
file, Shell, plugin, Goal, approval, or Session authority.

## Official behavior and chosen bounds

The fixed evidence is
`deepseek-ai/deepseek-harness@47f943859bef60e4160492346772ded9b24f765a`:

- `packages/interaction/tool-ask-user/src/index.ts` makes `options` optional
  and returns `selected` plus optional `custom` in one compact result;
- `packages/interaction/user-questions/src/types.ts` defines option-free and
  custom answers without creating a separate model message;
- `packages/client/ui-user-questions/src/client/QuestionComposer.tsx` trims
  custom text at submit, treats blank text as unanswered, and makes custom text
  replace the selected label for a single-select question;
- `packages/client/ui-user-questions/tests/user-questions-composer.client.spec.tsx`
  fixes the corresponding browser-visible behavior.

Latest `master` at
`cd5ef8148158c3a752a658978873241fdf8e2bbc` keeps this tool contract. Rust keeps
its existing one-to-three question cap and two-to-four option cap. It adds a
4,096-byte custom-answer cap so a terminal interaction cannot retain unbounded
text. These limits are visible in the schema or product documentation.

## Types, ownership, and result

Each response item is exactly one of:

- `Selected(index)`, valid only when the question has that displayed option;
- `Custom(text)`, valid after trimming only when the result is nonblank and at
  most 4,096 UTF-8 bytes.

The capacity-one broker validates all response items against the original
questions before converting indices to exact labels. The model-visible shapes
are:

```json
{"answers":[{"id":"mode","selected":["Focused"]}]}
```

```json
{"answers":[{"id":"detail","selected":[],"custom":"只跑必要检查"}]}
```

The batch is still all-or-nothing. Earlier local answers do not become model
context before the final question settles.

## Terminal flow and draft preservation

An option question first displays numbered choices plus a numbered custom
entry. Selecting that entry, or reaching an option-free question, opens the
custom editor after a fresh input fence.

Enhanced mode temporarily lends the existing grapheme-safe Composer to the
question. `InputMemory` swaps the user's current next-turn draft and history
navigation into a private overlay, starts an empty question draft, and restores
the original state when custom input submits, cancels, errors, or the turn
ends. Enter submits, Ctrl+J inserts a newline, ordinary Unicode navigation and
editing keys continue to work, and Escape cancels the entire question batch.

Linear mode reads one bounded canonical record and submits it at Enter. It does
not promise multiline editing. Both modes trim only at submission and retry the
same question for blank or oversized input.

Session order remains:

```text
assistant tool call -> tool/call -> terminal questions/custom editor -> one tool/result
```

No editor keystroke, local draft, or partial batch answer is written to the
Session.

## Failure, cancellation, recovery, and safety

Missing `options` and an empty option array both mean free text. One option or
more than four options is invalid. Duplicate labels, malformed fields,
multi-select requests, invalid response shapes, blank custom text, and custom
text above the cap fail closed.

Escape returns the existing `ASK_CANCELLED` result. Ctrl+C, EOF, terminal
failure, Provider failure, timeout, and panic follow the existing turn-owned
cancellation path and restore any borrowed Composer draft before the next
prompt. A crash may leave an unresolved tool intent; recovery never replays it.
The human wait remains bounded by the existing 30-minute turn deadline.

## Verification and status

Focused unit tests cover optional options, response validation, exact JSON,
custom override semantics, empty/oversized retry, Unicode editing, and Composer
draft restoration. Real fake-Provider PTY journeys cover option-free Unicode
text, choice-to-custom continuation, and a mixed batch. Existing choice and
cancel journeys remain green.

The compatibility row remains `partial`: Rust still lacks multi-select, skip,
plan-review presentation, unbounded official batch sizes, product subagent
routing, and the latest answerer waterfall. The Phase 19 gate is local focused
tests, all-target compilation, format, Clippy with warnings denied, and a
whitespace check; remote and cross-platform reruns are intentionally omitted.
