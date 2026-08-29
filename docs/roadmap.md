# Product Roadmap

This roadmap records implementation status. Phases 0–9 remain the finite v0.1
plan; Phases 10–53 are explicitly approved post-v0.1 extensions. This is a
plan, not a list of current product features. `README.md` remains the source
for behavior that users can run today.

| Phase | Scope | Status | Acceptance record |
| --- | --- | --- | --- |
| 0 | Reproducible Rust CLI foundation | `complete` | [`validation/phase-0.md`](validation/phase-0.md) |
| 1 | Core types and in-memory session | `complete` | [`validation/phase-1.md`](validation/phase-1.md) |
| 2 | DeepSeek streaming provider | `complete` | [`validation/phase-2.md`](validation/phase-2.md) |
| 3 | Agent Loop | `complete` | [`validation/phase-3.md`](validation/phase-3.md) |
| 4 | Read-only tools | `complete` | [`validation/phase-4.md`](validation/phase-4.md) |
| 5 | File changes and approval | `complete` | [`validation/phase-5.md`](validation/phase-5.md) |
| 6 | Shell, timeout, and cancellation | `complete` | [`validation/phase-6.md`](validation/phase-6.md) |
| 7 | Interactive CLI/TUI | `complete` | [`validation/phase-7.md`](validation/phase-7.md) |
| 8 | Local session continuity and one-pass automatic context compaction | `complete` | [`validation/phase-8.md`](validation/phase-8.md) |
| 9 | v0.1 integration and release candidate | `complete` | [`validation/phase-9.md`](validation/phase-9.md) |
| 10 | Bounded subprocess tool plugins and examples | `complete` | [`validation/phase-10.md`](validation/phase-10.md) |
| 11 | TUI v2: semantic conversation UI, composer, dock, review, and accessibility | `complete` | [`validation/phase-11.md`](validation/phase-11.md) |
| 12 | Bounded same-process Goal automation | `complete` | [`validation/phase-12.md`](validation/phase-12.md) |
| 13 | Durable Goal events and disarmed Session recovery | `complete` | [`validation/phase-13.md`](validation/phase-13.md) |
| 14 | Caller-attested Goal tool authority | `complete` | [`validation/phase-14.md`](validation/phase-14.md) |
| 15 | Official Goal tool contract and configurable cap | `complete` | [`validation/phase-15.md`](validation/phase-15.md) |
| 16 | Autonomous Goal terminal wrap-up context | `complete` | [`validation/phase-16.md`](validation/phase-16.md) |
| 17 | Bounded interactive model-to-human question | `complete` | [`validation/phase-17.md`](validation/phase-17.md) |
| 18 | Sequential bounded user-question batches | `complete` | [`validation/phase-18.md`](validation/phase-18.md) |
| 19 | Bounded custom and option-free user answers | `complete` | [`validation/phase-19.md`](validation/phase-19.md) |
| 20 | Bounded multi-select user questions | `complete` | [`validation/phase-20.md`](validation/phase-20.md) |
| 21 | Per-question skip in bounded question batches | `complete` | [`validation/phase-21.md`](validation/phase-21.md) |
| 22 | Draft-preserving user-question pager | `complete` | [`validation/phase-22.md`](validation/phase-22.md) |
| 23 | Durable terminal Plan Mode and reviewed exit | `complete` | [`validation/phase-23.md`](validation/phase-23.md) |
| 24 | Durable model Todo list and terminal standing plan | `complete` | [`validation/phase-24.md`](validation/phase-24.md) |
| 25 | Durable startup/resume workspace instructions | `complete` | [`validation/phase-25.md`](validation/phase-25.md) |
| 26 | Tool-driven nested workspace-instruction refresh | `complete` | [`validation/phase-26.md`](validation/phase-26.md) |
| 27 | Idle manual `/compact` command | `complete` | [`validation/phase-27.md`](validation/phase-27.md) |
| 28 | Bounded DeepSeek-backed `web_search` tool | `complete` | [`validation/phase-28.md`](validation/phase-28.md) |
| 29 | Current-master multi-query search and public `web_fetch` | `complete` | [`validation/phase-29.md`](validation/phase-29.md) |
| 30 | Bounded parallel-safe tool scheduling | `complete` | [`validation/phase-30.md`](validation/phase-30.md) |
| 31 | Advisory repeated-tool-call reminder | `complete` | [`validation/phase-31.md`](validation/phase-31.md) |
| 32 | Fixed-upstream `str_replace_editor` file editing | `complete` | [`validation/phase-32.md`](validation/phase-32.md) |
| 33 | Bounded private Shell output spill files | `complete` | [`validation/phase-33.md`](validation/phase-33.md) |
| 34 | Fixed-upstream `write` and `edit` file tools | `complete` | [`validation/phase-34.md`](validation/phase-34.md) |
| 35 | Bounded project-local Skills catalog and loader | `complete` | [`validation/phase-35.md`](validation/phase-35.md) |
| 36 | Bounded same-workspace persisted-session search | `complete` | [`validation/phase-36.md`](validation/phase-36.md) |
| 37 | Configured bounded stdio LSP code navigation | `complete` | [`validation/phase-37.md`](validation/phase-37.md) |
| 38 | Durable opt-in per-step time context | `complete` | [`validation/phase-38.md`](validation/phase-38.md) |
| 39 | Bounded exact navigation inside prior Session events | `complete` | [`validation/phase-39.md`](validation/phase-39.md) |
| 40 | Bounded prior-Session lineage and event relationship traces | `complete` | [`validation/phase-40.md`](validation/phase-40.md) |
| 41 | Bounded filters for prior-Session and event search | `complete` | [`validation/phase-41.md`](validation/phase-41.md) |
| 42 | Bounded process-local background Shell jobs | `complete` | [`validation/phase-42.md`](validation/phase-42.md) |
| 43 | Background-job completion notices and bounded idle wakeups | `complete` | [`validation/phase-43.md`](validation/phase-43.md) |
| 44 | Consuming incremental output for background Shell jobs | `complete` | [`validation/phase-44.md`](validation/phase-44.md) |
| 45 | Durable first-prompt Session titles | `complete` | [`validation/phase-45.md`](validation/phase-45.md) |
| 46 | Title-enriched prior-Session search | `complete` | [`validation/phase-46.md`](validation/phase-46.md) |
| 47 | Title-enriched historical event and lineage tools | `complete` | [`validation/phase-47.md`](validation/phase-47.md) |
| 48 | Durable manual Session title rename | `complete` | [`validation/phase-48.md`](validation/phase-48.md) |
| 49 | Explicit cancellable Session title refresh | `complete` | [`validation/phase-49.md`](validation/phase-49.md) |
| 50 | Safe current-Session raw log export | `complete` | [`validation/phase-50.md`](validation/phase-50.md) |
| 51 | Completed-turn current-Session fork | `complete` | [`validation/phase-51.md`](validation/phase-51.md) |
| 52 | Idle Session model and reasoning-effort selection | `complete` | [`validation/phase-52.md`](validation/phase-52.md) |
| 53 | Durable safe permission presets | `complete` | [`validation/phase-53.md`](validation/phase-53.md) |

Only one phase may be `in-progress`. A phase becomes `complete` only after its production path, tests, compatibility evidence, validation record, and repository-wide checks pass.

## Phase 45 boundary (2026-08-29)

Phase 45 adds a readable title to each Session without delaying or endangering
the main conversation. The first direct human prompt immediately produces a
bounded, control-cleaned fallback title. After the main request route has been
recorded, one bounded DeepSeek request may replace it with a natural-language
title. Title failure, timeout, cancellation, malformed output or shutdown keeps
the fallback and never changes the turn result.

