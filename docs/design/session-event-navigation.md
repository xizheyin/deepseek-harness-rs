# Phase 39 prior-Session event navigation design

## 1. Problem and scope

`session_search` currently returns only one 240-character lead per old Session.
The model cannot search deeper inside that Session or read the exact cited
event. Phase 39 adds `session_event_search` and `session_event_read` so a lead
can be verified without resuming or modifying historical state.

This phase does not add lineage traces, live/current-Session reads, metadata
filters, cursor pagination, titles, SQLite, an index, Session export or
background work.

## 2. Upstream basis

The fixed baseline is
`47f943859bef60e4160492346772ded9b24f765a`. Exact inspected service, tool,
SQLite and test paths are recorded in `docs/upstream.md`. Latest master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` retains both observable tool
contracts.

## 3. Inputs, outputs, state and order

`session_event_search` accepts exactly `session_id` and `query`.
`session_event_read` accepts exactly `session_id`, `seq`, and optional `before`
and `after`. The id is the canonical local `session-<UUIDv4>` spelling, query is
the existing 1–1,024-byte literal phrase, sequence is a non-negative safe
integer, and each side window is 0–50.

Search returns at most 20 events ranked by occurrence count descending,
semantic-document length ascending, time descending and sequence descending.
Each row includes sequence, type, `current|shadowed|log-only`, UTC time and a
240-code-point snippet. Read returns the target's complete validated event
envelope as pretty JSON and only bounded semantic summaries for neighbors.

The existing Agent pipeline preserves the durable order:

```text
tool/call → strict old-journal observation → normalized tool/result
```

No new Session event type or hidden mutable index is introduced.

## 4. Normal, failure, cancellation and timeout behavior

- Normal search/read: return one untrusted-history notice and the bounded data.
- Unknown, caller, busy/live or unauthorized id: one indistinguishable
  `SESSION_QUERY_SESSION_NOT_FOUND`; no existence detail leaks.
- Missing sequence: `SESSION_QUERY_EVENT_NOT_FOUND`.
- Bad id/query/sequence/window: fail before opening a journal.
- Corrupt, unsupported, changed or oversized source: stable sanitized
  `SESSION_QUERY_UNAVAILABLE`; no partial results.
- Output larger than 64 KiB: `SESSION_QUERY_OUTPUT_TOO_LARGE`; the exact event
  is never silently truncated.
- Caller cancellation or five-second deadline: signal the blocking scan, wait
  for it to stop, then return cancellation or timeout. Historical state is
  unchanged.

Rejection and approval are not applicable because both operations are
read-only and confined to already authorized history.

## 5. Side effects

The only durable side effects are the current Session's ordinary tool call and
result. The implementation obtains a temporary shared file lock and reads one
local JSONL journal. It performs no write, repair, network request, subprocess,
approval or credential access.

## 6. Ownership and interfaces

`session::SessionSearchRuntime` continues to own store/workspace/caller
authorization, blocking scans, deadlines and result facts. `tools::session_search`
owns the two schemas, argument validation and model-facing rendering.
`LocalToolRegistry` only advertises and dispatches the tools. Agent and TUI
consume ordinary tool facts and do not gain another state machine.

## 7. Recovery, replay and compaction

The target journal is strictly cold-scanned through the same projection used by
resume, but never repaired or made live. The current Session records the tool
call and returned text normally, so its own recovery and compaction semantics
are unchanged. A later replay never reruns the historical read.

## 8. Security and resource limits

Authorization uses retained workspace device/inode identity, canonical private
journal names, owner/mode/link checks, a nonblocking shared lock, strict header
identity and full replay validation. Caller and live/busy sources are excluded.
One scan is capped at 16 MiB, five seconds and existing journal/event limits.
Search retains only 21 best candidates. Read retains one target plus at most
100 bounded summaries. Complete output must fit the existing 64 KiB tool cap.

Historical text is explicitly marked untrusted. Tool errors do not echo paths,
source content, provider diagnostics, credentials or hidden ids.

## 9. Tests and comparison evidence

A fixed-source fixture records official schemas, rendering, ranking and the
reduced Rust boundary. Unit tests cover closed schemas, canonical ids, literal
query matching, ranking, all three surfaces, exact JSON, before/after limits,
output rejection, not-found/caller/busy/cross-workspace/corrupt/oversized
sources, cancellation and timeout. Registry tests prove both schemas are
reachable and read-only. A real two-process loopback CLI journey creates old
history, searches inside its event rows, reads the returned sequence and
continues without approval.

The final gate is local formatting, all-target check, one serial all-target test
run, Clippy with warnings denied and `git diff --check`. No real model or remote
CI is required.

## 10. Intentional differences

Official dsh uses a live-preferred corpus, permits omitted id for the current
Session, hides the active step, supports filters/cursors/titles, and normally
uses SQLite FTS5. Rust requires one explicit closed prior-Session id and scans
its strict JSONL directly. Rust caps results at 20 instead of 100, exposes no
filters, and rejects an oversized exact rendering. These choices preserve the
existing auditable workspace boundary and keep compatibility `partial`.
