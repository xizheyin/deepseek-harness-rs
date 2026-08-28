# Phase 27 local validation — 2026-08-29

## Result

Phase 27 is complete under the requested local-only, necessary-check gate.
Both enhanced and zero-escape linear terminals accept exact idle `/compact`.
The command summarizes one balanced older prefix below automatic pressure,
does not open or consume an Agent turn, and reports the selected history-item
and estimated-token counts.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` and latest inspected master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` were checked for exact parsing,
  no-history, idle-only admission, manual range selection, null-turn lifecycle,
  cancellation, failure, and flush behavior.
- `AgentLoop::compact_now` is the sole Session/Provider owner. It checks an idle
  materialized Session, returns before generating an event or request when no
  balanced prefix exists, and reuses the existing bounded compaction request.
- A started command records a full Rust pre-request dispatch with trigger
  `manual`, then keeps `sourceCommandId` and `turn: null` across start, summary,
  checkpoint, and end. The next turn number is unchanged.
- Success installs only a strictly smaller checkpoint. Provider failure,
  malformed or non-shrinking output, timeout, and Ctrl+C close the bracket with
  an error and leave the previous visible surface unchanged.
- Success also reconciles workspace instructions against the replaced surface;
  a shadowed instruction baseline is rearmed before the next model request.
- Exact arguments are rejected locally. Enhanced active input reports busy and
  cannot enter the next-turn FIFO. Linear mode keeps its established rule of
  ignoring ordinary input while a turn is active.
- A real enhanced PTY journey runs the development CLI against a loopback fake
  DeepSeek server, observes the success notice, verifies exactly one additional
  summary request, and inspects the durable JSONL four-event transaction.
- Upstream's generic `command/run` and `command/done` events remain an explicit
  difference. Rust does not add a one-command-only generic event schema.

## Local commands

```console
cargo test --lib manual_compaction -- --nocapture
cargo test --lib failed_manual_summary_closes_the_bracket_and_preserves_the_surface -- --nocapture
cargo test --test interactive_cli manual_compact_runs_one_idle_request_and_persists_a_null_turn_transaction -- --nocapture
cargo test --all-targets -- --test-threads=1
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Tests ran locally on macOS arm64 with Rust 1.85.0. They use in-memory fixtures,
temporary directories, local subprocesses, PTYs, a loopback fake Provider, and
fake credentials. No real model request, API charge, remote CI, or additional
platform matrix was used.

## Known limitations

- Manual compaction sends one summary request per command and does not loop.
- A summary can omit or distort facts; the command provides bounded context
  reduction, not lossless compression.
- Generic durable `command/run` and `command/done` facts are not implemented.
- Linear mode does not render an active-turn busy message because it does not
  accept any ordinary input while a turn is running.
- There is no generated upstream-vs-Rust oracle, so compatibility remains
  `partial` rather than `compatible`.
