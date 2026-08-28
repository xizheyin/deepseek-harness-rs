# Titles in prior-Session search

## Scope

Phase 46 makes `session_search` results recognizable by displaying the latest
durable title already admitted by Phase 45. It does not change ranking,
authorization, filters, event extraction or the 20-result cap. Title enrichment
for event operations and lineage traces remains out of scope.

## Upstream basis

The fixed baseline is
`47f943859bef60e4160492346772ded9b24f765a`. Relevant behavior comes from
`packages/session-query/session-query/src/index.ts`, the title observation tests
in `session-query/tests/session-query.spec.ts`, and
`tool-session-query/src/{workspace-access,presentation,operations}.ts` plus its
title/cancellation tests. Latest master `cd5ef8148158c3a752a658978873241fdf8e2bbc`
retains title-enriched search presentation.

## Data flow and ownership

`SessionStore::list_metadata` already obtains an optional title only from a
closed, shared-locked, strictly valid journal no larger than 16 MiB. The same
metadata object is authorized and opened as a search candidate. When that
candidate produces a best match, its title is copied into `SessionSearchHit`.
The renderer prints `Session <id> — <title>` or `— untitled`.

No second trust boundary is introduced: title text was normalized before its
event entered Session, the cold scanner revalidates the whole journal, and the
ordinary bounded tool result remains model-visible untrusted history.

## Failure, cancellation and resources

Title absence or title-only inspection failure never removes a search hit.
Busy/malformed/oversized candidates follow the existing search behavior; a
candidate accepted for base search but lacking readable title displays
`untitled`. Cancellation and deadlines remain owned by the existing candidate
scan. At most 20 titles of 80 bytes enter the final result, so the existing
64-KiB result bound still has ample room.

## Compatibility and differences

Official tooling batches title observations and can annotate an individual
title backend failure with a sanitized code while preserving the base result.
Rust reuses the already bounded local metadata observation and cannot currently
distinguish “no title” from “title unavailable”; both render as `untitled`.
Focused tests pin title/untitled rendering, ranking stability and maximum-result
size. The compatibility status remains `partial`.
