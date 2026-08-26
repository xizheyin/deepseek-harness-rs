# TUI v2 design

## Status and decision

Phase 11 is a user-approved post-v0.1 product-quality extension. The Phase 9
linear renderer remains the tested escape hatch. Production-reachable slices
now add the owned composer, inline dock, one final truth-safe card per settled
tool, one joined turn receipt, bounded assistant-only presentation, and a
generator-provenanced semantic preview for real `apply_patch` approvals. A
bounded current-turn Inspect and one-summary Review now use the same primary-
screen ledger. Six closed semantic palettes, bounded source-preserving tables,
and a closed seven-command completion palette now use the same transactional
screen ownership. File suggestions, reduced motion, the Session picker,
installed screenshots, and the real-emulator matrix remain incomplete.
Phase 11 therefore stays `in-progress`. It keeps the accepted Agent, Session,
approval, cancellation, and process semantics and replaces only their
interactive presentation and input ownership.

The primary design is a **hybrid inline TUI**:

```text
native terminal scrollback: committed conversation, completed tool summaries,
                            diffs, errors, and work receipts

small dynamic dock:         composer, current activity, queued prompts,
                            approval choice, and contextual key hints
```

The default Focus path does not use the alternate screen. Native selection,
search, copy, and ordinary terminal history remain available on the verified
primary-screen profiles. Auto mode conservatively avoids tmux, GNU Screen, and
Zellij; users may explicitly request enhanced mode there, but no cross-emulator
scrollback guarantee is claimed yet.
Inspect and Review use a temporary, read-only panel inside the
existing primary-screen Dock. The panel is owned by the same `InlineScreen`
ledger as the composer, so committed output continues into native scrollback
above it and no second transcript is retained or replayed. The panel is never
written into scrollback as a report merely because the user opens it. A later
Session picker may use an alternate screen only after a separate terminal
ownership checkpoint proves every partial-write, signal, suspend, resize, and
Drop path; Phase 11 does not rely on that acceleration. A bounded plain
renderer remains
authoritative for `--tui linear`, `--no-color`, `NO_COLOR`, `TERM=dumb`,
non-TTY output, and screen-reader use. `--tui auto` selects enhanced only for a
colored `xterm*` profile with no known multiplexer and an initial size of at
least 44 columns by 12 rows; every other profile starts in linear mode.

## Evidence and comparison boundary

The DeepSeek semantic baseline remains
`deepseek-ai/deepseek-harness@47f943859bef60e4160492346772ded9b24f765a`.
The pinned tree has no built-in human TUI. Relevant fixed paths are the same
Phase 7 seams recorded in `docs/upstream.md`:

- ACP multi-turn, cancellation, approval, and owner cleanup;
- Agent chunk/final/tool event ownership;
- approval asked/decided ordering and cancellation precedence;
- Web conversation partial/final projection, approval composer takeover, and
  running-input queue/steer behavior;
- applied-diff presentation models.

