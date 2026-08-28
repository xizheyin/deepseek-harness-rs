# Phase 38 local validation — 2026-08-29

## Result

Phase 38 is complete under the requested local-only, necessary-check gate. The
real CLI now accepts an explicit canonical `--time-zone` and records one
bounded time snapshot for every entered model step. The message contains the
turn and step, an IANA/DST-aware whole-second timestamp, terminal zone and
elapsed time. It is an append-only Session fact, so reconstruction, cold resume
and compaction keep the evidence that earlier model reasoning actually saw.

Invalid or noncanonical zones fail before Session creation, credential access,
plugin/LSP startup or network connection. Clock, formatting, ID or message
failure closes the turn with `AGENT_TIME_CONTEXT` before a step or Provider
request starts. The feature adds no approval, tool, filesystem, subprocess or
network authority.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the pre-step order,
  snapshot ownership, exact first/later-step elapsed wording, zone derivation,
  whole-second formatting and durable recovery semantics recorded in
  `docs/upstream.md`. Fetched latest master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains the model-visible
  contract while preserving extra pre-step decision fields.
- `tests/fixtures/context/upstream_phase38_time_context.json` records the fixed
  source paths, source shape, first/later-step text, event order and deliberate
  terminal-zone difference.
- Unit tests cover canonical, alias and invalid zones; UTC and DST offsets;
  whole-second rendering; compact duration and backward-clock clamping; exact
  snapshot shape; malformed-shape rejection; compaction shadowing; and replayed
  timing baselines without process memory.
- Agent tests cover one reading per step, two-step request accumulation, event
  order, unchanged request header, clock failure, pre-cancellation, balanced
  turn closure and no Provider dispatch.
- Real binary loopback tests cover configured-zone request/session wiring,
  approval absence, a second process replaying the first reading and appending
  a new turn reading, and invalid-zone failure before Session, secret or socket
  access.

## Local commands

```console
cargo test time_context --lib -- --nocapture
cargo test --test cli_smoke real_script_records_configured_time_context_before_the_model_request -- --exact --nocapture
cargo test --test cli_smoke resumed_script_replays_old_time_context_and_appends_a_fresh_reading -- --exact --nocapture
cargo test --test cli_smoke invalid_time_zone_fails_before_session_credentials_or_network -- --exact --nocapture
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets --quiet -- --test-threads=1
cargo clippy --all-targets -- -D warnings
git diff --check
```

Tests ran locally on macOS arm64 with Rust 1.85.0 and locked dependencies. They
use fixed clocks, fake providers, temporary workspaces and loopback HTTP. No
real DeepSeek request, API credential, charge, remote CI, extra operating-
system matrix, browser, Schedule product or public-network product test was
used. Network access was used only for the separate upstream source fetch
already recorded in `docs/upstream.md` and dependency retrieval when required.

## Dependency review

Phase 38 adds exact `jiff 0.2.35` with default features disabled and only `std`
plus `tzdb-zoneinfo`. Rust's standard library cannot safely supply canonical
IANA aliases, daylight-saving offsets and zoned ISO formatting without
process-global `TZ` mutation. Jiff is MIT/Unlicense, supports Rust 1.70+, and
therefore does not raise this repository's Rust 1.85 minimum.

## Review

The implementation was reviewed locally for startup order, canonical input,
fixed message bounds, append-before-request order, projection/replay truth,
compaction shadowing, cancellation, stable errors, request-header isolation,
approval/tool authority and secret exposure. The compiler, focused tests,
serial all-target run and Clippy provide the automated checks. No subagent was
used because this continuation was not authorized for delegation.

## Known limitations

- Configuration is explicit and process-local. Resume requires passing
  `--time-zone` again for new readings; old readings remain in history.
- One reading is added to every entered step, so it consumes a small amount of
  context. There is no configurable positive refresh interval.
- The standalone terminal does not reproduce official browser unique/mixed/
  missing-zone provenance, browser RPC, ambient process fallback or Schedule
  defaults. It always says `Terminal time zone`.
- A fixed-source fixture and real Rust production path are tested, but no
  generated TypeScript oracle proves broad cross-language compatibility. The
  compatibility row therefore remains `partial`, not `compatible`.
