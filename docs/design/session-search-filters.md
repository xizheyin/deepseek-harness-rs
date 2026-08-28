# Phase 41 bounded Session search filters design

## 1. Problem and scope

The five historical Session tools are reachable, but both search tools still
accept only a text query. A model that knows an approximate time, event type,
sequence range, parent Session or surface cannot narrow the scan. Phase 41 adds
the fixed upstream's public filter fields to `session_search` and
`session_event_search` while preserving Rust's strict, normally closed
same-workspace corpus.

This phase does not add current/live-Session reads, titles, cursor pagination,
SQLite, a derived index, Session export, subagents or background jobs. Exact
read and both trace schemas remain unchanged.

## 2. Upstream basis

The fixed baseline is
`47f943859bef60e4160492346772ded9b24f765a`. Exact inspected input, operation,
provider-independent predicate, SQLite-ordering and focused test paths are
recorded in `docs/upstream.md`. Latest master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` preserves the filter schemas and
semantics; its direct tool-input diff is empty.

## 3. Inputs, outputs, state and order

`session_search` keeps required `query` and adds optional:

- `session_ids`, `created_at_from`, `created_at_to`;
- `parent_session_ids`, `include_root_sessions`, `availability`;
- `event_seq_from`, `event_seq_to`, `event_time_from`, `event_time_to`;
- `event_types`, `event_surfaces`.

`session_event_search` keeps required `session_id` and `query`, and adds
optional `seq_from`, `seq_to`, `time_from`, `time_to`, `event_types` and
`surfaces`.

Different filter fields are ANDed. Values inside one array are ORed. Ranges are
inclusive. Text remains literal, Unicode case-insensitive and whitespace-
flexible. Filter predicates run before the existing relevance order:
occurrence count descending, semantic-document length ascending, event time
descending and sequence descending. Cross-Session search still returns only
the strongest accepted event from each accepted Session.

Timezone-qualified ISO 8601 bounds accept `Z` or a numeric offset and compare
at the exact written fractional precision before projection onto the integer
millisecond Session clock. A lower bound inside a millisecond advances to the
next integer millisecond; an upper bound remains at the containing integer
millisecond. An exactly ordered sub-millisecond interval containing no integer
timestamp is a valid empty filter, not an invalid range.

The existing durable order is unchanged:

```text
tool/call → bounded strict observation/filter/rank → tool/result
```

Filters are immutable values owned by one call and create no Session event or
persistent index.

## 4. Normal, failure, cancellation and timeout behavior

- A valid filter set returns only matching results, with existing cap notices.
- Empty results are ordinary success and reveal no hidden Session existence.
- Reversed sequence or exact timestamp ranges, empty arrays, invalid enums,
  malformed timestamps, non-canonical ids, unknown fields and resource-limit
  violations fail before journal reads with `SESSION_QUERY_INVALID_FILTER` or
  the existing invalid-query result.
- Selecting only `live` availability returns an empty result because Rust's
  authorized corpus contains persisted, normally closed sources only.
- Requested parent ids are usable only when the corresponding parent journal
  is itself fully validated in the same bounded corpus. Unauthorized ids are
  silently removed; roots remain independently selectable.
- Corrupt, unsupported, busy, other-workspace or oversized sources keep the
  existing hidden/skip behavior. No partial target journal is returned.
- Cancellation and the five-second deadline stop and join the blocking scan as
  before.

Approval and rejection are not applicable: filters add no write or external
authority.

## 5. Side effects

Only the current Session's ordinary tool call and result are durable. The
runtime temporarily shared-locks and reads existing private JSONL journals. It
does not write, repair, start a process, access the network, prompt for
approval or read credentials.

## 6. Ownership and interfaces

`session::search` owns immutable typed filter values and applies them against
strict header/event observations. `tools::session_search` owns JSON schemas,
closed parsing, exact timestamp normalization and stable model errors. The
registry and Agent continue to see the same two tool names and ordinary result
pipeline.

## 7. Recovery, replay and compaction

Filtering reuses the strict cold scanner and never changes a historical
journal. The returned text is recorded in the caller normally, so resume and
compaction replay the result rather than rerunning the search. A filter carries
no authority into a later call.

## 8. Security and resource limits

Existing workspace identity, owner/mode/link, canonical filename, nonblocking
lock, strict replay, 16 MiB per journal, 64 MiB aggregate, 20-result and
five-second limits remain. Arrays are additionally capped: 128 Session or
parent ids, 64 event types, three surfaces and two availability values. Event
type strings are at most 128 UTF-8 bytes and timestamp strings at most 64.
Candidate matches inside one journal are bounded by the existing maximum
logical event count.

Parent filtering never treats a guessed id as authorized merely because it
appears in a child's header. Historical content remains labelled untrusted;
errors do not echo hidden ids, paths or event text.

## 9. Tests and comparison evidence

A fixed-source fixture records every official field, AND/OR/range semantics,
rank order and Rust caps. Parser tests cover closed schemas, empty/oversized
arrays, enum/id/type validation, offsets, leap dates, sub-millisecond and
reversed bounds. Core tests cover session id/time/parent/root/availability and
event sequence/time/type/surface combinations, filtering-before-ranking,
authorized parent ids, corrupt/busy sources, cancellation and timeout. A real
two-process CLI journey calls both search tools with filters and verifies the
filtered result in the next Provider request without approval.

Acceptance uses focused tests plus one local all-target run, formatting, check,
Clippy and `git diff --check`. No real DeepSeek request, remote CI, public
network or extra platform/stress matrix is required.

## 10. Intentional differences

Official dsh searches a live-preferred corpus, accepts arbitrary branded
Session id strings, can select the current Session and uses cursor-backed
provider pages. Rust accepts only canonical local UUIDv4 Session ids, requires
an explicit target for event search, and exposes only normally closed persisted
sources. `availability:["live"]` is therefore valid but empty. Rust caps every
array and scans strict JSONL directly. It omits `cwd` as a public filter because
all visible candidates are already confined to the one retained workspace
identity. These differences are visible, tested and keep compatibility
`partial`.
