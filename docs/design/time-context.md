# Phase 38 durable per-step time context design

## 1. Problem and scope

Models otherwise receive no reliable terminal-user date or elapsed-time fact.
Phase 38 adds an opt-in durable reading before every entered model step so a
multi-step turn and a resumed Session retain the exact clock evidence used by
earlier reasoning.

The public input is `--time-zone <IANA_ZONE>`. This phase does not add a clock
tool, browser provenance, Schedule defaults, ambient zone guessing, refresh
intervals, replacement system prompts or background timers.

## 2. Upstream basis

The fixed baseline is
`47f943859bef60e4160492346772ded9b24f765a`. Exact inspected package, test,
README and decision-note paths are recorded in `docs/upstream.md`. Latest
master `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains the same observable
reading and ordering.

## 3. Input, output, state and order

`--time-zone` accepts at most 64 UTF-8 bytes, no controls, and one canonical
IANA name such as `Asia/Shanghai` or `UTC`. A valid value creates one immutable
`TimeContextRuntime`; absence disables the feature and changes no request.

For each step the Agent checks cancellation, prepares other dynamic context,
samples the time, and adds at most one message. The durable order is:

```text
turn/start → step/start → proposed/context messages → time-context user/message
→ request header/context when changed → Provider attempt
```

The message has one text block and exact snapshot source
`{kind: plugin, plugin: time-context, form: snapshot, sections: [...]}`. Its
three lines name turn/step, timestamp and terminal zone, then the appropriate
elapsed baseline. It never enters the system prompt or request header.

Projection owns only three small facts: latest model-visible message time,
latest time-context time in the open turn, and the open-turn reset. Raw event
rows remain the source of truth; this projection survives durable cold replay
and compaction without retaining message bodies.

## 4. Normal, failure, cancellation and timeout behavior

- Normal: one fresh message is recorded and included in that step's request.
- Invalid/noncanonical zone: CLI fails before Session creation, credentials,
  plugin/LSP startup or network access.
- Clock, formatting, ID or message failure: the open turn closes with stable
  `AGENT_TIME_CONTEXT`; no step or Provider request starts.
- Cancellation before preparation: no time message is generated or recorded.
- Cancellation after step entry follows the existing Agent attempt closure;
  the reading truthfully remains because the step was entered.
- There is no separate time-context timeout or asynchronous task. Its work is
  bounded synchronous formatting; the existing turn deadline is rechecked
  before step entry.

## 5. Side effects

The only lasting side effect is one append-only Session message per enabled
entered step. Startup may read the host zoneinfo database through the time-zone
library. No network, file write, subprocess, approval or tool side effect is
introduced.

## 6. Ownership and interfaces

`time_context` owns zone validation, clock sampling, duration/timestamp
formatting and message construction. `AgentLoop` owns when it is called.
`session::Projection` owns durable baseline facts. CLI assembly constructs and
installs the optional runtime; TUI and tools do not access it.

The exact dependency is `jiff 0.2.35`, licensed MIT/Unlicense with MSRV 1.70.
Only `std` and the system zoneinfo database feature are enabled. It is used
because correct IANA aliases, daylight-saving offsets and ISO formatting are
not available in Rust's standard library; process-global `TZ` mutation would
race with the Agent and tests.

## 7. Recovery, replay and compaction

Every reading is already a normal `user/message`, so existing JSONL recovery
and request reconstruction retain it. The projection folds timing baselines
from all committed events, including messages later shadowed by compaction.
Resume never invents a new reading or replays an old operation. Passing
`--time-zone` again enables future readings; omitting it preserves history but
adds none.

## 8. Security and resource limits

Zone input is bounded and canonicalized before use. Model content cannot choose
or change it. A reading has fixed prose plus a maximum 64-byte zone, one text
block and one source section. The existing 64-step, message, request, Session
event and durable-byte limits remain authoritative. Debug output contains no
timestamp or user content. Zoneinfo is read-only local data and no secret or
environment dump enters the message.

## 9. Tests and comparison evidence

A fixed-source fixture records exact official wording/order and the deliberate
terminal-line change. Unit tests cover canonical/alias/invalid zones, DST and
UTC rendering, whole-second truncation, duration units/backward time, exact
source shape, one-per-step behavior and projection replay. Agent tests cover
event order, two-step accumulation, no request-header mutation, cancellation,
clock failure, compaction and reconstructed Session behavior. One real CLI
loopback journey proves the flag reaches the second Provider request without an
approval. An invalid-zone smoke proves failure before Session/network work.

Local formatting, check, all-target tests, Clippy and diff checks are the final
gate. A real model, browser, Schedule app, remote CI and extra platform matrix
are intentionally outside this user-approved validation boundary.

## 10. Intentional differences

Official Web request sources can carry one, mixed or missing browser zones and
may use a configured/process fallback. Rust's standalone terminal instead
requires one explicit process-launch zone and renders `Terminal time zone`.
Official positive refresh intervals can skip readings across steps and resume;
Rust Phase 38 always samples every entered step. Rust also applies its existing
strict resource limits. These differences improve terminal clarity but prevent
a broad `compatible` claim; the row remains `partial` after implementation.
