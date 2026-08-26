# Phase 11 validation record

## Status

`in-progress`

Phase 11 is the user-approved TUI v2 extension. This record intentionally has
no final release candidate, Phase 11 completion claim, screenshot digest, or
platform success yet. A production-reachable enhanced composer, inline Dock,
truth-safe final tool cards, and joined turn receipt now exist on conservative
terminal profiles; the strict Phase 9 linear path remains the fallback. Phase
11 now also has bounded assistant-only Markdown/code/fenced-diff presentation
and a generator-provenanced semantic preview for real built-in `apply_patch`
approvals. Bounded current-turn Inspect and one-summary Review are also
production reachable in the primary-screen ledger. Six closed semantic themes,
bounded source-preserving tables, a closed eight-command completion palette,
bounded workspace-file suggestions, and process-local Reduced Motion now
transactionally share that ledger. It is still partial because the Session picker, installed Phase 11 acceptance,
real screenshots, real-emulator evidence, and same-candidate dual-platform CI
are not complete.

## Frozen boundary

The accepted design is [`../design/tui-v2.md`](../design/tui-v2.md). It keeps
the DeepSeek Harness semantic baseline and the existing Session, approval,
cancellation, plugin, and process-cleanup contracts. The default experience is
an inline, native-scrollback Focus view with a bounded dynamic dock; explicit
Inspect and Review use a bounded read-only panel in that same primary screen.
Only a future Session picker may consider the alternate screen after a separate
ownership proof. `--tui linear` is the implemented zero-ESC,
no-dynamic-control accessible path.

The implementation work is divided into these checkpoints:

1. semantic `CommittedUiEvent` and `UiProjector` foundation;
2. long-lived cbreak ownership, Unicode decoder/composer, history, safe paste,
   next-turn queue, bounded inline dock, enhanced approval, and resize recovery;
3. truth-safe semantic tool cards and the joined turn receipt;
4. bounded assistant markup and semantic canonical-patch approval preview;
5. bounded Focus/Inspect/Review, context estimates, and compaction facts;
6. themes (green), tables (green), commands (green), file suggestions (green),
   Reduced Motion (green), then Session picker;
7. installed-binary PTY journey, real screenshots, clean-target repository
   gates, independent review, and macOS/Ubuntu CI.

## Semantic foundation slice — 2026-08-19

The first part of checkpoint 1 is implemented behind the unchanged Phase 9
renderer:

- `CommittedUiEvent` retains bounded user, assistant, usage, request-context,
  tool, retry, approval, and compaction facts. Large opaque payloads are either
  retained up to 64 KiB or explicitly marked omitted; identifiers use a 4 KiB
  display bound plus a stable fingerprint for correlation. The opaque-payload
  and identity Debug views report sizes rather than their retained text.
- `UiProjector` owns `(turn, step, call-id)` tool lifecycles, distinguishes
  requested/approval/completed/failed/unknown facts, treats a nonzero shell exit
  as neutral completion rather than invented success, and interprets patch or
  shell metadata only when the exact known contract shape is present.
- prune markers remain pending until the immediately following historical
  surface replacement confirms them. That replacement is not treated or
  rendered as a second tool execution. The recorded token count is explicitly
  the old node's shadowed estimate, not a claim about tokens removed.
- wider memory-compatible/imported fact sequences degrade through bounded
  conflict/omission counters rather than cancelling valid Agent work. The
  existing renderer observes the projector only in fail-open shadow mode.

This slice did **not** implement the Phase 11 receipt, enhanced composer/dock,
Markdown/diff presentation, alternate-screen views, or PTY journey. Those
boundaries are updated by the following slice rather than retroactively claimed
by the semantic commit.

Local validation used Rust 1.85.0 on macOS arm64 without network access, a real
API key, or model billing. `./scripts/verify.sh` passed on the final working
tree: formatting, all-target checks, 547 library tests plus 305 integration
tests (852 total, zero failed and zero ignored), Clippy with warnings denied,
and whitespace checks. Focused suites additionally name the new boundaries in
`session::observer::tests`, `tui::projector::tests`, `cli::live::tests`, and
`session::phase7_tests`. The immutable implementation commit is
`bc08c6e12a0b8de3e95ab5e31948b3aebf4aba77`; it was pushed non-forced to
`origin/main`. The preceding design commit is
`dc616e46bf595ec1b45aa971dd9336891a44eb0a`.

## Composer and inline-dock slice — 2026-08-19

Implementation commit
`88ddadf1504f553294787ae0040df2dd29298113` makes these production paths real:

- `--tui auto|enhanced|linear` is a closed CLI surface. Linear is always plain
  and contains zero ESC bytes. No color, `TERM=dumb`, an unknown terminal,
  tmux/Screen/Zellij, or an initial terminal below 44×12 makes Auto choose the
  conservative linear path. Explicit enhanced remains an opt-in escape hatch
  for known multiplexers, while the same initial geometry gate still applies.