Both the title request and accepted title are append-only log facts and remain
outside the model-visible conversation. Normally closed local journals expose
their latest title in `--list-sessions` and the interactive resume picker.
Phase 45 does not add manual rename, title refresh, fork inheritance, a title
index/database or background work that survives process shutdown.

## Phase 46 boundary (2026-08-29)

Phase 46 threads the already validated, latest durable Session title into each
`session_search` result. A missing, busy, malformed or over-limit title degrades
to `untitled`; it never removes the base match or weakens the existing strict
same-workspace journal scan. Event read/search and lineage trace title headers,
live sessions, title-specific indexing and unavailable error codes remain
future work.

## Phase 47 boundary (2026-08-29)

Phase 47 applies the same already-authorized title metadata to
`session_event_search`, `session_event_read`, `session_trace` and
`session_event_trace`. It changes only human/model-facing headings and lineage
rows; event payloads, filters, ranking, ancestry, surface classification and
workspace authorization remain unchanged. Missing title metadata renders as
`untitled` and never removes the underlying result.

## Phase 48 boundary (2026-08-29)

Phase 48 adds idle terminal `/rename <TITLE>` for the current Session. A valid
title is normalized to the existing 80-byte terminal-safe bound, appended as a
user-sourced `session/title` fact, and becomes the durable latest title used by
resume and historical tools. Renaming supersedes any in-flight automatic title
request, and later prompts cannot overwrite the user title.

The command changes no conversation messages, starts no model/tool request and
requires no approval. It is accepted only while the Agent is idle. `/rename`
without a title reports the current title and usage; a title containing no
visible characters is rejected without changing the existing title. Official
explicit title refresh/unpin, rename APIs outside this CLI, and cross-process
live-session mutation remain future work.

## Phase 49 boundary (2026-08-29)

Phase 49 adds idle `/refresh-title` for explicitly retrying the configured
first-prompt title provider. It keeps a bounded projection of the first direct
human text and sequence, including after durable resume. A provider refresh
records `session/title-llm-request` before the network call and appends a
provider title only after a valid successful response. A fallback-only route
replaces a user-pinned title with the deterministic first-prompt fallback.

The command is cancellable with `Ctrl+C`, starts no Agent turn or tool, and
requires no approval. Missing input, provider failure, invalid output, timeout
or cancellation preserve the current title. Rust serializes explicit refreshes
through its one idle Agent instead of exposing the official service's
overlapping caller API. All-messages title providers, remote/cross-process
refresh, refresh queues and title projection caching remain out of scope.

## Phase 50 boundary (2026-08-29)

Phase 50 maps the fixed upstream's pathless `/export` onto the local terminal.
The current idle Session is synchronized, then its exact durable JSONL prefix
is copied in bounded chunks to a new owner-only file in the already-opened
workspace. The generated filename contains only a sanitized Session id and a
bounded collision suffix. Existing files are never overwritten. Success prints
the filename, workspace-root location and byte count without opening a model
turn, tool or approval.

`Ctrl+C`, destination failure or source failure never reports success and
removes the incomplete output when possible. The source journal is not parsed,
re-encoded or mutated. Rust exports one raw `.jsonl` because this product has
one current Agent and no image-attachment store; it does not claim the official
multi-Session ZIP, descendant or media behavior. The copy is an external
artifact, not a registered resumable Session or a substitute for a backup.

## Phase 51 boundary (2026-08-29)

Phase 51 adds idle `/fork [EVENT_SEQ]` for creating a separate resumable child
from the current Session. With no argument, the cut uses the latest completed
turn. An optional non-negative event sequence anchors the containing completed
turn: the first `turn/end` at or after the anchor is selected, and stable
out-of-band facts immediately after it are included until the next
`turn/start`. A past-end anchor falls back to the latest completed turn; an
anchor inside a still-open turn fails rather than silently clipping backward.

The child receives a new random Session id, the same workspace identity,
`parentSession`, `seedLength`, the exact selected source event bytes and one
new `session/end-seed` marker. If the source has a title, the child gets a
bounded user title with a trailing fork counter such as `(1)` or `(2)`. The
child file is owner-only, collision-safe and published only after a bounded,
cancellable copy succeeds. The parent remains the active terminal Session;
success prints a copyable `dsh --resume <child-id>` command, matching the
official service contract where callers choose whether to open the child.

Phase 51 does not add product subagents, fork a different cold Session directly,
copy external attachments, create a shared live Agent, or allow a model tool to
fork. Resume validates the child through the existing strict recovery path;
forking creates no parent conversation event, model request, tool or approval.

## Phase 52 boundary (2026-08-29)

Phase 52 adds idle terminal `/model`, `/model MODEL` and
`/model MODEL EFFORT`. The no-argument form reports the current effective
DeepSeek selection and the built-in advisory choices. A selection accepts one
bounded model id, including an unlisted pass-through id, plus optional exact
`off`, `high` or `max` effort. It is validated synchronously by the configured
Provider and replaces only the selection used by the next model assembly.

Selection creates no turn, message, model request, tool, approval or immediate
Session event. The next real request records the chosen route through the
existing `request/header`; after that, ordinary Session recovery preserves it.
An invalid selection leaves the prior one untouched. A selection made after a
request is recorded with reason `change`, while the first request remains
`initial`. The command is idle-only because this terminal owns one mutable
Agent; an active-turn attempt reports busy instead of altering an already
assembled step or entering the prompt queue.

This phase remains DeepSeek-only and does not add a Provider marketplace,
remote catalog refresh, global default settings, image-capability negotiation,
or the official Web popup. A selection that is never consumed by a later model
request is process-local and is lost on exit, matching the upstream rule that
Session durability begins when a request header consumes it.

## Phase 53 boundary (2026-08-29)

Phase 53 adds idle `/permission [ask|auto-edit]` with a durable, log-only
`permission/preset` Session fact. `ask` retains confirmation for file changes,
Shell and plugins. `auto-edit` allows ordinary file-changing tools without a
prompt but deliberately leaves Shell and plugin policies at Ask. It does not
claim or add an operating-system sandbox.

The latest valid preset survives resume and fork. An omitted startup flag uses
that Session value; an explicit `--approval-mode` overrides and durably records
it before any turn or tool side effect. Re-selecting the effective value is a
no-op. The command is model-invisible and idle-only; active enhanced input is
consumed locally as busy.

This phase does not expose official `danger-full-access`, automatic Shell or
plugin approval, a deployment-wide default, arbitrary custom bundles, sandbox
events, or a Web permission picker.

## Phase 8 revised boundary (2026-08-18)

The user explicitly prioritized a useful Agent over database-grade session
durability. Phase 8 therefore requires only:

- bounded local JSONL save/list/resume for a normally closed current-version
  session;
- clear refusal of corrupt or unsupported history before a new model request or
  tool side effect;
- no automatic replay of a tool whose previous outcome is unknown;
- one bounded automatic summary transaction on the real Agent path when the
  committed context reaches its pressure threshold or the pending request does
  not fit;
- preservation of a recent balanced tail, followed by a successful retry of
  the same user input;
- clear failure, with no loop or tool replay, when that one reduction still
  cannot make the request fit;
- repository-wide checks, one real CLI save/resume smoke, one real automatic-
  compaction-and-continue acceptance test, Phase 8 evidence, and independent
  review.

The following are useful hardening work but no longer block v0.1: power-loss
durability, proof for every crash/repair prefix, NFS or cloud-filesystem
semantics, a near-512-MiB cold-scan stress proof, exhaustive old/future schema
migration, provider-overflow automatic replay, and the complete
32/64/96/192-MiB physical-allocation ownership proof. Existing tested locks,
barriers, repair code, and limits remain in place. The tradeoff is explicit:
`SIGKILL`, power loss, disk failure, or filesystem failure may lose the final
session tail or make that session impossible to resume.

