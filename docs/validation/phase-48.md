# Phase 48 validation — manual Session title rename

Date: 2026-08-29

## Delivered behavior

- Idle `/rename <TITLE>` works in both enhanced and linear terminals.
- Input uses the existing terminal-safe 80-byte normalizer. Whitespace is
  collapsed, controls/invisible direction markers are removed, and an empty
  visible result is rejected without changing the title.
- A successful rename appends one log-only `session/title` event with empty
  `messageSeqs` and `source.kind=user`; durable replay and every existing title
  consumer therefore see the new value without a schema change.
- Rename cancels and joins a scheduled/running automatic-title task before the
  user event is appended. A late provider result cannot overwrite it, and a
  later prompt does not schedule another automatic title.
- Exact `/rename` shows the current title and usage. During an active turn the
  enhanced terminal reports that rename must wait instead of queueing a hidden
  metadata mutation.
- The command starts no model/tool request and requires no approval.

## Evidence

- Fixed-upstream fixture:
  `tests/fixtures/tools/upstream_phase48_manual_session_title.json`.
- Agent test covers normalization, cancellation observation, user-source event
  shape, late-result suppression, a later eligible prompt and empty rejection.
- Parser/palette tests cover closed command recognition, prefix safety and the
  eleven-command bounded display window.
- Real local PTY tests cover a durable enhanced rename and a zero-escape linear
  rename; both prove that no model request was sent.
- Final local gates passed:

```console
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets -q
cargo clippy --all-targets -- -D warnings
git diff --check
```

The clean all-target run passed 1,391 tests with no ignored tests. An earlier
run had one unrelated existing plugin-cancellation exit timeout; its immediate
isolated rerun passed. A later run also saw one transient PTY admission failure;
that test passed immediately in isolation. Both were followed by the clean full
run above.

No remote CI, live DeepSeek request or extra stress matrix was run.

## Known limits

- Official explicit `refresh()` is not implemented, so a manual title remains
  pinned for the Session.
- Rust changes only the current idle Agent; it does not expose a cross-process
  live-session rename service.
- The operation may wait up to one second while reclaiming a non-cooperative
  automatic-title task.
