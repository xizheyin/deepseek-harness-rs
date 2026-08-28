# Phase 31 advisory repeated-tool reminder design

This design uses fixed DeepSeek Harness commit
`47f943859bef60e4160492346772ded9b24f765a`. Latest inspected master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` keeps the same detection and
delivery behavior. Exact source and test paths are recorded in
`docs/upstream.md`.

## Problem and scope

A model can repeatedly issue one byte-equivalent operation after a failure or
unchanged result, spending time and tokens without learning anything. Fixed
upstream enables `repeat-tool-reminder` in its base bundle. It detects exact
repetition and gives the model advice at counts 3, 5, and 8.

Phase 31 adds that default loop-hygiene behavior to the Rust Agent. It is not a
new tool, hard tool-call blocker, retry policy, approval shortcut, background
worker, fuzzy similarity detector, or persisted counter. It does not add the
upstream plugin configuration surface, wildcard include/exclude patterns,
subagent isolation machinery, or Cordis hooks.

## Upstream behavior and observable order

Fixed `packages/guard/repeat-tool-reminder/src/index.ts`, its README and tests,
`packages/bundle/base/cordis.patch.yml`, and core Agent/tool-call sources define
these rules:

1. one live Agent owns one in-memory chain `(tool name, canonical arguments,
   count)`;
2. canonical arguments are parsed JSON with object keys sorted at every depth;
   malformed JSON falls back to a JSON string containing the raw input;
3. an identical tracked call increments the count; another tracked call resets
   it to one;
4. exact counts 3, 5, and 8 trigger notices; later counts are silent;
5. count 3 uses the fixed gentle text; counts 5 and 8 name the tool, count, and
   a head-capped canonical argument preview;
6. ordinary errors and denied calls count because observation happens after
   tool policy/execution has produced a result;
7. a direct human message at an accepted pre-step clears the chain; injected
   contexts and autonomous continuation do not;
8. the tool result is committed unchanged; the notice enters the next-step
   inbox and is later logged as a source-attributed `user/message` after the
   triggering step ends;
9. a resumed/new Agent starts with no chain; the notice itself remains a normal
   replayable Session fact after it is logged.

## Rust state and data flow

`agent/repeat_tool_reminder.rs` owns a small `RepeatToolReminder` value inside
each `AgentLoop`. This naturally supplies upstream's per-agent isolation and
fresh-on-resume behavior without a registry or trait. Its only retained
data-dependent value is one canonical argument string, bounded by the existing
tool-argument/request limits; it never retains a result.

The Agent follows this order for one definite result:

```text
tool/call -> tool body/policy -> tool/result commit
          -> update repeat chain -> maybe stage bounded notice
          -> step/end -> next step/start -> user/message notice -> model request
```

The serial and Phase 30 parallel result paths both call the same observer only
after authoritative model-order result settlement. Parallel completion timing
therefore cannot alter which call becomes repeat 3, 5, or 8. Before an entered
turn begins, the Driver clears the chain only if that accepted proposal
contains a direct-human source. Goal-round plugin input is not a reset.

Notices are accumulated in a small current-turn vector. A pending notice forces
one next step even if the triggering tool requested turn conclusion, matching
upstream's non-empty next-step inbox rule. The next step reserves and appends
the message through the ordinary Session path, so a budget or storage failure
cannot create a model-visible fact that was not logged.

## Failure, cancellation, recovery, and safety

Calls with a definite normal/model-error result count once. An
`ABORTED_BEFORE_DISPATCH` synthetic pair does not: upstream creates those
outside the tool post-execute seam. Infrastructure failures with unknown
outcomes do not count or fabricate a notice. A cancellation may close an
already-started result, but the turn still ends under the existing cancellation
reason and does not start work merely to deliver a reminder.

Canonicalization accepts only Rust's already bounded JSON domain. Object order
is deterministic because this build's `serde_json::Map` is a `BTreeMap`; number
normalization uses the existing `JsonValue` JavaScript-safe rules. If parsing or
bounded normalization rejects input, the raw string fallback keeps exact-match
detection deterministic. Detailed previews expose at most 500 Unicode scalar
values and report the omitted scalar count. This differs from JavaScript's
UTF-16 code-unit slicing only for non-BMP text and avoids storing an invalid
half-surrogate in a Rust `String`.

Reminder construction failure is an Agent error rather than a silent change of
model context. No external file, process, network, approval, or secret is
introduced. Reminder text can quote model-supplied tool arguments, which were
already model-visible and durable; the cap prevents a repeated large payload
from being duplicated without limit.

## Verification and intentional differences

Deterministic tests use fixed IDs and fake tools/providers to prove the fixed
texts, source `{plugin: repeat-tool-reminder, form: notice, summary: name ×
count}`, exact event order, result preservation, next-request replay,
canonicalization, reset rules, failures, multiple/parallel calls, preview cap,
and fresh state after reconstruction. A real loopback `dsh --prompt` journey
proves the production CLI reaches the guard.

Rust intentionally ships only the fixed default roster: all model-requested
tools are tracked, thresholds are `[3, 5, 8]`, and preview length is 500. There
is no wildcard config or direct-registry-call concept in the Agent-owned seam.
Rust rejects oversized/deep/unsafe JSON earlier and counts Unicode scalars in a
preview instead of UTF-16 units. These choices reduce public surface and keep
memory bounded; default ordinary JSON behavior and model-visible texts match.
A notice waits in the current Rust turn until the next step reserves its
ordinary `user/message`; upstream first persists a separate inbox-splice fact.
Thus a crash in that narrow pre-step interval can lose the heuristic notice in
Rust, but cannot expose unlogged model context or lose/alter the tool result.
The committed fixture is source-and-test transcribed rather than generated by
an executable cross-language producer, so compatibility remains `partial`
rather than `compatible`.
