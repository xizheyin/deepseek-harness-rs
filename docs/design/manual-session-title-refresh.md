# Explicit Session title refresh

## Problem and scope

A manually renamed Session stays pinned, and an automatic title may be stale or
poor. Phase 49 adds `/refresh-title` so a person can deliberately retry title
generation or return a fallback-only Session to its first-prompt title. The
command applies only to the current idle Agent. It does not add an all-messages
provider, remote refresh API, title queue, index or projection cache.

## Upstream basis

The semantic baseline remains
`47f943859bef60e4160492346772ded9b24f765a`. The focused sources and tests are:

- `packages/session/session-title/src/index.ts` and
  `tests/{rename,service-contracts,provider,persistence}.spec.ts`;
- `packages/session/session-title-first-prompt-llm/src/index.ts` and
  `tests/provider.spec.ts`;
- `packages/session/session-title-llm/src/index.ts` for the existing exact
  request/stream contract.

Fresh `origin/master` at
`cd5ef8148158c3a752a658978873241fdf8e2bbc` changes only the optional title
projection-cache registration in this area. Refresh, cancellation, first-prompt
selection and accepted event behavior are unchanged.

## Input, output and event order

`/refresh-title` is an exact no-argument idle command. The Session projection
retains at most the first direct `source.kind=user` text, truncated to the
existing 4096-byte title input limit, plus its event sequence. Other user
sources, later prompts and non-text input do not replace it.

With a configured title Provider, the transaction is:

```text
cancel/join older title work
append session/title-llm-request(first prompt, current route, 64-token cap)
start one provider request
on valid text + stop: append session/title(source=provider)
```

The accepted result becomes the latest durable title. Failure or cancellation
adds no title event and leaves the previous title latest. With no title
Provider, a user-sourced current title is replaced by the deterministic
first-prompt fallback; an already automatic title is returned unchanged.

## Ownership and recovery

`Projection` owns the bounded first-prompt fact because durable `Session` does
not retain an all-history event vector. The fact is reconstructed by normal
journal replay and survives compaction because compaction only changes the
model-visible surface, not earlier append-only events. `AgentLoop` owns the
idle transaction and `SessionTitleRuntime` owns Provider preparation,
cancellation and title acceptance.

No new serialized event or schema field is required. Recovery keeps using the
existing request and title events; an interrupted refresh is never replayed
automatically.

## Failure, cancellation and timeout

No eligible first prompt returns a no-history result without materializing a
new journal or calling the Provider. A pre-cancelled command changes nothing.
After the request fact commits, Provider preparation/stream/protocol/output
failure and the existing 60-second timeout return a visible failure while
preserving the current title. `Ctrl+C` cancels the request and the terminal
waits for the refresh future to finish; no detached task remains.

Session append failures remain Agent infrastructure errors. They are never
reported as a successful refresh.

## Side effects, safety and limits

The only possible external effect is one request to the already configured
title-capable Provider. It contains only the bounded first direct human text,
the title system instruction and no tools, workspace context or conversation
history. The request intent is logged first. Fallback-only refresh performs one
bounded Session append and no network call. Approval is not relevant because
the command changes only current-Session metadata.

The projection retains at most 4096 text bytes and one sequence per Session.
Provider output remains bounded to 80 UTF-8 bytes and terminal-normalized.

## Tests and compatibility

The source-attributed fixture fixes first-prompt selection, request/title order,
fallback unpin, empty input, failure and cancellation. Projection tests prove
bounded capture and replay. Fake-provider Agent tests cover success, failure,
cancellation and no replay. Real enhanced and linear PTY journeys cover the
fallback-only command, including durable resume and zero model requests.

The compatibility row remains `partial`: Rust implements the shipped
first-prompt provider behavior but not the official generic all-messages
provider surface, overlapping explicit callers, remote API or projection cache.

## Intentional differences

Official `SessionTitleService.refresh()` is an async service method and permits
overlapping callers, with the newest revision superseding older work. Rust's
CLI serializes commands through its sole idle `AgentLoop`; another metadata
command cannot overlap the mutable Session writer. `/refresh-title` is a Rust
terminal name, not an upstream CLI command. Provider failures are summarized
as a bounded terminal notice instead of exposing backend error text, avoiding
accidental secret leakage.