- `TerminalSession` owns long-lived cbreak/no-echo mode, preserves kernel
  signal handling, distinguishes carriage-return submit from Ctrl+J newline,
  enables bracketed paste, and restores the exact original termios on ordinary,
  signal, suspend, EOF, output-failure, and unwind paths.
- `KeyDecoder`, `Composer`, and `InputMemory` implement fragmented UTF-8 and
  escape decoding, grapheme-safe Unicode editing, multiline navigation,
  bounded undo/yank, current-process committed history, reverse search, atomic
  64 KiB paste, and an eight-item/256 KiB next-turn FIFO. Queued text enters
  Session only when it becomes the admitted next turn; cancellation and
  suspension cannot confuse a committed prompt with a local draft.
- `InlineScreen` is the sole coordinate owner. It uses only full-screen native
  scrolling, a fixed bottom Dock, transactional generation checks, a software
  cursor, and a small deterministic terminal model. Partial coordinate writes
  poison the ledger; in-process ED2 recovery keeps paste framing enabled, while
  suspend/exit ED2 disables paste and restores the real cursor. Partially drawn
  draft or approval text is not scrolled into history during recovery.
- enhanced approval keeps Reject selected, renders the trusted preview once,
  and grants only after a current-epoch direction key followed by a later CR
  Enter. Printable shortcuts, paste, unknown input, Ctrl+J, and direction plus
  Enter in the same read cannot authorize a side effect. The strict linear
  selector retains its established record-oriented behavior.
- an already enhanced session has a compact 12×5 rescue Dock. Below that exact
  floor it clears stale geometry, restores the terminal, and fails closed.

The two small dependencies are pinned: `unicode-segmentation = 1.13.3` provides
extended grapheme boundaries at the repository's Rust 1.85 MSRV, and
`unicode-width = 0.2.2` (default features disabled) provides deterministic cell
widths. Both are MIT OR Apache-2.0 and add no ordinary transitive dependency.

Local validation used Rust 1.85.0 on macOS arm64 with fake models, loopback
HTTP, temporary workspaces, and obvious fake credentials. `./scripts/verify.sh`
passed on the implementation tree: formatting, all-target checks, 610 library
tests plus 327 other tests (937 total), zero failed/ignored, Clippy with warnings
denied, and whitespace checks. The same run includes all four Phase 9 release
acceptance tests and all eleven real/fault plugin example tests. High-value PTY
regressions cover Unicode/CR-vs-LF, fragmented and rejected paste fences, busy
draft/FIFO admission, cancel-then-continue, exact terminal restoration,
directional approval, 44×12 startup and 12×5 runtime geometry, conservative
Auto profiles, output deadlines, partial writes, and signal identity.

This is a green implementation checkpoint, not a Phase 11 release candidate.
Its docs evidence is committed immediately after the implementation anchor and
both commits are published together without force. Real iTerm/Terminal/VS Code
resize/reflow/copy capture, Phase 11 installed acceptance, and current enhanced
screenshots remain pending.

## Truthful tool cards and receipt slice — 2026-08-19

Implementation commit
`4c5285bdb5d16859fa65de0a0e98095bd26e61d7` makes the next Focus-path slice
production reachable:

- `UiProjector` still owns correlation by `(turn, step, call-id)`. A request and
  optional approval update the Dock, while only the first genuine result emits
  one immutable final card. Duplicate results and historical surface
  replacements do not create a second execution card; a capacity one-over
  produces a bounded generic card instead of cancelling Agent work.
- patch cards interpret only the closed patch metadata contract, including the
  important case where a change committed but a later warning made the result
  an error. Foreground-Shell cards distinguish exit zero, nonzero exit, signal,
  timeout, pre-start failure, and result failure. Plugin cards expose only the
  public plugin ID and dispatch/settlement/quiescence facts; executable path,
  configured argv, stderr, protocol ID, and result body stay out of Focus.
- `TOOL_OUTCOME_UNKNOWN` remains the exact Session/model-facing no-replay
  failure, while Focus labels it `Outcome unknown`. An unpaired plugin
  dispatch, lost quiescence, and contradictory imported facts likewise never
  become a green success. A plugin cannot be called completed unless it was
  dispatched, peer-settled, and quiescent.
- the receipt joins the exact committed turn, `turn/end` sequence, and reason
  returned by `TurnOutcome`. It says `tool requests`, counts only strict patch
  effects and foreground process starts, and never infers test counts or pass
  status from assistant prose, command names, stdout, or exit zero alone.
- enhanced assistant text now preserves structural line feeds. Every styled
  run still passes the same visible-control sanitizer, and the presentation
  builder independently rejects terminal controls and bidi/default-ignorable
  formatting. The linear renderer keeps its accepted zero-ESC output.

The real PTY fixtures now wait for `Turn complete` rather than counting the
dynamic Dock prompt, so they prove the receipt was committed before exiting.
Plugin fault journeys additionally prove that the unknown-outcome internal code
`TOOL_OUTCOME_UNKNOWN` stays out of Focus while the next model request retains
the unknown/no-replay result.
The test harness serializes only concurrent PTY allocation, terminal setup, and
child exec on all tested Unix platforms; complete journeys remain parallel.
The lock was motivated by Darwin's high-concurrency terminal-admission race and
does not relax the product's fail-closed terminal checks.

