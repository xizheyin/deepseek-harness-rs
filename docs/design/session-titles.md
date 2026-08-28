# Durable first-prompt Session titles

## Problem and scope

Session ids and timestamps are hard to recognize in a resume list. Phase 45
adds one short title derived from the first direct human prompt. It includes an
immediate deterministic fallback, one optional DeepSeek refinement, durable
replay and display in local Session listing. Manual rename, refresh, fork
inheritance, SQLite indexing and cross-process title workers are out of scope.

## Upstream basis

The semantic baseline is fixed at
`47f943859bef60e4160492346772ded9b24f765a`. The implementation follows:

- `packages/session/session-title/src/{index,normalize,types}.ts` and tests;
- `packages/session/session-title-llm/src/index.ts` and tests;
- `packages/session/session-title-first-prompt-llm/src/index.ts` and tests;
- `packages/bundle/base/cordis.patch.yml` for the shipped bounds.

Fresh `origin/master` at `cd5ef8148158c3a752a658978873241fdf8e2bbc`
keeps the same title semantics; its relevant change is projection-cache
plumbing rather than a user-visible title rule.

## State and event order

The first non-empty text message whose source is exactly `user` is eligible.
The Agent records `session/title` with `source.kind=fallback` and that message's
sequence. Before the auxiliary provider side effect it records the exact
`session/title-llm-request`, including route, system text, framed message and
64-token ceiling. A successful plain-text `stop` response appends a second
`session/title` with provider attribution. Replay uses the latest title.

The two title event types are log-only: they never enter the conversation
surface or a later model request.

## Bounds and normalization

Fallback uses at most five whitespace-delimited words and 40 UTF-8 bytes.
Accepted provider output is at most 80 UTF-8 bytes. Input text is capped at
4096 bytes; the provider stream, assembled text and event fields remain under
existing request/session bounds. Normalization removes terminal escape/control
and invisible directionality characters, collapses whitespace, trims, and
truncates only at a UTF-8 boundary.

## Ownership and concurrency

`AgentLoop` owns at most one title task and its cancellation token. The task
owns only the provider request and returns a candidate string; it cannot write
the Session. The Agent appends a completed result at a safe point after a turn,
before the next turn, or during orderly shutdown. This keeps the Session a
single-writer append-only log and prevents the title path from delaying normal
provider streaming.

## Failure, cancellation and recovery

Preparation, preflight, provider panic/error, invalid stream grammar,
reasoning/tool output, non-`stop` finish, empty/oversized title, timeout or
cancellation all discard the candidate and preserve the fallback. Shutdown
cancels and joins unfinished work. A resumed log containing a title request is
not automatically retried, so a result-unknown auxiliary call is never
replayed. Title failures do not change step or turn closure.

## Side effects and safety

The only extra external effect is one bounded request to the already selected
provider. It sends only the first direct human text and no tools, workspace
context, history or secrets. Display reads only normally closed, shared-locked,
same-format local journals and ignores unavailable/corrupt title metadata.

## Tests and compatibility

Tests fix normalization, fallback, exact event payloads, first-prompt-only
scheduling, successful replacement, provider failure/cancellation, replay,
closed-journal listing and picker rendering. The source-attributed fixture in
`tests/fixtures/tools/upstream_phase45_session_titles.json` records the compared
official limits and sequence. The row remains `partial`: Rust does not yet
implement manual rename/refresh, fork inheritance or official projection-cache
integration.

## Intentional differences

Official Cordis observers can append the replacement immediately when their
task settles. Rust's single Session writer collects it only at an Agent safe
point, so a very late title can appear at the next turn or orderly shutdown.
This avoids concurrent journal writers; users still see the same latest-title
result after normal shutdown. Rust also bounds metadata scans and omits titles
from busy or malformed journals instead of delaying listing.
