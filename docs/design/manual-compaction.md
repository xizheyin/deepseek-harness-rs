# Phase 27 manual `/compact` design

This design is based on DeepSeek Harness commit
`47f943859bef60e4160492346772ded9b24f765a`; current master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` retains the same production command
behavior. Exact inspected paths are listed in `docs/upstream.md`.

## Problem and scope

Automatic compaction waits for context pressure. A human sometimes knows that
a finished work segment should be summarized earlier, so `/compact` must run
the same bounded summarizer while the Agent is idle. This phase adds only that
local command. It does not add repeated compaction, background jobs, generic
command persistence, MCP, Skills, Hooks, or subagents.

## State and event order

The `AgentLoop` remains the only owner of Session and Provider state. For one
exact `/compact`, it materializes the Session, rejects any non-idle boundary,
and asks Projection for the largest balanced older prefix while retaining the
minimum recent complete tail. With no candidate it returns a no-op before any
Provider request or event.

For a candidate it generates bounded command, message, and compaction IDs and
appends:

```text
compaction/start    turn=null, sourceCommandId, complete manual dispatch
compaction/summary  same sourceCommandId and selected prefix
user/message        replacement checkpoint citing start, summary, and old seqs
compaction/end      turn=null, same sourceCommandId, success or error
```

The summary request uses purpose `compaction`, the current canonical request
header, no pending human input, no tool execution, at most 8,192 output tokens,
the existing stream byte/chunk caps, and the ordinary configured turn timeout.
Success is committed only when the replacement is strictly smaller than the
selected prefix. The command does not open a turn, change `next_turn`, expose
its text to the model, or replay any tool.

After success, the existing workspace-instruction runtime reconciles the new
visible surface. If compaction shadowed the active instruction fact, the Agent
holds one rebuilt context message for the next ordinary turn. A rearm failure
stops Agent reuse before another model request.

Unlike the upstream manual start payload, Rust records the complete prepared
request recipe with trigger `manual` before sending it. This is the same
intent-before-model-call rule already used by automatic Rust compaction and
lets recovery audit exactly what the auxiliary model saw.

## Failure, cancellation, and side effects

Argument errors and an empty candidate have no Session or Provider side
effect. After `compaction/start`, Provider failure, malformed/tool-calling
output, timeout, cancellation, a changed Session, or non-shrinking output
settles `compaction/end` with an error. No checkpoint is installed on those
paths. Ctrl+C cancels the request and the terminal continues only after the
bracket and durable writer have settled. Terminating signals cancel first and
exit after cleanup. The command has no filesystem, Shell, approval, or plugin
side effect.

## Upstream alignment and intentional difference

The selected range, standalone null owner, source-command correlation,
summary/checkpoint/end adjacency, no-history result, argument rejection, and
idle-only behavior follow the pinned command and compaction-basic tests.
Upstream also records generic `command/run` and `command/done` events. Rust's
existing local commands do not have such a generic schema; Phase 27 therefore
records `sourceCommandId` only on the standalone compaction transaction. Adding
generic command facts for every slash command is a separate schema migration,
not approximated for one command.

## Verification

Focused local tests cover parsing, no history, success below pressure, exact
null-owner/source-ID ordering, no turn consumption, Provider failure,
non-shrinking output, cancellation closure, recovery, workspace-instruction
rearming, enhanced active-turn busy handling, and one real CLI journey against
a loopback DeepSeek-shaped server.
Only focused tests plus the repository's required local Rust gates are run; no
remote CI or real model request is required.
