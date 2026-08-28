# Phase 32 local validation — 2026-08-29

## Result

Phase 32 is complete under the requested local-only, necessary-check gate. The
real CLI now advertises the fixed upstream's `str_replace_editor` with `view`,
`create`, `str_replace`, and `insert`. Views need no approval. Mutations reuse
the existing safe file pipeline: default `ask` shows the complete semantic
change, while explicit `--approval-mode auto-edit` commits without opening the
selector. Shell and plugin approval behavior is unchanged.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the schema, absolute-path
  rule, canonical small outputs, line-range and insertion boundaries, unique
  literal matching, directory filtering/depth, clipping notice, and default
  bundle registration recorded in `docs/upstream.md`.
- `tests/fixtures/tools/upstream_phase32_str_replace_editor.json` records those
  source paths and the canonical create/view/replace/delete/insert journey.
  The Rust Agent comparison proves exact success/view text and final bytes.
- Every mutation calls the same retained-capability finalizer as `apply_patch`.
  It therefore inherits complete preview facts, `FileChangePolicy`,
  intent-before-side-effect ordering, late baseline and directory identity
  checks, atomic publication, committed/not-committed result metadata,
  cancellation, and trusted workspace-instruction touch.
- Focused tests cover the closed schema, absolute-path rule, clipping, all four
  commands, empty creation, end/deletion semantics, ambiguity line numbers,
  relative-path refusal, policy denial, external conflict, and filtered
  two-level directory traversal.
- Two real enhanced-PTY journeys prove both user modes. Default `ask` renders a
  trusted `Proposed update` diff and changes nothing before explicit Allow;
  `auto-edit` changes the same file with no selector. Both render one semantic
  `Updated` card and continue the same model turn.
- The final clean local suite passed: 878 library tests, 36 script CLI journeys,
  117 enhanced/linear real-PTY journeys, and every remaining Agent, Provider,
  file, Shell, plugin, persistence, resume, release, and example target.

## Local commands

```console
cargo test --lib str_replace_editor -- --nocapture
cargo test --test file_changes editor -- --nocapture
cargo test --test interactive_cli literal_editor -- --nocapture
cargo test --all-targets --quiet
cargo test --all-targets --quiet -- --test-threads=1
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Tests ran locally on macOS arm64 with Rust 1.85.0. They use fake providers,
temporary workspaces, local subprocesses/PTYs, and loopback HTTP servers. No
public-network request, real DeepSeek call, API charge, remote CI, or additional
platform matrix was used.

Parallel repository-wide runs twice hit one existing long-session PTY at its
startup boundary with `CLI_TERMINAL_UNSUPPORTED`—first
`long_reasoning_across_three_turns_continues_past_the_old_event_ceiling`, later
`durable_session_continues_after_crossing_the_old_real_event_ceiling`. Both
immediate focused retries passed. A separate run found one old exact
schema-order assertion, which was updated to include the new built-in tool. The
final complete all-target run used one local test thread to avoid competing PTY
allocation and was clean, including all 117 terminal journeys.

## Known limitations

- Rust keeps the repository's stricter workspace, no-symlink mutation,
  no-hardlink mutation, text-size, line-count, line-size, and Session-output
  limits. Valid official calls outside those limits can fail closed.
- Existing mutation text must use uniform LF or CRLF. Mixed or standalone CR is
  rejected; creation currently accepts the safe LF text vocabulary. This is
  stricter than the upstream JavaScript string path.
- Output clipping counts Rust Unicode scalar values and also obeys the 64 KiB
  encoded tool-result cap; JavaScript uses UTF-16 code units, so an emoji-heavy
  boundary can clip at a different character.
- Rust has no Cordis observation-policy plugin. It prepares from the current
  retained file capability and asks by default instead of requiring a separate
  earlier model-facing read.
- Empty creation or a byte-identical replacement has no material unified-diff
  hunk, so approval uses a truthful bounded opaque preview. Publication and
  committed metadata remain unchanged.
- The fixture is source-attributed but manually transcribed rather than emitted
  by a checked-in TypeScript producer; the compatibility row therefore remains
  `partial` rather than `compatible`.
