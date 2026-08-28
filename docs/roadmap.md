# Product Roadmap

This roadmap records implementation status. Phases 0–9 remain the finite v0.1
plan; Phases 10–28 are explicitly approved post-v0.1 extensions. This is a
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

Only one phase may be `in-progress`. A phase becomes `complete` only after its production path, tests, compatibility evidence, validation record, and repository-wide checks pass.

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
themes with transactional redraw, a closed ten-command completion palette, a
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

## Still deferred

- Web or desktop GUI
- Cordis/npm plugin compatibility, arbitrary hooks, hot reload, and native dynamic libraries
- MCP, Hooks, Skills, subagents, and background jobs
- Multiple model providers
- Untested operating systems or sandbox claims
- Feature-for-feature or visual copying of Claude Code