Local validation used Rust 1.85.0 on Darwin 27.0.0 arm64 with fake models,
loopback HTTP, temporary workspaces, and obvious fake credentials. No real API
key, public network request, or model billing was used. `./scripts/verify.sh`
passed on the implementation commit: formatting, all-target checks, 626
library tests plus 327 other tests (953 total), zero failed/ignored, Clippy with
warnings denied, and whitespace checks. Focused evidence includes 10 timeline
tests, 17 live-renderer tests, 62 tests in the real-binary PTY target, 7
plugin-CLI tests, 11 real/fault plugin tests, and all 4 release-acceptance
tests. Two independent read-only reviews found no remaining P0/P1 truth,
safety, terminal, or UX issue in this slice.

This remains a green implementation checkpoint, not Phase 11 completion. A
locally interrupted turn still uses the already accepted signal-safe
`stopped; skipped …` summary rather than the ordinary joined receipt. The next
sections record bounded assistant markup and the canonical-patch approval
slice; tables, Inspect/Review, context/compaction presentation, themes, Session
picker, final screenshots, real-emulator capture, installed Phase 11
acceptance, and same-candidate macOS/Ubuntu CI remain pending.

## Bounded assistant-markup slice — 2026-08-19

Implementation commit
`1ab879433d5f213eedf42ac67a074b47ad44830b` adds production-reachable semantic
styling for assistant paragraphs, level 1–3 headings, bullet and numbered
lists, quotes, paired single-backtick inline code, triple-backtick code fences,
and case-insensitive `diff`/`patch` fences. The subset is intentionally small:
tables, emphasis, links, images, and HTML are not interpreted. Real canonical
`apply_patch` approval previews remain safely escaped Warning text rather than
semantic diff rows.

The parser receives visible-control-sanitized text and the closed presentation
builder rejects controls a second time. Parsing is independent of Provider
fragment boundaries. A matching authoritative assistant final may close a
fence at EOF without a trailing line feed; retry, correction of an old stream,
stream-key change, `StepEnd`, `TurnEnd`, or Ctrl+C instead aborts pending syntax
as ordinary assistant text. A partial fence therefore cannot be made to look
complete by cancellation.

The implemented resource contract is:

- 64 sanitized UTF-8 bytes for a line-prefix candidate;
- 4 KiB for a complete inline-code candidate, including delimiters;
- 32 ASCII bytes for a fence language label, restricted to alphanumerics and
  `_+.-`;
- 64 KiB for one complete retained fence, including delimiters and line feeds;
- 4,096 semantic non-plain style starts per assistant stream;
- a 96 × 1,024-item presentation-frame soft budget with 8,208 items of
  conservative parser headroom;
- a 768 KiB sanitized-text soft budget per presentation frame;
- the existing 128 × 1,024-item and 1 MiB `PresentedChunk` hard limits.

Inline, fence, and style-run overflow falls back to ordinary copyable text.
Frame item/text overflow produces exactly one fixed
`[assistant display omitted: presentation limit exceeded]` marker and suppresses
the remaining display for that assistant block. Session facts remain intact and
the Agent turn continues. Sanitizer expansion is measured before the visible
output length can cross the markup soft limit, so a raw chunk made mostly of
controls or bidi/Cf characters follows the same omission path rather than
becoming an output failure.

Local validation used Rust 1.85.0 on Darwin 27.0.0 arm64 with fake models,
loopback HTTP, temporary workspaces, and obvious fake credentials. No real API
key, public network request, or model billing was used. `./scripts/verify.sh`
passed on the implementation commit: formatting, all-target checks, 651
library tests plus 330 other tests (981 total), zero failed/ignored, Clippy with
warnings denied, and whitespace checks. Focused evidence includes 16 markup
tests, 23 live-renderer tests, 101 TUI tests, and 65 tests in the real-binary PTY
target (63 journeys plus 2 harness regressions). The deterministic terminal
model covers 44/80/112 columns under both supported history policies; enhanced
PTY covers fragmented heading/code/diff/inline output and Ctrl+C during an open
fence, while linear PTY preserves literal source with zero ESC bytes. Two
independent read-only reviews found no remaining P0/P1 safety, truth, terminal,
or integration issue.

This is a green implementation checkpoint, not Phase 11 completion. The next
section records the canonical-patch approval slice. Tables, alternate views,
themes, Session picker, installed-binary acceptance, current screenshots,
real-emulator capture, and same-candidate macOS/Ubuntu CI remain pending.

## Canonical patch-approval slice — 2026-08-19

Implementation commit
`a06d43a9fd6175264fdb1e997fc9d0e163832f27` makes the real built-in
`apply_patch` approval preview semantic without changing the Session schema or
permission policy. Patch preparation now produces the canonical single-file
diff and, at that same boundary, a bounded row-kind vector plus closed
operation, path, hunk, addition, and removal counts. This is process-local Rust
type provenance, not a cryptographic signature.

