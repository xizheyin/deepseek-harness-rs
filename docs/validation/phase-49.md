# Phase 49 validation — explicit Session title refresh

Date: 2026-08-29

Tested tree: Phase 49 working tree immediately before its green commit.

Environment: macOS 27.0 arm64, Rust 1.85.0
(`4d91de4e48198da2e33413efdcd9cd2cc0c46688`).

## Delivered behavior

- Exact idle `/refresh-title` is available in enhanced and linear terminals,
  the twelve-command palette and `/help`. Arguments are rejected locally.
- The Session projection retains only the first direct-human title input,
  bounded to 4096 UTF-8 bytes and redacted from `Debug`. Durable replay rebuilds
  it without a new schema field or all-history in-memory vector.
- A title-capable Provider refresh records `session/title-llm-request` before
  one tool-free request. Only a normalized text response ending in `stop`
  appends a provider title sourced to the first prompt.
- Invalid output, Provider failure, caller cancellation and the existing
  60-second timeout append no replacement title and preserve the current one.
  Cancellation reaches the Provider token and leaves no detached task.
- With no title-capable Provider, an automatic title is unchanged; a manually
  pinned title is replaced by the deterministic first-prompt fallback without
  a network call.
- Empty input and pre-cancelled refreshes have no Session or Provider side
  effect. The command opens no Agent turn/tool and requires no approval.

## Evidence

- Source-attributed fixture:
  `tests/fixtures/tools/upstream_phase49_session_title_refresh.json`.
- Title/projection unit tests cover direct-human filtering, UTF-8 bounds,
  redacted debug, first-prompt stability and no-history behavior.
- Fake-provider Agent tests cover provider success and exact request/title
  order, invalid output, cancellation, timeout, fallback-only unchanged/unpin,
  retained user title on failure and absence of automatic replay.
- Real PTY tests cover linear fallback refresh with no extra request and an
  enhanced create → rename → exit → resume → refresh journey. The resumed
  journal ends with the reconstructed first-prompt fallback.
- Local gates passed:

```console
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets -q
cargo clippy --all-targets -- -D warnings
git diff --check
```

The clean all-target run passed 1,399 tests with no ignored tests. No network,
real DeepSeek credential, remote CI or extra stress matrix was used.

Independent review was not separately delegated under the user-requested
local-minimal validation scope; compiler, Clippy, deterministic fixtures, full
repository tests and real PTY journeys form the acceptance evidence.

## Known limits

- The shipped Rust title Provider follows official `first-prompt` selection;
  it does not expose the generic all-messages provider surface.
- Rust serializes refresh through the current idle Agent. It does not implement
  overlapping callers, remote/cross-process refresh or newest-revision races.
- `/refresh-title` is a Rust terminal command name, not an upstream CLI command.
- A Provider failure is intentionally summarized without backend text so a
  terminal notice cannot expose secrets.
