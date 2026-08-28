# Phase 34 local validation — 2026-08-29

## Result

Phase 34 is complete under the requested local-only, necessary-check gate. The
real CLI now advertises the fixed official `write` and `edit` schemas. `write`
creates or completely replaces one UTF-8 workspace file; `edit` requires one
literal match unless `replace_all: true` explicitly requests every
non-overlapping match. Both use the existing semantic diff, approval, conflict
and atomic publication pipeline.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the schemas, defaults,
  exact success strings, unique/all matching and error vocabulary recorded in
  `docs/upstream.md`. Latest inspected master retains that contract.
- `tests/fixtures/tools/upstream_phase34_write_edit.json` records exact source
  paths, required fields, default unique behavior, normal result strings, final
  bytes and stable error codes.
- The real Agent fixture runs create, blind full overwrite, unique edit and
  explicit replace-all through model call, preparation, result and next-step
  continuation. It compares normalized official result text and final bytes.
- Focused failure tests cover closed/wrong arguments, empty write, no match,
  ambiguous default match, pre-allocation expansion rejection, policy denial,
  human rejection, cancellation during approval and a late external conflict.
  Existing shared mutation tests continue to cover links, hard links, unsafe
  text, modes, CRLF preservation, file/line limits, publication races and
  cancellation around the final atomic boundary.
- A real enhanced-PTY journey proves `edit` reaches the shipped CLI, presents
  the trusted semantic diff, changes nothing before explicit Allow, replaces
  every requested match, renders one `Updated` card and returns the exact tool
  result to the next model step. Its first request also proves both new schemas
  are advertised.
- The final local repository suite passed: 885 library tests, 24 real file-
  change integration tests, 119 enhanced/linear real-PTY journeys, 16 Shell
  integration tests and every remaining Agent, Provider, plugin, persistence,
  resume, release and example target.

## Local commands

```console
cargo test --lib tools::write_edit
cargo test --test file_changes workspace_registry_keeps_the_fixed_editor_and_patch_schemas_before_todo_write
cargo test --test file_changes write_edit -- --nocapture
cargo test --test file_changes
cargo test --test interactive_cli official_edit_replace_all_uses_the_normal_semantic_diff_approval
cargo test --test shell_tools local_registry_exposes_a_closed_foreground_bash_schema
cargo test --lib tui::projector
cargo test --lib tui::timeline
cargo test --lib cli::approval_join
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo fmt --all -- --check
git diff --check
```

Tests ran locally on macOS arm64 with Rust 1.85.0 and locked dependencies. They
use fake providers, temporary workspaces, local PTYs and loopback HTTP. No
public-network request, real DeepSeek call, API charge, remote CI or additional
platform matrix was used.

## Review

The primary implementation diff was manually reviewed for authority ownership,
argument closure, cancellation, allocation-before-limit risk, call-before-
side-effect ordering, conflict checks, Session result truth and UI provenance.
That review found and fixed the replace-all expansion allocation risk before
the final run. Compiler, Clippy, focused tests and the all-target suite provided
independent automated checks. No delegated subagent review was run in this
single-agent continuation.

## Known limitations

- The official default filesystem observation policy requires a prior model
  `read` before overwrite/edit. Rust instead prepares the complete bounded
  baseline, asks by default and revalidates that exact baseline before publish.
  Therefore explicit `auto-edit` can perform a blind complete overwrite.
- Rust paths stay inside the retained workspace and file mutations reject
  symbolic links, multi-linked files and changed parent identities. Official
  providers can accept paths Rust refuses.
- New content uses the existing safe LF vocabulary; updates preserve a uniform
  existing LF or CRLF style. Mixed line endings, NUL and unsafe controls fail.
- Agent tool-call arguments cap practical write/edit input below the separate
  16 MiB mutation-file ceiling. Replace-all computes its result size before
  allocation and rejects an expansion beyond that ceiling.
- Rust persists its existing relative mutation metadata and result path text,
  not the official execution-local full before/after value or absolute local
  display path. There is no `read_image` or filesystem sandbox-escalation
  argument in this phase.
- The source-attributed fixture is manually transcribed rather than emitted by
  a checked-in TypeScript producer, so compatibility remains `partial`, not
  `compatible`.