Claude Code's dated public
[interactive-mode](https://code.claude.com/docs/en/interactive-mode),
[fullscreen](https://code.claude.com/docs/en/fullscreen),
[accessibility](https://code.claude.com/docs/en/accessibility), and
[permissions](https://code.claude.com/docs/en/permissions) documents are UX
research only. Rust does not copy its brand, layout, strings, feature set, or
wire behavior. The comparison establishes the modern minimum—editable
multiline input, quiet tool summaries, inspectable transcript, responsive
approval, long-session stability, and accessible fallback—then fixes a
Rust-specific design with deterministic tests.

## Goals

1. Make the user's request and the authoritative assistant result visually
   primary; routine internal activity is secondary and collapsible.
2. Render one user-facing lifecycle for each tool call instead of separate
   requested/arguments/approval/result log lines.
3. Provide a bounded Unicode multiline composer with editing, history,
   bracketed paste, preserved drafts, and explicit next-turn queueing.
4. Show exactly what is running, why the user is waiting, what changed, and how
   to intervene without inventing progress or success facts.
5. Present Markdown, code, diffs, errors, and approvals with a consistent,
   responsive semantic hierarchy.
6. Keep all model/tool/path/error text unable to inject ANSI, OSC, cursor
   movement, bidi controls, or terminal clipboard operations.
7. Preserve the existing tested `Ctrl+C`, `Ctrl+Z`, HUP/QUIT/TERM, EOF, output
   deadline, approval fence, and owned process cleanup behavior.
8. Keep memory, pending input, redraw rate, output, and every dynamic region
   explicitly bounded.

## Non-goals

- no Web or desktop GUI;
- no visual or feature-for-feature Claude Code clone;
- no alternate-screen requirement, mouse requirement, or hidden terminal query
  that races ordinary input; the first Inspect/Review path stays in the owned
  primary-screen Dock;
- no new approval bypass, sandbox claim, Provider, Hook, MCP, background agent,
  or arbitrary status-line command;
- no persisted prompt-history file or secret-bearing UI cache;
- no claim that model-generated explanations are security decisions;
- no Session schema rewrite solely for presentation.

## Product principles

### Attention follows consequence

Routine reads and successful searches are quiet. Assistant results are normal
high-contrast text. A saturated accent appears only for one current focus:
running work, approval, or an error. Green confirms a completed action once;
amber means a decision is required; red means a real failure. Color is always
paired with a word or symbol.

### One fact, one component

A `tool/call`, optional approval pair, and `tool/result` project into one
`ToolActivity`. The component changes state in the dock and commits one final
summary to scrollback. Internal call IDs remain correlation state, but the
first Inspect view reports only their retained/original byte availability.

### Progressive disclosure

- **Focus**: user prompts, assistant answers, compact tool groups, decisions,
  errors, compaction notices, and final work receipt.
- **Inspect**: retained reasoning, payload/identity availability, retry facts,
  and commit times. Turn, step, and committed sequence are its visible
  correlation keys. It says `omitted` when a bounded source is not retained;
  it does not call bounded data complete.
- **Review**: changed-file summaries, proven process outcomes, failures, and
  the joined turn receipt. Canonical diffs and full command records require a
  later closed presentation supplement and are not reconstructed from prose.

No fact is silently deleted. The view changes only the default presentation.

### Truth before animation

The TUI consumes committed Session facts. A local activity may say `Waiting`,
`Preparing`, or `Cleaning up`; it must not say `Running`, `Changed`, `Passed`,
or `Done` until the owning production path proves that fact. A spinner is
delayed for 300 ms so short operations do not flash. Elapsed time appears after
one second. At five seconds, the dock adds the concrete wait/cancel hint that
the host actually knows.

## Visual system

### Semantic roles

```text
text.primary       user and assistant content
text.muted         timing, counts, key hints, secondary metadata
accent             current focus and input cursor
success            completed effect
warning            approval, limit, retry, compaction pressure
danger             error and unresolved outcome
border              composer/dock divider only
diff.add           inserted lines and positive diffstat
diff.remove        deleted lines and negative diffstat
selection          focused menu item, never the only focus signal
```

The terminal background is inherited. Conversation messages have no box.
Borders are reserved for the composer, approval choice, serious errors, and
temporary menus. Bold is used for one heading or selected action, not every
status line. Icons have one stable meaning:

```text
● running     ✓ success     × failure     ? approval
○ denied/skipped            ◇ system/compaction
```

Built-in palettes are Adaptive, Midnight, Paper, Color-blind, High Contrast,
and Mono.
Adaptive uses the terminal's ordinary ANSI palette so it remains usable on
unknown light/dark backgrounds. Themes are semantic token maps, never embedded
escape strings in business events. Reduced Motion is a later product slice;
when implemented it will disable spinner animation and use text changes only.

The first theme checkpoint keeps that surface deliberately closed. Exact local
commands `/theme adaptive`, `/theme midnight`, `/theme paper`,
`/theme color-blind`, `/theme high-contrast`, and `/theme mono` select one of
six compile-time palettes; `/theme` reports the current choice and the finite
list when the width-aware Dock has room, while narrow layouts may visibly
truncate that notice. The parser's six-name set remains exact at every width.
Unknown names are local input errors and never become model prompts or next-turn
queue entries. Selection is process-local presentation state, not a Session
fact, and a resumed process starts from Adaptive again.

Every palette maps the same closed `TextStyle` roles to fixed SGR sequences.
No palette sets a background, emits OSC, queries the terminal, or accepts a raw
escape string. Mono uses only attributes; High Contrast and Color-blind retain
the same textual icons and labels, so color is never the only status signal.
A switch redraws only the owned bottom surface. It does not replay or recolor
native scrollback, and future transcript chunks use the new palette only after
the redraw transaction commits. Resize, partial output, suspend, approval
takeover, and terminal failure keep the existing poison/recovery and
default-Reject rules. Linear mode recognizes the same exact commands locally
but reports that it is always plain, stores no inactive palette choice, and
emits zero ESC bytes.

## Responsive layouts

### 112 columns × 34 rows

```text
 dsh · ds-harness-rs · deepseek-v4 · Manual · context 42%

 YOU
 Fix the authentication timeout and run the related tests.

 DSH
 I will inspect the request path and its timeout tests first.

   ✓ Read      src/auth/session.rs                         12 ms
   ✓ Search    "request_timeout"                    8 matches
   ✓ Updated   src/auth/session.rs                        +12 −3
   ● Testing   cargo test auth                              8.2s
               Running the focused suite             Ctrl+C stop

──────────────────────────────────────────────────────────────────────────────
❯ Continue typing; Enter queues the next turn…
  Enter queue · Ctrl+J newline · @ files · / commands · ? help
```

### 80 columns × 24 rows

```text
 dsh · ds-harness-rs · deepseek-v4 · context 42%

 YOU  Fix the authentication timeout and run its tests.

 DSH  I will inspect the request path first.

   ✓ Read     src/auth/session.rs
   ✓ Search   "request_timeout" · 8 matches
   ● Testing  cargo test auth · 8.2s

────────────────────────────────────────────────────────────
❯ Draft the next message…
  Enter queue · Ctrl+J newline · Ctrl+C stop
```

### 44 columns × 20 rows

```text
 dsh · deepseek-v4 · 42%

 YOU
 Fix the authentication timeout.

 DSH
 I will inspect the request path first.

 ✓ Read src/auth/session.rs
 ● cargo test auth · 8.2s

────────────────────────────────────────────
❯ Next message…
  Enter queue · Ctrl+C stop
```

An initial terminal below 44 columns or 12 rows starts in the linear plain
presentation. Once enhanced mode owns the terminal, a resize down to 12×5 uses
a four-row compact rescue dock so drafts and approvals remain visible. A resize
below 12×5 clears uncertain geometry, restores the terminal, and fails closed;
switching a live cbreak session into canonical linear input is not guessed.

## User-facing state

The UI does not copy the Agent state machine. It owns how committed facts and
local input are presented:

```rust
enum Interaction {
    Idle,
    Running { turn: TurnId },
    Approving { turn: TurnId, request: ApprovalRequestId },
    Cancelling { turn: TurnId },
    Suspended,
    Exiting,
}

enum ViewMode {
    Focus,
    Inspect,
    Review,
}
```

`UiState` also owns a `Composer`, bounded `PromptQueue`, current
`TurnPresentation`, dock geometry, a transactional view mode, theme,
scrollback commit marker, and approval focus. `ViewMode::Inspect` and
`ViewMode::Review` hide the composer visually but never move or clear its
draft, undo state, history navigation, or queued prompts. A pure reducer
consumes:

```text
keyboard / paste / resize / timer / signal / committed Session fact
                                ↓
                             UiEvent
                                ↓
                         UiState + UiEffect
                                ↓
submit/queue/cancel/approve/write/suspend/exit
```

No lock is held across an async effect. Rendering cannot call Provider, tools,
approval policy, Session mutation, filesystem operations, or subprocesses.

One `TerminalSession` owns the terminal descriptors, the exact original
termios, the derived application termios, and bracketed-paste state. The first
Inspect/Review implementation does not add alternate-screen state. One
`InteractiveDriver` event loop is the only TTY
reader and writer and the only owner that changes input focus. Decoder,
composer, projector, layout, and renderer are pure or synchronously bounded
components beneath that owner; no second input task may race them.

## Presentation vocabulary

```rust
enum TimelineItem {
    User(UserMessageView),
    Assistant(AssistantView),
    ToolGroup(ToolGroupView),
    Decision(DecisionView),
    Error(ErrorView),
    Notice(NoticeView),
    Compaction(CompactionView),
    Receipt(WorkReceiptView),
}

enum ToolActivityState {
    Preparing,
    AwaitingApproval,
    Executing,
    Settling,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
    OutcomeUnknown,
}
```

`tool/call` proves only committed intent, so it enters `Preparing`, never
`Executing`. `Executing` requires a bounded supplement from the owning runtime
after it has dispatch evidence. Approval Allow proves permission, not dispatch.
If no live supplement exists, the UI stays at `Preparing`/`Settling` until the
committed result provides the final state. A turn closing with an unresolved
call becomes `OutcomeUnknown`, never implicit success.

The projector correlates tool state by `(turn, step, call_id)` and approval by
request ID. It produces bounded, tool-specific summaries without changing the
persisted event schema:

- `list`: relative path and returned/truncated entry count when provable;
- `glob`: pattern and returned/truncated path count;
- `grep`: pattern and match/file count when provable;
- `read`: relative path and visible line range/count;
- `apply_patch`: relative file, create/update class, `+N −N`, and canonical
  diff in Review;
- `bash`: visibly escaped command, relative workdir, elapsed time, and exact
  exit/timeout/signal state;
- plugin: configured public plugin/tool IDs and normalized result state, never
  executable path, argv, stderr, or internal protocol ID.

If a structured count is unavailable, the UI says only what is known. It never
parses arbitrary command text to invent a test count or success claim.

The live presentation contract has two fact sources. `CommittedUiEvent`
projects durable ordering, user/assistant messages, retry, usage, request
context, compaction, approval, and tool correlation IDs from a committed
`SessionEvent`. A bounded `ToolPresentation` or `ApprovalPresentation` may be
attached only by the owning Agent/tool path after the corresponding Session
fact commits. It contains closed, tool-specific display facts such as a
workspace-relative path, canonical diffstat, exit classification, or plugin
public ID. It never carries an executable path, plugin argv/stderr, secret,
unbounded tool body, or an uncommitted success claim. The projector may show a
generic lifecycle when this presentation is unavailable; it must not parse an
arbitrary result string to synthesize one.

The initial event projection must stop reducing these user-facing facts to a
type name: user message content; token usage; provider/model/context window;
safe compaction phase/count fields; retry delay/failure; and assistant source,
usage, provider, and model. Compaction summary/raw-output bodies remain hidden.
Historical receipts are rebuilt only from facts actually retained by the
Session; absent timing or tool-presentation facts remain absent.

## Composer and queue

The enhanced terminal uses a long-lived, owned cbreak mode: `ICANON`, echo,
`ECHONL`, `ICRNL`, `IXON`, and `IXOFF` are disabled; `ISIG`, `IEXTEN`, output
post-processing, and the validated interrupt/suspend/quit characters remain
enabled; and `VMIN=1`/`VTIME=0`. Clearing `ICRNL` is what keeps carriage return
(`Enter`, submit) distinct from line feed (`Ctrl+J`, newline). This is not
Crossterm raw mode. TTY identity/foreground validation is separate from
canonical/application-mode validation. The exact original termios is restored
before suspension, exit, terminal failure, or returning an error.

Approval no longer owns a temporary termios guard. It changes only input focus
inside the long-lived application mode. The existing arming barrier remains:
the trusted preview must finish writing, input must stay quiet for 100 ms, the
kernel input queue is flushed once, and the decoder epoch is reset before an
approval key can count.

The composer supports:

- UTF-8 assembly across reads and grapheme-safe cursor/delete operations;
- Left/Right, Home/End, Ctrl+A/E, Backspace/Delete, Ctrl+W/U/K, `Ctrl+_`
  undo, and bounded yank. `Ctrl+Z` remains the kernel suspend character and is
  never reused for undo;
- Up/Down bounded in-session history when the cursor cannot move vertically;
- Ctrl+R bounded reverse history search;
- Ctrl+J or Shift+Enter where the terminal reports it for a newline; Enter for
  submit/queue;
- bracketed paste whose newlines never submit and whose control bytes that
  actually reach the application are rendered as visible text. `VINTR`,
  `VSUSP`, and `VQUIT` remain kernel signal characters even inside paste
  because preserving `ISIG` is the stronger cleanup and safety contract;
- slash-command and bounded file-suggestion modes with explicit focus;
- a draft that survives running work, approval, resize, cancellation, and
  temporary menus.

While a turn runs, Enter creates a visible **next-turn queue item**. It does
not steer the current model request and does not enter Session until it becomes
the next admitted turn. Up from the first composer row retrieves the newest
queued item for editing. `Esc` closes a temporary menu; during approval it and
`Ctrl+C` stop the active turn and retain queued prompts. Queue capacity is eight prompts,
64 KiB per prompt, and 256 KiB aggregate. Overflow is a visible local error and
does not drop the draft.

Queued prompts are admitted FIFO only after the current turn, required cleanup,
and Session settlement complete. A fatal error, explicit exit, terminal loss,
or shutdown never sends them automatically; they remain visible until the
process can safely offer editing again, otherwise they disappear with process
memory and were never recorded as user messages.

Prompt history currently records committed human prompts observed by this
process and keeps them only in bounded memory. Rebuilding it from a resumed
Session snapshot is still part of the later Session-picker/history slice.
Phase 11 writes no separate history file.

Attaching the UI returns a `UiFeed` with a bounded initial snapshot plus the
live receiver. A new Session has an empty snapshot. A resumed Session rebuilds
only the current model-visible surface, model/context facts, and safe
compaction markers; it marks `history_truncated` when the append-only journal
cannot be reconstructed through the existing public Session view. Live events
start strictly after the snapshot sequence and must not duplicate it. Phase 11
does not claim complete historical timings, tool receipts, or hidden compacted
messages without a separately bounded journal scan.

## Scrollback and dock rendering

Completed transcript blocks are appended exactly once through structured
`PresentedChunk` runs. `InlineScreen` is the sole cursor owner: the primary
screen always uses full-screen scrolling (`CSI r`, never a partial DECSTBM), a
fixed-height dock occupies the bottom rows, and the composer uses a software
cursor. Initial attachment emits `dock_rows + 1` full-screen line feeds so the
pre-existing bottom of the terminal moves into native history instead of being
overwritten. Input-only redraws clear and replace only the owned dock and never
replay transcript text.

Each coordinate batch is staged against one ledger generation and committed
only after the whole write succeeds. A zero-byte resize can be restaged. A
partially written coordinate batch poisons the ledger; the driver then clears
the uncertain visible viewport with ED2, keeps bracketed paste enabled during
in-process recovery, establishes a fresh transcript boundary, and redraws the
dock. Suspend and exit use a separate reset that disables paste and shows the
real cursor before restoring termios. Clearing the viewport is an intentional
failure-path tradeoff: it prevents a partial private draft or approval from
being scrolled into history, while committed facts remain in Session.

`SIGWINCH` or a detected winsize change recomputes Unicode display widths and
starts a fresh transcript boundary; it does not claim that an unfinished
physical line can be portably reflowed after the emulator has already resized.
The small deterministic terminal model covers full-screen-only and
top-anchored history policies, while real xterm/iTerm/Terminal/VS Code shrink,
reflow, and copy behavior remains a release-checkpoint matrix. Resize cannot
decide an approval or submit a prompt.

Ordinary stream refresh is capped at 30 frames/second, spinner animation at
8 frames/second, and idle mode has no periodic wake. Input, signal, approval,
terminal failure, and final committed facts take priority over animation ticks.
Each output batch keeps the existing absolute five-second write deadline.

## Markdown, code, and diff

The production path supports a bounded, streaming-safe subset only for
assistant text:

- paragraphs and terminal soft wrapping;
- headings made from one to three `#` characters followed by a space;
- `- `, `* `, and `+ ` bullets, plus one- to three-digit numbered markers
  followed by `. `;
- `> ` block quotes;
- paired single-backtick inline code;
- line-leading triple-backtick fences whose optional language label is at most
  32 ASCII bytes and contains only alphanumerics or `_+.-`;
- case-insensitive `diff` or `patch` fences with visual file-header, hunk,
  addition, deletion, and context styles.

The parser does not decode HTML entities or interpret raw ANSI. Emphasis,
links, images, and HTML rendering remain unimplemented. Assistant
fenced `diff`/`patch` remains presentation-only and never proves a file effect.
Real `apply_patch` approval uses a separate provenance path described below;
it never promotes assistant text or a generic diff-looking prompt.

The table checkpoint recognizes one deliberately source-preserving subset. A
candidate header, delimiter, and every body row must start and end with `|`.
The delimiter has the same 2–8 columns as the header; each trimmed delimiter
cell is three or more ASCII hyphens with optional leading/trailing `:`. Body
rows must keep that column count, and every trimmed header cell is non-empty.
Escaped pipes, multiline cells, nesting, and
column spans are outside this subset and remain ordinary assistant text.

Recognition buffers only the candidate/table line needed to avoid styling a
false header. Once the delimiter commits the table, the renderer preserves
every source byte and line feed: pipe separators and the delimiter use
`Border`, header cell text uses `Heading`, and body cell text uses `Assistant`.
It does not pad or rewrite cells, so native terminal wrapping remains truthful
at 44, 80, and 112 columns and copied text stays the model's exact Markdown.
Linear mode keeps the same literal source with zero ESC bytes. An incomplete
candidate, stream correction, retry, cancellation, or non-authoritative abort
flushes held bytes as ordinary assistant text; only an authoritative final may
accept a valid final body row without a trailing line feed.

One table keeps at most 8 columns, 64 body rows, 16 KiB per physical source
row, and 64 KiB aggregate source. Exact limits remain semantic. A one-over row,
column mismatch, invalid delimiter, or aggregate overflow closes recognition
and renders that line and later text normally; it never omits Session facts or
cancels the Agent turn. Visible-control sanitization happens before recognition,
and only the closed semantic styles reach `InlineScreen`.

Untrusted content is converted to visible text before parsing, and the closed
presentation builder rejects terminal controls again. Only a matching
authoritative assistant final may recognize a closing fence at EOF without a
trailing line feed. A stream-key change, retry/correction boundary, `StepEnd`,
`TurnEnd`, or cancellation aborts pending syntax and flushes it as ordinary
assistant text; an abort never promotes incomplete output to code. Code and
diff retain copyable source text in native scrollback. Linear mode retains
reasoning in its established plain record; enhanced Focus suppresses it and the
bounded Inspect archive owns its current-turn presentation.

## Approval

The committed `approval/asked` fact and the matching trusted preview must join
before the approval UI exists. The preview is appended once to scrollback; the
dock then presents the decision:

```text
Approval required
Proposed update · not applied
  src/message.txt · +1 -1 · 1 hunk
  One workspace file · no shell command

--- a/src/message.txt
+++ b/src/message.txt
@@ -1 +1 @@
-old
+new

Approval required | proposed action above
  Allow once | apply exact preview
> Reject | make no change
  Stop turn | cancel work
Not sandboxed | Reject is the safe default
Arrow keys move | Enter confirms | Esc stops
```

The patch tool attaches closed provenance while it builds the canonical
single-file diff. This is a Rust type boundary, not a cryptographic signature.
One immutable preview string remains the source for the approval card and the
eventual tool-result `meta.diff`; a compact, bounded row-kind vector records
file headers, hunks, context, additions, removals, and no-newline markers.
Operation, workspace-relative path, hunk count, and `+N/-N` are produced at the
same boundary. This is process-local presentation provenance and is not added
to the Session schema.

`ApprovalPrompt::new` always creates an opaque preview. A tool name, model
reason, raw tool arguments, result prose, or text that merely resembles a diff
can never acquire patch styling. Only the patch preparation path can construct
the closed canonical-patch presentation. The row vector is aligned with every
physical preview line and is validated before the prompt can exist. This also
keeps hunk content such as `--- a/decoy` classified as a deletion rather than a
file header. The enhanced presenter first makes every variable character
terminal-visible, then applies the provenance-tagged row styles without
changing the copyable text. The linear renderer ignores the metadata and
preserves its complete zero-ESC record.

The card says **proposed** / **not applied** until a real tool result arrives.
It shows the closed operation, path, hunk and line counts, the fact that this is
one workspace file with no Shell command, and the complete diff. A malformed
or resource-invalid semantic preview fails before the selector can accept
input; it is never partially trusted or silently truncated. Generic Shell and
plugin previews remain opaque Warning text.

Shell and plugin approvals visibly state that native execution is not a
sandbox. Risk statements come from closed local action contracts. The model's
reason is displayed separately and never changes the decision policy. Reject
is selected by default. Only a directional navigation key received in the
current armed modal epoch may focus Allow, and a later Enter confirms it.
Printable `y`, stale Enter, paste, unknown CSI, resize, output failure, or a
lost approval owner cannot select Allow. `Esc` and `Ctrl+C` choose Stop turn;
closing the modal without a decision would strand the Agent, so it is not a
state. The remainder of the read batch containing a decision is discarded
rather than becoming composer input. Plain mode uses an explicit
`Approve this action? [y/N/c]:` record with the same default-deny semantics.

## Work receipt, context, and sessions

At turn end, the enhanced renderer appends one compact receipt from projector
facts only after the committed `turn/end` and returned `TurnOutcome` agree on
turn, sequence, and reason. The current Focus slice renders exact
step/tool-request/retry/output-token counters, strict patch effects, strict
foreground-Shell starts, and issue counts:

```text
Turn complete
  5 steps | 4 tool requests | 1 retry | 842 reported output tokens
  2 files changed (+12 -3) | 1 command run | 1 issue
```

It cannot claim test counts or pass status unless a structured, trusted source
provides them. The current slice also does not claim an execution duration.
The current Review expands trusted changed-file and process/plugin outcome
summaries, errors, denials, cancellations, and unknown outcomes. Canonical
diffs and full command records still require a later closed supplement.

Focus does not claim a live context percentage from the latest Provider usage.
The first truthful status line uses the Session projection's own bounded
surface estimate, sampled by the CLI before and after a turn, and labels it as
an estimate. Inspect separately shows the latest reported input/output/cache
usage and the configured context window. A route change never combines a new
model with an older window or usage record. If the safe join is unavailable,
the percentage is omitted rather than guessed.

Compaction emits one quiet marker and an Inspect expansion; it does not expose
the summary or raw Provider output. A successful marker says that earlier
context was summarized and, when known, that the shadowed nodes were estimated
at a given token count. It never says that exactly that many tokens were
deleted or freed. A prune marker is not called complete until its matching
surface replacement commits.

### Bounded view archive and primary-screen detail panels

`LiveRenderer` remains the sole owner of `UiProjector` for the first detail
slice. Beside it, one `ViewArchive` retains only the current turn and one
frozen last-successfully-joined review. It observes complete
`CommittedUiEvent` values so sequence and commit time are not discarded, but
it does not reimplement tool outcome correlation. Inspect and Review builders
read this archive and the projector through immutable snapshots.

The archive is presentation state, not a second Session log:

- Inspect retains bounded reasoning text, retry facts, approval outcomes,
  compaction phases, and payload/identity availability
  (`retained/original bytes`, omitted parts). Raw tool arguments/results/meta
  and literal internal correlation IDs are not shown by default and never
  enter Debug output. Turn, step, and committed sequence are the visible
  correlation keys in this first slice.
- Review is frozen only after the committed `turn/end` and returned
  `TurnOutcome` agree on turn, sequence, and the receipt-relevant reason key.
  It contains trusted changed-file summaries, strict foreground process
  outcomes, plugin settlement facts, issues, and the same receipt used by
  Focus.
- The first Review slice is deliberately summary-only. It does not call Git,
  reread the workspace, parse model/tool prose, or claim a complete historical
  diff/command record. A canonical diff may appear later only through a closed
  post-commit presentation supplement from the owning patch path.
- A resumed observer begins at the live seam. Inspect therefore says that
  earlier details are unavailable, and Review does not synthesize historical
  counters or timings without a retained `TurnOutcome` join.

`ViewMode` has a desired state and a screen-committed state. A key may request
Inspect or Review, but the transition becomes visible only after the matching
`InlineScreen` transaction commits. Changing panel height uses the same-size
re-anchor path: clear every old owned bottom row, scroll only the additional
height needed when a panel grows, retain the existing transcript cursor when a
panel shrinks, then draw the new fixed-height panel. It does not reuse the more
conservative physical-resize path, which intentionally creates a completely
new boundary. Closing restores the unchanged Composer Dock.
Program-driven transitions therefore do not append panel snapshots to native
history or replay transcript facts.

The terminal emulator resizes before `SIGWINCH` reaches the process. A hostile
or unusual emulator may therefore move old primary-screen rows into its own
history during shrink before dsh can clear them. Phase 11 can prove that dsh
does not actively append or duplicate panel text; it does not claim universal
control over emulator-owned resize history.

While a detail panel is open, the event loop continues draining and reducing
the bounded observer. Assistant/tool output still commits once above the
panel, while the panel coalesces redraws. `Ctrl+C` keeps its existing turn
cancellation meaning. Paste, Enter, printable text, and unknown CSI cannot
submit, queue, or approve from a detail panel. `Ctrl+O`, `Esc`, or `q` returns
to Focus; arrows and PageUp/PageDown scroll. Exact local `/inspect`, `/review`,
and `/focus` commands never enter Session or the next-turn FIFO.

Approval has higher focus than a detail panel. Once a matching committed
approval question exists, the driver first commits a panel-to-Focus Dock
re-anchor. Only then may it append the trusted preview. After that preview
commits, the existing 100 ms quiet period, input flush, decoder epoch reset,
and default-Reject selector arm exactly as before. Approval never automatically
returns to the old detail view.

`dsh --resume` without an ID may later open a bounded picker after the Session
root and workspace policy are validated. The first picker may show only facts
already present in the bounded session listing: a safe workspace basename,
creation time, and shortened ID. A last-message summary or last-active time
would require a separately bounded read-only journal scan and is not inferred
from header metadata. No history is opened, mutated, or resumed merely by
moving selection.

## Commands and suggestions

`/` opens a finite command palette whose entries are product-owned. `@` asks a
bounded read-only suggestion provider supplied by CLI assembly; the TUI itself
does not traverse the filesystem. Suggestions are capped, cancellable, and
visibly relative to the workspace. Selecting a suggestion inserts text only;
it never reads the file into the model request by itself.

Ctrl+O opens Inspect, while `/review` opens Review. `/inspect` and `/focus`
provide explicit equivalents. In a running turn these exact commands change
the local panel immediately; they are never queued as model prompts. During an
approval, the approval focus wins and view-switch keys cannot authorize or
replace the question. Focus/Inspect/Review, theme,
reduced motion, help, status, sessions, exit, and quit are local commands.
Commands that would change Agent or Session semantics must follow the ordinary
audited boundary rather than being hidden UI actions.

The first command-palette checkpoint is narrower than that future combined
surface. It contains exactly the seven local commands that already exist in
production: `/help`, `/inspect`, `/review`, `/focus`, `/theme`, `/exit`, and
`/quit`. Theme names remain arguments to `/theme`; future commands such as
status, sessions, Reduced Motion, or file suggestions must not appear before
their own production path exists. The closed entry table owns a short ASCII
description for each command, and neither model, Session, workspace, nor
configuration text can add an entry. This listed order is also the stable menu
order, and the first match is the default selection.

The enhanced driver owns one `CommandPaletteState` beside `InputMemory`; it
stores only an optional closed command identity and an optional dismissed
Composer revision. Composer continues to own text/cursor/revision, and Dock is
a pure layout consumer of an immutable palette snapshot. Resize, approval
takeover, and a temporary detail view do not copy or mutate palette state. A
Composer content edit clears a dismissal; cursor movement and resize do not.

Enhanced Focus derives palette visibility only when the **entire** draft is a
single line whose first byte is `/` and whose cursor is at the draft's byte end.
Leading whitespace, a slash after other text, any LF, or a cursor inside the
draft hides it. Filtering is case-sensitive ASCII prefix matching against the
seven closed spellings. Selection stays by command identity while the prefix,
width, or height changes only while that identity remains in the current match
set. If an edit removes it, selection moves to the first match in closed-table
order; no matches means no selectable identity. Up/Down move within matches,
Tab/BackTab move forward/backward; all four clamp at the first/last match rather
than wrapping. Every palette-navigation key ends the current decoder read
batch, even when clamping means the identity did not change, so a following
Enter byte cannot complete or submit in that batch. A timed standalone Esc
dismisses the palette without changing the draft. Any later edit clears that
dismissal and recomputes the finite matches. An unknown prefix may show a fixed
`No matching local command` row; that row cannot be activated, and the draft
still remains an ordinary prompt if the user later submits it. While this
empty-state row is visible, Up/Down/Tab/BackTab are no-op menu keys that still
end the read batch; they never fall through to history or queue recall.
Rejected key input or a rejected paste dismisses a visible palette at the
current Composer revision, reports the existing local error, and leaves draft
and selection identity unchanged; a later content edit may reopen it.

Enter on a non-exact selected prefix replaces that token with the selected
command and ends the current decoder read batch. It does **not** submit, queue,
open a view, change theme, or exit; only a fresh later Enter may pass the
existing local-command classifier. Enter when the Composer already equals one
complete command keeps today's ordinary submit behavior. Paste never selects a
menu item: it only edits the draft, after which the palette may be derived for
display, and the paste-completed input fence still ends that read before any
completion or submit. Completion atomically replaces the entire draft as one
undoable Composer edit, places the cursor at the command end, detaches any
InputMemory history/reverse-search navigation, and leaves queue/history
contents unchanged.
Approval focus suppresses the palette, and Inspect/Review keep their existing
input rules. During a running turn, completing `/inspect`, `/review`, `/focus`,
or `/theme` still cannot queue it; the fresh submitted command follows the
existing exact local-command path. All seven exact commands are classified
before `PromptQueue` in both idle and running states and never enter the FIFO or
Session. `/help` stays local; view/theme commands keep their current local
actions; `/exit` and `/quit` use the existing owned shutdown path, including
turn cancellation, required cleanup, Session settlement, terminal restoration,
and no automatic admission of queued prompts. They therefore cannot be
triggered by an arrow and Enter arriving in one terminal read.

In a non-compact Dock, visible menu rows are
`min(matches-or-empty-row, width_cap, rows - 8)`, where `width_cap` is three
below 60 columns and seven otherwise; the subtraction preserves the ordinary
status/divider/four Composer/hint rows plus at least one transcript row. Thus
80/112-column layouts with at least 15 rows may show all seven, while the 44×12
threshold shows at most three selected-centered rows. In every compact layout
the selected command (or fixed empty row) replaces the ordinary status row, so
the Composer and compact `Enter · Esc` hint remain visible with one transcript
row still available; a real selection also keeps its explicit `>` marker.
Wider layouts use `Enter complete · Esc close`. Truncation changes only product-owned
descriptions, never command spelling. Linear mode retains its current zero-ESC
whole-line behavior and prints no dynamic palette.

For `n` visible match rows and selected index `s`, the deterministic window
starts at `min(s.saturating_sub((n - 1) / 2), matches.len() - n)`; even windows
therefore keep one more row below the selection until clamped at an edge. A
wide no-match row uses `No matching local command`. Compact no-match uses the
separate product-owned `! No match` spelling and no `>` selection marker, so it
fits the 11-cell rescue width without pretending the empty row is actionable.

The next suggestions checkpoint adds one product-owned `@` source for regular
workspace files. This is an intentional terminal feature, not a claim that the
fixed upstream has file mentions. The fixed upstream Web client supplies a
useful interaction baseline in
`packages/client/ui-input-trigger/src/{types,core/detect,client/controller}.ts`:
it detects whitespace-bounded `/`/`@` tokens, stamps the draft revision into a
pick span, supplies a cancellable request, and ignores stale generations.
Its shipped `@` provider in `packages/client/ui-subagent/src/client/index.ts`
lists running child agents and inserts literal `@label ` text; it does not list
files or read file content. Rust adopts the cancellable request, generation,
literal-insertion, and compare-at-swap ideas while deliberately
using one closed workspace-file source instead of the upstream dynamic browser
registry, reference codec, or subagent meaning.

The enhanced driver owns one `FileSuggestionController` beside `InputMemory`
and `CommandPaletteState`. CLI assembly gives it a clone of the same retained
`WorkspaceAuthority` capability used by local tools; no path is reopened from
ambient process state. The controller owns the current token hit, monotonic
activation, blocking-job, and menu revisions, an optional selected relative
path, an optional dismissed Composer revision, one optional bounded catalogue,
and at most one owned blocking job plus its cancellation token.

The pure TUI state never traverses the filesystem. Dock borrows an immutable
requested snapshot containing only status, relative paths, the selected index,
and a bounded menu revision. The controller separately owns the presented
`Absent | Valid | Invalidated` state for suggestion surfaces. No absolute
workspace root, file bytes, modification
time, Session object, model object, Provider credential, or approval capability
crosses this seam.

`Debug` is manually redacted for the controller, lifecycle states, token hit,
blocking-job result, requested/staged/presented snapshot, and presentation
credential. It may expose only closed state names, activation/job/menu
revisions, counts, byte lengths, capped/handle/capability booleans, and selected
index presence. It never formats query text, relative or absolute paths,
catalogue entries, selected path text, raw I/O errors, or terminal controls.

An active file token is derived from Composer whenever all of these rules hold;
it is visible and input-actionable only in enhanced Focus:

- the cursor is at the byte end of a non-whitespace token, so completion never
  leaves an unseen suffix behind;
- scanning backward from the cursor reaches an `@` before any Unicode
  whitespace; an `@` that fails the boundary rule is ordinary token text and
  scanning continues left, so `@foo@bar` uses the first `@`;
- the character before `@`, if any, is Unicode whitespace or is neither a
  Unicode alphanumeric character nor `_`; this keeps `user@host` inert while
  allowing `(@src` and `see @src`;
- the query after `@` is at most 1,024 UTF-8 bytes and contains no control
  character.

The hit records `{start, end, content_revision}` on UTF-8 boundaries. An `@`
token may appear at the start of a draft or inline after ordinary prompt text,
including on a later line. `/` inside the file query is ordinary path text. If
an `@` hit is active, its menu has input priority over the whole-draft command
palette; otherwise the existing `/` behavior is unchanged. Moving the cursor
inside a token, typing whitespace, submitting, clearing the draft, or a
revision-scoped Esc dismissal closes the active token. Inspect/Review instead
suppresses its presentation and input without closing the token or changing its
activation, so bounded work may settle while the detail surface is visible. A
later content edit clears the dismissal and recomputes the hit. Linear
presentation treats every `@` as ordinary prompt text and performs no workspace
scan.

The controller lifecycle is explicit: `Dormant`, `Scanning`, `Filtering`,
`Cancelling`, `Ready`, or `Failed`. `Scanning` owns the activation and job revision,
cancellation token, and one `spawn_blocking` filesystem join handle.
`Filtering` moves the retained `Vec<String>` catalogue into one cancellable
blocking ranking job for the latest query; its join result returns both the
unchanged catalogue and the ranked roster. Cancellation or a checked filtering
failure also returns catalogue ownership, so the latest pending query can retry
without a second filesystem scan; a join panic loses it and degrades to
unavailable. `Cancelling` keeps the same single
handle, its work kind, and at most one latest pending hit; a filtering handle
still owns the catalogue until it joins. Thus
the controller owns at most one blocking job of either kind. Closing the token
or approval takeover cancels and hides immediately; it never awaits cleanup on
the input or approval path.
Reopening while `Cancelling` only replaces the bounded pending hit. The event
loop polls the old join handle alongside terminal, Session, and signal work,
recovers every returned owner, and only then may start pending work. If a
cancelled filter and pending hit have the same activation, query supersession
discards only the stale roster and reuses the returned catalogue. If the token
closed, approval suppressed it, or the pending hit has a new activation, the
returned catalogue is discarded and reopening performs a fresh scan. A join
handle is never overwritten or dropped to detach work. `Ready` owns one
catalogue plus the ranked requested snapshot for the current hit, while
`Failed` owns only the redacted local status.

Query edits during `Scanning` update the latest query without restarting the
filesystem walk; the scan's settled catalogue is filtered only for that latest
query. Query edits during `Filtering` cancel the ranking job and replace the
pending query; a new filter starts only after the old handle joins. Query edits
during `Ready` retain the catalogue and enter `Filtering`. Filesystem scanning
therefore occurs once per active token, while every potentially adversarial
ranking pass remains off the UI worker and cancellable.

The first active token starts one lazy catalogue scan and shows `Scanning
workspace...`. The complete scan runs on Tokio's blocking pool, not on an async
or UI worker. It uses an iterative depth-first walk with at most 64 descendant
levels and at most root-plus-64 directory handles. Every descendant directory
is opened component by component relative to the retained capability with
`O_NOFOLLOW`; a directory-to-symlink replacement therefore fails the whole
catalogue without returning a partial result. The worker checks cancellation
before each open, directory batch, and entry, closes frames as it leaves them,
and never uses recursive Rust calls or retains one descriptor per wide child.
An individual kernel directory call cannot be forcibly cancelled and may still
delay task settlement; this design makes no hard-timeout claim for stuck kernel
I/O.

The scan accepts only UTF-8 regular-file paths without control characters and
never follows a symlink. The closed skipped-directory set is `.git`, `.svn`,
`.hg`, `.bzr`, `.jj`, `.sl`, `target`, `node_modules`, `.venv`, `venv`,
`.cache`, `.next`, `__pycache__`, `build`, and `dist`. Every other directory,
including other dot directories, is eligible. Exact component equality applies;
names such as `builder` are not skipped.

Every directory entry observed before file-kind or skip decisions counts toward
the 10,000-entry ceiling, including directories, symlinks, and special files.
After 10,000 admitted entries the worker performs one non-retaining probe: EOF
accepts the catalogue, while a 10,001st item fails it. The 8-MiB counter is the
checked sum of each validated relative display path exactly once, including
skipped and non-file entries; exactly 8 MiB is accepted and byte +1 fails.
Catalogue strings, DFS frames, and directory batches move those charged paths.
Vector capacities, the at-most-10,000 fixed rank records, the selected path,
and three at-most-256-KiB requested/staged/presented roster copies are
independently bounded. Candidate copies use `String::try_reserve` and preserve
the exact source text. Every explicit collection growth uses checked arithmetic
and `try_reserve`; its capacity failure becomes the same redacted unavailable
state. As elsewhere in this Rust binary, unrecoverable global allocator
exhaustion may abort the process and is not misrepresented as a catchable
result.

Closing and later opening a new token starts a fresh activation and scan so
files created during the process can appear. A late scan result is accepted
only for the same still-active activation, including while a detail view
suppresses it, and is then filtered for the latest hit. A late filter result
also requires its exact job revision, query, Composer revision, and span.
Anything else is discarded without changing requested or presented state.
Normal, signal, terminal-error, and approval-exit paths call the controller's
async shutdown. Enhanced shutdown first cancels and restores/detaches the exact
terminal mode and screen, then awaits the owned blocking join before process
exit; stuck kernel I/O may delay process exit but cannot leave the user's
terminal in cbreak. Controller replacement uses the same async settle-before-
replace path. No production path relies on `Drop` to join asynchronously.

The controller is constructed outside the enhanced driver's future-level panic
boundary together with an outer current-turn cancellation registry; the inner
UI future only borrows them. A trusted UI-helper panic is caught there and its
payload is never persisted. This catch does not suppress Rust's process-global
panic hook, which may already have written a diagnostic as recorded in
`docs/compatibility.md`.

The outer owner synchronously triggers both current Agent/tool cancellation and
the scan/filter token before waiting for either side, then immediately
restores/detaches the terminal. It concurrently drains the existing owned
Agent/tool cleanup and the suggestion join; Session closes only after Agent
facts settle. A stuck directory syscall may delay final process exit, but it
cannot delay the tool-cancellation signal or terminal restoration. Unwinding
therefore cannot drop-detach the blocking job before cleanup regains ownership.

Filtering is deterministic and does not perform fuzzy inference. The scanner
sorts each directory's admitted entries by raw UTF-8 name bytes before its
depth-first step and assigns each file a stable catalogue ordinal. An empty
query matches every catalogued path in that deterministic order. A non-empty
query ranks in this order: exact path, case-sensitive path prefix,
case-sensitive path-component prefix, case-sensitive substring, then the same
three non-exact classes using ASCII-only case folding. Non-ASCII text is
therefore case-sensitive. Within a
class, an earlier match byte wins, then fewer UTF-8 bytes, then catalogue
ordinal. Duplicate display paths are removed before the bounded roster copies.
The blocking filter preprocesses at most two 1,024-byte KMP failure tables and
uses linear byte scans for case-sensitive and ASCII-folded substring matching;
prefix and component-prefix checks inspect each path byte only a fixed number
of times. It maintains the best 256 score/index records in a bounded binary
heap instead of sorting or comparing path text. At most eight integer-score
comparisons insert into a non-full heap; a full custom max-heap uses one root
comparison plus at most eight two-comparison sift-down levels, for at most 17
integer comparisons per catalogued path. The complete refinement has an
explicit 64-MiB byte-inspection ceiling, checks cancellation for every path and
each 4-KiB inner-loop block, uses checked counters, and never runs on the
terminal event-loop worker. Exactly 256 paths and exactly 256 KiB of path text
are accepted. A 257th ranked path or the first ranked path byte that would
exceed 256 KiB is omitted with every later match and labels the retained
deterministic prefix `showing top matches`; this candidate cap is not a scan
failure. A path is completion-eligible only when
`draft_bytes - span_bytes + 1 + path_bytes + 1 <= 64 KiB`, accounting for `@`
and the trailing space; other paths are excluded before ranking. A zero-match
result shows `No matching workspace file`. Neither status row is actionable.
Filesystem or explicit collection-capacity failure uses the same unavailable
row with no host path or operating-system error detail and does not end the
terminal Session. A query of
exactly 1,024 bytes is active; one of 1,025 bytes hides the menu and performs no
scan rather than retaining or displaying an oversized query.

Selection is kept by exact relative-path identity across query refinement and
resize while that path still matches; otherwise it falls back to the first
ranked path. Up/Down and Tab/BackTab clamp rather than wrap. Those four keys are
menu keys even during loading, no-match, or unavailable states and always end
the current decoder read batch, so they never fall through to Composer history
or queue recall and a following Enter byte in the same terminal read cannot
pick or submit. A timed standalone Esc dismisses the current revision and
cancels unfinished scanning or filtering without editing the draft. Rejected
input or paste dismisses both dynamic menus at the current revision and retains
the existing local error. Paste only changes the draft; the existing paste fence completes
before any suggestion can be picked or submitted.

Requested state is not authority to act. Presented state is one of `Absent`,
`Valid(RankedSnapshot)`, or `Invalidated`. Every staged Focus Dock carries a
bounded presentation credential containing its activation, menu revision,
Composer revision and span, status, an owned bounded candidate
roster, and its optional selected index; the selected path is
`roster[selected]`, never a fourth text copy. The staged roster is a fallible
bounded text copy, not a self-reference into a catalogue that a filtering job
may own. Only complete success of that exact Dock transaction atomically
installs `Valid`; a successful hidden/detail surface installs `Absent`. A
non-resize abort before writing any byte retains the preceding fully committed
state because the terminal did not change.

Any partial write, physical resize, or poisoned screen sets `Invalidated`
before terminal reads resume. While invalidated, the decoder epoch is reset and
all input bytes are discarded behind a fresh-read fence; Enter cannot submit
and file navigation cannot move or pick. Signal and cleanup polling continue.
Only a full recovery transaction may replace it with `Valid` or `Absent`.
Candidate work may therefore become Ready while the terminal still displays
`Scanning workspace...`, but those requested paths are not actionable yet.

Outside `Invalidated`, the input reducer reads only the presented state, never
the newer requested selection. The branches are closed: `Absent` or a Valid
loading/empty/unavailable snapshot has no actionable row, so Enter follows the
ordinary submit/queue rule and file navigation is a fenced no-op; a Valid real
row whose activation, Composer revision, and span match picks/navigates exactly
the displayed roster; a Valid real row with a stale hit consumes Enter and
navigation as fenced no-ops without submitting. Same-read fencing remains in
force. A newly requested result or selection can replace this authority only
after the corresponding redraw commits, so no arrow or Enter can operate on an
unseen file.

Enter with a real selection performs a compare-at-swap check: the current
Composer revision and exact `@query` span must still equal the hit that produced
the menu. On success it atomically replaces only that span with
`@relative/path ` as one undoable edit, leaves all surrounding prompt text
byte-for-byte unchanged, moves the cursor after the trailing ASCII space,
detaches history/reverse-search navigation, and ends the read batch. Spaces in
a selected path are inserted literally; the trailing delimiter closes the
menu. Picking never submits, queues, opens, reads, hashes, or attaches the file.
Only a fresh later Enter sends or queues the ordinary prompt, and the model sees
the same literal text the user sees. Every Enter attempt made while a real file
selection is visible ends the current decoder read batch even when the
compare-at-swap, Composer-capacity, or allocation check fails. If the draft
revision, token span, activation, or presented menu revision changes, the compare-at-swap
fails and the pick is a local no-op followed by resynchronization. The file may
be renamed or removed after catalogue creation; because picking deliberately performs no
filesystem revalidation, an otherwise current selection still inserts its
literal relative path; a later explicit file tool revalidates existence under
its ordinary policy. Enter with no real selection keeps the ordinary
submit/queue behavior.

Approval remains the strongest input owner from committed question through
preview, arming, rendering, and acceptance. Takeover cancels unfinished file
scanning or filtering, invalidates any Ready catalogue or Failed status,
suppresses the menu, and resets the enhanced decoder epoch. It retains only the draft and selected
path identity, never stale candidate rows, and cannot turn stale arrows or
Enter into a file pick or an approval. In the exact
`Cancelling { pending: Some(_) } + ApprovalQuestion` transition, pending becomes
`None` and the controller is marked suppressed. If the old join settles while
the question exists, it becomes suppressed `Dormant` and cannot spawn. After
approval settles, the controller derives a new hit from the then-current
Composer rather than reviving pending data, and the still-active token always
starts a fresh activation; once that catalogue settles, the
retained selected path is restored only if it still ranks. Inspect/Review, by
contrast, suppress the menu without changing the draft or selection; a
settled bounded catalogue may be reused when Focus returns. During a running
turn the same feature edits only the next-turn draft. A catalogue result that
arrives while transcript output is partially written requests a coalesced Dock
redraw after that transaction; it never aborts, duplicates, or reorders the
committed transcript. Resize changes only the selected-centered window.

The non-compact file window shows at most
`min(matches-or-status-row, 12, rows - 8)` rows, preserving the ordinary
status/divider/four Composer/hint rows and one transcript row. The selected
index uses the same centered-window formula as commands. Compact rescue shows
only the selected `@path` or one fixed loading/empty/unavailable row in the
status position, plus the Composer and `Enter · Esc` hint. Path text is passed
through the visible-control renderer and truncated only for display; the
underlying completion string is unchanged. All Dock and screen-transaction
limits remain authoritative. A capped non-compact result reuses the existing
status row as `Workspace files · showing top matches` (prefixed by `Working ·`
during a turn), so it consumes no candidate or transcript row. The 12×5 compact
rescue deliberately omits this advisory label and keeps the exact selected row,
Composer, and `Enter · Esc` hint; resizing back to non-compact reveals it.

## Plain and accessible rendering

Plain output contains zero ESC bytes and complete textual labels. It uses
canonical input, numbered or lettered decisions, and append-only status lines;
there is no cursor animation or dock rewrite. Every enhanced interaction has a
plain semantic path, though mouse and cursor-rich editing are accelerators, not
requirements. Mono and High Contrast never use color as the only distinction.
The later Reduced Motion slice will remove spinner frames. An optional
product-owned bell or title
notice may fire only when approval is ready or a long turn completes; model
text never controls it and the default is off.

## Security and terminal ownership

- `CommittedUiEvent` remains the live fact boundary.
- Model, tool, path, diff, error, queue, and Session text pass through the
  existing visible-control sanitizer before layout.
- Untrusted text becomes sanitized `PresentedChunk` items with closed
  `TextStyle` roles. Only `InlineScreen` serializes fixed cursor/clear/SGR
  commands; no untrusted value becomes a cursor count, SGR parameter, OSC
  payload, or terminal query.
- Entering enhanced mode flushes stale input and enables bracketed paste only
  after exact termios capture. Leaving or suspending disables paste, leaves any
  optional alternate screen, and restores termios in that order, including
  panic/unwind best effort.
- Ctrl+C/Z and terminating signals preserve the existing tools-first cleanup
  and Session shutdown order.
- A partially written coordinate frame poisons the screen ledger and is
  recovered with the bounded ED2 path above; it does not roll back, replay, or
  fabricate a Session fact.
- Output failure, resize, and unknown input fail closed. They cannot authorize
  or start a side effect.

## Resource limits

| Resource | Limit |
| --- | ---: |
| composer prompt | 64 KiB UTF-8 |
| undo history | 128 inverse edits and 1 MiB deleted payload |
| yank buffer | 64 KiB UTF-8 |
| queue items | 8 |
| one queued prompt | 64 KiB UTF-8 |
| queued prompt aggregate | 256 KiB |
| in-memory prompt history | 128 entries and 1 MiB |
| bracketed paste | 64 KiB UTF-8 |
| CSI sequence | 32 bytes |
| projected tool activities / approval links | 256 each |
| projected tool summary / Dock activity source | 4 KiB UTF-8 each |
| canonical patch approval path | 4 KiB UTF-8 |
| canonical patch approval preview | existing 64 KiB UTF-8 |
| canonical patch row provenance | one byte per physical preview row, at most 64 Ki entries |
| final tool-card headline / detail | 256 UTF-8 bytes each |
| receipt headline / counters / effects | 4 KiB UTF-8 each |
| Inspect committed-fact rows | 512 per turn |
| Inspect retained text aggregate | 512 KiB UTF-8 |
| retained reasoning for Inspect | 256 KiB UTF-8 per turn |
| Inspect reasoning blocks / omission-step entries | 128 each |
| frozen joined Review snapshots | 1 |
| Review activities | 256 |
| Review archived text aggregate | 144 KiB UTF-8 |
| detail-panel source lines | 4,096 |
| detail-panel physical rows | 4,096 after wrapping |
| detail-panel source text | 1 MiB UTF-8 |
| built-in semantic palettes | 6 compile-time variants; longest name 13 ASCII bytes |
| table columns / body rows | 8 / 64 |
| table physical source row | 16 KiB sanitized UTF-8 |
| table aggregate source | 64 KiB sanitized UTF-8 |
| markup line-prefix candidate | 64 sanitized UTF-8 bytes |
| complete inline-code candidate, including delimiters | 4 KiB sanitized UTF-8 |
| fence language label | 32 ASCII bytes |
| complete retained fence candidate, including delimiters/newlines | 64 KiB sanitized UTF-8 |
| semantic non-plain style starts | 4,096 per assistant stream |
| markup presentation-frame soft item budget | 96 × 1,024 items, including 8,208-item conservative headroom |
| markup presentation-frame soft text budget | 768 KiB sanitized UTF-8 |
| sanitized owned text / presented text | 1 MiB |
| presented items | 128 Ki items |
| screen transaction | 2 MiB |
| retained split grapheme | 1 KiB |
| visible suggestion rows | 12 |
| command palette entries | 7 compile-time entries; at most 3 visible at 44×12 and 1 at 12×5 |
| file suggestion query | 1,024 UTF-8 bytes |
| file suggestion scan/filter | 10,000 directory entries, 8 MiB cumulative validated path text, and 64 MiB matching inspections; one owned cooperatively cancellable blocking job |
| file suggestion candidate roster | 256 paths and 256 KiB path text per copy; at most requested + staged + presented |
| dynamic dock | 24 rows |
| composer visible height | 8 rows |
| ordinary refresh | 30 FPS |
| animation refresh | 8 FPS |
| enhanced minimum | 44 columns × 12 rows |
| already-enhanced compact rescue | 12 columns × 5 rows |
| terminal write batch | existing 8 KiB chunks and 5-second deadline |
| poisoned visual reset | 250 ms |

Input, queue, card, receipt, and Dock limits apply to source UTF-8 bytes unless
their row says otherwise. Markup limits explicitly apply after visible-control
sanitization. The presentation-frame text budget counts text-run UTF-8 bytes;
structural `LineFeed` items are governed by the item budget instead. The table
source cap separately includes its physical line feeds. Inline/fence/style
overflow degrades to ordinary copyable text;
frame item/text overflow emits one fixed
`[assistant display omitted: presentation limit exceeded]` marker and suppresses
the remainder of that assistant block's display. These are presentation-only
decisions: they neither modify Session facts nor cancel a valid Agent turn.
Implemented markup, input, queue, decoder, and screen limits have exact and
one-over tests. Complete exact/one-over evidence for every card, receipt, and
Dock field remains a release-checkpoint gate.
Existing Session, Provider, tool, approval-preview, and terminal-output limits
remain in force.

## Failure and cancellation matrix

| Situation | Required UI behavior |
| --- | --- |
| invalid UTF-8 key bytes | visible local input error; draft and Session unchanged |
| incomplete/unknown CSI | dismiss command and file menus at the current Composer revision with draft unchanged; cancel approval or insert visible text elsewhere as specified; never Allow |
| command prefix has no match | fixed local empty-state row; draft remains submittable as an ordinary prompt |
| resize/partial write while command palette is open | preserve draft and selected command identity through the existing screen transaction; no command action |
| approval arrives while command palette is open | suppress palette, commit approval takeover, and retain default Reject; draft remains unchanged |
| file suggestion scan is pending | fixed loading row; draft stays editable; no implicit submit or file read |
| file suggestion scan/filter is cancelled or settles late | join owned work and discard a mismatched activation or job revision; no stale Dock mutation |
| file suggestion scan fails or exceeds a bound | fixed unavailable row without absolute path or OS error detail; ordinary prompt submission remains available |
| file query has no match | fixed non-actionable empty row; ordinary prompt submission remains available |
| draft or token span changes before pick | Composer revision/span compare fails locally; resynchronize without editing or reading a file |
| catalogued file changes or disappears before an otherwise current pick | insert the same literal relative path; never imply that it still exists and perform no filesystem revalidation |
| approval arrives while file suggestions are open | cancel unfinished scan, suppress menu, reset stale input, and retain default Reject |
| oversized prompt/paste/queue | reject locally without losing the previous draft |
| resize during stream | reflow dock; no repeated transcript or cursor loss |
| resize during approval | preserve Reject/selection; no decision |
| resize during Inspect/Review | clamp the viewport; preserve draft/queue; never replay transcript |
| approval while Inspect/Review is open | commit Focus Dock first, then preview and the unchanged Reject-first fence |
| Inspect archive overflow | one `details omitted` fact; Session and current turn continue |
| Ctrl+C while running | keep draft/queue, show stopping/cleanup, then next prompt |
| Ctrl+C while idle | clear draft first; second explicit action may exit per documented policy |
| Ctrl+Z | restore terminal, finish required cleanup, suspend, revalidate/redraw on resume |
| HUP/QUIT/TERM | restore terminal and finish owned cleanup before stable exit |
| output deadline/failure | restore termios; preserve primary error; no side effect after failed approval display |
| Provider/tool error | one actionable error component plus raw code in Inspect |
| outcome unknown | prominent unresolved result; never render success or replay |
| Session/compaction error | explain what remains safe and whether continuation is possible |
| panic in trusted UI helper | outer owner signals Agent/tool and suggestion cancellation, restores terminal, concurrently joins both cleanup paths, then closes Session; panic payload is not persisted, while the global hook limitation remains |

## Acceptance tests

1. Pure reducer tests cover every `Interaction`, desired/committed view, tool, approval, queue,
   command, resize, cancellation, and terminal-failure transition.
2. Semantic golden tests render 112×34, 80×24, and 44×20 scenes in Adaptive,
   Midnight, Paper, Color-blind, High Contrast, Mono, and plain modes, including Chinese, emoji,
   combining characters, long paths, hostile controls, Markdown, diff, errors,
   compaction, and queue overflow.
3. Real PTY tests cover Unicode editing, multiline, history, bracketed paste,
   Agent-working drafts/queues, command/file palettes, approval, resize storms,
   scrollback uniqueness, signals, output backpressure, and exact termios
   restoration on macOS and Ubuntu.
4. A deterministic 100-chunk/second stream keeps input responsive, loses no
   Session fact, and has p95 committed-event-to-frame latency below 50 ms on the
   release acceptance host.
5. Fifty thousand synthetic presentation facts keep dynamic rendering bounded
   by visible/dock state; the implementation does not keep a second unbounded
   transcript.
6. Resize 100 times while streaming and approving preserves draft, queue,
   selection, scrollback count, and side-effect count.
7. The installed journey performs read/search, approved patch, approved shell,
   cancel-and-continue, resume, compaction, plugin approval, Focus/Inspect/Review,
   queueing, and final receipt without a real API key.
8. README screenshots come from the installed candidate's real PTY bytes, not
   mockups; overview, approval, and review scenes are captured at declared
   terminal sizes.
9. `./scripts/verify.sh`, Phase 9/10 acceptance, the new Phase 11 acceptance,
   `git diff --check`, independent safety/UX review, and macOS/Ubuntu CI all
   pass with zero ignored tests.
10. Command-palette reducer tests cover all seven closed entries/order/default,
    exact whole-draft/cursor visibility, case-sensitive prefix filtering,
    stale-selection fallback, no-match, Esc dismissal, and all four
    Up/Down/Tab/BackTab clamp plus read-fence transitions. Completion tests
    assert one undoable atomic edit, cursor-at-end, detached history/reverse-
    search navigation, and unchanged queue/history contents. Dock/InlineScreen
    tests cover the exact width/height row formula, selected-centered windows,
    untruncated command spelling, compact `>`/hint plus one transcript row,
    112→44→12×5→80 identity retention, and zero/partial-write recovery without
    committing an action. Real PTY journeys prove that Down/Tab plus Enter in
    one read only navigates, a later fresh Enter completes a prefix, and another
    fresh Enter executes it; an already exact command after same-read navigation
    also waits for a fresh Enter. Paste plus same-read CR cannot execute. All
    seven exact commands remain queue/Session-isolated during a running turn,
    and an unknown slash draft remains the only ordinary queued prompt. A real
    `apply_patch` approval arriving over an open palette suppresses it, discards
    stale palette input, keeps Reject selected and the file unchanged, then
    restores the exact draft after rejection. A linear partial prefix emits no
    dynamic palette and remains zero-ESC.
11. File-suggestion pure tests cover Unicode-safe trigger boundaries,
    `user@host`, invalid-nearer-`@` scan-through, inline/multiline hits,
    cursor-at-token-end, control queries, `@` priority over a whole-draft `/`
    menu, every cursor/whitespace/submit/clear close condition, separate
    Inspect/Review suppression, Rejected/PasteRejected error preservation,
    stale activation/job/menu revision rejection, failure/empty/loading states, deterministic ranking
    and identity retention, all four navigation clamps, revision-scoped
    dismissal, and exact span compare-at-swap. Manual Debug tests place
    `SECRET`, ESC, bidi, hostile relative paths, raw I/O detail, and an absolute
    root sentinel in every controller/job/snapshot state; formatted output
    contains only revisions, counts, lengths, and closed booleans. Boundary tests accept a
    1,024-byte query and suppress a 1,025-byte query with zero scan; accept
    10,000 scanned entries and 8 MiB of scan paths, while +1 yields unavailable
    with no partial catalogue; accept 256 candidates and 256 KiB, while
    candidate +1 or byte +1 stops at the labeled deterministic prefix; and
    accept a completion whose exact whole-draft replacement is 64 KiB and
    exclude 64 KiB + 1. Checked arithmetic and fixed rank-record capacity use a
    deterministic allocation failpoint for every `try_reserve` failure; charged
    path and roster counters prove the catalogue plus three bounded text-copy
    owners match the declared model. A separate process-abort test is neither
    required nor falsely presented as recovery from global allocator OOM. The
    combined worst-case temporary-memory model has an exact test.

    Workspace tests cover the shared retained authority, regular files, every
    skipped directory plus an included dot directory, symlink/non-UTF-8/control
    denial, and absence of file content or absolute-root output. A gated
    directory-to-symlink replacement never crosses the link and returns no
    partial catalogue. Depth 64 is accepted, depth 65 fails, and a wide fanout
    proves the descriptor count stays at root-plus-64 rather than one per child.
    Controlled scan tests separately close the token, expire Esc, start
    approval, shut down, and replace the controller. Close and approval cancel
    without awaiting the gated worker; a reopen stores only the latest pending
    hit, and the next scan starts only after the old join settles. Shutdown
    restores the exact terminal before waiting for that join. A late mismatched
    activation or job revision cannot change state or Dock. A forced trusted UI panic gates both
    an active tool and suggestion worker: both cancellation tokens are observed
    before either gate is released, the terminal restores before either join,
    Agent/tool and suggestion cleanup drain concurrently, Session closes last,
    and the payload never enters Session. The test makes no false assertion that
    `catch_unwind` silences the process-global panic hook. An exhaustive reducer permutation covers
    `Cancelling(pending) → ApprovalQuestion → old join → approval settle`: the
    pending hit is cleared, no work starts under approval, and only a newly
    derived Composer hit may restart. Filtering-query supersession returns the
    same catalogue from the cancelled job, runs only the latest query, and does
    not increment the filesystem-scan counter. By contrast,
    `Filtering(activation A) → close → reopen(activation B)` joins and discards
    A's catalogue, then increments the scan counter exactly once for B. An adversarial 8-MiB repeated-prefix
    catalogue plus 1,024-byte near-match query stays within the 64-MiB matching
    counter and 17-integer-comparison-per-path heap bound while input, signal,
    and approval polling remain responsive.
    Loading, no-match, and unavailable
    states each consume all four navigation keys and fence same-read CR without
    history or submission; one fresh CR alone takes the ordinary submit path.

    Composer/InputMemory tests prove one undoable token replacement,
    surrounding-text preservation, cursor/trailing-space behavior, history
    detachment, unchanged queue/history, stale draft/span rejection, and literal
    insertion after the catalogued file is renamed or removed. A stale visible
    selection followed by `CR CR` in one read fences after the failed first CAS,
    so the second CR cannot pick or submit. When an old Ready roster remains
    presented while a changed query is Filtering, its owned neighbor list is
    never replaced by requested data; because the hit is stale, arrows and Enter
    are fenced no-ops until the new roster commits. Dock/InlineScreen tests cover the
    exact `min(matches-or-status, 12, rows - 8)` counts at `112×34` (12),
    `80×24` (12), and `44×12` (4), selected-centered windows, the exact
    `12×5` one selected/status row plus Composer and `Enter · Esc` rows,
    non-compact capped status with no extra row, compact cap-label omission,
    visible-control safety, unchanged completion bytes after display truncation,
    and `112×34→44×12→12×5→80×24` selection/path identity. A gated scan
    settles during both zero-byte and partial transcript writes: requested
    candidates cannot answer Enter or arrows before their presentation
    credential commits; the still-presented loading state alone governs input.
    Arrow is a fenced no-op and Enter takes only the ordinary loading-state
    submit/queue path with the literal draft, never a hidden pick. The flow
    causes no transcript abort, duplication, or reordering. A non-resize
    zero-byte abort retains the old complete credential; partial, poison, and
    resize paths enter `Invalidated`. Injected CR/arrows during that recovery
    gap are discarded and cannot submit, move, or pick before the full recovery
    commit. The flow then coalesces to one later Dock redraw/credential commit.

    Real PTY journeys cover idle and running inline completion, same-read
    arrow/Enter fencing, paste fencing, spaces in paths, rescan after a newly
    created file, scan failure, and approval takeover with exact draft/selection
    restoration, default Reject, unchanged file, and joined scan cancellation.
    Stale file-menu Down/Enter bytes are injected before the approval preview
    and arming fence; they neither pick a path, move to Allow, nor decide, and
    only a fresh Reject is accepted. Ready/Failed data is invalidated and the
    post-settlement scan counter increases by exactly one rather than reusing an
    old catalogue.
    Inspect and Review separately suppress suggestions without draft/selection
    mutation; a scan settling in either detail view does not draw there, and
    returning to Focus reuses that catalogue without another scan. Picking at
    idle leaves queue length, Session UserMessage count, Provider request count,
    and content-read probes at zero; fresh Enter produces exactly one literal
    UserMessage and one request. Picking during a turn still changes none of
    those counts and does not queue; fresh Enter adds exactly one next-turn
    item, whose later admission makes the second request contain the exact
    visible literal text. Linear mode records zero suggestion scans, sends the
    ordinary literal `@` prompt once, and emits zero dynamic ESC output. Every
    default test stays offline through temporary workspaces, controlled
    enumerators, allocation failpoints/charged test types, in-memory Session
    facts, and a loopback fake Provider.

## Implementation checkpoints

1. **Design checkpoint (green)**: this document, roadmap/compatibility/upstream
   status, state tables, wireframes, and the frozen red-test inventory.
2. **Semantic foundation (green)**: bounded committed UI facts, reducer,
   correlation, truth-safe metadata, and fail-open shadow observation.
3. **Composer + inline Dock (green)**: owned long-lived cbreak, decoder,
   Unicode editing, current-process history, paste fences, FIFO, full-screen
   scroll ledger, compact layouts, enhanced approval, exact restoration, and
   PTY failures. This is the first production enhanced path.
4. **Truthful timeline slice (green)**: at most one final card per projected
   lifecycle, emitted by the first non-replacement result or a turn-end unknown
   fallback; strict patch/Shell/plugin facts; and an exact Session/TurnOutcome
   receipt join.
5. **Bounded assistant markup (green)**: fragment-independent headings, lists,
   quotes, inline code, fenced code and fenced diff; authoritative-finish versus
   abort semantics; visible-control safety; graceful display omission;
   44/80/112-column terminal models; and enhanced/linear PTY evidence.
6. **Canonical patch approval (green)**: generator-provenanced, single-source
   `apply_patch` facts; one copyable semantic diff card; unchanged linear
   record; and the existing default-Reject input fence.
7. **Bounded detail views (green)**: current-turn committed-fact archive,
   reasoning moved out of enhanced Focus, exact context estimates, truthful
   compaction chronology, one joined summary Review, and transactional primary-
   screen Inspect/Review panels with local commands and approval takeover.
8. **Remaining product checkpoint (partial)**: the six closed, transactional
   semantic themes, bounded source-preserving tables, and closed seven-command
   palette are green. File suggestions, reduced motion, and the Session picker
   remain. Each sub-slice stays independently green and pushed; this line does
   not claim that the remaining items are implemented.
9. **Release checkpoint**: remove the replaced log renderer, installed-binary
   journeys, screenshots, documentation, full clean-target gates, independent
   review, non-force push, dual-platform CI, and a separate completion-status
   commit.

Each checkpoint must be coherent and green before it is pushed. Phase 11 stays
`in-progress` until the final candidate and status commit both pass the declared
platform matrix.