## Phase 9 terminal experience gate (2026-08-18)

The user explicitly requires a polished Claude Code-style terminal experience,
not merely a technically functional line protocol. Phase 9 therefore also
requires a real, tested interaction pass before v0.1 can be called ready:

- permission prompts use a compact selection UI with Allow once, Reject, and
  Cancel; a human must never need to copy a random identifier;
- the selected action is visually clear, Enter confirms, Escape cancels, and
  Ctrl+C still cancels the current turn without leaking a side effect;
- file approvals present a readable diff, while Shell approvals present the
  exact command, working directory, and important environment changes;
- streaming text, working/tool state, errors, and the next input prompt have a
  consistent hierarchy; product-owned color may enhance a terminal, but
  `--no-color` and non-TTY output remain readable;
- terminal mode is restored after approval, cancellation, suspension, EOF,
  output failure, and every supported exit signal;
- real PTY tests cover keyboard selection, stale pasted input, cancellation,
  cleanup, and terminal restoration on macOS and Ubuntu.

The short `y`/`n`/`c` line answers added during Phase 8 are an immediate
usability repair, not completion of this Phase 9 gate.

## Phase 10 boundary

Phase 10 starts only after Phase 9 is complete. It adds explicitly configured local tool-plugin executables, not Cordis/npm compatibility or a general extension framework. The first protocol stays deliberately small: bounded versioned NDJSON over stdin/stdout, targeting only `hello`, `call`, `cancel`, and `result`; stderr is bounded diagnostics. Plugin tools still pass through dsh's existing schema validation, approval, append-only intent/result recording, cancellation, timeout, and owned process cleanup.

Acceptance requires two useful no-side-effect examples (`text-stats` and `json-format`) plus one protocol/cancellation fault plugin, all exercised through the real CLI. The default offline matrix must cover malformed and oversized messages, crash, timeout, cancellation, backpressure, restart/configuration, and absence of orphan processes on macOS and Ubuntu. A plugin remains a trusted local executable rather than a sandboxed capability.

## Phase 11 TUI v2 boundary (2026-08-18)

The user explicitly raised the post-v0.1 terminal-experience target after
reviewing the installed Phase 9 screenshots. Phase 11 replaces the current
developer-log presentation with a polished, quiet, and trustworthy hybrid
inline TUI while preserving the already accepted Agent, approval, Session, and
process-cleanup semantics.

The default enhanced renderer keeps completed conversation content in native
terminal scrollback and owns only a small dynamic dock for the composer,
current work, and approval. It must add a semantic UI projection, one
human-readable lifecycle per tool call, bounded Markdown/code/diff rendering,
a Unicode multiline composer, safe bracketed paste, visible drafts/queued
follow-ups, responsive 44/80/112-column layouts, Focus/Inspect/Review views,
work receipts, context/compaction facts, semantic themes, and an equivalent
plain/screen-reader path. Internal IDs and duplicated event-log lines are not a
user interface.

Focus uses that small Dock. Inspect and Review temporarily replace it with a
bounded read-only detail Dock while continuing to drain the same committed
Session facts; returning to Focus restores the composer without replaying the
transcript.

Phase 11 does not change the DeepSeek Harness semantic baseline, bypass
approval, add a Web/desktop GUI, introduce background agents, or claim a
sandbox. Completion requires real installed-binary PTY journeys and screenshots,
hostile-control and signal restoration tests, bounded resize/stream/paste/queue
tests, full Phase 0–10 regression gates, and successful macOS/Ubuntu CI.

The current green checkpoints implement bounded assistant-message markup,
source-preserving 2–8-column pipe tables, six closed process-local semantic
themes with transactional redraw, a closed fourteen-command completion palette, a
generator-provenanced semantic card for the real single-file `apply_patch`
approval preview, bounded workspace-file suggestions, and bounded primary-screen Inspect/Review panels. Inspect
shows only current-turn committed metadata and retained reasoning; Review keeps
one exactly joined summary and does not invent full historical diffs or command
records. The exact canonical approval source is still shown before the existing
default-Reject selector, while lookalike generic text remains opaque. File
suggestions insert only bounded relative-path literals from the retained
workspace capability and never read content. Reduced Motion now provides a
process-local flag/command, bounded turn-owned clock, and preemptible screen
transaction without changing Session or Provider facts. Bare interactive
`--resume` now provides a bounded header-only Session picker and hands only the
confirmed ID to the existing recovery lifecycle. A local installed-binary
journey now produces the overview, approval, and Review screenshots from real
PTY bytes, and the same candidate passes the macOS/Ubuntu CI matrix.
On 2026-08-28 the user explicitly changed the acceptance scope to local-only
necessary verification so implementation could move to the next product gap.
The installed-binary PTY acceptance, deterministic screenshots, 1,138-test
local regression run, and already-green macOS/Ubuntu candidate matrix close
Phase 11. Real-emulator capture, the optional remaining exact-limit checks, and
a second final review stay as non-blocking hardening work; no emulator or
pixel-compatibility claim is made.

The user also requested less approval friction on 2026-08-26. The resulting
green checkpoint is frozen in `docs/design/approval-modes.md`: explicit,
process-local interactive `--approval-mode auto-edit` allows only the already
prepared and workspace-confined built-in patch action. Shell and plugin actions
continue to ask, script mode continues to deny mutations, and the default
remains `ask`. This checkpoint does not broaden the Phase 11 completion claim
or add a sandbox.

The second approval-friction checkpoint, requested on 2026-08-26, is now green
and recorded in `docs/design/approval-modes.md`. A fully prepared
built-in Shell approval gains one explicit `Allow exact Shell for this process`
choice. The Agent may remember at most 64 sealed execution identities in RAM,
and only after a clean command plus committed result; it never restores them
from Session. `Allow once`, default Reject, script Deny, plugin Ask, all
workdir/process/resource checks, and the no-sandbox warning remain unchanged.
Real enhanced and linear PTY journeys cover repeated calls, and a same-process
Shell-then-plugin journey proves the grant cannot authorize a plugin. This is a
completed checkpoint inside Phase 11.

## Phase 12 Goal automation boundary (2026-08-28)

The user explicitly selected official Goal behavior as the next gap and asked
for rapid progress with local necessary validation only. Phase 12 adds one
bounded Goal to the interactive process: `/goal` creates and manages it, the
model can inspect and settle it through closed tools, and an active armed Goal
queues sequential same-session rounds until completion, blocking, cancellation,
or a fixed round cap.

The first slice is deliberately process-local. Goal state is not restored after
restarting or resuming `dsh`, Goal rounds carry a bounded generated text prompt,
and image attachments are not supported. `Ctrl+C` cancels the current Goal
round and pauses automatic continuation. Goal tools remain ordinary validated
tools and do not gain filesystem, Shell, approval, or Session-writing authority.
These differences and the fixed upstream evidence are recorded in
`docs/design/goal-automation.md`.

Acceptance is intentionally local: focused parser/state/tool tests, one real
interactive auto-continuation journey, cancellation/cap coverage, format,
all-target compilation, Clippy with warnings denied, and a whitespace check.
The full repository suite and remote/cross-platform reruns are useful but do not
block this user-directed fast checkpoint.

## Phase 13 durable Goal boundary (2026-08-28)

Phase 13 continues the active product Goal with the next explicit Phase 12
gap. Every accepted Goal mutation becomes a typed, versioned, non-surface
`goal/change` Session event. Session replay validates revision, identity,
phase, timestamp, round-count, and clear-tombstone transitions. Admitted Goal
round user messages carry goal ID, revision, and positive round number so the
round counter is also reconstructible from the log.

