# Phase 12 validation

Status: `complete`

Phase 12 implements the bounded same-process Goal slice frozen in
`docs/design/goal-automation.md`. The fixed semantic baseline is DeepSeek
Harness commit `47f943859bef60e4160492346772ded9b24f765a`; latest `master` was
inspected separately without changing that baseline.

## User-selected gate

On 2026-08-28 the user requested local-only necessary verification and fewer
repeated checks. Completion therefore requires focused Goal tests, one installed
or real-binary interactive journey, cancellation/cap coverage, formatting,
all-target compilation, Clippy with warnings denied, and `git diff --check`.
Remote CI, emulator capture, and another full repository-wide test pass are not
blocking this checkpoint.

## Implemented production path

- `src/goal.rs` owns one bounded process-local revisioned Goal, exact command
  parsing, state transitions, the 32-round cap, distinct-round blocking
  discipline, and generated `<goal_round>` prompts.
- Enhanced and linear interactive drivers accept show/create/edit/pause/resume/
  clear, prioritize queued human input, run one automatic round at a time, and
  pause/disarm after cancellation or failure.
- Interactive tool assembly exposes closed `get_goal`, `create_goal`, and
  `update_goal` schemas through the ordinary Agent tool pipeline. Script mode
  does not expose process-local Goal tools.
- Goal-round user messages carry a distinct recorded `{kind:"goal",round:N}`
  source. Goal commands themselves remain local and are not sent to the model.
- README, command palette, help, upstream record, compatibility table, and
  Phase 11's user-approved completion boundary reflect the shipped behavior.

## Local verification — 2026-08-28

Rust 1.85.0 on Darwin arm64 passed the deliberately reduced local gate:

```console
cargo fmt --all -- --check
cargo test --lib
cargo test --test interactive_cli goal_command_runs_sequential_rounds_until_the_model_completes_it -- --nocapture
cargo test --test interactive_cli cancelling_a_goal_round_pauses_automatic_continuation -- --nocapture
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
git diff --check
```

The library run passed 785 tests with zero failed or ignored. The real-binary
enhanced PTY auto-completion journey made exactly three Provider requests: Goal
round 1, Goal round 2 with `update_goal complete`, and the correlated post-tool
response; no fourth automatic round occurred. The cancellation journey closed
the stalled request, returned to idle, and showed the Goal as paused. All model
traffic used bounded loopback fixtures and a fake key.

The full repository/integration suite, remote CI, cross-platform rerun, and
terminal-emulator capture were intentionally not repeated under the user's
local-only fast gate. The unchanged Phase 11 candidate already had a green
macOS/Ubuntu matrix; this Phase 12 claim is local macOS only.

## Known gap

This checkpoint does not claim complete official Goal compatibility. Official
Goal changes are durable Session events and the latest upstream also supports
image attachments. Rust loses Goal state on process exit/resume, accepts text
only, and uses a 32-round cap rather than 256. These differences are visible in
README and remain `partial` in `docs/compatibility.md`.
