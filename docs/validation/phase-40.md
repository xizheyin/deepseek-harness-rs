# Phase 40 local validation — 2026-08-29

## Result

Phase 40 is complete under the requested local-only, necessary-check gate. The
real CLI now advertises `session_trace` and `session_event_trace`, completing
the fixed upstream session-query package's five model-facing tool names.
Session trace reports validated ancestor and descendant relationships; event
trace reports positional replacement chains separately from direct source and
derived-event links.

Both tools preserve the stricter Rust boundary: only normally closed,
current-version, strict-replay-valid journals from the exact retained workspace
are visible. Caller, busy/live, other-workspace, corrupt, unsupported,
oversized and absent targets reveal no content. The tools are bounded,
read-only, Agent-serialized and require no approval.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the trace algorithms,
  relation types, child ordering, cycle behavior, workspace pruning and exact
  output fields recorded in `docs/upstream.md`. Fetched latest master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` has no tracing-algorithm diff.
- `tests/fixtures/tools/upstream_phase40_session_tracing.json` records the
  exact source paths, official/Rust required fields, headings, relation order,
  surfaces and persisted-only differences.
- Core tests cover complete and unresolved ancestry, creation-time/id child
  ordering, nested descendants, target-connected cycles, immediate/chained
  replacements, replaced ranges, source/derived links, all three surfaces,
  missing events, caller/workspace/busy/corrupt exclusion, cancellation and
  deadlines.
- Tool tests cover both closed schemas, canonical ids, safe sequence bounds,
  lineage/event rendering, incomplete-corpus notice and result metadata.
- A real two-process binary journey creates durable history, then runs
  `session_search` → `session_event_search` → `session_event_read` →
  `session_event_trace` → `session_trace`, verifies all five reconstructed
  Provider requests and call/result pairs, and proves no approval event was
  emitted.

## Local commands

```console
cargo test 'session::search::tests' --lib -- --nocapture
cargo test 'tools::session_search::tests' --lib -- --nocapture
cargo test 'tools::registry::tests' --lib -- --nocapture
cargo test 'cli::assembly::tests::system_prompt_matches_the_shipped_extension_boundary' --lib -- --exact --nocapture
cargo test --test cli_smoke real_script_searches_a_closed_same_workspace_session_and_continues -- --exact --nocapture
cargo fmt --all -- --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets --quiet -- --test-threads=1
git diff --check
```

All commands passed on macOS arm64 with Rust 1.85.0 and locked dependencies.
The serial all-target run passed the 930-test core library plus every
integration, binary and example target. Tests use fake providers, private
temporary workspaces and loopback HTTP. No real DeepSeek request, API key,
charge, remote CI, public-network product test, extra operating-system matrix
or stress run was used. Network access was used only to inspect the separate
upstream research clone.

## Review

The implementation was reviewed locally for canonical target parsing,
workspace/file/lock authorization, strict full-log replay, replacement versus
source-edge separation, ancestor cycles, deterministic child order, bounded
corpus behavior, sanitized failures, cancellation cleanup, call-before-result
order, output limits and approval absence. Focused tests, the real CLI journey,
compiler, one serial all-target run and Clippy provide automated evidence. No
subagent was used because this continuation was not authorized for delegation.

## Known limitations

- All five Session query tools inspect only normally closed prior Sessions;
  they do not read the caller or any live/busy Session.
- Both trace tools require an explicit canonical id. Rust renders no Session
  title and only persisted availability.
- An unobservable parent is shown as an opaque boundary. Descendants outside
  the bounded, validated 64 MiB corpus are omitted with an explicit notice.
- Filters, cursors, current-step cutoff, live-preferred SQLite and persistent
  derived indexes remain absent.
- A fixed-source fixture and real Rust production path are tested, but no
  generated TypeScript producer proves broad cross-language compatibility. The
  compatibility rows therefore remain `partial`, not `compatible`.