New and explicitly resumed Goals may arm automatic continuation. Merely
reopening a Session restores the durable phase but always starts disarmed;
`/goal` can inspect it and `/goal resume` records a fresh revision before any
new automatic model request. This prevents a process restart from silently
replaying uncertain work.

The local-only acceptance gate covers deterministic event codec/fold fixtures,
exact tool-call/change/result ordering, corrupt or stale mutation rejection,
real save/resume/rearm behavior, format, all-target compilation, focused tests,
Clippy with warnings denied, and the whitespace check. Remote CI and the full
repository matrix are not repeated unless a focused check exposes a broader
regression.

## Phase 17 user-question boundary (2026-08-28)

Phase 17 adds the first production terminal slice of the fixed official
`ask_user_question` behavior. During an interactive turn, the model can pause
one ordinary tool call, show one question with two to four labelled options,
and continue after the human selects a number. The answer is retained as the
ordinary correlated compact-JSON tool result; waiting creates no hidden model
context and grants no approval authority.

The terminal broker has capacity one, model strings and counts are bounded,
input buffered before the question frame is discarded, Escape returns a
model-visible cancellation, and Ctrl+C still cancels the whole turn. Human
waiting bypasses the ordinary 30-second tool timeout but remains bounded by the
existing 30-minute turn deadline. Script mode does not advertise a question it
cannot answer.

This first slice is deliberately `partial` against the official package:
question batches, free-form answers, multi-select, plan-review presentation,
product subagent routing, and the latest Cordis answerer waterfall remain
outside Phase 17. The exact choice, risks, and evidence are in
`docs/design/user-question.md` and `docs/validation/phase-17.md`.

## Phase 18 question-batch boundary (2026-08-28)

Phase 18 extends `ask_user_question` to one through three questions per tool
call. The terminal presents one bounded question at a time with explicit batch
progress, fences input again before every later question, and returns one
ordered compact answer array only after the final selection. Escape or turn
cancellation closes the whole batch without publishing partial choices.

Question IDs are unique, every response index is rechecked against the exact
displayed options, and the existing one-shot capacity-one broker still owns the
entire interaction. The Session continues to contain one tool intent and one
correlated result; no intermediate UI choice becomes model context.

The compatibility row remains `partial`: Rust caps a batch at three and still
does not collect official custom-text, option-free, multi-select, or plan-review
forms. Evidence and the deliberate bounds are in
`docs/design/user-question-batch.md` and `docs/validation/phase-18.md`.

## Phase 19 custom-answer boundary (2026-08-28)

Phase 19 closes the next official `ask_user_question` gap: a question may omit
choices and collect free text, while a question with choices gains one explicit
custom-answer path. In the existing single-select form, custom text replaces a
choice and returns `selected: []` plus trimmed `custom`; ordinary choices keep
returning the exact displayed label.

Custom input is capped at 4,096 UTF-8 bytes. Enhanced terminals reuse the
Unicode-aware composer with bounded multiline editing, while preserving and
restoring any draft the user had already typed for the next turn. Linear mode
accepts one canonical text record. Empty text retries the same question;
Escape or turn cancellation closes the whole batch without publishing partial
answers.

Multi-select, skip, plan-review presentation, product subagent routing, and a
general answerer waterfall remain outside this phase. Design and local evidence
live in `docs/design/user-question-custom.md` and
`docs/validation/phase-19.md`.

## Phase 20 multi-select boundary (2026-08-28)

Phase 20 adds the official `multi_select: true` form to the existing bounded
question batch. A numbered option toggles on or off without advancing; Enter
submits after at least one choice. Choosing the custom entry opens the same
bounded editor, and nonblank custom text supplements rather than replaces the
selected labels.

The terminal displays the selected option numbers in the active Dock, retains
click order in the model-visible `selected` array, rejects duplicates and
foreign indices at the broker boundary, and restores any next-turn draft after
submit or cancellation. Linear mode uses one number per line to toggle and a
blank line to submit.

Skip, backward page navigation, plan-review presentation, product subagent
routing, and a general answerer waterfall remain outside this phase. Design
and local evidence live in `docs/design/user-question-multi-select.md` and
`docs/validation/phase-20.md`.

## Phase 21 per-question skip boundary (2026-08-28)

Phase 21 adds the official ability to skip only the current question. Earlier
answers remain local, the next question opens normally, and final output encodes
the skipped item as an empty `selected` array without `custom`.

Enhanced selection screens use `s`; an active custom editor uses Ctrl+S so an
ordinary letter `s` remains valid text. Linear mode accepts `s` plus Enter.
Skip restores any borrowed Composer overlay, keeps the whole-batch cancellation
path separate, and still publishes only one final correlated tool result.

Pager navigation, plan-review presentation, product subagent routing, and a
general answerer waterfall remain outside this phase. Design and evidence live
in `docs/design/user-question-skip.md` and `docs/validation/phase-21.md`.

## Phase 22 question-pager boundary (2026-08-28)

Phase 22 closes the fixed official terminal-relevant pager gap. A bounded batch
now owns one draft per question instead of only a completed prefix. Moving
backward or forward preserves single selection, ordered multi-selection,
custom text, and skip state; revisiting a question can edit that draft. A final
submit scans the complete batch and returns to the first unanswered question
without publishing a partial tool result.

Enhanced selection pages use `[` and `]`; the enhanced custom editor uses
Ctrl+P and Ctrl+N so printable answer text remains available. Linear prompts
use `[` or `]` followed by Enter. The question overlay still restores the
ordinary next-turn Composer draft exactly, and Escape/Ctrl+C semantics are
unchanged. Plan-review presentation, product subagent routing, and the general
answerer waterfall remain deferred. Design and local evidence live in
`docs/design/user-question-pager.md` and `docs/validation/phase-22.md`.

## Phase 23 Plan Mode boundary (2026-08-28)

Phase 23 adds the official Plan Mode loop as a real terminal capability:
durable `plan/mode` state, `/plan` and `/plan off`, a stable
`exit_plan_mode` schema, plan-only system guidance, and a human review of the
exact submitted markdown plan. Approval records the exit before the next model
request; Keep planning and feedback remain failed tool results so the model
must revise; dismissing the review keeps Plan Mode active and returns control
to the human.

Plan Mode is guidance, not a sandbox. It does not remove tools or bypass the
existing file, Shell, plugin, approval, timeout, Session, or cancellation
pipelines. The Rust terminal does not copy the Web card: it renders the exact
bounded plan through the existing trusted/untrusted terminal projection and
uses the existing question answerer. Image attachments and runtime-owned
subagents remain unavailable. Acceptance is local-only and focused as requested.

## Phase 24 Todo boundary (2026-08-29)

Phase 24 closes the existing half-built Todo gap: the Session already knows the
official `todo/write` event, but no production model tool can create it. The
phase adds the fixed upstream `todo_write` whole-list contract, strict bounded
input, `tool/call` → `todo/write` → `tool/result` ordering, last-write-wins
recovery, and a terminal standing-plan summary that clears on the next
`turn/start` without deleting history.

The Rust Agent executes one foreground action at a time and has no product
subagents or background jobs, so it enforces at most one `in_progress` item.
This is the upstream tool's supported single-active configuration, although
the official code preset currently selects parallel mode. Todo writes affect
only Session/UI state, need no filesystem/Shell approval, and cannot bypass the
existing tool, cancellation, result, or persistence pipeline. Input is capped
at 64 items and 512 UTF-8 bytes per trimmed line. Acceptance remains local-only
and focused as requested.

## Phase 25 workspace-instruction boundary (2026-08-29)

