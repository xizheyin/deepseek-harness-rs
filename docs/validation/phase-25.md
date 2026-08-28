# Phase 25 local validation — 2026-08-29

## Result

Phase 25 is complete under the user-requested local-only, necessary-check gate.
The production CLI now loads bounded `AGENTS.md`/`CLAUDE.md` guidance before
the first model request, records it after the direct prompt, and reconciles
confirmed changes without duplicating unchanged context after resume.

## Evidence

- Fixed upstream workspace-instruction discovery, rendering, state, lifecycle,
  spec and optional e2e sources were inspected. A fresh fetch confirmed latest
  master remains `cd5ef8148158c3a752a658978873241fdf8e2bbc`.
- Five unit tests cover global/project precedence, local overlays, trimmed
  duplicate collapse, delimiter escaping, exact structured source, broad-file
  omission, UTF-8 truncation, unchanged resume, add/replace/remove transitions,
  cancellation, and unavailable oversized/invalid/symlink sources.
- Two Agent tests prove the baseline follows the claimed input exactly once and
  that cancellation before step entry neither commits nor consumes it.
- One real enhanced PTY journey starts and resumes the actual CLI three times.
  It proves first-request ordering, changed-file `replace`, unchanged-resume
  reuse, exact model-visible content, and two—not three—durable instruction
  messages in JSONL.
- Approval/rejection and process timeout are not applicable: loading is a
  bounded read-only startup operation with no filesystem mutation, subprocess,
  network request, or approval decision. Cancellation and read failure are
  covered above.

## Local commands

```console
cargo test --lib workspace_instructions -- --nocapture
cargo test --lib workspace_context -- --nocapture
cargo test --test interactive_cli workspace_instructions_persist_reconcile_and_do_not_duplicate_on_resume -- --nocapture
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Tests ran locally on macOS arm64 with Rust 1.85.0. They used temporary
directories, a loopback fake DeepSeek server, and a conspicuous fake key; no
real API credential, model request, or user project was used. The full
repository suite, remote/cross-platform CI, and an independent subagent review
were not repeated under the requested fast local gate.

## Known limitations

- Only the user-global file and exact opened workspace root are discovered.
- Instruction-file symlinks are unavailable rather than followed.
- Phase 26 subsequently closed successful built-in file-touch discovery and
  same-process post-compaction rearming; see `phase-26.md`.
- Candidate names and budgets are fixed in this first production slice.
- The row remains `partial` without a generated cross-language oracle and the
  broader discovery/configuration behaviors listed in Phase 26.
