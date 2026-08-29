# Phase 52 validation — idle Session model selection

Date: 2026-08-29

Tested tree: Phase 52 working tree immediately before its green commit.

Environment: macOS 27.0 arm64, Rust 1.85.0
(`4d91de4e48198da2e33413efdcd9cd2cc0c46688`).

## Delivered behavior

- Idle `/model` reports the effective current model and reasoning effort plus
  the built-in DeepSeek suggestions. `/model MODEL` resolves the configured
  Provider default; `/model MODEL off|high|max` selects an exact effort.
- Model ids are one control-free token capped at 256 UTF-8 bytes. The two
  built-in ids are advisory, so another valid DeepSeek model id passes through.
  Unknown effort, extra arguments or an oversized id show local usage and never
  enter a model prompt.
- Selection is validated synchronously through the configured Provider without
  a credential read or network request. Failure preserves the exact prior
  route. Multiple choices before a prompt collapse to the latest one.
- The command creates no turn, message, tool, approval or immediate Session
  event. The next real model request records the selected route through the
  existing `request/header`: `initial` for the first request and `change` after
  an earlier request.
- A consumed model and explicitly selected effort survive Session resume. An
  adapter-materialized default is deliberately resolved afresh after restart,
  while a new startup `--model` override clears a prior explicit effort.
- The enhanced command palette now has fifteen commands. Active enhanced input
  consumes `/model` locally with a busy notice rather than queuing it for the
  model. Linear output remains free of escape bytes.

## Evidence

- Source-attributed fixture:
  `tests/fixtures/tools/upstream_phase52_session_model_selection.json`.
- Agent tests cover effective default resolution, no immediate event, an
  unlisted model, unsupported-effort preservation, last-choice wins, exact
  Provider dispatch, first `initial`, later `change`, and identical no-op.
- Assembly tests distinguish a stored explicit effort from an adapter default
  and prove that only the explicit selection survives resume.
- Parser, command-palette and Dock tests cover the closed grammar, catalogue
  count, navigation windows and bounded model id.
- One real enhanced PTY journey shows current selection, rejects invalid input,
  selects `deepseek-v4-pro/max`, and verifies the exact loopback HTTP wire and
  durable header. The same journal is then resumed in the linear UI, which
  reports and reuses `max` with zero ANSI bytes.
- A pre-existing two-request Web-search barrier fixture used a five-second
  socket-read timeout and twice reported `WouldBlock` only during a loaded full
  repository run while passing immediately alone. Its test-only bound is now
  fifteen seconds; the concurrency assertion and production timeout are
  unchanged. The final ordinary parallel all-target run passed.
- Local gates passed:

```console
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets -q
cargo clippy --all-targets -- -D warnings
git diff --check
```

The final all-target run passed 1,422 tests with no ignored tests. Focused Agent,
assembly, parser, palette, Dock and real-PTY tests also passed. No public
network, real DeepSeek credential, remote CI or extra stress matrix was used by
the validation commands.

Independent review was not separately delegated under the user-requested
local-minimal validation scope. The fixed/current source review, deterministic
fixture, compiler, Clippy, full repository suite, exact loopback wire, durable
resume and both terminal modes form the acceptance evidence.

## Known limits

- Rust exposes a text command for its one configured DeepSeek route, not the
  official dynamic multi-Provider Web popup or remote catalog refresh.
- Selection is idle-only. It cannot modify the route for a currently assembled
  step; the user waits for that turn or cancels it first.
- Rust does not save a deployment-wide default. A choice is Session-durable
  only after a later request header consumes it, so selecting and immediately
  exiting loses the unconsumed choice.
- Model ids containing whitespace cannot be selected through this command, and
  image-capability negotiation is absent because the terminal has no image
  prompt path.