Phase 25 closes the highest-value remaining default-context gap: official dsh
loads `AGENTS.md`-compatible guidance before the first model request, while
dsh-rs currently ignores it. The phase adds a bounded initial and resume-time
baseline from the user-global instruction and the exact opened workspace root,
using the official candidate order, same-directory trimmed-content dedup,
`<system-reminder>` framing, most-specific-first budget policy, structured
source facts, and direct-prompt-then-instructions durable ordering.

On resume, an unchanged compatible visible baseline is reused rather than
duplicated. Confirmed additions, replacements, removals, and budget changes are
appended as new user-role instruction facts; history is never rewritten. A
temporarily unreadable or oversized candidate is not treated as deleted.

Rust keeps the exact opened workspace as its authority boundary instead of
walking above it to a parent `.git`, and refuses instruction-file symlinks
rather than letting repository text point at an arbitrary host file. This is a
deliberate privacy difference from upstream. Dynamic discovery of nested
instructions after a successful `read`/`apply_patch`, and same-process rearming
after compaction, require the tool-result boundary and remain the next explicit
gap rather than being approximated by shell parsing or a file watcher. Local,
focused acceptance remains in force.

## Phase 26 dynamic workspace-instruction boundary (2026-08-29)

Phase 26 closes the deferred active-turn half of Phase 25. A successful
built-in `read` result or a definitely committed built-in `apply_patch`
publishes one private, capability-relative file-touch fact. After the enclosing
`step/end`, the Agent batches those facts, checks root plus applicable nested
instruction scopes, and places at most one bounded instruction message into
the next model step. A failed read, rejected or uncommitted patch, cancelled
tool, Shell command, search tool, or subprocess plugin cannot create this fact.

The touch is private process state, not model-visible result metadata and not
reconstructed from a model argument inside the Agent. This keeps a plugin
named `read` from impersonating a built-in file tool. Loaded nested scopes are
reconciled from the visible append-only Session facts, so change, removal,
resume, and post-compaction rearming use the same state rather than a hidden
second database. Discovery stays within the exact opened workspace, retains
the Phase 25 no-symlink policy, batches at most the existing per-step tool-call
limit, and retains the existing 1 MiB per-source and 65,536-byte rendered
limits. Acceptance is local-only and focused as requested.

## Phase 27 manual compaction boundary (2026-08-29)

Phase 27 exposes the existing bounded summary machinery as the official idle
`/compact` command. It selects a balanced older prefix even below automatic
pressure, sends one tool-free compaction request, and on success appends a
standalone `compaction/start → summary → checkpoint → end` transaction whose
owner is `turn: null`. It does not create or consume an Agent turn.

No compactable history is a local successful no-op. Arguments are rejected
locally. Provider failure, invalid or non-shrinking output, cancellation, and
timeout close the started bracket with an error and leave the visible
conversation unchanged. The command is accepted only from an idle terminal.
Enhanced active input reports busy instead of becoming model input or a queued
prompt; the linear path retains its existing rule that it does not accept
ordinary input while a turn runs. Phase 27 reuses the current request, output,
time, Session, and provider bounds and adds no background task.

Rust records its existing complete pre-request dispatch snapshot with a
`manual` trigger even though upstream's manual start omits that extension.
This preserves the repository rule that model-visible input is logged before
the request. Rust does not yet have upstream's generic durable `command/run` and
`command/done` envelope for its other local slash commands. This phase keeps a
bounded `sourceCommandId` on every started manual compaction event instead of
adding a one-command-only generic event family. Exact source paths and this
intentional difference are recorded in `docs/upstream.md`,
`docs/design/manual-compaction.md`, and `docs/compatibility.md`.

## Phase 28 bounded web-search boundary (2026-08-29)

Phase 28 closes the largest remaining ordinary-tool gap in the fixed official
CLI composition: a search-only `web_search` tool backed by DeepSeek's separate
Anthropic-compatible Messages endpoint and native server-side search. The
conversation tool takes one nonblank query, returns at most eight deduplicated
sources with bounded title/snippet/date fields, and tells the model to cite the
returned URLs. It does not add arbitrary URL fetch, browser cookies, ambient
HTTP headers, a second credential, or a new approval prompt.

The query is already durably recorded in the ordinary `tool/call` before the
network request. The search uses `DEEPSEEK_API_KEY`, a separate
`DEEPSEEK_SEARCH_BASE_URL` override, no redirects or proxy inheritance, a
60-second whole-operation deadline, cooperative cancellation, and strict
response/body/output limits. Missing credentials and failures remain ordinary
correlated tool errors, so they cannot crash or poison the Agent.

The fixed baseline exposes one `query` and disables `web_fetch`; current master
changes search to a bounded multi-query array and enables a separately hardened
fetch path. This phase deliberately lands the fixed search-only contract first
and records the latest extensions as follow-up gaps. Acceptance is local-only:
fake providers, a loopback DeepSeek-shaped server, focused Agent ordering, and
the required local Rust gates; no real API request or remote CI.

## Phase 29 current-master web-tools boundary (2026-08-29)

Phase 29 closes the two explicit Phase 28 follow-up gaps from inspected master
`cd5ef8148158c3a752a658978873241fdf8e2bbc`: `web_search` accepts one to four
queries and merges them fairly, while `web_fetch` retrieves one anonymous
public HTTP(S) page with no cookies, credentials, proxy inheritance, or human
approval. Search keeps the existing DeepSeek native provider; fetch uses a
separate local HTTP provider and never consumes the DeepSeek API key.

The fixed baseline's fetch provider explicitly lacked private-network/SSRF
protection and was disabled in the shipped preset. The current implementation
must therefore follow the newer safety boundary: validate the URL, reject any
DNS answer set containing a non-public destination, pin the connection to the
validated addresses, follow only same-origin redirects, and re-resolve and
revalidate every accepted hop. Responses, decoded text, conversion work,
redirects, time, and final tool output are all bounded. External page content
is labelled untrusted and HTML is converted conservatively rather than exposed
as active markup.

Acceptance remains local-only. Deterministic fake-provider and policy tests plus
a real CLI journey use an injected resolver and loopback HTTP server to prove
the production transport shape without weakening production's loopback block.
No public network, real API call, remote CI, or unrelated exhaustive check is
required; the local Rust gates still protect the repository-wide build.

## Phase 30 bounded parallel-tool boundary (2026-08-29)

Phase 30 closes the core scheduling gap that makes independent reads wait for
one another. Matching the fixed upstream default, only tools that explicitly
and synchronously opt in may overlap. The shipped opt-in list is `read`,
`web_search`, and `web_fetch`; file mutation, Shell, plugins, questions, Goal,
Plan Mode, Todo, list/glob/grep, and unknown tools remain exclusive ordering
barriers. An exclusive call between two safe groups must wait for the earlier
group to drain and must finish before the later group starts.

The rolling pool defaults to ten in-flight calls and has a fixed Rust safety
ceiling. Every call intent and its dispatch barrier still commit before that
body starts. Calls begin in model order, later completions may free capacity,
but results, workspace-touch context, output-budget decisions, and any stateful
post-processing commit in model order. Cancellation stops replenishment,
cooperatively drains started work, and gives every undispatched model call a
correlated `ABORTED_BEFORE_DISPATCH` result on the ordinary durable path.

Acceptance is local-only: deterministic gated fake tools prove real overlap,
the cap, rolling refill, result order, exclusive barriers, cancellation, and
failure quiescence. A real CLI loopback journey asks for two independent Web
searches; its server withholds both responses until both connections arrive,
then verifies model and durable result order. No public network, real API,
remote CI, or unrelated exhaustive check is required.

