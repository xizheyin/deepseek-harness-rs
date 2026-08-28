# Phase 44 local validation — 2026-08-29

## Result

Phase 44 is complete under the requested local-only, necessary-check gate.
`job_output` can now inspect an approved background Bash command while it is
still running. Each call returns only stdout/stderr observed since the previous
read; after settlement, unread tail bytes remain available once.

The implementation does not read the process twice or add a second execution
path. The existing process collector is the sole writer and publishes a
bounded in-memory tail to the owning job record. `job_list` remains
non-consuming, a terminal read still suppresses a duplicate completion notice,
and all calls/results use the existing approval and append-only Session path.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the consuming stream
  cursor, independent stdout/stderr offsets, terminal unread delivery,
  non-consuming list behavior and explicit lossy reads. Latest fetched master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains that contract.
- `tests/fixtures/tools/upstream_phase44_incremental_job_output.json` records
  the source paths, observable stream behavior and Rust resource boundary.
- Process tests prove exact one-shot deltas, retained-tail loss, UTF-8-safe
  rendering, final spill publication and real output visibility before exit.
- Job tests prove live/repeated/terminal reads, stdout-before-stderr rendering,
  one-shot fallback diagnostics, wait cancellation and completion-notice
  suppression.
- A real terminal journey approves delayed background Bash, waits for its
  terminal output, then proves a second `job_output` returns
  `(no new output)` with the unchanged terminal status.
- The full local suite exposed an old approval-scope PTY test whose instant
  background commands raced the separate completion-notice contract. Keeping
  those fixture jobs live until teardown makes that test exercise only its
  intended exact-grant boundary; both focused reruns and the full rerun passed.

## Local commands

```console
cargo test --lib tools::process::incremental::tests
cargo test --lib tools::process::api_tests::background_tap_exposes_consuming_output_before_and_after_exit
cargo test --lib tools::jobs::tests
cargo test --test shell_tools local_registry_exposes_background_bash_and_closed_job_schemas
cargo test --test interactive_cli background_shell_is_approved_started_and_collected_through_real_terminal_tools
cargo test --test interactive_cli exact_background_shell_choice_reuses_only_the_same_detached_shape -- --exact
cargo fmt --all -- --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
git diff --check
```

All commands passed on local macOS arm64 with Rust 1.85.0 and locked
dependencies. The green all-target run passed 951 core-library tests, 124 real
interactive-terminal tests, 20 Shell integration tests and every other binary,
example and integration target.

No real DeepSeek request, API key, charge, public-network product test, remote
CI, extra operating-system matrix or stress run was used. Network access was
used only for the already recorded upstream fetch and source inspection.

## Review

The local review covered cursor monotonicity, live/terminal races, independent
stream ordering, retained-window loss, finalized spill paths, output encoding
bounds, fallback failures, wait cancellation, completion suppression, process
cleanup and foreground regression. No subagent was used because delegation was
not authorized for this continuation.

## Known limitations

- Only Bash jobs owned by the current CLI process publish incremental output.
- Job output and cursors are not restored after process exit; old ids remain
  unknown and are never replayed.
- Each job retains 64,000 bytes per stream in memory. A caller that falls
  behind receives the retained tail and an explicit loss notice.
- Spill paths are revealed only after final flush, so a lossy live read can
  temporarily say that missing output is unavailable.
- There is no terminal input, PTY session, final-output-only producer,
  multi-Agent routing or configurable cursor window.
- No generated TypeScript producer proves broad cross-language compatibility,
  so the compatibility row remains `partial`, not `compatible`.
