# Phase 43 local validation — 2026-08-29

## Result

Phase 43 is complete under the requested local-only, necessary-check gate.
Background Bash completions now notify the same Agent without polling: an
active turn continues with a model-visible next-step notice, while an idle
interactive terminal may open a bounded automatic turn.

The notice is not a hidden shortcut. It is a normal user-role message with
`plugin=tool-jobs` and `form=notice`, claimed and written through the existing
step reservation before the next Provider request. File, Shell, approval,
Session and cancellation authority are unchanged.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied exact small notice text
  and source, busy injection, idle wake, three-turn budget, human-only reset
  and terminal-read/kill/teardown suppression. Latest fetched master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains those semantics.
- `tests/fixtures/tools/upstream_phase43_background_job_notices.json` fixes the
  official observable contract and Rust's finite boundary.
- Inbox tests prove exact content/source, 64-entry concrete capacity plus one
  overflow fact, human-only wake reset, atomic completed-step state and close
  or job-specific suppression.
- Agent tests prove an already pending notice joins ordinary input, an idle
  claim opens a later turn, and a completion arriving with the final chunk of
  a busy step causes a second step in the same turn.
- Job-runtime tests prove terminal non-wait output, terminal wait and explicit
  kill remove pending delivery. Existing shutdown tests plus inbox close prove
  teardown does not wake a dying Agent.
- A real linear PTY journey starts an approved delayed background Bash command,
  observes the first turn become idle, then observes a second automatic model
  turn whose request contains the durable `tool-jobs` notice.

## Local commands

```console
cargo test 'agent::job_notice::tests' --lib -- --nocapture
cargo test completion_during_a_busy_turn_is_claimed_by_its_next_step --lib -- --nocapture
cargo test background_job_notices_enter_as_plugin_input_and_can_open_an_idle_turn --lib -- --nocapture
cargo test terminal_output_kill_and_wait_suppress_completion_notices --lib -- --nocapture
cargo test idle_background_completion_opens_one_notice_turn_in_the_real_terminal --test interactive_cli -- --exact --nocapture
cargo test local_registry_exposes_background_bash_and_closed_job_schemas --test shell_tools -- --exact --nocapture
cargo fmt --all -- --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets --quiet -- --test-threads=1
git diff --check
```

All commands passed on local macOS arm64 with Rust 1.85.0 and locked
dependencies. The final serial run passed 943 core-library tests, 124 real
interactive-terminal tests, 20 Shell integration tests and every other binary,
example and integration target. One first full run exposed only the obsolete
system-prompt assertion that automatic notices were absent; updating that
assertion to the new shipped contract made the complete rerun green.

No real DeepSeek request, API key, charge, public-network product test, remote
CI, extra operating-system matrix or stress run was used.

## Review

The local review covered notice-before-request order, exact source identity,
the busy-complete/idle-wake race, wake-loop bounds, direct-human reset,
terminal-read/kill/teardown suppression, queue overflow, cancellation closure,
Session replay facts and Phase 42 foreground/background regression. No
subagent was used because delegation was not authorized for this continuation.

## Known limitations

- Only Bash jobs owned by the current CLI process can notify this one Agent.
- Jobs, pending notices and wake counters are not restored after process exit.
- Running output is still not incremental. The notice reports terminal status;
  `job_output` remains the source of final command output.
- Delivery and the three-turn budget use fixed upstream defaults rather than
  user configuration.
- Rust caps pending delivery at 64 concrete notices and one overflow fact.
- No generated TypeScript producer proves broad cross-language compatibility,
  so the compatibility row remains `partial`, not `compatible`.