## Phase 31 repeated-tool reminder boundary (2026-08-29)

Phase 31 adds the fixed upstream's default advisory loop guard. For one live
Agent, completed model-requested calls are compared by tool name and
canonicalized JSON arguments. The first identical tracked call has count one;
exact counts 3, 5, and 8 enqueue respectively one gentle and two detailed
plugin notices for the next model step. A different tracked call resets the
chain. A newly accepted direct-human prompt also resets it, while Goal-driven
automatic continuation does not. The chain is deliberately process memory,
so constructing or resuming a new Agent starts fresh.

The notice never changes, blocks, delays, retries, approves, or replaces a
tool result. It is appended as a bounded, source-attributed user-role context
after the triggering step's results, then reconstructed from ordinary Session
facts for the next request. Denied and model-facing failed calls count once;
calls skipped before dispatch, unknown infrastructure outcomes, cancellation
recovery, and direct registry calls do not. The fixed CLI has no exposed guard
setting, so Rust ships the official defaults and a 500-character detailed
argument preview rather than adding another public configuration surface.

Acceptance is local-only: deterministic Agent tests cover default escalation,
deep key-order canonicalization, resets, different-call reset, failure/denial,
multiple calls in one step, bounded preview, source and event order, next-model
replay, resume freshness, and parallel result ordering. One real script CLI
journey must loop three times and prove the notice reaches the fourth request
without changing tool results or requiring approval. No public network, real
API, remote CI, or unrelated exhaustive check is required.

## Phase 32 `str_replace_editor` boundary (2026-08-29)

Phase 32 closes a remaining fixed-upstream default-tool gap. The model receives
one `str_replace_editor` schema with `view`, `create`, `str_replace`, and
`insert`. Paths must be absolute and remain confined to the retained workspace.
Views use one-based line numbers; directory views are deterministic, omit the
official hidden/dependency/cache names, descend at most two levels, and clip at
the fixed 16,000-character presentation budget. Literal replacement requires
exactly one non-empty match and insertion uses the official zero-based boundary
without silently adding a trailing newline.

Every mutation reuses the existing two-stage file path: strict argument and
UTF-8 validation, retained capability, complete preview, `FileChangePolicy`,
intent-before-side-effect Session order, late conflict checks, atomic publish,
truthful committed outcome, cancellation, and workspace-instruction touch.
Default interactive mode therefore still asks; the already explicit
`--approval-mode auto-edit` also covers these prepared built-in file edits.
Shell and plugin approval behavior is unchanged. The older `apply_patch` tool
remains available for multi-line unified diffs.

Acceptance is local-only: a source-attributed fixed fixture, focused unit and
Agent approval tests, one real CLI loopback journey, and the normal local Rust
gates. No public network, real model call, remote CI, additional platform
matrix, or unrelated exhaustive stress run is required.

## Phase 33 bounded Shell output spill boundary (2026-08-29)

Phase 33 closes a practical fixed-upstream Shell gap: once stdout or stderr no
longer fits the 64,000-byte in-memory tail, dsh writes every byte it actually
captures for that stream to a randomly named owner-only file in a private
temporary directory. The model-facing result and terminal card expose the path,
so a later approved Bash command can inspect the missing head instead of
guessing from the tail.

The file is created lazily and never for small output. Directory and file modes
are 0700 and 0600. Creation, writes, and final flush are best effort: failure
keeps the old bounded tail and does not turn a successful command into a tool
error or advertise an incomplete locator. Paths and captured-byte counts enter
the ordinary correlated Shell result; the full bytes do not enter Session.

Rust retains its existing 8 MiB combined observed-output stop, rather than the
official 64 MiB per-stream spill followed by unbounded tail collection. If that
stop fires, the spill contains the bounded captured prefix and is labelled
`captured output`, not falsely called the full command output. The normal clean
case uses the official full-output notice. Spill files are convenience
artifacts under the OS temporary directory, not durable session storage; they
may expire independently, and approved Shell remains the retrieval path because
the workspace-confined `read` tool must not gain arbitrary host-file access.

Acceptance is local-only: a fixed source-attributed fixture, collector/process/
renderer/UI tests, real approved-Shell CLI output, cancellation/output-limit/
failure cleanup cases, and the normal local Rust gates. No public network, real
model call, remote CI, additional platform matrix, or large stress run is
required.

## Phase 34 fixed-upstream `write` and `edit` boundary (2026-08-29)

Phase 34 adds the two remaining ordinary text-mutation names from the fixed
official filesystem tool package. `write` creates or completely replaces one
UTF-8 workspace file. `edit` performs one literal replacement by default and
can replace every non-overlapping occurrence only when `replace_all: true` is
explicit. Both schemas are closed and accept workspace-relative or
inside-workspace absolute paths.

Both tools reuse the existing prepared-mutation owner rather than introducing a
second writer: retained workspace capability, safe text and size limits,
complete canonical diff, default Ask or explicit process-local `auto-edit`,
call-before-side-effect Session order, late conflict detection, atomic
publication, truthful committed metadata, cancellation and trusted workspace-
instruction refresh all remain unchanged. Direct executor calls cannot bypass
preparation.

The fixed official default observation policy refuses an overwrite or edit
until that session has read the file. Rust intentionally does not add a second
hidden read-version cache: it reads the complete bounded baseline while
preparing the exact diff, asks the human by default, and revalidates that exact
baseline immediately before publication. This keeps one authority owner and is
stronger against changes during approval, but allows a human-approved blind
overwrite that the official default policy rejects. Rust also keeps its
workspace confinement, no-symlink/no-hardlink mutation rules and 16 MiB safe-
text limit.

Acceptance is local-only: a fixed source-attributed fixture, closed schema and
parser tests, real Agent create/update/unique/replace-all journeys, rejection,
stale/cancel/invalid/no-match/ambiguous cases, one real CLI approval journey,
and the normal local Rust gates. No public network, real model call, remote CI,
additional platform matrix or unrelated stress run is required.

## Phase 35 bounded project-local Skills boundary (2026-08-29)

Phase 35 adds the fixed official model-facing `skill` contract over real local
Markdown bundles. The retained workspace is scanned one level below
`.dsh/skills` and `.agents/skills`; a directory contributes `<name>/SKILL.md`
and a flat `<name>.md` file is also accepted. Valid model-invocable names and
normalized descriptions enter a bounded durable catalog after the direct user
message. The model can then call `skill` with one exact name to load the current
body and a workspace resource-base hint through the ordinary tool-call/result
Session order.

The implementation is deliberately read-only and process-free. It reuses the
opened workspace capability, rejects symbolic links and paths outside that
capability, caps roots, entries, file bytes, descriptions and rendered output,
and checks cancellation around each blocking scan/read. `.dsh/skills` wins a
duplicate before `.agents/skills`. Discovery is repeated before a new turn and
between tool steps, so first-party or external changes become visible without a
long-lived filesystem watcher.

This first Rust boundary does not scan `$DSH_HOME`, `~/.agents`, custom, bundled,
remote or opaque providers; it does not follow skill symlinks, run scripts, or
implement direct `/skill-name` gestures. Frontmatter accepts the common scalar
`name`, `description`, optional `whenToUse`, `disable-model-invocation`, and
`user-invocable` fields rather than arbitrary YAML. These are explicit safety
and complexity differences, not broad compatibility claims.

Acceptance is local-only: source-attributed schema/catalog/result fixtures,
strict discovery/parser/precedence/limit/symlink/cancellation tests, real Agent
catalog → tool call → current body continuation, catalog refresh and resume
coverage, one real enhanced-PTY journey, and the normal local Rust gates. No
public network, real DeepSeek call, remote CI or extra platform matrix is
required.

