# Phase 30 bounded parallel-safe tool scheduling design

This design uses fixed DeepSeek Harness commit
`47f943859bef60e4160492346772ded9b24f765a`. Latest inspected master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` retains the same scheduler source;
its current standard preset still marks file `read` and the two Web tools as
concurrency-safe. Exact paths are recorded in `docs/upstream.md`.

## Problem and scope

When one model step requests several independent reads, dsh-rs currently waits
for each complete result before starting the next. Fixed upstream instead uses
a bounded rolling pool for calls whose tool definition explicitly declares
that overlapping bodies commute. Phase 30 adds that core behavior to reduce
latency without weakening mutation, approval, Session, or cancellation rules.

This phase does not parallelize `apply_patch`, Shell, plugins, Goal changes,
Plan exits, Todo writes, human questions, list/glob/grep, or unknown tools. It
does not add background jobs, product subagents, Skills, workflows, detached
tasks, or concurrent Agent turns. Those are separate or explicitly deferred
product capabilities.

## Upstream behavior and invariants

Fixed `packages/core/agent-loop/src/tool-calls.ts`,
`tests/tool-calls.spec.ts`, `packages/core/tools/src/index.ts`, and the file/Web
tool sources establish these observable rules:

1. only an exact positive concurrency classification permits overlap;
2. exclusive calls are barriers between parallel groups;
3. calls start in model order under a default cap of ten;
4. a settled call replenishes the pool even if an earlier sibling is pending;
5. `tool/result` and additional result context commit in model order despite
   out-of-order body settlement;
6. each `tool/call` commits before its body begins;
7. cancellation stops new starts, drains started calls, and records synthetic
   results for calls that never dispatched;
8. an internal scheduler failure stops new starts and drains in-flight work
   before the turn closes.

The fixed built-in opt-ins are `read`, `web_search`, and `web_fetch`. The fixed
filesystem search tools do not opt in, even though they look read-only, so Rust
keeps list/glob/grep exclusive rather than making an unsupported safety claim.

## Rust ownership and classification

`ToolExecutor` owns one prompt, pure, fail-closed `execution_mode(name)`
classifier. The default is `Exclusive`; a panic or any non-opt-in name remains
exclusive. `LocalToolRegistry`, `WorkspaceToolRegistry`, and
`ReadOnlyToolRegistry` opt in only the shipped names above that they actually
expose. A parallel classification is ignored for crate-controlled action or
human-interaction claim profiles, so an executor cannot accidentally make an
approval or state mutation overlap merely by returning the wrong mode.

Upstream can classify from softly parsed arguments and re-read a dynamically
replaceable registry before each start. Rust's registry/schema snapshot is
immutable for the Agent lifetime and its current built-in classifiers are
unconditional, so the Rust seam is name-based. This is an intentional API
difference with the same shipped-tool decisions. Future argument-dependent
tools must remain exclusive until the seam is deliberately extended and
tested.

`AgentLimits` owns `max_parallel_tool_calls`, default ten and bounded by the
existing maximum calls per step. The public builder allows deterministic
embedding/tests; the CLI uses the default. Rust has no dynamic Settings panel,
so the cap cannot change during a live group.

## Scheduling and event order

The Agent first reserves the complete step's assistant/call/result capacity as
it does today. After the assistant completion anchor and dispatch barrier:

```text
classify next call
  exclusive -> commit intent -> barrier -> run and settle alone
  parallel  -> fill rolling pool in model order up to cap
                 | each start: commit intent -> barrier -> poll body
                 | each settlement: store slot
                 | commit contiguous ready slots in model order
                 | refill one available slot from the same safe group
              -> drain group before the next exclusive barrier
```

Only the executor's safe preparation/body future overlaps. Session reservation,
dispatch barriers, result normalization, result-byte accounting, workspace
instruction touches, and stateful post-processing remain on the owning Agent
future and execute in model order. No parallel worker receives mutable Session
access, and no task is detached or spawned beyond the scheduler-owned futures.

Rust's ordinary safe tools currently combine validation and body work inside
`prepare`; unlike upstream's deeper middleware split, that whole post-intent
future may overlap. This is safe only because the explicit shipped whitelist
has no approval, mutation, or parent-state commit in that future. The result
commit remains serial and authoritative.

## Cancellation, timeout, and failure

The caller token and the shared turn deadline reach every started future. Once
cancellation or the deadline is observed, the scheduler starts nothing else,
notifies all started children, and drains them under each tool's existing
bounded cleanup rule. Remaining declared calls receive model-ordered synthetic
call/result pairs on the normal durable path, so replay stays valid. A slow or
non-cooperative sibling cannot outlive the existing one-second cleanup grace.

A normal tool error is a result and does not cancel siblings. An executor
infrastructure failure stops replenishment and drains everything already
started. Results safely committed before the failure remain facts. Durable
sessions use the existing unknown-outcome closure for a started call whose
result cannot be trusted, then close later started or skipped calls without
replaying them. In-memory test sessions preserve the existing unresolved-call
failure semantics rather than inventing success. Panic payloads and extension
details never enter the Session.

Per-tool timeouts remain independent. If the outer turn deadline wins, the
first observed turn stop remains authoritative even if another sibling later
settles. Output and event budgets are applied in model order, so settlement
timing cannot decide which result gets the remaining durable capacity.

## Side effects, replay, and security

Parallel-safe production tools perform only bounded file reads or anonymous/
DeepSeek-backed Web reads. Exact arguments are already in `tool/call` before
the read or network request. Full results stay correlated by call ID, and the
next model request is derived from the same model-ordered Session surface.
Recovery never re-executes an unresolved old call.

The whitelist is a safety contract, not an inference from names such as
"read-only". Adding another tool requires proving that its execution commutes,
uses no ambient mutable parent state, owns all resources, cooperates with
cancellation, and leaves all authoritative post-processing to the serial result
path. Parallelism is bounded; it is not a sandbox or rate-limit guarantee for
external services.

## Verification and intentional differences

Deterministic gated executors prove simultaneous starts, a configurable cap,
rolling refill, model-order result/context commits, exclusive barriers,
cancellation during a group, skipped-call pairs, infrastructure drain, and no
detached scheduler future. Limit tests freeze the upstream default of ten and
the Rust ceiling; the existing shared tool-run tests continue to cover argument,
ordinary error, timeout, panic, and cleanup behavior. Registry tests freeze the
exact shipped whitelist. A real CLI loopback test issues two independent
`web_search` calls whose server withholds both responses until both connections
arrive, then inspects the next request and durable order.

Rust intentionally differs by using an immutable name-based classifier, a
fixed-per-Agent cap rather than live Settings, its existing one-second
non-cooperative cleanup grace, and existing durable unknown-outcome repair.
These choices keep the local CLI bounded and auditable; they can delay or reject
custom argument-dependent parallel tools, but do not change the shipped
read/Web decisions. No generated cross-language scheduler oracle is claimed;
the compatibility row remains `partial` until one exists.
