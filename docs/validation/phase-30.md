# Phase 30 local validation — 2026-08-29

## Result

Phase 30 is complete under the requested local-only, necessary-check gate. The
real CLI now overlaps independent built-in `read`, `web_search`, and
`web_fetch` calls from one model step in a rolling pool capped at ten. Every
other tool remains exclusive. Call intentions still precede bodies; results,
workspace context, and the next model request remain in model order.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the exclusive-barrier,
  rolling-refill, default-cap-ten, ordered-finalization, cancellation-drain,
  and scheduler-failure rules recorded in `docs/upstream.md`. Latest inspected
  master `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains the same scheduler
  and standard-preset opt-ins.
- `ToolExecutor::execution_mode` is fail-closed: its default is `Exclusive`, a
  panic is caught as exclusive, and crate-controlled Shell/plugin actions or
  human interaction cannot opt in. Only the shipped `read`, `web_search`, and
  `web_fetch` names return `Parallel`; list/glob/grep, mutation, Shell, plugin,
  Goal, Plan, Todo, questions, and unknown names are barriers.
- `AgentLimits` defaults to ten parallel calls and accepts only 1–64. The
  scheduler owns a `FuturesUnordered` rolling pool without detached tasks. A
  settled later call can replenish the pool while an earlier call is pending,
  but authoritative result settlement remains serial and model ordered.
- Deterministic gated Agent tests prove a cap of two, rolling refill,
  call-before-body, out-of-order body settlement with ordered results and next
  context, safe/exclusive/safe barriers, cancellation without refill, paired
  `ABORTED`/`ABORTED_BEFORE_DISPATCH` results, and infrastructure failure that
  waits for already-started siblings before returning.
- The registry test freezes the exact production whitelist. A real
  `dsh --prompt` test sends two separate `web_search` tool calls; its loopback
  search server withholds both responses until both connections arrive. The
  CLI then completes, the second model request contains tool results in model
  order, durable events are call 1 → call 2 → result 1 → result 2, and no
  approval is requested.
- The complete local suite passed: 872 library tests, 35 script CLI journeys,
  115 enhanced/linear real-PTY journeys, and all remaining Agent, Provider,
  file, Shell, plugin, persistence, resume, release, and example targets.
  Compilation, Clippy with warnings denied, formatting, and diff whitespace
  checks also passed.

## Local commands

```console
cargo test --test agent_loop parallel_ -- --nocapture
cargo test --test cli_smoke real_script_overlaps_independent_web_tool_calls_and_preserves_model_order -- --nocapture
cargo test --all-targets --quiet
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Tests ran locally on macOS arm64 with Rust 1.85.0. They use fake executors,
temporary workspaces, fake credentials, local subprocesses/PTYs, and loopback
HTTP servers. No public-network request, live Web page, real DeepSeek call, API
charge, remote CI, or additional platform matrix was used.

## Known limitations

- The classifier is immutable and name-based for an Agent's schema snapshot;
  upstream can reclassify arguments against a dynamically replaceable registry.
  Custom argument-dependent tools must remain exclusive in Rust.
- Rust overlaps its ordinary safe preparation/body future, while upstream
  serializes more pre-execution middleware and overlaps dispatch. Shipped
  opt-ins have no approval, mutation, or parent-state commit in that future.
- Rust keeps the existing one-second cleanup grace and durable
  `TOOL_OUTCOME_UNKNOWN` repair instead of waiting without a generic deadline.
- There is no user-facing live cap setting, background task, multi-turn
  concurrency, or generated cross-language scheduler oracle. Compatibility
  therefore remains `partial` rather than `compatible`.
