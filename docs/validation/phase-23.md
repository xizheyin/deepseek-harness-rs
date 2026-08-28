# Phase 23 local validation — 2026-08-28

## Result

Phase 23 is complete under the user-requested local-only, necessary-check gate.
The production terminal now has a durable Plan Mode, exact bounded plan review,
and an approved exit that takes effect only before the next model request.

## Evidence

- The fixed upstream Plan Mode, user-question intent, Web review panel, and
  projection sources/tests were inspected; latest master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains the core contract.
- Codec and projection tests prove strict `plan/mode` decoding, last-value
  recovery, and rejection of unknown payload fields.
- Tool tests prove a closed 16 KiB schema, required Markdown heading, inactive
  refusal, and cancellation without a pending exit.
- Three enhanced real-PTY journeys cover exact approval, custom feedback, and
  Escape dismissal. Requests prove that feedback/dismissal keep the plan
  policy while approval removes it only on the following step.
- The approval journey reads the real JSONL and proves `tool/result` precedes
  `plan/mode { active: false }`, which precedes the changed request header.
- One zero-ESC linear PTY journey covers `/plan <message>`, model-visible plan
  policy, and idle `/plan off`.

## Local commands

```console
cargo test --lib plan_mode -- --nocapture
cargo test --lib inactive_or_cancelled_plan_exit_fails_without_arming_a_transition -- --nocapture
cargo test --lib user_question -- --nocapture
cargo test --test session_codec plan_mode_events_round_trip_and_reject_unknown_payload_fields -- --nocapture
cargo test --test interactive_cli plan_mode -- --nocapture
cargo test --test interactive_cli linear_plan_command_enters_sends_and_manually_exits_without_escape_bytes -- --nocapture
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

The user explicitly asked to reduce checks and validate only on this machine.
The full repository test suite and remote/cross-platform CI were therefore not
repeated for this checkpoint.
