# Sequential bounded user-question batches

## Scope

Phase 18 extends the Phase 17 `ask_user_question` production path from exactly
one question to one through three questions in one tool call. Each question
remains a closed single-select form with two through four labelled options.
The terminal asks them in request order and publishes one compact answer array
only after every question has an answer.

Custom text, option-free questions, multi-select, plan-review presentation,
background interaction, and subagent routing remain out of scope. This phase
changes no file, Shell, plugin, Goal, approval, or Session authority.

## Official behavior and chosen difference

The fixed evidence remains
`deepseek-ai/deepseek-harness@47f943859bef60e4160492346772ded9b24f765a`:

- `packages/interaction/tool-ask-user/src/index.ts` maps every input question
  into one UI request and every returned answer into the ordered compact JSON
  array;
- `packages/interaction/tool-ask-user/tests/tool-ask-user.spec.ts` fixes
  multiple answer projection, option-label identity, and one ordinary tool
  result;
- `packages/interaction/user-questions/src/{index,types}.ts` and its tests make
  the whole request one awaited interaction and publish no partial answer into
  model context.

Latest `master` at
`cd5ef8148158c3a752a658978873241fdf8e2bbc` keeps that model contract. Rust caps
one batch at three questions, while the official schema has no equivalent fixed
count limit. The cap keeps terminal work and retained strings small and is
advertised directly in the Rust schema.

## Data and event flow

The domain request becomes an ordered non-empty vector. IDs must be unique
inside the batch. The terminal envelope stays capacity one: it holds the whole
batch and one one-shot response containing exactly one selected index per
question. The broker verifies response count and every index, then maps indices
back to the exact displayed labels.

The UI owns `current_question` plus previously selected indices. A choice for a
non-final question advances to a fresh input-fenced frame. A final choice sends
the complete response. Escape cancels the whole pending batch; no partial answer
is returned. Ctrl+C cancels the whole turn through the existing token.

Session order is unchanged:

```text
assistant tool call -> tool/call -> question 1..N in terminal -> one tool/result
```

The resulting text is the official compact shape:

```json
{"answers":[{"id":"mode","selected":["Fast"]},{"id":"tests","selected":["Focused"]}]}
```

## Failure, recovery, security, and bounds

Zero or more than three questions, duplicate IDs, any invalid question, a
short/long response vector, or an out-of-range selection fails closed. A
cancelled batch publishes only `ASK_CANCELLED`; it never exposes its partial
local selections. A crash leaves the existing unresolved tool intent, and
resume never replays the interaction.

Every question retains the existing byte/count/control-character checks.
Rendering is sequential, so only one bounded question is presented at a time.
Input is flushed before every question frame, preventing one stale or pasted
record from answering later questions. The wait remains under the existing
30-minute turn limit and owns no background task or external side effect.

## Verification and status

Unit tests cover schema count, duplicate IDs, ordered index-to-label mapping,
short responses, sequential UI advance, whole-batch cancel, and compact JSON.
A real fake-Provider PTY journey answers two questions and asserts both exact
answers in the next request. Existing single-question select and cancel journeys
remain green.

The compatibility row stays `partial` because custom and multi-select answers
remain absent and Rust keeps a three-question ceiling. The Phase 18 gate is
local all-target compilation, focused unit/PTY tests, format, Clippy with
warnings denied, and whitespace checks; no remote or cross-platform rerun.