## Phase 36 bounded persisted-session search boundary (2026-08-29)

Phase 36 adds the highest-value first slice of the fixed upstream's optional
session-query tool family: `session_search { query }`. The model can search
semantic text from normally closed persisted sessions that belong to the exact
opened workspace, while the current session and journals held by another live
process remain unavailable. Search is read-only and never resumes, repairs,
rewrites or replays a historical session.

The query is a literal, Unicode-aware, case-insensitive and whitespace-flexible
phrase. The schema exposes no path, workspace, result limit, cursor or session
identifier. Rust scans a bounded number of already private store entries on a
blocking worker, enforces per-session and aggregate byte budgets plus a fixed
deadline, checks cancellation between records, and returns at most 20 ranked
sessions with 240-code-point excerpts. Results are explicitly labelled as
untrusted historical data and enter the ordinary call-before-result Session
order without approval.

This first slice deliberately omits the official optional filters, SQLite FTS
index, live-session corpus, titles, lineage, surface classification,
`session_event_search`, `session_trace`, `session_event_trace`, and
`session_event_read`. Rust ranks by phrase occurrence count followed by recent
event/session time instead of SQLite BM25. These limits make the new path small
and auditable while still solving the common “find what an earlier session
already learned” workflow.

Acceptance is local-only: a fixed source-attributed schema/result fixture,
scanner and store-boundary tests for matching, workspace isolation, busy/current
exclusion, malformed/oversized input, result/output bounds and cancellation;
one real CLI two-session journey; and the normal local Rust gates. No public
network, real model call, remote CI, SQLite dependency or unrelated stress run
is required.

## Phase 37 configured stdio LSP boundary (2026-08-29)

Phase 37 adds the fixed upstream's model-facing `lsp` tool and a generic local
stdio language-server host. The tool exposes exactly four read-only operations:
`goToDefinition`, `findReferences`, `goToImplementation`, and `hover`. Model
coordinates are positive one-based UTF-16 positions; the protocol uses
zero-based UTF-16 positions. The model cannot choose a language server,
program, environment, workspace, timeout, or output cap.

Language servers are enabled only by an explicit private version-1 JSON file
passed through `--lsp-config`. The configuration maps file extensions to a
trusted absolute executable and language id. Executables are validated before
the tool schema is published, while server processes start lazily on the first
matching query. One owned actor serializes each server's transient
`didOpen` → query → `didClose` lifecycle and keeps the initialized process for
later calls in the same CLI process. An optional bounded `toolTimeoutMs` remains
user-controlled and invisible to the model; its default is 60 seconds.

Every source is resolved and read through the existing retained workspace
capability, rejects symbolic links and invalid UTF-8, and is capped at 4 MiB.
JSON-RPC headers, messages, queues, stderr, total protocol output, rendered
locations, rendered characters, call duration, cancellation grace and process
teardown are bounded. Cancellation sends `$/cancelRequest`; a server that does
not settle is terminated as an owned process group. Server requests for
`workspace/configuration` receive the static configured value, bookkeeping
requests receive `null`, and `workspace/applyEdit` or any other authority-
seeking request is rejected. LSP never edits files and needs no per-call
approval after the user explicitly enabled its executable at launch.

The Rust config deliberately requires an absolute, stable regular executable
rather than resolving a mutable `PATH` entry or symbolic-link shim. It uses the
existing stricter macOS/Linux process observer, environment scrub and aggregate
protocol-output limits. It has one workspace per CLI process rather than the
official provider's multi-workspace pool. These differences reduce race and
cleanup ambiguity without changing the four model-visible query operations.

Acceptance is local-only: fixed-source schema/result fixture, parser,
normalization, framing, config, workspace, capability, response-bound,
cancellation and process-cleanup tests; one real fake-stdio-server CLI journey
covering schema → initialize → open → query → close → next model request; and
the normal local Rust gates. A real third-party language server, public network,
real DeepSeek request, remote CI and unrelated platform/stress matrix are not
required.

## Phase 38 durable per-step time context boundary (2026-08-29)

Phase 38 adds the fixed upstream's durable preparation-time clock semantics to
the terminal product. An explicit `--time-zone <IANA_ZONE>` enables one
snapshot-form `user/message` after each entered `step/start` and before request
derivation. The message records turn, step, an ISO-shaped whole-second
timestamp with numeric offset and canonical IANA zone, and elapsed time from
the preceding model-visible message or the preceding time-context reading.

The time zone is validated once before Session creation, Provider credentials,
plugins or network work. Unlike the browser product, this terminal has no
browser RPC provenance: the explicitly supplied CLI zone is the user-owned
client zone and the durable message says `Terminal time zone`. No zone is
guessed from the server process, Session header or workspace. Existing readings
remain ordinary append-only Session facts and survive resume or compaction;
new readings resume only when the flag is passed to the new process again.

The Agent samples only after cancellation and pre-step policy permit progress.
Sampling, formatting or message construction failure closes the turn with one
stable error before a Provider request. A rejected or cancelled pre-step writes
no reading. The context owns no tool, approval, filesystem, subprocess or
network authority and adds no request-header field. At most one reading exists
per entered step, so the existing 64-step limit also bounds event and token
growth.

Rust keeps the primary official every-step mode and does not expose the
optional positive refresh interval, browser mixed-zone policy or ambient
process-zone fallback in this phase. It uses a pinned IANA/DST-aware Rust time
library instead of mutating process-global `TZ`. These differences are explicit
terminal-product choices and keep compatibility status at `partial`.

Acceptance is local-only: fixed-source fixture; exact zone, timestamp,
duration, source-shape, event-order, request-reconstruction, cancellation,
failure, compaction and resume tests; one real script CLI request in a fixed
zone; and the normal local Rust gates. No real DeepSeek request, public-network
product test, remote CI, browser, Schedule product or extra platform matrix is
required.

## Phase 39 prior-Session event navigation boundary (2026-08-29)

Phase 39 extends the existing same-workspace `session_search` path with the
fixed upstream's `session_event_search` and `session_event_read` tool names.
The first searches semantic events inside one selected prior Session; the
second reads one exact validated event plus optional bounded neighbor
summaries. Together they let the model turn a short cross-session lead into
auditable historical evidence without resuming or mutating the old Session.

Both tools require an explicit canonical `session_id`. Rust deliberately keeps
the Phase 36 authorization boundary: only normally closed, current-version,
strictly replay-valid journals from the exact retained workspace are visible.
The caller, another workspace, a lock-busy live process, malformed history and
an absent id never disclose content. Unlike the upstream live-preferred corpus,
this phase does not read the current Session or another live Agent.

Search accepts one literal, case-insensitive, whitespace-flexible query, returns
at most 20 ranked hits with 240-code-point excerpts, and classifies each event
as current, shadowed or log-only from the same completed projection. Exact read
accepts one safe sequence number and at most 50 neighbors on either side. The
target is rendered as complete pretty JSON; if the complete response cannot fit
the ordinary 64 KiB tool-result limit, the call fails rather than silently
calling a truncated value exact.

Each operation scans at most one 16 MiB journal under the existing shared-lock
and five-second cooperative deadline, reusing the ordinary strict cold scanner.
Cancellation waits for the blocking reader to stop. The only new durable facts
are the current turn's ordinary correlated tool call and result. The tools are
read-only, need no approval and gain no file, Shell, process or network
authority.

At this checkpoint Rust deferred the official optional filters/cursors,
current-session cutoff, session/event lineage traces (added in Phase 40),
live-preferred SQLite index and persistent derived index. Acceptance is
local-only: fixed-source fixture; schema, authorization,
ranking, surface, exact JSON, window, size, corrupt/busy/not-found, timeout and
cancellation tests; one real two-process CLI search → event search → exact read
journey; and the normal local Rust gates. No real DeepSeek request, remote CI,
public network or extra platform/stress matrix is required.