The immutable canonical text is shared through `Arc<str>` by the prepared
prompt, terminal request, decline result, and commit-result builders. The
enhanced presenter therefore shows `Proposed` / `not applied`, the
workspace-relative path, `+N/-N`, hunk count, one-file/no-Shell scope, and the
complete copyable diff. Row styles come from the generator rather than text
prefixes, so hunk content such as `--- a/decoy` remains a removal and
`+++ b/decoy` remains an addition. Generic prompts created through
`ApprovalPrompt::new` remain opaque even if their tool name and text look like
a patch.

All variable path and content text is made terminal-visible before the
provenance-tagged row styles are applied, and the closed presentation builder
rejects controls a second time. Debug output retains only kind, byte, row, and
counter facts. The existing 64 KiB preview, new 4 KiB path, and at-most-64-Ki
row-entry provenance bounds have exact/one-over coverage. A preview must be
fully committed to `InlineScreen` before the existing quiet/flush approval
fence can arm; build, output,
deadline, cancellation, or terminal failure still prevents the file effect.
Enhanced Focus hides internal call IDs for this card. Linear mode ignores the
presentation metadata and retains the complete Phase 9 record with zero ESC.

Responsive evidence covers full 44/80/112-column approval surfaces and the
12×5/15×6 compact rescue, where `Not applied` and the default `Reject` remain
visible. A real PTY uses header-looking hunk content, verifies header/hunk/red/
green styles, resizes before confirmation, proves the file is unchanged until
a later Enter, and observes each immutable diff sentinel once. Other PTY tests
retain printable/same-read/paste/Ctrl+J rejection, bidi-visible linear output,
output-deadline failure without a write, signals, suspension, and terminal
restoration. The installed README and resumable release journeys now wait for
the user-visible approval surface rather than an internal patch call ID.

Local validation used Rust 1.85.0 on Darwin 27.0.0 arm64 with fake models,
loopback HTTP, temporary workspaces, and obvious fake credentials. No real API
key, public network request, or model billing was used. The final same-tree
`./scripts/verify.sh` passed: formatting, all-target checks, 659 library tests
plus 330 other tests (989 total), zero failed/ignored, Clippy with warnings
denied, and whitespace checks. Focused evidence is green: 2
approval-provenance tests, 8 patch-generator tests, 6 approval-join tests, 27
live-renderer tests, 6 Dock layout tests, 19 real file-change tests, 65 tests in
the real-binary PTY target (63 journeys plus 2 harness regressions), and all 4
release-acceptance tests. Three independent read-only reviews found no
remaining P0/P1 safety, UX, or integration issue.

This is a green product checkpoint, not Phase 11 completion. It does not add
multi-file patches, diff-driven permission decisions, Review mode, tables,
themes, screenshots, or a new upstream compatibility claim.

## Bounded Inspect/Review slice — 2026-08-19

Implementation commit
`c4d7917ba632e2e3e78a9c89e153de893857e49e` makes the first progressive-
disclosure views production reachable without introducing an alternate screen
or a second Session log:

- `ViewArchive` borrows each complete `CommittedUiEvent` and retains only
  selected, bounded presentation facts for the current turn. It keeps commit
  sequence and signed Unix timestamp, current-turn reasoning, retry and usage
  metadata, approval outcomes, request context, compaction chronology, and
  payload/identity availability. Raw tool arguments, results, metadata, user
  text, literal call/approval/compaction correlation IDs, and compaction
  summaries are not copied into the view or its Debug output.
- enhanced Focus no longer prints reasoning. Inspect exposes retained streamed
  or authoritative reasoning with original/retained/omitted byte facts; linear
  mode keeps its established complete zero-ESC record. Authoritative finals
  transactionally replace same-step streamed reasoning, including previously
  attributed omissions.
- the context line samples the Session projection at one exact next-sequence
  boundary and calls the value an estimate. A mismatched boundary omits it;
  zero window omits a percentage; usage greater than the window remains
  visibly greater than 100% rather than being clamped into a false claim.
  Compaction says started/prepared/committed or failed, uses
  estimated/shadowed-token language, and pairs a prune marker with its actual
  replacement sequence without claiming tokens were removed, freed, or saved.
- Review freezes only after committed `turn/end` and `TurnOutcome` agree on
  turn, sequence, and receipt-relevant reason. It reuses the same `Arc` receipt
  shown by Focus and keeps one bounded set of truth-safe patch, foreground
  process, plugin, failure, denial, cancellation, and unknown summaries.
  Wrong anchors preserve the previous Review and receipt. This first Review is
  deliberately summary-only: canonical diffs, full commands, execution
  duration, and history before a resume seam remain unavailable.
- `ViewMode` separates requested from screen-committed state. Inspect/Review
  are fixed-height read-only primary-screen panels owned by `InlineScreen`;
  same-size re-anchor clears old rows, scrolls only a positive height delta,
  and never replays transcript bytes. The panel continues to drain live facts,
  supports 44/80/112-column layouts, and falls back to Focus below 44×12 while
  keeping enhanced mode's existing 12×5 rescue floor.
