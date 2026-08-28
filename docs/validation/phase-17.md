# Phase 17 validation

Status: `complete`

Phase 17 implements the bounded first slice in
`docs/design/user-question.md` against fixed upstream commit
`47f943859bef60e4160492346772ded9b24f765a`. Latest `master` was separately
confirmed at `cd5ef8148158c3a752a658978873241fdf8e2bbc` without changing the
semantic baseline.

## Delivered evidence

- Interactive assembly advertises `ask_user_question`; script mode has no
  answerer and does not advertise it.
- The closed schema/parser accepts one question, two to four unique labelled
  options, optional bounded descriptions/header, and single-select only.
- A capacity-one broker waits lazily, correlates the displayed request and
  selected index, prioritizes turn cancellation, and fails closed for full,
  closed, dropped, cancelled, or invalid responses.
- The Agent records the ordinary tool intent before the wait. Human waiting
  bypasses only the 30-second tool timer and remains under the existing
  30-minute turn deadline and cancellation token.
- Enhanced and linear parsers accept only a displayed numeric choice. The
  terminal flushes input before rendering the question, so earlier typing
  cannot answer it.
- A successful result uses the official compact JSON shape. Escape produces a
  structured model-visible cancellation and never fabricates a selection.
- Two real enhanced PTY journeys prove select-and-continue and
  cancel-and-continue. The second Provider request is inspected for the exact
  call ID and answer/error text.

## Local verification

Environment:

- macOS 27.0 (26A5416b), Apple Silicon host
- `rustc 1.85.0 (4d91de4e4 2025-02-17)`
- base commit before the Phase 17 tree: `e3b32e2`

Commands:

```console
cargo check --all-targets
cargo test user_question --lib -- --nocapture
cargo test --test interactive_cli user_question_ -- --nocapture
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
git diff --check
```

Results: all-target compilation passed; 6 focused unit tests passed; 2 real
PTY journeys passed; format, Clippy with warnings denied, and whitespace checks
passed. Tests used a loopback fake Provider, temporary workspaces, no API key,
and no real model request.

The unrelated full repository suite, remote CI, cross-platform reruns, and a
delegated independent review were omitted under the user-selected fast local
gate. The main implementation pass manually reviewed the new argument, event,
cancellation, stale-input, and authority boundaries.

## Known limitations

This is not broad compatibility with the official question form. One call
accepts exactly one question with two to four options. Custom text, multi-select,
question batches, plan-review presentation, product subagents, and a general
answerer waterfall remain unimplemented, so `docs/compatibility.md` correctly
keeps the row at `partial`.
