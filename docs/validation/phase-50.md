# Phase 50 validation — safe current-Session raw log export

Date: 2026-08-29

Tested tree: Phase 50 working tree immediately before its green commit.

Environment: macOS 27.0 arm64, Rust 1.85.0
(`4d91de4e48198da2e33413efdcd9cd2cc0c46688`).

## Delivered behavior

- Exact idle `/export` is available in enhanced and linear terminals, the
  thirteen-command palette and `/help`. Arguments are rejected locally.
- The current Session crosses its durability barrier, then copies its exact
  accepted JSONL prefix through the journal's owned descriptor in bounded
  64 KiB chunks. Positional reads do not move the append cursor.
- A generated `dsh-session-<safe-id>.jsonl` is created in the retained
  workspace root with mode `0600` and `create_new`; collisions receive bounded
  numeric suffixes and existing files are never overwritten.
- Success reports the generated filename, workspace-root location and exact
  byte count. Export creates no model request, Agent turn, tool, approval or
  Session event.
- `Ctrl+C`, destination failure and cancellation settle the owned copy and
  remove an unaccepted partial output when possible. Destination failure does
  not poison the source Session; a source failure does.
- Exported files are external artifacts: they are not registered for resume
  and interrupted exports are not replayed.

## Evidence

- Source-attributed fixture:
  `tests/fixtures/tools/upstream_phase50_session_log_export.json`.
- Journal tests cover exact bytes, append-cursor preservation, cancellation
  between bounded chunks and a still-usable source after destination failure.
- Export-target tests cover private permissions, safe names, collision
  suffixes, bounded exhaustion, no overwrite and uncommitted-file cleanup.
- Agent tests cover idle-only admission, pre-cancellation and memory-Session
  rejection.
- Real enhanced and linear PTY tests cover argument rejection, exact raw bytes,
  a second non-overwriting export, private permissions, local-only execution
  and zero ANSI in the linear path.
- Local gates passed:

```console
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets -q
cargo clippy --all-targets -- -D warnings
git diff --check
```

The clean all-target run passed 1,408 tests with no ignored tests. No network,
real DeepSeek credential, remote CI or extra stress matrix was used.

Independent review was not separately delegated under the user-requested
local-minimal validation scope; compiler, Clippy, deterministic fixtures, full
repository tests and real PTY journeys form the acceptance evidence.

## Known limits

- Rust exports one current raw `.jsonl`, not the official ZIP containing a root
  Session, descendants and media. This product currently has no product
  subagents or image-attachment store to archive.
- The generated file is written only to the already-authorized workspace root;
  `/export` accepts no caller-selected path and provides no import command.
- The raw Session may contain prompts, tool output or other sensitive content;
  mode `0600` protects its initial local permissions but is not encryption.
- Generic upstream `command/run` and `command/done` lifecycle facts remain
  absent until Rust has one designed local-command event schema.