- `Ctrl+O`, exact `/inspect`, `/review`, and `/focus`, Tab, arrows,
  Home/End, PageUp/PageDown, `q`, and a timed standalone Esc are local controls.
  Printable text, Enter, paste, and unknown sequences cannot submit or queue
  from a detail view. A requested-but-uncommitted transition discards input,
  and a modal change ends the current read batch, so `Ctrl+O + Enter`,
  PageDown + Enter, and Esc + Enter cannot create a hidden request.
- approval remains higher priority. The driver resets the detail decoder,
  commits Focus, appends the immutable trusted preview, completes the existing
  quiet/flush fence, and then arms the default-Reject selector. Suspend restores
  exact termios and resume redraws the requested panel without losing the
  hidden draft.

The implemented resource contract adds 512 Inspect rows, 512 KiB aggregate
retained Inspect text, a 256 KiB reasoning subset, 128 reasoning blocks and
128 omission-step entries, one frozen Review, 256 Review activities, 144 KiB
Review text, 4,096 source lines, 4,096 wrapped physical rows, and 1 MiB detail-
document source text. Exact/one-over tests cover Inspect row/text/reasoning and
detail source/wrapped-row acceptance, omission accounting, and truthful markers
without cancelling Agent work. The Review activity/text caps and reasoning
omission-step table are implemented bounds whose direct exact/one-over tests
remain in Evidence pending.
One overlong grapheme is replaced by a fixed visible placeholder so a hostile
zero-width cluster cannot invalidate panel geometry.

Local validation used Rust 1.85.0 on Darwin 27.0.0 arm64 with fake models,
loopback HTTP, temporary workspaces, and obvious fake credentials. No real API
key, public network request, or model billing was used. The final same-tree
`./scripts/verify.sh` passed: formatting, all-target checks, 682 library tests
plus 335 other tests (1,017 total), zero failed/ignored, Clippy with warnings
denied, and whitespace checks. Focused evidence includes 11 view/archive/detail-
state tests,
14 deterministic InlineScreen tests across both history policies, 9 Dock tests,
28 LiveRenderer tests, 12 interactive-driver tests, and 70 tests in the real-
binary PTY target (68 journeys plus 2 harness regressions). The PTY journeys
cover local and active views, reasoning suppression, scroll/resize/compact
fallback, hidden-draft preservation, same-read fences, paste, approval takeover,
suspend/resume, exact terminal restoration, and the unchanged linear path.
Two independent read-only reviewers found no remaining P0/P1 truth, privacy,
terminal, input, or integration issue.

This is a green product checkpoint, not Phase 11 completion. Standard ANSI
cannot undo terminal-emulator reflow that happens before `SIGWINCH`; the tested
claim is that dsh does not actively append or replay panel snapshots. Real
iTerm2, Terminal.app, and VS Code resize/reflow/copy evidence remains pending,
as do tables, commands/suggestions, Session picker, installed Phase 11
acceptance, current screenshots, and same-candidate macOS/Ubuntu CI.

## Transactional semantic-theme slice — 2026-08-23

Implementation commit
`78a81200b4bdc976709c03cf67914dc970126291` makes six compile-time palettes
production reachable through exact local `/theme` commands: Adaptive,
Midnight, Paper, Color-blind, High Contrast, and Mono. The command without an
argument reports the current palette and the finite set; unknown or extra
arguments remain local errors and are never sent to the model or queued as a
later turn. The linear path recognizes the same command surface but remains
plain and emits zero ESC bytes.

The palette maps only the closed `TextStyle` roles to fixed SGR strings. It
never accepts a user-defined escape sequence, emits OSC, sets a background, or
queries the terminal. Text labels and selection markers remain authoritative,
so High Contrast, Color-blind, and Mono do not use color as the only signal.
Approval still starts on Reject, including the 12×5 compact rescue surface.

Theme state has separate requested and screen-committed revisions. A palette
becomes current only after the complete `InlineScreen` transaction commits;
partial output enters the existing poisoned-screen recovery and the requested
palette is redrawn before it commits. Input arriving during that transition is
discarded behind a fresh decoder epoch. Changing a theme redraws only the owned
Dock and never replays or recolors native scrollback. The choice is not a
Session fact: a real two-process PTY journey proves that a Paper session resumes
with Adaptive and that theme commands are absent from its JSONL journal.

Local validation used Rust 1.85.0 on macOS arm64 with fake models, loopback
HTTP, temporary workspaces, and obvious fake credentials. No real API key,
public network request, or model billing was used. The final same-tree
`./scripts/verify.sh` passed: formatting, all-target checks, 689 library tests
plus 338 other tests (1,027 total), zero failed/ignored, Clippy with warnings
denied, and whitespace checks. Focused evidence includes the closed-palette and
request/commit unit tests, Dock and `InlineScreen` geometry/scrollback tests,
and 73 tests in the real-binary PTY target (71 journeys plus 2 harness
regressions). The PTY journeys cover all six choices, width changes, active-turn
query/invalid isolation, same-read fences, no transcript replay, suspend/resume,
approval takeover and compact default safety, linear zero-ESC output, and the
cross-process Adaptive reset.

