# Phase 31 local validation — 2026-08-29

## Result

Phase 31 is complete under the requested local-only, necessary-check gate. A
live Agent now notices exact repeated tool calls and, at the fixed upstream
counts 3, 5, and 8, adds an advisory plugin notice to the next model step. The
notice does not alter the tool result, perform another action, or bypass an
approval. A direct-human turn resets the chain; autonomous Goal-style plugin
continuation keeps it; rebuilding/resuming an Agent starts fresh.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the default bundle
  enablement, exact gentle/detailed texts, 3/5/8 thresholds, canonical argument
  identity, direct-human reset, definite-error counting, result preservation,
  and next-step event order recorded in `docs/upstream.md`. Latest inspected
  master `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains that behavior.
- `RepeatToolReminder` is owned by one `AgentLoop`, retains only the latest
  bounded name/canonical-argument chain, and has no worker or detached task.
  JSON object keys are deterministic at every depth and JavaScript-safe integral
  values normalize before comparison; malformed or rejected JSON uses a quoted
  raw-string identity.
- Result settlement is the single observation point for both serial and
  parallel execution. Definite successes and model-facing failures count in
  model order. `ABORTED_BEFORE_DISPATCH`, durable-limit skips, and unknown
  infrastructure outcomes do not count. Existing tool output, approval,
  cancellation, timeout, and Session provenance remain authoritative.
- The notice has source `plugin=repeat-tool-reminder`, `form=notice`, and summary
  `<tool> × <count>`. It is staged only after the triggering result commits,
  then reserved and appended through the next ordinary step before the model
  request. The detailed argument preview is capped at 500 Unicode scalars and
  reports the omitted count.
- The committed deterministic fixture
  `tests/fixtures/agent/upstream_phase31_repeat_tool_reminder.json` transcribes
  the fixed default five-call scenario and exact source/test paths. The Rust
  comparator proves both exact notices, source facts, result 3 → step end 3 →
  step start 4 → notice order, unchanged results, and next-request replay.
- Focused Agent tests cover direct-human reset, autonomous plugin/Goal-style
  continuation, fresh reconstruction, failed results, and Phase 30 parallel
  model-order integration. Unit tests cover deep key ordering, number
  normalization, malformed fallback, different-call reset, thresholds, and
  Unicode-safe preview.
- A real `dsh --prompt` loopback journey reads the same real workspace file
  three times, proves no notice reaches request 3, proves the gentle notice and
  unchanged read result reach request 4, verifies durable source/event order,
  and observes no approval.
- The clean complete local suite passed: 876 library tests, 36 script CLI
  journeys, 115 enhanced/linear real-PTY journeys, and all remaining Agent,
  Provider, file, Shell, plugin, persistence, resume, release, and example
  targets. One first run hit the existing timing-sensitive plugin
  `hello_followed_by_immediate_exit` assertion; its immediate focused rerun and
  the complete second all-target run both passed. No test was ignored or
  changed to hide it.

## Local commands

```console
cargo test --lib repeat_tool_reminder -- --nocapture
cargo test --test agent_loop repeat -- --nocapture
cargo test --test cli_smoke real_script_reminds_the_model_after_three_identical_tool_calls -- --nocapture
cargo test --all-targets --quiet
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Tests ran locally on macOS arm64 with Rust 1.85.0. They use fake
providers/executors, temporary workspaces, fake credentials, local
subprocesses/PTYs, and loopback HTTP servers. No public-network request, real
DeepSeek call, API charge, remote CI, or additional platform matrix was used.

## Known limitations

- Detection is exact after JSON canonicalization. Similar but non-identical
  arguments evade the chain; legitimate identical polling still receives an
  advisory reminder.
- Rust ships only the fixed defaults: all Agent tools are tracked, thresholds
  are 3/5/8, and the preview cap is 500. There is no CLI wildcard include/
  exclude or live setting.
- The counter is intentionally in memory. Resume/new construction starts fresh,
  while already logged notice messages remain ordinary Session history.
- Rust stages a notice in the owning turn until the next step reserves it,
  rather than persisting upstream's separate durable inbox-splice fact. A crash
  in that narrow interval can lose only the heuristic notice, never a model-
  visible or tool-result fact.
- Rust preview truncation counts Unicode scalar values instead of JavaScript
  UTF-16 code units, avoiding half-surrogate strings. A generated executable
  cross-language producer is absent, so compatibility remains `partial`.
