# Phase 36 local validation — 2026-08-29

## Result

Phase 36 is complete under the requested local-only, necessary-check gate. The
real CLI now exposes a read-only `session_search { query }` tool that can find
useful text in normally closed sessions from the exact opened workspace. It
excludes the current session and journals locked by another process, performs
no recovery or replay, requires no approval, and labels returned history as
untrusted data.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the extraction,
  workspace filtering, query-tool and presentation behavior recorded in
  `docs/upstream.md`. Fetched latest master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains the five-tool session-query
  family.
- `tests/fixtures/tools/upstream_phase36_session_search.json` records the fixed
  source paths, reduced closed schema, bounds, result shape and stable Rust
  error codes.
- Unit tests cover literal Unicode-aware case-insensitive matching, flexible
  whitespace, exact and over-limit queries, semantic event extraction,
  occurrence/recency ranking, snippet and rendered-output bounds.
- Store/runtime tests cover same-workspace authorization, current-session and
  busy-journal exclusion, malformed and oversized journal skipping,
  cancellation, deadline expiry and strict cold-log validation.
- A real two-process script journey creates and closes one session, starts a
  second CLI process, calls `session_search`, verifies the untrusted-history
  notice reaches the next model request, and proves call-before-result order
  without an approval event.
- The full local run passed 900 library tests, 37 script CLI smoke tests and all
  other Agent, Provider, tool, persistence, plugin, PTY, release and example
  targets.

## Local commands

```console
cargo test session_search --lib
cargo test 'session::search::tests' --lib
cargo test --test cli_smoke real_script_searches_a_closed_same_workspace_session_and_continues
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
git diff --check
```

The first repository-wide run reached Clippy after every test passed and found
one internal constructor with eight arguments. The plugin configuration and
its cancellation token were grouped into one small launch value; the focused
Clippy rerun and diff check then passed. No test or lint was disabled.

Tests ran locally on macOS arm64 with Rust 1.85.0 and locked dependencies. They
use fake providers, temporary workspaces, local subprocesses and loopback HTTP.
No real DeepSeek call, API credential, charge, remote CI or additional platform
matrix was used. Network access was used only for the separate upstream source
inspection already recorded in `docs/upstream.md`.

## Review

The implementation was reviewed locally for workspace capability ownership,
symlink and metadata checks, file-lock behavior, call/result ordering,
quiescent-session selection, bounded allocation, cancellation and timeout
cleanup, secret/path exposure and historical prompt-injection wording. The
compiler, focused tests, complete all-target run and Clippy provide the
automated checks. No subagent was used because this continuation was not
authorized for delegation.

## Known limitations

- Rust implements only `session_search`; the official optional package also
  provides event search, trace, event trace and exact event read tools.
- Only closed, unlocked, current-version local JSONL journals are scanned.
  Live sessions and a persistent derived index are intentionally absent.
- Rust identifies a workspace by device/inode identity and ranks by phrase
  occurrence then recency. Official dsh uses workspace metadata plus SQLite
  FTS5/BM25 and exposes additional filters, titles and lineage.
- Results are capped at 20 with 240-code-point excerpts; a session is capped at
  16 MiB, one call at 64 MiB aggregate and five seconds. Skipped candidates make
  the result incomplete rather than unsafe.
- A checked-in TypeScript fixture producer is absent, so the compatibility row
  remains `partial`, not `compatible`.
