# Phase 42 local validation — 2026-08-29

## Result

Phase 42 is complete under the requested local-only, necessary-check gate. The
real CLI now exposes approval-gated background Bash through
`run_in_background`, plus `job_list`, bounded `job_output` waits and
`job_kill`. Background work has one process-local owner, finite records,
bounded output and time, and is cancelled and joined during tool-runtime
shutdown.

The original safety order remains: the Shell call intent and approval decision
are recorded before ownership handoff; a rejected or pre-handoff-cancelled call
never creates a job. A successful handoff returns a correlated `bash-N` result.
Later observation or cancellation is another ordinary correlated tool call.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the registry lifecycle,
  `run_in_background`, three controller schemas, wait/kill semantics and
  teardown ownership recorded in `docs/upstream.md`. Fetched latest master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` changes only prompt-section order
  in the relevant production files.
- `tests/fixtures/tools/upstream_phase42_background_jobs.json` fixes the
  official names, acknowledgement, states and lifecycle rules plus Rust's
  finite boundary.
- Job unit tests cover closed schemas, strict ids/reasons, wait bounds,
  suffix-preserving output limits, model-facing label limits, wait timeout and
  the rule that cancelling a wait does not cancel its job.
- Shell/Agent tests cover the foreground/background schema, approval preview
  and rejection-before-start, background completion with a non-consuming final
  read, explicit kill, process-group cleanup, registry-shutdown cleanup and all
  pre-existing foreground cases.
- One real linear terminal journey approves a background Bash command, observes
  the `bash-1` ack, collects its final output through `job_output` and verifies
  both correlated Provider requests. A second real enhanced-terminal journey
  proves explicit exact-Shell reuse suppresses only the second identical
  background prompt; the detached shape is distinct from foreground authority.

## Local commands

```console
cargo test 'tools::jobs::tests' --lib -- --nocapture
cargo test 'tools::shell::tests' --lib -- --nocapture
cargo test --test shell_tools -- --nocapture
cargo test background_shell_is_approved_started_and_collected_through_real_terminal_tools --test interactive_cli -- --exact --nocapture
cargo test exact_background_shell_choice_reuses_only_the_same_detached_shape --test interactive_cli -- --exact --nocapture
cargo fmt --all -- --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets --quiet -- --test-threads=1
git diff --check
```

All commands passed on macOS arm64 with Rust 1.85.0 and locked dependencies.
The final serial run passed the 935-test core library plus every integration,
binary and example target, including 123 real interactive-terminal tests and
20 Shell integration tests. Tests use fake providers, private temporary
workspaces and loopback HTTP. No real DeepSeek request, API key, charge, remote
CI, public-network product test, extra operating-system matrix or stress run
was used. Network access was used only to inspect the separate upstream clone.

## Review

The implementation was reviewed locally for intent-before-side-effect order,
approval and exact-grant separation, pre/post-handoff cancellation, finite
admission, status monotonicity, wait races, output/result bounds, process-group
kill, retained monitor ownership, shutdown joins, no-replay recovery and
foreground regression. Focused tests, two real terminal journeys, compiler,
one serial all-target run and Clippy provide the automated evidence. No
subagent was used because this continuation was not authorized for delegation.

## Known limitations

- Only Bash commands started by the current CLI process are jobs. There are no
  non-Shell producers, shared/multi-Agent registry or persisted job handles.
- Background commands keep the Rust Shell maximum of 295 seconds. Official
  background Bash applies no command timeout after handoff.
- Running output is not streamed incrementally. `job_output` returns no new
  output while live and an idempotent bounded final output after settlement.
- Completion does not inject a notice or wake an idle Agent. The shipped system
  prompt requires explicit collection with `job_output` and cleanup of
  irrelevant work with `job_kill`.
- Process exit or resume discards the registry. Historical start/result facts
  remain evidence, but an old id is unknown and is never restarted.
- A fixed-source fixture and real Rust production paths are tested, but no
  generated TypeScript producer proves broad cross-language compatibility; the
  compatibility row therefore remains `partial`, not `compatible`.
