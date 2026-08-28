# Phase 45 validation — durable first-prompt Session titles

Date: 2026-08-29
Scope: local-only necessary validation, as requested by the user.

## Delivered behavior

- The first non-empty direct-human text records a terminal-safe fallback title
  of at most five words and 40 UTF-8 bytes.
- The official non-loopback DeepSeek provider records one exact
  `session/title-llm-request` before starting a tool-free, 64-token auxiliary
  request. A plain-text `stop` response may replace the fallback with an
  80-byte title.
- Title preparation, provider failure, malformed output, timeout, cancellation
  and shutdown do not alter the main turn result. Shutdown cancels and joins or
  aborts the owned title task within a fixed grace period.
- `session/title` and `session/title-llm-request` are append-only log events,
  stay outside the model-visible surface, and replay with latest-title-wins
  semantics. A resumed result-unknown title request is not repeated.
- `--list-sessions` exposes an optional fourth title column. The interactive
  picker displays title plus workspace for normally closed, shared-locked,
  strictly valid journals no larger than 16 MiB; unavailable title metadata
  degrades to the old workspace/time/id row.
- Loopback endpoints keep the deterministic fallback and skip the additional
  model request, which preserves one-response offline tests and avoids a
  surprising auxiliary call to a local mock server.

## Evidence

- Upstream source paths and current-master comparison:
  `docs/upstream.md`, Phase 45.
- Design and failure analysis: `docs/design/session-titles.md`.
- Source-attributed fixture:
  `tests/fixtures/tools/upstream_phase45_session_titles.json`.
- Focused tests cover normalization, exact Agent event/request/replacement,
  remote-versus-loopback provider policy, latest title scanning, safe list
  rendering and title-first picker rows.
- The real CLI smoke suite confirms a loopback conversation creates a fallback
  title, shuts down normally, and exposes it through the fourth list field
  without an extra provider request.

## Local checks

The following commands passed from the repository root:

```console
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets -q
cargo clippy --all-targets -- -D warnings
git diff --check
```

The complete local test run included 956 library tests, 46 CLI smoke tests and
124 real interactive-terminal tests, plus the remaining integration/example
targets. No network API, remote CI, emulator pass or extra stress matrix was
run for this phase.

## Known limits

Manual rename/refresh, fork inheritance, official projection caching, title
exposure in historical-search tool results and a generated cross-language
oracle remain absent. A title finishing after a turn is appended only at the
next safe Agent boundary or orderly shutdown. Busy, malformed and larger-than-
16-MiB journals remain listable by safe header facts but omit the title.
