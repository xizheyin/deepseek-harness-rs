# Phase 24 local validation — 2026-08-29

## Result

Phase 24 is complete under the user-requested local-only, necessary-check
gate. The production Agent can now persist a bounded model-maintained Todo
list, expose it through both terminal modes, restore it from Session state,
and refuse to commit a prepared snapshot after cancellation.

## Evidence

- The fixed upstream Todo tool, Session event/invariant, code preset, and Web
  presentation sources/tests were inspected; latest master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains the observable contract.
- The registry test proves the closed whole-list schema, trimming, duplicate,
  status, field, count and byte bounds, empty clear, single-active rule, and
  exact canonical result.
- The Session test proves last-write-wins projection, turn-end retention,
  next-turn standing-state clearing, and durable invariant rejection.
- The Agent test proves cancellation after preparation cannot append
  `todo/write` or leave an open turn.
- The terminal unit test proves resume restoration, whole-list replacement,
  compact counts, full list rendering, and next-turn clearing.
- One enhanced and one zero-ESC linear real-PTY journey prove the production
  model tool path, exact Provider-visible result, standing summary, complete
  list markers, and durable `tool/call` → `todo/write` → `tool/result` order.

## Local commands

```console
cargo test --lib todo_write_schema_parser_and_result_are_closed_bounded_and_canonical
cargo test --lib standing_todos_restore_replace_and_clear_at_the_next_turn
cargo test --test session_core todo_snapshots_are_last_write_wins_and_clear_from_standing_state_next_turn
cargo test --test agent_loop cancellation_between_todo_preparation_and_commit_never_writes_the_snapshot
cargo test --test interactive_cli todo_write_updates_the_enhanced_standing_plan_in_durable_order
cargo test --test interactive_cli todo_write_prints_the_complete_bounded_list_in_zero_escape_linear_mode
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

The user explicitly asked to reduce checks and validate only on this machine.
The full repository test suite and remote/cross-platform CI were therefore not
repeated for this checkpoint.
