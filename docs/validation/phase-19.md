# Phase 19 validation

Status: `complete`

Phase 19 implements `docs/design/user-question-custom.md` against fixed
upstream commit `47f943859bef60e4160492346772ded9b24f765a`. Latest `master` was
separately confirmed at `cd5ef8148158c3a752a658978873241fdf8e2bbc` without
moving the fixed compatibility baseline.

## Delivered evidence

- The model schema and parser accept omitted or empty choices as free text,
  retain the existing two-to-four bound when choices exist, and continue to
  reject multi-select and unknown fields before terminal dispatch.
- One typed response item is either an exact displayed option index or bounded
  custom text. The broker revalidates every item against the original ordered
  request and returns official-shape compact JSON with either one selected
  label or `selected: []` plus trimmed `custom`.
- Enhanced mode reuses the grapheme-safe Composer for Unicode, navigation,
  editing, paste, and Ctrl+J newlines. A private overlay restores the exact
  pre-question next-turn draft, cursor, and history-navigation state after
  submit, Escape, EOF, failure, or turn cleanup.
- Custom text is limited to 4,096 UTF-8 bytes. The exact limit remains editable;
  one more byte is rejected without discarding the retained answer. Blank,
  untrimmed wire responses, invalid controls, oversized values, wrong counts,
  and out-of-range choices fail closed.
- Escape while custom text is present cancels the whole batch and returns only
  `ASK_CANCELLED`; partial local text never appears in the Provider request or
  Session.
- Real enhanced PTY journeys cover ordinary choice, choice cancellation,
  sequential choice batch, option-free Unicode multiline custom text, mixed
  choice/custom batch ordering, and partial-custom cancellation. Every success
  checks the exact correlated tool result in the second fake-Provider request.

## Local verification

Environment:

- macOS 27.0 (Darwin 27.0.0), Apple Silicon host
- `rustc 1.85.0 (4d91de4e4 2025-02-17)`
- base commit before the Phase 19 tree: `577d917`

Commands:

```console
cargo check --all-targets
cargo test user_question --lib -- --nocapture
cargo test question_overlay --lib -- --nocapture
cargo test question_custom_editor --lib -- --nocapture
cargo test --test interactive_cli user_question_ -- --nocapture
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
git diff --check
```

Results: all-target compilation passed; 13 focused question protocol/UI tests,
2 Composer-overlay tests, 1 exact-cap editor test, and 6 real PTY journeys
passed. Format, Clippy with warnings denied, and whitespace checks passed. The
PTY tests used a loopback fake Provider, temporary workspaces, no API key, and
no real model request.

The unrelated full repository suite, remote CI, cross-platform reruns, and a
delegated independent review were omitted under the user-selected fast local
gate. The main pass manually reviewed result shape, cap enforcement, batch
atomicity, Escape timing, Composer restoration, cleanup-before-return, and
unchanged approval authority.

## Known limitations

The official UI can also submit multiple selected values, skip a question, and
render plan-review intent. Rust retains its three-question and 4,096-byte custom
caps, one terminal answerer, no product subagent routing, and no general
answerer waterfall. The compatibility row therefore remains `partial`.
