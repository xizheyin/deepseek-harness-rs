# Phase 42 bounded background Shell jobs design

## 1. Problem and scope

The current `bash` tool holds the Agent turn until a command exits or reaches
its 295-second cap. That is awkward for builds, test suites and local servers
whose useful work can overlap later model steps. The fixed upstream exposes
`run_in_background` plus `job_list`, `job_output` and `job_kill` in its normal
tool composition. Phase 42 adds the same four model-facing names to the real
Rust CLI while retaining the existing Shell approval, workspace, environment,
output, timeout and process-group boundaries.

This phase supports only Bash processes started by the current process. It does
not add persisted/recoverable jobs, non-Shell producers, subagents, terminal
sessions, automatic completion wakeups or live incremental output reads.

## 2. Upstream basis

The semantic baseline remains fixed at
`47f943859bef60e4160492346772ded9b24f765a`. Exact inspected service, local
registry, Bash producer, controller and focused test paths are recorded in
`docs/upstream.md`. Freshly fetched master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` keeps the job schemas and lifecycle;
its relevant production diff only replaces numeric prompt-section order with a
named constant.

## 3. Inputs, outputs, state and order

`bash` gains optional boolean `run_in_background`. `false` or omission keeps
the existing foreground behavior. `true` still performs argument validation,
workspace resolution, Shell policy and human approval. After the final
workspace/process preflight, ownership moves atomically into one process-local
job runtime and the correlated result is `started background job bash-N`.

The controller tools are:

- `job_list {}`: list retained jobs in creation order;
- `job_output { job_id, wait?, timeout_ms? }`: return the bounded final output
  when available plus a terminal/running status; an optional wait observes up
  to its bound without cancelling the job;
- `job_kill { job_id, reason? }`: request cancellation and return immediately.

Jobs move monotonically through `running`, optional `stopping`, then exactly
one of `completed`, `killed` or `failed`. IDs are predictable `bash-N` values,
so the boundary is ownership by the one `LocalToolRegistry`, not secrecy.
Timestamps are Unix epoch milliseconds used only for model-facing snapshots.

The durable Session order remains:

```text
bash tool/call → approval decision → process/job ownership handoff → bash tool/result
job control tool/call → in-memory observation/cancellation → job control tool/result
```

Background settlement itself is process-local state and creates no hidden
Session event. A later `job_output` records the observed result normally.

## 4. Normal, failure, rejection, cancellation and timeout behavior

- Foreground Bash behavior is unchanged.
- A background start rejected by Shell policy or approval never creates a job.
- Cancellation before the job-runtime handoff prevents process creation.
  Cancellation after the successful handoff stops only the caller turn; the
  detached job is controlled by `job_kill` or registry shutdown.
- Start/preflight/spawn failure settles the registered job as `failed` and is
  visible through `job_output`.
- Natural exit, including nonzero exit, settles as `completed` with the exit
  fact. Explicit job cancellation settles as `killed`. Resource, I/O, timeout
  or ownership failures settle as `failed`.
- `job_output` without `wait` never blocks. A timed-out wait returns the latest
  running/stopping state and leaves work alive. Cancelling the wait cancels
  only that tool call.
- `job_kill` needs no extra approval because it can only reduce an already
  approved side effect. A finished job returns `already-finished`.
- Tool-runtime shutdown cancels every live job, waits for process-group cleanup
  and then shuts down the existing LSP/plugin processes.

## 5. Side effects

Only an approved `bash` start may create an external process and its existing
private spill files. Listing and reading are in-memory observations. Killing
sends the existing cooperative cancellation into the owned process runner,
which terminates and reaps the complete process group. Ordinary tool call,
approval and result facts remain append-only Session events.

## 6. Ownership and interfaces

`tools::jobs` owns IDs, lifecycle records, wait notification, finite admission
and shutdown joins. `tools::shell` continues to own argument validation,
approval preview, workspace revalidation, process request construction and
Shell result rendering. `LocalToolRegistry` owns one job runtime and routes the
three controller schemas/calls to it. The Agent action contract gains an
explicit detached-success outcome rather than falsely calling a live process
quiescent.

No state lock is held across an `.await`. Every spawned monitor handle is
retained and joined during shutdown.

## 7. Recovery, replay and compaction

Jobs intentionally do not survive process exit or `--resume`. The start ack and
later observations remain durable evidence, but a resumed CLI reports an old
job id as unknown and never restarts it. This preserves the existing rule that
an uncertain side effect is not replayed. Compaction may summarize recorded
tool facts but cannot reconstruct process-local ownership.

## 8. Security and resource limits

The existing exact Shell approval and no-sandbox warning remain. Background
mode is included in the exact-grant digest, so permission for one foreground
shape does not silently authorize a detached shape. Commands keep the 32 KiB
input, retained workspace, fixed `/bin/bash --noprofile --norc`, environment
allowlist, 8 MiB observed-output, private spill and 64 KiB model-result bounds.

At most eight jobs may be live and 64 records retained. Model-facing command
labels are reduced to one line of at most 240 UTF-8 bytes. Finished records are
evicted oldest-first only when admitting another job. Background commands keep
the same 1–295,000 ms command timeout; waits default to 30 seconds and are
capped at 295 seconds. Job ids and reasons are bounded and strict. The runtime
holds no API key or ambient secret.

## 9. Tests and comparison evidence

A fixed-source fixture records official names, states, status text and ownership
rules plus Rust limits/differences. Focused tests cover schemas, parsing,
approval-before-start, cancellation before/after handoff, natural/nonzero
completion, wait timeout/cancellation, kill/idempotence, admission, output
bounds, shutdown cleanup and foreground regression. One fake-provider CLI
journey starts a real background command, waits for its output and continues
with the correlated result.

Acceptance is local-only: focused tests, that real CLI journey, formatting,
check, Clippy, one serial all-target run and `git diff --check`. It uses no real
DeepSeek request, public network, remote CI or extra platform/stress matrix.

## 10. Intentional differences

Official jobs can expose incremental unread output, have no Bash timeout in
background mode, inject completion into a busy Agent or wake an idle Agent,
use owner-session fencing across multiple live Agents and support other job
producers. Rust Phase 42 returns idempotent final output only after settlement,
keeps its existing 295-second Shell safety cap, requires the model/user to call
`job_output`, and owns one process-local registry for the one CLI Agent. Rust
also caps live/retained jobs. These visible differences are tested and keep the
compatibility claim `partial`.