Three independent read-only reviews found no P0/P1 safety, truth, terminal, or
test-coverage problem after the identified documentation and recovery/resume
evidence gaps were closed. This is a green product checkpoint, not Phase 11
completion. Reduced motion, tables, commands/file suggestions, Session picker,
installed Phase 11 acceptance, screenshots, real-emulator capture, and the
same-candidate macOS/Ubuntu CI remain pending.

## Bounded source-preserving table slice — 2026-08-23

Implementation commit
`2f89cdeae005b222db11827bd59d2dd7fd79c1b6` makes a deliberately small
assistant pipe-table subset production reachable. A header, delimiter, and
body row must start and end with `|`; the header has 2–8 non-empty cells, the
delimiter has the same column count and at least three ASCII hyphens per cell,
and each body row keeps that count. Escaped pipes, multiline cells, nesting,
column spans, and malformed delimiters remain ordinary assistant text.

Once the delimiter proves a table, `Border` styles only pipes and delimiter
cells, `Heading` styles header cells, and `Assistant` styles body cells. The
renderer preserves every source byte and structural line feed, adds no padding,
and leaves 44/80/112-column wrapping to the terminal. Linear mode emits the
same literal source with zero ESC bytes. A held or malformed candidate, abort,
retry/correction, row/aggregate overflow, and non-authoritative final all
degrade to copyable plain text without changing Session facts or cancelling the
Agent turn; only an authoritative final may accept a valid last row without LF.

The closed resource contract is 8 columns, 64 body rows, 16 KiB per physical
source row, and 64 KiB aggregate table source. Fragmented overflow remains
plain until the real physical LF, so a Provider boundary cannot forge a new
heading or fence inside an over-limit row. Retained table/fence bytes and
presentation items are combined with the current fragment before the 768 KiB
text and 96×1,024-item soft gates. Exact and one-over tests cover same-frame,
cross-frame, mid-line, fallback, authoritative-finish, and abort paths; overflow
emits the fixed display-omission marker rather than reaching the 1 MiB/128 Ki
hard builder limits.

Local validation used Rust 1.85.0 on Darwin 27.0.0 arm64 with fake models,
loopback HTTP, temporary workspaces, and obvious fake credentials. No real API
key, public network request, or model billing was used. The final same-tree
`./scripts/verify.sh` passed: formatting, all-target checks, 694 library tests
plus 338 other tests (1,032 total), zero failed/ignored, Clippy with warnings
denied, and whitespace checks. Focused evidence includes 21 markup tests, the
renamed 44/80/112 `InlineScreen` uniqueness test, and 73 tests in the real-
binary PTY target (71 journeys plus 2 harness regressions). Existing enhanced
and linear journeys now prove fragmented semantic tables and literal zero-ESC
fallback; the active resize journey proves a held header is not displayed
before its delimiter and enters native scrollback exactly once afterward.

Three independent read-only reviews found no remaining P0/P1/P2 safety,
fragmentation, resource-accounting, terminal, or test-evidence problem after
the identified physical-line and retained-budget issues were fixed. This is a
green product checkpoint, not Phase 11 completion. Commands/file suggestions,
Reduced Motion, Session picker, installed Phase 11 acceptance, screenshots,
real-emulator capture, and same-candidate macOS/Ubuntu CI remain pending.

## Closed local-command palette slice — 2026-08-26

Implementation commit
`6611d4bf1bfc48fc23605d054032e11aa1631e16` makes a bounded completion
palette production reachable in enhanced Focus. Its compile-time catalogue is
exactly `/help`, `/inspect`, `/review`, `/focus`, `/theme`, `/exit`, and
`/quit`, in that stable order with product-owned ASCII descriptions. The
entire single-line draft must start with `/` and keep its cursor at byte end;
filtering is case-sensitive, an unknown prefix remains an ordinary prompt, and
neither model, Session, workspace, nor configuration text can add an entry.

One driver-owned state keeps only a selected command identity and an optional
dismissed Composer revision. Dock receives an immutable snapshot. Up, Down,
Tab, and BackTab clamp and end the current decoder read even at an edge or with
no matches. Enter on a prefix performs one whole-draft undoable completion and
also ends that read; only later fresh Enter input can execute the exact local
command. Paste edits only and retains its existing input fence. The command
classifier runs before the next-turn FIFO in idle and active states, while an
automatic reserved queue front remains a prompt and is never reinterpreted as
a command.

