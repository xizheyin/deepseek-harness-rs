# Phase 39 local validation — 2026-08-29

## Result

Phase 39 is complete under the requested local-only, necessary-check gate. The
real CLI now advertises `session_event_search` and `session_event_read` beside
`session_search`. A model can find a normally closed Session from the exact
workspace, search semantic events inside it, then read one complete validated
event with optional bounded neighbor summaries. All returned history is marked
untrusted and neither tool requests approval or mutates the old Session.

Authorization deliberately remains stricter than official dsh's live-preferred
corpus: the caller, active/lock-busy Sessions, other workspaces, malformed or
unsupported journals and absent ids expose no content. Exact output that cannot
fit the 64 KiB tool-result limit fails with
`SESSION_QUERY_OUTPUT_TOO_LARGE` rather than truncating the event.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the two tool names,
  schemas, semantic extraction, rank keys, surface vocabulary, exact fenced
  JSON and default 50-event side window recorded in `docs/upstream.md`.
  Fetched latest master `cd5ef8148158c3a752a658978873241fdf8e2bbc`
  preserves those observable contracts.
- `tests/fixtures/tools/upstream_phase39_session_event_navigation.json` records
  exact source paths, official/Rust required fields, output markers, surfaces,
  window limit and intentional persisted-only differences.
- Core tests cover literal query behavior, occurrence/document/time/sequence
  ranking, current/shadowed/log-only classification, exact target recovery,
  neighbor summaries, event/session not-found, caller exclusion, other-
  workspace exclusion, lock-busy exclusion, malformed and oversized sources,
  cancellation and deadline behavior through the shared strict cold scanner.
- Tool tests cover both closed schemas, canonical UUIDv4 ids, integer/window
  bounds, search/read rendering, exact pretty JSON and non-truncating output-
  limit failure.
- A real two-process binary journey creates durable history, runs
  `session_search` → `session_event_search` → `session_event_read`, verifies all
  three call/result pairs and reconstructed Provider requests, and proves no
  approval event was emitted.

## Local commands

```console
cargo test 'session::search::tests' --lib -- --nocapture
cargo test 'tools::session_search::tests' --lib -- --nocapture
cargo test 'tools::registry::tests' --lib -- --nocapture
cargo test --test cli_smoke real_script_searches_a_closed_same_workspace_session_and_continues -- --exact --nocapture
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets --quiet -- --test-threads=1
git diff --check
```

All commands passed on macOS arm64 with Rust 1.85.0 and locked dependencies.
The serial all-target run passed the 927-test core library plus every
integration, binary and example target. Tests use fake providers, private
temporary workspaces and loopback HTTP. No real DeepSeek request, API key,
charge, remote CI, public-network product test, extra operating-system matrix
or stress run was used. Network access was used only to confirm the separate
upstream research clone's latest master.

## Review

The implementation was reviewed locally for canonical target parsing,
workspace identity, filename/file-mode/link/lock checks, strict full-log replay,
surface derivation, rank determinism, exact-versus-summary output, event and
response bounds, sanitized error codes, cancellation cleanup, call-before-
result order, approval absence and historical prompt-injection wording. The
focused tests, real CLI journey, compiler, serial all-target run and Clippy
provide automated evidence. No subagent was used because this continuation was
not authorized for delegation.

## Known limitations

- All three historical query tools are serialized per Agent to cap local disk
  and memory pressure. They do not read current or other live Sessions.
- `session_event_search` requires a canonical explicit id and exposes no event
  type/time/surface filters or cursor. It returns at most 20 events.
- `session_event_read` returns one exact event and at most 50 semantic neighbor
  summaries per side. If the exact result does not fit 64 KiB, it fails.
- Session and event lineage traces, titles, SQLite/live indexes, persistent
  derived indexes and cross-workspace reads remain absent.
- A fixed-source fixture and real Rust production path are tested, but no
  generated TypeScript producer proves broad cross-language compatibility. The
  compatibility row therefore remains `partial`, not `compatible`.
