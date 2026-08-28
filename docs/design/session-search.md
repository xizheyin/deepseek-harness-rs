# Persisted session search design

## Problem and scope

Long-running coding work is often split across several dsh sessions. Today the
model cannot discover that an earlier session already investigated the same
error or design choice. Phase 36 adds one read-only model tool,
`session_search { query }`, that searches safely reusable history from the same
opened workspace.

This phase does not add a general database, cross-workspace memory, current/live
session search, exact event reads, lineage, titles, background indexing, or any
write/repair path. The other four official session-query tools remain follow-up
work.

## Upstream basis

The behavioral baseline is DeepSeek Harness commit
`47f943859bef60e4160492346772ded9b24f765a`. The inspected sources are:

- `packages/session-query/session-query/src/{extraction,filters,documents,types}.ts`;
- `packages/session-query/session-query-sqlite/src/{index,query}.ts` and its tests;
- `packages/session-query/tool-session-query/src/{index,input,operations,presentation,workspace-access}.ts`;
- `packages/session-query/tool-session-query/tests/{tool-session-query,sqlite-integration}.spec.ts`.

Latest inspected master `cd5ef8148158c3a752a658978873241fdf8e2bbc`
retains the five tools and the same workspace-authorized, cursor-free model
boundary. Its larger changes add observation/export infrastructure and
documentation, not a smaller replacement contract.

## Input, output, and event order

The closed input object contains one required string `query`. It is trimmed,
internal whitespace is treated flexibly, NUL is rejected, and the UTF-8 input
is capped at 1,024 bytes. The model cannot choose a store path, workspace,
session id, cursor, page size, timeout, or result count.

For each accepted model call, the ordinary Agent pipeline records `tool/call`
before starting the search. A success returns either
`No prior session matches found.` or at most 20 entries containing session id,
creation time, the strongest matching event's sequence/type/time, and a
whitespace-normalized excerpt capped at 240 Unicode code points. The result is
labelled as untrusted historical data. The ordinary `tool/result` then records
that bounded text.

## State ownership and authorization

`SessionSearchRuntime` owns a clone of `SessionStore`, the opened workspace's
device/inode identity, and the caller session id. `LocalToolRegistry` owns that
runtime and exposes the schema only when assembly supplies it. Model arguments
never participate in authorization.

The store lists at most its existing bounded session slots filtered by exact
workspace identity. Each candidate is reopened without following symlinks,
validated as an owner-only regular one-link journal, and admitted only after a
nonblocking shared lock succeeds. The caller id is always excluded as a second
independent check. Thus a live writer, including the caller, is never inspected.

## Search and resource bounds

Search runs on one owned `spawn_blocking` worker so file reads and regex scans do
not block Tokio. It examines newest sessions first, reads no session larger than
16 MiB, reads at most 64 MiB in total, accepts no more than the store's 128
canonical slots, and stops after five seconds. Cancellation and the deadline
are checked between directory entries and journal lines. No background task or
persistent index survives the call.

Only complete current-version journals whose strict projection is quiescent are
eligible; an `end-seed` is accepted when present but is not required for a
fresh session that closed normally. Sequence gaps, malformed envelopes, incomplete lines, unsupported
required events, workspace/header mismatches, or resource-limit failures skip
that candidate without exposing its existence. Store-wide authority or I/O
failure fails the tool with one stable model-safe error.

Searchable semantic text follows the fixed upstream extraction policy: user
and final assistant text, tool names/arguments/results/errors, todo status/text,
and non-success turn endings. Raw assistant chunks, reasoning, request
envelopes, and unknown events are not indexed. Matching is a literal,
Unicode-aware, case-insensitive phrase whose whitespace runs may differ.

Within one session, the event with the most phrase occurrences wins; ties use
the later event. Sessions are ordered by occurrence score, then matching-event
time, creation time, and id. This is deterministic but is not SQLite BM25.

## Failure, cancellation, and recovery

- Invalid arguments become a correlated `SESSION_SEARCH_INVALID` tool error.
- A pre-cancelled or mid-scan call becomes the normal correlated aborted result.
- A deadline becomes `SESSION_SEARCH_TIMEOUT`.
- Unsafe store state or an uncontained operational failure becomes
  `SESSION_SEARCH_UNAVAILABLE` without paths or OS diagnostics.
- A bad individual historical journal is skipped; it is never repaired.
- Search has no side effect, approval, subprocess, network request, or replay.
- Resume constructs a fresh runtime from the same session id and workspace
  identity, so the current resumed journal remains excluded.

## Security analysis

Historical text can contain old external content or prompt injection. The
system guidance and every result mark it as untrusted history, never authority.
Search does not expose cross-workspace ids, filesystem paths, provider cursors,
or raw session JSON. It uses the existing private store and capability-derived
workspace identity rather than trusting a path supplied by the model.

## Tests and compatibility status

The source-attributed fixture fixes the reduced schema, literal matching,
excerpt size, result cap, and output envelope. Pure tests cover semantic
extraction, case/whitespace/literal behavior, ranking and excerpt boundaries.
Store tests cover workspace filtering, caller/busy exclusion, corrupt and
oversized journals, cancellation and timeout. A two-process CLI test creates a
real first session, lets a second session call `session_search`, and proves the
result reaches the next model request without approval.

The compatibility row remains `partial`: the core cross-session discovery flow
exists, but Rust ships one of five tools, scans closed JSONL directly instead
of SQLite/live corpus, exposes no filters or titles, and uses a documented
ranking difference.
