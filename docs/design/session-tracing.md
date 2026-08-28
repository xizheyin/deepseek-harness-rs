# Phase 40 prior-Session relationship tracing design

## 1. Problem and scope

Phase 39 can find and read old events, but it cannot explain two important
relationships: which old Session is a parent or child of another Session, and
which event replaced or directly cited another event. Phase 40 adds the
official `session_trace` and `session_event_trace` tool names on the existing
strict prior-Session query boundary.

This phase does not create subagents, parent links or replacement events. It
only reports relationships already present in valid stored journals. It also
does not add current/live-Session reads, titles, metadata filters, cursors,
SQLite, an index, Session export or background work.

## 2. Upstream basis

The fixed baseline is
`47f943859bef60e4160492346772ded9b24f765a`. Exact inspected trace algorithms,
tool operations, presentation and tests are recorded in `docs/upstream.md`.
Latest master `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains the same trace
algorithms and model-visible output fields.

## 3. Inputs, outputs, state and order

`session_trace` accepts exactly one explicit canonical `session_id`.
`session_event_trace` accepts exactly `session_id` and a non-negative safe
integer `seq`. Requiring an id preserves Phase 36's old, closed Session
boundary; the official optional current-Session default is intentionally not
enabled.

Session tracing reports the target's creation time and persisted availability,
then known ancestors nearest first and a deterministic descendant tree.
Children are ordered by creation time ascending and id ascending, matching the
fixed upstream. If a parent is outside the visible, validated corpus, the
result says `[outside workspace boundary]` without disclosing that Session.
A target-connected parent cycle fails closed.

Event tracing reports the target's type, surface (`current`, `shadowed` or
`log-only`) and time, then:

- its immediate positional replacement and full replacement chain;
- the surface events directly removed when the target itself replaced a range;
- earlier events cited directly by `sourceEventSeqs`;
- later events that directly cite the target.

The strict scanner validates the complete log first. Replacement links are
derived from the surface that existed at each replacement, not guessed from
text or event type.

The existing Agent pipeline preserves the durable order:

```text
tool/call → strict old-journal observation → normalized tool/result
```

No new Session event, index or hidden persistent state is introduced.

## 4. Normal, failure, cancellation and timeout behavior

- Normal trace: return one untrusted-history notice and bounded relationship
  facts.
- Unknown, caller, busy/live or unauthorized target: return
  `SESSION_QUERY_SESSION_NOT_FOUND` without revealing why it is hidden.
- Missing sequence: return `SESSION_QUERY_EVENT_NOT_FOUND`.
- Bad id or sequence: fail before opening a journal.
- Corrupt, unsupported, changed, cyclic or oversized data: return a stable,
  sanitized `SESSION_QUERY_UNAVAILABLE`; never return a partial target trace.
- A lineage scan that reaches its aggregate observation cap remains explicit:
  the target must have been fully validated, and an unresolved parent is shown
  only as a boundary. Descendants outside the validated corpus are omitted.
- Caller cancellation or the five-second deadline signals the blocking scan,
  waits for it to stop, then returns cancellation or timeout.

Approval is not applicable because both operations only inspect already
authorized, normally closed history.

## 5. Side effects

The only durable side effects are the current Session's ordinary tool call and
result. The implementation temporarily obtains shared file locks and reads
local JSONL journals. It performs no write, repair, network request,
subprocess, approval or credential access.

## 6. Ownership and interfaces

`session::SessionSearchRuntime` owns store/workspace/caller authorization,
strict scans, lineage/event derivation, deadlines and returned facts.
`tools::session_search` owns schemas, closed argument parsing and model-facing
rendering. `LocalToolRegistry` only advertises and dispatches the tools. Agent
and TUI keep consuming ordinary tool events and gain no trace state machine.

## 7. Recovery, replay and compaction

Every observed journal is processed by the same strict cold projection used by
resume, but is neither repaired nor activated. The current Session records the
returned trace normally, so recovery and compaction do not rerun historical
queries. Relationship results are evidence from that one bounded observation,
not a durable foreign-session reference that grants later authority.

## 8. Security and resource limits

Authorization keeps the retained workspace device/inode identity, canonical
private journal names, owner/mode/link checks, nonblocking shared locks, strict
header identity and full replay validation. The caller and busy sources are
excluded. Each journal is capped at 16 MiB; the lineage corpus at 64 MiB and
the store's existing 128 canonical Session slots; each operation at five
seconds and the existing event limits. Event tracing retains only bounded
relationship sequence arrays already limited by the journal event cap.

Historical text is not rendered by either trace tool, but the response is
still marked untrusted. Errors never echo hidden ids, paths, provider
diagnostics, event content or credentials.

## 9. Tests and comparison evidence

A fixed-source fixture records the official names, inputs, relation order and
the reduced Rust boundary. Unit tests cover complete and unresolved ancestry,
deterministic nested descendants, cycles, all event surfaces, direct and
chained replacements, source and derived links, invalid/missing targets,
workspace/caller/busy/corrupt/oversized sources, cancellation and deadline.
Tool tests cover closed schemas, canonical ids, rendering, output bounds and
stable errors. The existing real two-process CLI journey is extended through
event trace and root Session trace before its final answer, proving all five
query tools are reachable without approval.

The final gate is local formatting, all-target check, one serial all-target
test run, Clippy with warnings denied and `git diff --check`. No real model,
remote CI, public-network product test or additional platform matrix is
required.

## 10. Intentional differences

Official dsh traces a live-preferred corpus, permits omitted ids for the
current Session, includes titles and live/persisted availability, and prunes
cross-workspace branches after tracing. Rust observes only strict, normally
closed journals from the exact retained workspace and therefore renders no
titles or live nodes. It requires explicit ids and treats missing ancestors as
an opaque boundary. Descendants not admitted to the bounded strict corpus are
omitted. These choices prevent a historical convenience tool from becoming a
live-state or cross-workspace channel and keep compatibility `partial`.
