# Per-question skip

## Scope and official behavior

Phase 21 implements the fixed Web QuestionComposer's per-question Skip action.
The evidence is `deepseek-ai/deepseek-harness` commit
`47f943859bef60e4160492346772ded9b24f765a`, specifically
`packages/client/ui-user-questions/src/client/QuestionComposer.tsx` and
`tests/user-questions-composer.client.spec.tsx`. Skip advances one question,
retains earlier drafts, and encodes the skipped item as `{ id, selected: [] }`.
Skipping the final item submits the whole ordered batch once.

The tool and user-question types already allow an empty selected array. Latest
`master` at `cd5ef8148158c3a752a658978873241fdf8e2bbc` keeps the same result
contract.

## Terminal mapping and state

The terminal needs an explicit key that does not collide with free text:

- enhanced selection mode: `s` skips;
- enhanced custom editor: Ctrl+S skips, while printable `s` remains text;
- linear mode: the exact record `s` skips.

This is a presentation difference from the Web button. Prompts and Dock hints
show the mapping. `UserQuestionUiState` converts Skip into a typed response and
uses the same ordered advance/finalize path as an answer.

## Result, failure, cancellation, and safety

The broker accepts `Skipped` for any original question and projects no label or
custom text. Other empty selected-only responses remain invalid, so a buggy UI
cannot silently turn an incomplete answer into a skip.

Skip is not cancellation: it advances or completes successfully. Escape still
cancels the whole batch, Ctrl+C still cancels the turn, and all unresolved
partial answers remain invisible to the model. When custom or multi-select mode
owns the Composer overlay, Skip restores the original next-turn draft before
advancing. It produces no filesystem, process, network, or approval side effect.

The existing one-to-three question, option, text, wait, and queue bounds remain
unchanged. Recovery never replays an unresolved question tool.

## Verification and limitations

Focused tests cover response-shape validation, middle/final skip ordering,
selection/custom/multi UI paths, overlay restoration, whole-batch cancellation,
and exact compact JSON. Real fake-Provider PTY journeys cover a middle custom
skip and a final multi-select skip.

The compatibility row remains `partial` because pager navigation, plan-review
presentation, official unbounded sizes, product subagent routing, and the
answerer waterfall remain absent. Verification is local and focused as requested.
