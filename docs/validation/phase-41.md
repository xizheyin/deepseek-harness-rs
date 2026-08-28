# Phase 41 local validation — 2026-08-29

## Result

Phase 41 is complete under the requested local-only, necessary-check gate.
`session_search` and `session_event_search` now expose the fixed upstream's
model-facing filter fields. The model can narrow normally closed prior history
by Session id, creation time, authorized parent/root status, persisted
availability, event sequence/time, event type and current/shadowed/log-only
surface before relevance ranking.

The existing strict boundary is unchanged. Filters do not reveal the caller,
busy/live, other-workspace, malformed or oversized sources; guessed parent ids
must themselves be validated in the bounded same-workspace corpus. Both tools
remain read-only, Agent-serialized, cancellable and approval-free.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the complete field names,
  AND/OR rules, inclusive ranges, exact ISO timestamp semantics, parent
  authorization and filter-before-rank behavior recorded in
  `docs/upstream.md`. Fetched latest master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` preserves the filter input file.
- `tests/fixtures/tools/upstream_phase41_session_search_filters.json` records
  all official fields, required arguments, vocabularies, rank order and Rust
  resource caps.
- Parser tests cover both closed schemas, complete combined filters, canonical
  ids, empty/oversized arrays, invalid enums/types, safe integer and reversed
  ranges, timezone requirement, leap-date validation, offsets and exact
  sub-millisecond bounds including a valid interval with no integer-millisecond
  event.
- Core tests cover Session id/creation/parent/root/availability predicates,
  authorization of requested parents, event sequence/time/type/surface
  predicates and filtering before relevance ranking. Existing strict-search
  tests continue to cover workspace/caller/busy/corrupt/oversized sources,
  cancellation and deadlines through the same runtime path.
- The real two-process five-tool CLI journey now sends filtered
  `session_search` and `session_event_search` calls, verifies both expanded
  schemas, observes the selected exact event in the following Provider
  requests, continues through read and relationship traces, and emits no
  approval event.

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
The serial all-target run passed the 932-test core library plus every
integration, binary and example target. Tests use fake providers, private
temporary workspaces and loopback HTTP. No real DeepSeek request, API key,
charge, remote CI, public-network product test, extra operating-system matrix
or stress run was used. Network access was used only to inspect the separate
upstream research clone.

## Review

The implementation was reviewed locally for closed schemas, exact timestamp
ordering and integer projection, filter-before-rank order, authorized parent
intersection, root semantics, live-only empty behavior, workspace/file/lock
authorization, memory/result/deadline caps, cancellation reuse, call-before-
result order and approval absence. Focused tests, the real CLI journey,
compiler, one serial all-target run and Clippy provide automated evidence. No
subagent was used because this continuation was not authorized for delegation.

## Known limitations

- Filters operate only over normally closed persisted Sessions. `live` is
  accepted in the official availability vocabulary, but a live-only selection
  is empty in this CLI.
- Session and parent id arrays require canonical local UUIDv4 ids and are
  bounded at 128; event-type arrays are bounded at 64 with 128-byte values.
- Event search still requires an explicit prior Session id. Current-step
  cutoff, titles, cursors, live-preferred SQLite and persistent indexes remain
  absent.
- Search scans strict JSONL directly and keeps the existing 20-result,
  16-MiB-per-journal, 64-MiB-aggregate and five-second limits.
- A fixed-source fixture and real Rust production path are tested, but no
  generated TypeScript producer proves broad cross-language compatibility. The
  compatibility rows therefore remain `partial`, not `compatible`.
