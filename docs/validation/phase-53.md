# Phase 53 validation — durable safe permission presets

Date: 2026-08-29

Tested tree: Phase 53 working tree immediately before its green commit.

Environment: local macOS arm64 with the repository's configured Rust 1.85.0
toolchain. No remote CI, public network, real DeepSeek credential, live API, or
extra stress matrix was used.

## Delivered behavior

- Idle `/permission` reports the current preset and the closed `ask` /
  `auto-edit` choices. Exact `/permission ask` and
  `/permission auto-edit` switch it; malformed names and extra arguments stay
  local and do not enter a model prompt.
- `auto-edit` permits only fully prepared built-in file mutations. Shell and
  subprocess plugins remain Ask, scripts remain Deny, and no sandbox or
  unrestricted-access claim was added.
- A real change appends one closed, log-only `permission/preset` event before
  changing the in-memory file policy. A no-op appends nothing. An append
  failure preserves the old policy.
- The event is excluded from the model-visible surface and survives strict
  Session replay, resume and fork. A missing legacy value means `ask`.
- Startup precedence is explicit `--approval-mode`, then the latest Session
  preset, then `ask`. An explicit different startup value is recorded before
  Provider setup, any turn, or any tool side effect.
- Active enhanced input consumes `/permission` locally with a busy notice. The
  enhanced palette now has sixteen commands; the linear form remains free of
  ANSI escape bytes.

## Evidence

- Source-derived fixture:
  `tests/fixtures/tools/upstream_phase53_permission_presets.json`.
- Session tests cover strict codec round-trip, last-value projection,
  model-surface exclusion, unknown preset rejection and unknown-field
  rejection.
- Agent tests cover the narrow file/Shell/plugin split, durable append, no-op,
  storage-capacity failure preserving Ask, idle-only admission, and the fixture
  provenance.
- Parser, command-palette, Dock-window, help and active-input branches cover
  the closed command surface and the sixteenth entry.
- A real enhanced PTY journey switches to `auto-edit`, commits a patch without
  approval, proves Bash still opens the safe-default selector, switches back
  to `ask`, and rejects the next patch. The journal asserts preset order and
  preset-before-tool-call ordering.
- A three-process PTY journey proves startup `auto-edit` persistence, resume
  reuse without a selector, and an explicit resumed `ask` override. A separate
  linear PTY journey proves zero ANSI and no model request.

The final local gates passed:

```console
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets -q
cargo clippy --all-targets -- -D warnings
git diff --check
```

The all-target suite passed 1,429 tests with no ignored tests. Focused tests
were used while implementing; one final ordinary repository-wide run is the
acceptance result. Per the requested fast local scope, no independent agent
review or remote platform run was added.

## Known limits

- Rust has no proven operating-system sandbox, so it does not expose official
  `workspace-write` or `danger-full-access` bundles.
- The Session event records the narrow preset only. Rust does not emit official
  `sandbox/mode` or whole-session `approval/policy` mechanism events.
- There is no deployment-wide default, settings file editor, Web picker,
  arbitrary custom bundle, automatic Shell/plugin approval, or running-turn
  mutation.
- Exact Shell grants remain separate bounded process-local facts and are not
  restored by this preset.
