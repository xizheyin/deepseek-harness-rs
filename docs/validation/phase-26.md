# Phase 26 local validation — 2026-08-29

## Result

Phase 26 is complete under the requested local-only, necessary-check gate. A
successful built-in `read` or definitely committed built-in `apply_patch` now
causes a bounded nested workspace-instruction reconciliation after the current
step closes. The resulting message, when any, enters the next Provider request
as an append-only Session fact.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` and latest inspected master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` were checked for success-only
  file touches, nested execution ancestry, `step/end` deferral, pre-step joins,
  nested discovery, reconciliation, cancellation, and compaction rearming.
- `ToolExecutionResult` carries a crate-private touch that public constructors
  and subprocess plugins cannot set. The built-in read registry adds it only
  after a successful confined read; patch preparation adds it only to
  `Committed` outcomes. Rejected and not-committed results carry none.
- The Agent retains a touch only after the preferred correlated `tool/result`
  commits, deduplicates and caps pending touches at 256, then refreshes only
  after `step/end`. A later sibling cancellation preserves the pending touch
  for the next non-empty turn.
- Workspace tests cover relative/absolute confinement, shallow-to-deep nested
  candidates, same-scope duplicate collapse, updates, removals, unavailable
  files, cancellation, the 256-directory runtime hint, and rearming. A real
  pressure-compaction Agent test proves the same process reinstalls the
  instruction before continuing the request.
- Built-in registry and patch tests prove success/failure/rejection/commit
  provenance. Agent tests prove `tool/result`/`step/end`/next-step ordering and
  sibling-cancellation retry.
- A real enhanced PTY journey runs the installed development CLI against a
  loopback fake DeepSeek server: request one calls `read` for
  `pkg/deep/file.txt`; request two contains the correlated tool result and the
  new `pkg/AGENTS.md` text; durable JSONL keeps the required event order.
- The full local suite exposed two old Phase 24 schema-order assertions that
  omitted the already-shipped `todo_write` tool and one Goal assertion that
  incorrectly required Goal schemas to be last. Only those stale test
  expectations were corrected; no related production behavior changed.

## Local commands

```console
cargo test --lib workspace_instructions -- --nocapture
cargo test --lib successful_builtin_read_refreshes_nested_instructions_after_step_end -- --nocapture
cargo test --lib committed_touch_survives_a_later_sibling_cancellation_until_the_next_turn -- --nocapture
cargo test --lib pressure_summary_compacts_once_and_continues_the_same_input -- --nocapture
cargo test --test interactive_cli successful_read_injects_nested_workspace_instructions_into_the_next_real_request -- --nocapture
cargo test --all-targets -- --test-threads=1
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Tests ran locally on macOS arm64 with Rust 1.85.0. They use temporary
directories, local subprocesses, PTYs, a loopback fake Provider, and fake
credentials. No real model request, API charge, remote CI, or additional
platform matrix was used.

## Known limitations

- Rust still treats the exact opened workspace as the root and refuses
  instruction-file symlinks; these are deliberate privacy differences.
- Candidate names and budgets remain fixed.
- A nested scope whose only visible fact was compacted can be rearmed during
  the same process through the bounded directory hint. After a restart it must
  become visible through retained Session context or a new trusted file touch.
- Rust has no nested composite-tool execution, so it does not implement
  upstream execution-ancestry bubbling.
- A generated cross-language dynamic oracle is still absent; the broad
  compatibility row therefore remains `partial`.
