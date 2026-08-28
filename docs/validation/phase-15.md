# Phase 15 validation

Status: `complete`

Phase 15 implements `docs/design/goal-tool-contract.md` against fixed upstream
commit `47f943859bef60e4160492346772ded9b24f765a`.

## Delivered evidence

- Goal schemas now expose official `goal_id`/`revision`/`action`, optional
  create/edit `max_goal_rounds`, and conditional `blocked_reason` fields.
- Parsing enforces a closed argument set, exact nonblank Goal ID, positive safe
  revision, bounded positive cap, action-specific fields, and trimmed blocker
  text before preparing any mutation.
- Goal results now use official `{goal, activation}` placement; blockers persist
  `{code:"model-reported",message}` and `get_goal` without state returns only
  `{goal:null}`.
- The event fold permits cap changes only on edit and keeps definition fields
  unchanged for pause/resume/complete/block. A cap-only edit can re-enable
  resume after the old budget is exhausted.
- Unit tests cover exact schemas, canonical output, wrong-ID rejection without
  revision change, custom create cap, cap-only edit with empty fillers,
  persisted blocker text, fold replay, and exhausted-budget rearm.
- The three real Goal PTY journeys use a bounded dynamic loopback fixture that
  extracts the actual generated ID/revision from the recorded Goal prompt and
  calls the strict official update contract.

## Local verification

```console
cargo check --all-targets
cargo test goal --lib -- --nocapture
cargo test --test interactive_cli goal_ -- --nocapture
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
git diff --check
```

Results: all-target compilation passed; 14 focused Goal/contract tests passed;
3 real Goal PTY tests passed; format, Clippy with warnings denied, and
whitespace checks passed. The unrelated full suite, remote CI, and
cross-platform reruns were intentionally omitted under the user-selected fast
local gate.

## Remaining gaps

Compatibility remains `partial` because Rust defaults to 32 rather than 256
rounds, limits caps to positive `u32`, emits no autonomous terminal wrap-up
context after complete/block, and has no Goal attachments, subagent graph, or
background worker.
