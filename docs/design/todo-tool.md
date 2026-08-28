# Durable model Todo list

## Scope and upstream basis

Phase 24 makes the existing `todo/write` Session event reachable through the
fixed upstream model tool. At commit
`47f943859bef60e4160492346772ded9b24f765a`, the contract is defined by
`packages/todo/tool-todo/src/{index,invariant,types}.ts`, its README and tests,
the core Session Todo types/invariant/tests, the code preset, and the Web
TodoPanel/TodoRow. Latest inspected master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` changes package ownership and
projection APIs but not the observable tool semantics.

This phase does not add priorities, stable item IDs, nested tasks, partial
updates, a read tool, Web UI, subagents, or background work. It does not turn a
Todo list into a Goal: Todo is model-maintained progress within ordinary turns;
Goal controls automatic future turns.

## Input, output, and event order

`todo_write` accepts exactly `{ todos: [{ content, status }] }`. Every call
contains the complete replacement list. Status is exactly `pending`,
`in_progress`, or `completed`. An empty list explicitly clears the standing
list. Content is trimmed before duplicate checking and storage.

The Rust tool adds resource bounds missing from upstream: at most 64 entries,
at most 512 UTF-8 bytes per stored line, and no control characters. It rejects
unknown root/item fields, non-arrays, missing/wrong fields, blank content,
duplicates after trimming, invalid status, and more than one `in_progress`
item. The single-active choice uses upstream's supported
`allowParallelInProgress: false` mode because dsh-rs currently owns no parallel
product workers.

On success, the Agent commits facts in this order:

1. the existing durable `tool/call` intent;
2. one log-only `todo/write` whole-list snapshot;
3. the correlated successful `tool/result` with exactly
   `Updated todo list: <pending> pending, <active> in progress, <completed> completed.`

Validation failure creates only the ordinary failed correlated result; no
`todo/write` lands. Cancellation before settlement cannot create a Todo
snapshot. Todo has no filesystem, process, network, or approval side effect.

## Ownership, replay, and terminal lifetime

The Session projection owns the durable current value. It validates the
upstream historical invariant (array entries are nonblank, already trimmed,
unique, and carry a known typed status), takes the most recent `todo/write`,
and clears only the standing presentation value on the next `turn/start`.
History remains append-only and replay produces the same value. Tool-specific
64/512 and single-active limits are not applied to older logs because upstream
explicitly keeps deployment policy out of the durable invariant.

The Agent is the only writer: the registry prepares canonical items and a
result, while the Agent appends `todo/write` before settling `tool/result`.
Direct executor calls fail closed rather than reporting success without a
Session owner.

The observer carries the bounded Todo snapshot to the terminal. Enhanced mode
shows a collapsed one-line standing summary in the Dock (counts plus the active
item) until a later `turn/start`; ordinary transient notices temporarily take
priority. Linear mode prints the complete bounded list when it changes. Resume
seeds the standing value from the same Session projection. A specialized final
tool card summarizes progress but never infers that work actually completed
beyond the statuses the model wrote.

## Failure, cancellation, and safety

Argument errors are model-visible tool failures and do not mutate Session
state. Allocation, Session append, projection, or result-settlement failure is
an Agent infrastructure failure and must close the step/turn truthfully. If a
prepared snapshot cannot be committed, its tool result cannot claim success.
There is no independent timeout because preparation is bounded CPU work; it
remains under the Agent's existing turn cancellation and timeout checks.

Todo content is untrusted model text. Terminal rendering uses the existing
visible-control sanitizer, fixed list/count limits, and no escape interpretation.
No Todo value grants approval or changes file/Shell/plugin policy.

## Verification and intentional differences

Deterministic tests cover closed schema/arguments, trimming, duplicate and
status rejection, item/content/single-active limits, empty clear, canonical
counts, durable event validation, last-write replay, next-turn clearing,
cancellation before commit, terminal restoration/replacement, and exact
call/write/result order. Real enhanced and zero-ESC linear PTY journeys cover
model invocation, the standing summary, full linear rendering, the exact
Provider-visible result, and durable JSONL ordering.

The 64-item and 512-byte line caps are Rust resource limits. Single-active is
an upstream-supported deployment setting but differs from the official code
preset's parallel choice. The Rust Dock provides no Web collapse control; it
shows the compact default summary and leaves the exact durable list visible in
linear output and Session facts. Without a generated cross-language oracle the
compatibility row remains `partial`.
