# Phase 18 validation

Status: `complete`

Phase 18 implements `docs/design/user-question-batch.md` against fixed upstream
commit `47f943859bef60e4160492346772ded9b24f765a`. Latest `master` was separately
confirmed at `cd5ef8148158c3a752a658978873241fdf8e2bbc` without moving the fixed
compatibility baseline.

## Delivered evidence

- The model schema and parser accept one through three questions and reject an
  empty/oversized batch or duplicate IDs before terminal dispatch.
- One capacity-one envelope owns the complete ordered request and one response
  vector. Response count and every selected index are checked before labels are
  projected.
- The terminal keeps only the current question plus bounded selected indices,
  displays `question N/M`, and fences input before every next question.
- Non-final choices never settle the broker or create a Session fact. The final
  choice returns one official-shape compact JSON array in request order.
- Escape after an earlier local choice cancels the whole batch and publishes no
  partial answer.
- A real enhanced PTY journey answers two sequential questions, continues the
  same Agent turn, and verifies both exact labels under the original call ID in
  the second Provider request. Phase 17 single-select and cancellation journeys
  remain green.

## Local verification

Environment:

- macOS 27.0 (26A5416b), Apple Silicon host
- `rustc 1.85.0 (4d91de4e4 2025-02-17)`
- base commit before the Phase 18 tree: `27fe6dd`

Commands:

```console
cargo check --all-targets
cargo test user_question --lib -- --nocapture
cargo test --test interactive_cli user_question_ -- --nocapture
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
git diff --check
```

Results: all-target compilation passed; 8 focused unit tests passed; 3 real PTY
journeys passed; format, Clippy with warnings denied, and whitespace checks
passed. Tests used a loopback fake Provider, temporary workspaces, no API key,
and no real model request.

The unrelated full repository suite, remote CI, cross-platform reruns, and a
delegated independent review were omitted under the user-selected fast local
gate. The main pass manually reviewed batch bounds, input fencing, all-or-
nothing publication, cancellation, result order, and unchanged approval
authority.

## Known limitations

The official tool can collect optional/custom answers and multi-select values,
and does not impose Rust's three-question ceiling. Those forms, plan-review
presentation, product subagent routing, and a general answerer waterfall remain
unimplemented, so the compatibility row stays `partial`.
