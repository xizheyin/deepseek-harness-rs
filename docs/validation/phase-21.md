# Phase 21 validation

Status: `complete`

Phase 21 implements `docs/design/user-question-skip.md` against fixed upstream
commit `47f943859bef60e4160492346772ded9b24f765a`. Latest `master` remains
`cd5ef8148158c3a752a658978873241fdf8e2bbc` and does not change the result shape.

## Delivered evidence

- Skip is a distinct typed response, not an empty incomplete answer. The broker
  projects it to the official `{id, selected: []}` shape without `custom`.
- Skipping a middle question keeps earlier answers, advances through the normal
  fresh input fence, and waits for later answers. Skipping the final question
  settles the complete ordered batch once.
- Enhanced selection mode uses `s`; enhanced custom editing uses Ctrl+S so
  printable `s` remains text; linear selection/custom modes use the exact `s`
  record. Prompts and Dock hints expose these mappings.
- Skip from custom or multi-select mode restores the Composer overlay and
  discards only the current partial answer. Escape remains whole-batch
  cancellation and Ctrl+C remains turn cancellation.
- Real PTY tests prove a middle option-free custom skip preserves surrounding
  answers and a final multi-select skip discards its partial selection. The
  second fake-Provider request verifies exact compact JSON under the original
  call ID.

## Local verification

Environment: macOS 27.0 on Apple Silicon; `rustc 1.85.0`; base commit `c532bae`.

Commands:

```console
cargo check --all-targets
cargo test user_question --lib -- --nocapture
cargo test --test interactive_cli user_question_ -- --nocapture
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
git diff --check
```

Results: 20 focused question tests and 11 real enhanced PTY journeys passed;
all-target compilation, format, Clippy with warnings denied, and whitespace
checks passed. Tests used only a loopback fake Provider and temporary
workspaces—no API key, real model request, or user project mutation.

The unrelated full repository suite, remote CI, cross-platform reruns, and a
delegated independent review were omitted under the user-selected fast local
gate. The main pass reviewed skip/empty-answer separation, batch order, partial
draft disposal, overlay restoration, cancellation separation, and unchanged
approval authority.

## Known limitations

The fixed official UI also supports pager navigation and plan-review
presentation. Rust retains its three-question and 4,096-byte caps, one terminal
answerer, no product subagent routing, and no answerer waterfall. The
compatibility row remains `partial`.