Approval questions own both ordinary input and pending Composer-Esc expiry
from the moment the joined question exists, including preview writing and the
quiet arming interval. The default remains Reject, stale palette input cannot
change the draft or exit, and rejection restores the same draft and selection.
Inspect and Review suppress the palette without copying its mutable state.
Zero-byte and partial Dock transactions, poisoned-screen recovery, and resize
rederive the current snapshot while preserving idle/running identity. The
12x5 rescue Dock keeps one transcript row, the selected command or fixed
no-match row, Composer, and the compact `Enter · Esc` hint. Linear mode keeps
whole-line commands and emits no dynamic palette or ESC bytes.

Local validation used Rust 1.85.0 on Darwin arm64 with fake models, loopback
HTTP, temporary workspaces, and obvious fake credentials. No real API key,
public network request, or model billing was used. The final same-tree
`./scripts/verify.sh` passed: formatting, all-target checks, 703 library tests
plus 341 other tests (1,044 total), zero failed/ignored, Clippy with warnings
denied, and whitespace checks. The 76-test real-binary PTY target contains 74
product journeys plus 2 harness regressions. Focused evidence covers all four
navigation keys and same-read fences, case-sensitive/no-match filtering,
completion undo/history isolation, 44/80/112 and 12x5 layout, zero/partial
screen recovery, idle and running local commands, exact full-request Session
isolation, unknown prompt truth, queued-exit cleanup, approval stale-input
takeover, and linear zero-ESC fallback.

Three independent read-only reviews found no remaining P0/P1/P2 algorithm,
safety, queue/Session, approval, terminal, or test-evidence problem. This is a
green product checkpoint, not Phase 11 completion. File suggestions, Reduced
Motion, the Session picker, installed Phase 11 acceptance, screenshots,
real-emulator capture, and same-candidate macOS/Ubuntu CI remain pending.

## Bounded workspace-file suggestion slice — 2026-08-26

Implementation commit
`5b9a0b3a2fa8ef62061029a202da04c8746dcfc0` makes the enhanced Focus `@`
picker production reachable. It derives a whitespace-bounded token from the
current Composer revision, scans relative paths through the same retained
`WorkspaceAuthority` used by the built-in tools, skips symlinks and a closed
set of generated/version-control directories, and never returns an absolute
host path. Linear mode owns no suggestion controller and performs no scan.

The scanner is iterative and capability-relative. It accepts exactly 10,000
directory entries, 8 MiB of validated displayed path text, and depth 64. A
real 10,000-entry/8-MiB tree proves the combined exact boundary and a one-byte
rename proves fail-closed overflow; another gated test replaces an enumerated
directory with a symlink before open and proves the retained `O_NOFOLLOW`
walk does not cross it. Per-directory ordering uses cancellable in-place
heapsort rather than retaining one descriptor per sibling.

Filtering is deterministic: exact, path prefix, component prefix, substring,
then the three non-exact ASCII-folded classes. A checked 64-MiB inspection
budget covers validation, FNV hashing, collision equality, matching and output
copy. Long loops interleave cancellation with at most 4-KiB real work blocks.
The best 256 score/index records use a custom max-heap; a full-sort oracle
matches its output and observes at most the designed 17 integer comparisons per
path. Candidate copies accept 256 KiB exactly. Completion performs one
revision/span-checked Composer edit to literal `@relative/path ` text and does
not read, hash, attach, log, or send the selected file.

Requested, staged, and presented credentials remain distinct. A screen may
act only on its fully committed owned roster. Partial/poisoned/resize writes,
approval takeover, and stale decoder epochs invalidate input authority. File
rows are staged before the render snapshot, so a recoverable allocation failure
renders and commits the same non-actionable `Unavailable` state. Controller
failpoints prove ranking-commit, roster-copy, running-scan, and running-filter
capacity failures preserve the ordinary Session and join the cancelled work.
Two panic owners cancel Agent/tool and suggestion work; the active owner uses
an allocation-free terminal restorer before concurrently draining both futures,
and the outer owner covers idle/helper panics before common shutdown.

Local validation used Rust 1.85.0 on Darwin 27.0.0 arm64 with fake models,
loopback HTTP, temporary workspaces, and obvious fake credentials. No real API
key, public network request, or model billing was used. The final same-tree
`./scripts/verify.sh` passed formatting, all-target/all-feature checks and
builds, 731 library tests plus 346 other tests (1,077 total), zero
failed/ignored, Clippy with warnings denied, and both whitespace gates. The
81-test real-binary PTY target contains 79 product journeys plus two harness
regressions. Five file-specific journeys cover idle completion and rescan,
local scan failure, active-turn queue fencing, approval stale-input takeover,
and literal zero-dynamic-menu linear fallback.

Three independent read-only reviews found no P0/P1 production defect after
capacity, credential, panic-owner, cancellation-budget, symlink-race, and heap
findings were fixed. This is a green product checkpoint, not complete
file-suggestion acceptance or Phase 11 completion. Still-open test evidence is
a forced gated trusted-UI panic ordering regression, direct wide-fanout file-
descriptor peak measurement, deterministic coverage for every fallible
allocation site and forced FNV collisions, and the remaining paste/detail/
transaction permutations in the frozen acceptance list. The Session picker,
installed Phase 11 acceptance, screenshots, real-emulator
capture, and same-candidate macOS/Ubuntu CI also remain pending.

