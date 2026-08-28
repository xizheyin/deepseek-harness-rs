# Phase 20 validation

Status: `complete`

Phase 20 implements `docs/design/user-question-multi-select.md` against fixed
upstream commit `47f943859bef60e4160492346772ded9b24f765a`. Latest `master` was
separately confirmed at `cd5ef8148158c3a752a658978873241fdf8e2bbc` without
moving the fixed compatibility baseline.

## Delivered evidence

- The model schema and parser accept boolean `multi_select`, preserve false as
  the default, and reject non-boolean values before terminal dispatch.
- A multi-select question toggles bounded option indices without advancing.
  Removing and reselecting an option moves it to the end of the response,
  matching the fixed Web draft-array behavior.
- Enter submits only after at least one selected option. The custom entry uses
  the existing bounded Composer; nonblank custom text supplements selected
  labels, while an empty custom editor may still submit existing selections.
- The broker independently rejects an empty selected-only answer, duplicate or
  out-of-range indices, a multi response for a single-select question, and a
  single-select response for a multi question. Valid indices become the exact
  displayed labels in user selection order.
- Enhanced mode hides and preserves the next-turn draft for the full multi-
  select interaction. The Dock displays the current selected option numbers.
  Linear mode accepts one toggle number per record and an empty record to
  submit.
- Selected-only, selected-plus-custom, Escape, EOF, turn cancellation, failure,
  and panic cleanup all retain the existing whole-batch/capacity-one ownership
  and restore the Composer overlay. Partial selections never become Provider or
  Session context.
- Real enhanced PTY journeys prove ordered toggle-off/on output, custom text
  supplement, and whole-question Escape cancellation through the original tool
  call ID and second fake-Provider request.

## Local verification

Environment:

- macOS 27.0 (Darwin 27.0.0), Apple Silicon host
- `rustc 1.85.0 (4d91de4e4 2025-02-17)`
- base commit before the Phase 20 tree: `b9feb1f`

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

Results: all-target compilation passed; 18 focused question protocol/UI tests,
2 Composer-overlay tests, 1 exact-cap editor test, and 9 real PTY journeys
passed. Format, Clippy with warnings denied, and whitespace checks passed. The
PTY tests used a loopback fake Provider, temporary workspaces, no API key, and
no real model request.

The unrelated full repository suite, remote CI, cross-platform reruns, and a
delegated independent review were omitted under the user-selected fast local
gate. The main pass manually reviewed response-shape separation, duplicate and
range checks, toggle order, empty-answer behavior, custom supplementation,
batch atomicity, overlay cleanup, and unchanged approval authority.

## Known limitations

The fixed official UI can skip individual questions, navigate earlier pages,
and render plan-review intent. Rust retains its three-question and 4,096-byte
custom caps, one terminal answerer, no product subagent routing, and no general
answerer waterfall. The compatibility row therefore remains `partial`.
