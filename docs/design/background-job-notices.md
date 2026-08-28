# Phase 43 background-job completion notices design

## 1. Problem and scope

Phase 42 made long Bash commands non-blocking, but the model must remember to
poll every job. The fixed upstream instead delivers one ordinary user-shaped
plugin notice when an unreported job settles: a busy Agent receives it in the
next step, while an idle Agent may open a new turn. Phase 43 adds that behavior
to the one-Agent Rust CLI.

This phase does not add live output cursors, persisted jobs, non-Shell job
producers, multiple Agents or a general event/hook system.

## 2. Upstream basis

The semantic baseline remains fixed at
`47f943859bef60e4160492346772ded9b24f765a`. The relevant fixed-commit
`tool-jobs` source and tests are recorded in `docs/upstream.md`. Freshly fetched
master `cd5ef8148158c3a752a658978873241fdf8e2bbc` changes prompt-section order
only; completion delivery is unchanged.

## 3. Inputs, outputs, state and order

Each natural or failed job settlement creates at most one bounded notice:

```text
background job bash-N (bash: <label>) finished [status: <status>, <detail>]. Read its output with job_output.
```

It is a normal `role=user` message with source
`{kind:"plugin", plugin:"tool-jobs", form:"notice", summary:<bounded>}`.
The notice therefore becomes append-only Session evidence only when an Agent
step claims it; the job registry never writes Session state directly.

One shared bounded inbox owns pending notices, whether an Agent turn is active,
and the consecutive idle-wake count. A busy turn drains notices before each
request. Immediately after a model-completed step it atomically either claims
new notices for another step or marks the Agent idle, preventing a lost
completion between those two decisions.

An idle interactive CLI opens a notice turn while fewer than three consecutive
completion turns have been opened. Claiming direct human input resets that
budget. Goal input and plugin notices do not. After the budget is spent,
notices remain queued and enter the next ordinary human or Goal turn.

## 4. Failure, cancellation and suppression

- `job_output` that observes a terminal state marks the job reported and
  removes a notice that has not yet entered a step.
- A terminal `job_output(wait=true)` suppresses the completion notice; a
  cancelled or timed-out wait that sees a live job does not.
- `job_kill` marks the selected job reported before requesting cancellation,
  including the already-terminal case.
- Tool-runtime shutdown marks all jobs reported, clears the inbox, wakes its
  waiter and then cancels/joins processes. It never opens a teardown turn.
- Agent cancellation, error or token/step limit closes the current turn
  truthfully. A concurrently queued notice remains eligible for a later idle
  wake rather than changing that stop reason.
- Message construction or Session admission failure does not silently discard
  a claimed notice.

## 5. Side effects and ownership

The job runtime owns settlement and report bookkeeping. The bounded inbox owns
delivery state only. `AgentLoop` alone converts a notice into a model-visible
message and records it through the normal step reservation. The interactive
terminal only waits for an idle-wake signal and runs the same Agent/UI path as
another turn. No new approval, file, process or network authority is added.

## 6. Recovery and replay

Pending in-memory notices and live jobs disappear on process exit. A notice
already claimed by a step is an ordinary durable message and replays normally.
Resume never recreates or wakes an old job. This is the same intentional
process-local boundary as Phase 42.

## 7. Resource and safety limits

The inbox retains at most 64 concrete notices. Overflow is represented by one
bounded aggregate notice instead of unbounded allocation or silent success.
Labels and status details already use the Phase 42 bounds; notice text and
source summary are bounded again before message construction. At most three
automatic turns may be opened without new direct human input. No lock is held
across `.await`, Provider work or Session I/O.

## 8. Tests and compatibility

A fixed-source fixture records exact notice/source text, busy injection, idle
wake, budget reset/degradation and report/teardown suppression. Focused tests
cover inbox races and bounds, Agent step order, human-only reset, terminal
read/kill/shutdown suppression, and one real linear-terminal idle wake journey.

Acceptance is local-only: focused tests, one all-target run, formatting,
compiler, Clippy and `git diff --check`. No real DeepSeek call, public network,
remote CI or extra platform matrix is required.

## 9. Intentional differences

Rust has one Agent and one process-local Bash registry, so it needs no
same-session owner replacement routing. Its 64-notice queue and overflow
summary are stricter than the upstream in-memory listener. The idle-delivery
mode and wake limit are fixed to upstream defaults instead of configurable.
These limits remain visible in the compatibility table, which stays `partial`.