## Reduced Motion slice — 2026-08-26

Implementation commit `7a1a81855cfcbc2e4d88a100ec5c53f2709d48d5` and
transactional correctness follow-up
`58abae7e997b40d0023acd4151c89b131c5b4d8a` make the frozen motion boundary
production reachable. `--reduced-motion` selects the static enhanced mode at
process start; exact local `/motion`, `/motion full`, and `/motion reduced`
commands query or change it in idle and active turns. The command palette now
contains exactly eight compile-time entries with `/motion` immediately after
`/theme`. Linear and script modes accept the startup flag without creating an
animation owner; linear commands report the inactive behavior with zero ESC
bytes. `--list-sessions` rejects the unrelated flag.

`MotionState` is owned beside Theme and keeps requested and screen-committed
revisions. A local change gains input authority only after the Dock transaction
commits; same-read input is fenced while the revisions differ. The preference
is deliberately absent from Session, Provider, workspace configuration, and
resume state. A second process therefore starts in `full` unless its own flag
requests reduced mode.

One active-turn `MotionClock` owns `(turn, generation, started_at,
eligible_since)` and an optional deadline; it starts no task. The ordinary
enhanced Focus `Working` row alone is eligible. Full motion delays its first
phase for 300 ms, then advances the separate ASCII `| / - \\` cell every 125
ms without catch-up. It adds whole seconds after one second and says `Still
working` after five. Reduced mode keeps the semantic `●` icon static and wakes
only for the `1s+` and `Still working` text milestones. Notices, menus, file
status, queued prompts, approval, Inspect, Review, idle, and settled turns own
no periodic motion deadline.

Motion-only Dock transactions remain preemptible while bytes are pending.
Zero-byte preemption discards the frame without committing its revision. A
partial write poisons the coordinate ledger, performs the fixed visual reset,
fences the same ordinary input read, processes the higher-priority Session/final
fact, approval, file settlement, signal, or turn completion, and only then
reattaches one authoritative Dock before accepting more input. Resize abandons
rather than replays the old phase. Existing non-motion writes retain their
original output deadline and serialization.

Paused-time tests freeze the exact 300-ms/125-ms/one-second/five-second
boundaries, all four ASCII phases, no catch-up, eligibility generations, stale
generation rejection, pre-deadline rejection, turn settlement, elapsed-age
retention across generation changes, and reduced-mode absence of periodic
deadlines. Transaction tests cover zero/partial abort, poison, resize discard,
and requested/committed input fencing. CLI tests cover the flag, duplicate,
list rejection, closed parsing, local notices, eight-entry palette, and linear
classification. Three real-binary motion journeys cover default animation and
active switching, startup reduced mode plus linear zero-ESC fallback, local
Session/Provider isolation, and default reset on resume. The complete current
PTY target remains green with 81 product journeys plus two harness regressions.

Dedicated motion PTY combinations for approval/detail/file/notice/queue
suppression, the four frozen resize sizes, hostile paste/approval bytes,
suspend/resume preference retention, reduced streaming, post-cleanup silence,
and a real-driver partial-write recovery trace remain Phase 11 acceptance debt.
The pure eligibility and priority tests cover the production decisions, but do
not claim those terminal combinations have all been exercised yet.

Local validation used Rust 1.85.0 on Darwin arm64 with fake models, loopback
HTTP, temporary workspaces, and obvious fake credentials. No real API key,
public network request, or model billing was used. `./scripts/verify.sh` passed:
formatting, all-target checks, 754 library tests plus 348 other tests (1,102
total), zero failed/ignored, Clippy with warnings denied, and whitespace checks.
Both implementation commits were pushed non-forced to `origin/main`.

Three independent read-only reviews repeatedly audited animation scheduling,
screen-transaction ownership, fact/input/approval priority, preference
transitions, hidden elapsed age, partial/resize recovery, TurnEnd, and Interrupt
cleanup. The final algorithm and safety reviews reported P0/P1/P2 = 0. The test
review reported production P0/P1 = 0 and only the mechanical evidence update
for the final SHA and test count; this record closes that item. The dedicated
motion PTY acceptance debt listed above remains explicit rather than being
misreported as completed coverage.

This is a green product checkpoint, not Phase 11 completion. The Session
picker, installed Phase 11 acceptance, screenshots, real-emulator capture, and
same-candidate macOS/Ubuntu CI remain pending.

## Evidence pending

- remaining product files and default-enabled Phase 11 acceptance tests;
- exact-limit and one-over tests for remaining card/receipt/Dock fields and
  the Review activity/text and reasoning omission-step caps, plus later picker
  resources;
- installed candidate SHA and release acceptance output;
- screenshot sizes and digests generated from real installed PTY bytes;
- macOS and Ubuntu job URLs for the same candidate;
- final compatibility status and README truth audit.

Phase 11 must remain `in-progress` until all of those fields are supported by a
green, pushed candidate and a separate green status commit.
