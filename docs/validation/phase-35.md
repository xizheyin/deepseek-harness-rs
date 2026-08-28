# Phase 35 local validation — 2026-08-29

## Result

Phase 35 is complete under the requested local-only, necessary-check gate. The
real CLI now discovers bounded project-local Markdown Skills, commits a complete
name/description catalog before Provider work, and exposes a parallel-safe
`skill {name}` loader. A load rereads the current winning file and returns the
fixed canonical resource/body frame through the ordinary correlated tool
result. No approval, process, network request or file write is introduced by
the Skill tool.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the name grammar,
  project-root priority, one-level bundle/flat discovery, invocation policy,
  schema, catalog prose and result frame recorded in `docs/upstream.md`.
  Fetched latest master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains that contract.
- `tests/fixtures/tools/upstream_phase35_project_skills.json` records the exact
  fixed sources, root ranks, closed schema, canonical small result and stable
  Rust error codes.
- Skill unit tests cover common scalar frontmatter, fail-closed invocation
  flags, kebab-case names, Unicode-safe description clipping, catalog escaping,
  `.dsh` precedence, directory and flat resources, disabled/malformed entries,
  current-body reload, unchanged/replacement catalogs, symlink rejection,
  256-KiB file and 256-entry root bounds, and pre-cancellation.
- The closed dispatcher test covers success, invalid name/extra field, unknown,
  model-disabled and cancelled results.
- A real Agent test starts with one catalog, uses the actual `write` pipeline to
  change its `SKILL.md`, observes a complete replacement in the next step,
  loads the updated body, and proves catalog → call → result order.
- A real enhanced-PTY journey proves the shipped CLI advertises the schema,
  commits the catalog after the direct prompt, loads the current body without an
  approval selector, and passes the correlated result to the second request.
- A two-process enhanced-PTY journey changes a Skill between exit and resume and
  proves a complete `update: true` catalog is appended before the resumed
  Provider request.
- The final repository run passed 891 library tests, 24 real file-change tests,
  121 enhanced/linear PTY tests, 16 Shell tests, and every Agent, Provider,
  plugin, persistence, resume, release and example target.

## Local commands

```console
cargo test --lib skills
cargo test --lib project_skill_catalog_call_and_body_continue_through_the_real_agent
cargo test --lib skill_dispatch_has_closed_success_error_and_cancellation_results
cargo test --test file_changes workspace_registry_keeps_fixed_file_and_skill_schemas_in_stable_order
cargo test --test interactive_cli project_skill -- --nocapture
cargo test --test interactive_cli resumed_project_skill_catalog_appends_a_complete_replacement_before_provider_work -- --nocapture
cargo test --test shell_tools
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo check --all-targets
cargo fmt --all -- --check
git diff --check
```

The first full all-target attempt had one unrelated terminal-profile PTY case
exit early with `CLI_TERMINAL_UNSUPPORTED`; its immediate focused rerun passed.
The second complete all-target run then passed that case and all other targets.
No test was ignored or weakened.

Tests ran locally on macOS arm64 with Rust 1.85.0 and locked dependencies. They
use fake providers, temporary workspaces, local PTYs and loopback HTTP. No real
DeepSeek call, API credential, charge, remote CI or extra platform matrix was
used. Network was used only to refresh the separate official research clone.

## Review

The implementation was manually reviewed for capability ownership, path races,
catalog-before-Provider order, tool-call/result correlation, stale reload,
resume behavior, cancellation, allocation bounds and presentation provenance.
That review found and fixed a check/open symbolic-link race: macOS/Linux now
opens every directory component and final file with `O_NOFOLLOW`, in addition
to the retained workspace capability. Compiler, Clippy, focused tests and the
complete all-target run provide independent automated checks. No subagent was
used because this continuation was not authorized for delegation.

## Known limitations

- Only workspace-root `.dsh/skills` and `.agents/skills` are scanned. Official
  dsh can also merge user, custom, bundled and other providers.
- Rust rejects all Skill symlinks. Official local discovery can follow them.
- Rust rescans at each new turn and between completed tool steps rather than
  running a background filesystem watcher. A file changed while no model
  boundary occurs is observed at the next boundary.
- The parser accepts common plain/single-quoted/JSON-double-quoted scalar
  frontmatter and the official boolean spellings; it is not a full YAML parser
  and omits arbitrary metadata objects and multiline scalar syntax.
- Direct human `/skill-name` injection, remote/opaque resource providers and
  executing Skill scripts are not implemented. Relative resource paths remain
  ordinary instructions that must use existing tools and permissions.
- Catalog descriptions use Rust Unicode-scalar clipping rather than JavaScript
  UTF-16 slicing. A checked-in TypeScript producer is absent, so compatibility
  remains `partial`, not `compatible`.