## Phase 40 prior-Session relationship tracing boundary (2026-08-29)

Phase 40 completes the fixed upstream's five-name session-query tool family by
adding `session_trace` and `session_event_trace` to the real CLI. Session trace
shows validated parent ancestry and deterministic descendant trees. Event
trace shows positional replacement chains separately from direct
`sourceEventSeqs` citations and later directly derived events.

Both tools require an explicit canonical Session id and reuse the Phase 36
persisted-only authorization boundary. The target, ancestors and descendants
must come from normally closed, current-version, strict-replay-valid journals
in the exact retained workspace. Caller, busy/live, other-workspace, malformed
and absent Sessions disclose no content. A missing visible parent becomes an
opaque boundary, and a target-connected parent cycle fails closed.

Event relationships are derived during one strict scan of at most 16 MiB.
Session lineage uses the existing store cap and observes at most 64 MiB across
candidate journals. Both operations have the existing five-second cooperative
deadline, wait for their blocking scan to stop after cancellation, are Agent-
serialized and produce no approval, file write, process or network side
effect.

At this checkpoint Rust did not add live/current-Session reads, titles, filters
(added in Phase 41), cursors, SQLite, an index or subagent creation. Acceptance
is local-only: fixed-source fixture;
schema, lineage, ordering, cycle, replacement, source/derived, authorization,
corrupt/oversized, cancellation and timeout tests; the real two-process CLI
journey extended through both trace tools; and the normal local Rust gates. No
real DeepSeek request, remote CI, public network or extra platform/stress
matrix is required.

## Phase 41 bounded Session search filter boundary (2026-08-29)

Phase 41 adds the fixed upstream's public filter fields to `session_search` and
`session_event_search`. The model can narrow old evidence by canonical Session
id, Session creation time, authorized direct parent/root status, persisted
availability, event sequence/time, event type and current/shadowed/log-only
surface before the existing deterministic relevance rank.

Filters are ANDed across fields and ORed within an array. Ranges are inclusive
and timezone-qualified ISO 8601 bounds retain exact sub-millisecond ordering
before being mapped onto Rust's integer-millisecond event clock. Empty,
oversized, malformed, reversed or unknown filters fail before journal reads.

The Phase 36 boundary remains unchanged: only normally closed, strict-valid
journals from the retained workspace identity are visible. `live`-only
availability is a valid empty query, guessed/hidden parent ids do not become
authorized, and no filter grants current-Session, cross-workspace or write
access. Per-journal, aggregate, result, deadline and cancellation limits remain.

Rust adds explicit array/string caps and does not add the upstream `cwd` field
because workspace identity is already mandatory. Cursors, titles,
live-preferred SQLite, indexes and Session export stay deferred. Acceptance is
local-only: source fixture, parser/filter/rank/authorization/cancellation tests,
one real filtered two-process CLI journey and the normal local Rust gates.

## Phase 42 bounded background Shell job boundary (2026-08-29)

Phase 42 adds the fixed upstream's `run_in_background`, `job_list`,
`job_output` and `job_kill` names to the real CLI. A background Bash command
still passes the existing closed parser, workspace preparation, Shell policy,
human approval and final process preflight. Only then does ownership move to a
process-local job registry and the ordinary correlated result returns a
`bash-N` id.

The registry admits at most eight live jobs and retains at most 64 records.
Commands keep the existing 295-second maximum, fixed environment, 8 MiB
observed-output stop, private spill and 64 KiB model-output bounds. Reads can
wait up to 295 seconds without killing the job; cancellation of the read leaves
work alive. `job_kill` requests process-group cancellation without another
approval. Registry shutdown cancels and joins every live job before the CLI
exits.

Background mode is part of the sealed exact-Shell grant identity. A user who
explicitly selects process-local exact reuse can therefore avoid a second
prompt for the exact same detached shape after the first ownership ack is
recorded, while foreground and background calls never share that authority.

Rust intentionally supports final idempotent output only after settlement and
does not implement upstream completion injection/idle wakeups. Jobs are not
persisted or replayed, support only Bash in the one CLI process, and old ids
become unknown after resume. Acceptance is local-only: fixed-source fixture;
schema, parsing, approval, exact-grant mode, completion, wait cancellation,
kill and shutdown cleanup tests; two real terminal journeys; and the normal
local Rust gates. No real DeepSeek request, remote CI, public network or extra
platform/stress matrix is required.

## Phase 43 background-job completion notice boundary (2026-08-29)

Phase 43 closes Phase 42's largest usability gap. An unreported Bash
completion now becomes an ordinary bounded `tool-jobs` notice. A busy Agent
claims it before the next Provider step; an idle interactive terminal opens a
new turn under the official default wake behavior.

The Agent/inbox boundary atomically decides whether a completed step continues
or the owner becomes idle, so a completion cannot disappear between those two
states. At most three consecutive completion turns open without direct human
input. A claimed human message resets that count; Goal and plugin messages do
not. After the cap, notices remain queued for the next ordinary turn.

A terminal `job_output`, an explicit `job_kill`, and registry teardown claim
the report and suppress duplicate notification. Pending delivery is capped at
64 concrete notices plus one overflow fact. Notice messages are recorded only
through the normal Agent step and append-only Session path; the job registry
does not write Session state or bypass approval.

Rust retains the process-local Bash-only, final-output-only and 295-second
Phase 42 boundaries. It does not add incremental output, persistence,
multi-Agent routing, configurable quiet delivery or other producers.
Acceptance is local-only: fixed-source fixture; exact notice/source, queue,
wake budget, same-turn injection, idle-turn, suppression and shutdown tests;
one real linear-terminal wake journey; and the normal local Rust gates. No
real DeepSeek request, remote CI, public-network product run or extra platform
matrix is required.

## Phase 44 incremental background-job output boundary (2026-08-29)

Phase 44 closes the final-output-only usability gap for the existing Bash
producer. `job_output` now owns one consuming cursor with independent stdout
and stderr offsets. A live read returns bytes observed since the prior read; a
repeat without new bytes says `(no new output)`. Unread terminal bytes remain
available once, while `job_list` and internal status reads do not move the
cursor.

The process runner is the only output writer. It publishes exact bounded tails
to a small shared tap before updating the existing spill capture. Falling
behind returns the retained tail plus an explicit loss notice; safe spill paths
are revealed only after final flush. A terminal read still suppresses an
unclaimed completion notice. Wait timeout/cancellation, process-group cleanup,
approval and append-only tool call/result ordering are unchanged.

Rust keeps one process-local Bash producer, a 64,000-byte tail per stream, the
8 MiB observed-output cap and 295-second command cap. It does not add job
persistence, terminal input, PTY sessions, multi-Agent routing or other job
producers. Acceptance is local-only: fixed-source fixture, bounded cursor and
real-process tests, one real terminal journey, one all-target run and the
normal local formatting/compiler/Clippy/diff gates. No real DeepSeek request,
remote CI or extra platform/stress matrix is required.

## Still deferred

- Web or desktop GUI
- Cordis/npm plugin compatibility, arbitrary hooks, hot reload, and native dynamic libraries
- MCP, Hooks, ambient/remote Skills, and subagents
- Background-job persistence, terminal input, PTY sessions, and non-Shell producers
- LSP diagnostics, rename/symbol/call-hierarchy operations, session-query cursors and a persistent derived search index
- Ambient/browser-derived time zones and configurable time-context refresh intervals
- Multiple model providers
- Untested operating systems or sandbox claims
- Feature-for-feature or visual copying of Claude Code
