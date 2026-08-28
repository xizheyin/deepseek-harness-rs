# Durable workspace instructions

## Scope and upstream basis

Phase 25 makes repository guidance visible to the model before it acts. At
fixed commit `47f943859bef60e4160492346772ded9b24f765a`, the contract lives in
`packages/context/agent-instructions/src/{config,files,render,state,index}.ts`,
the package README/spec/e2e tests, and the code preset's 65,536-byte setting.
Latest inspected master `cd5ef8148158c3a752a658978873241fdf8e2bbc`
preserves the behavior and only retains extra pre-step decision fields in its
production change.

This phase owns the initial and resume-time baseline. It does not yet watch
successful filesystem tool results, discover nested scopes during an active
turn, parse shell `cd`, run a file watcher, or rearm a compacted baseline in the
same process. Those behaviors need a separate tool-result integration so their
events cannot break tool/result/step adjacency.

## Discovery and precedence

The fixed candidates are loaded broad-to-specific:

1. `$DSH_HOME/AGENTS.md`, or `$HOME/.dsh/AGENTS.md` when `DSH_HOME` is absent;
2. `AGENTS.md` and `CLAUDE.md` at the exact opened workspace root;
3. `AGENTS.local.md` and `CLAUDE.local.md` at that root.

Every existing regular UTF-8 file is considered. One source is capped at 1
MiB. In the project directory, later candidates whose complete content matches
an earlier candidate after trimming surrounding whitespace are suppressed.
The user-global path is deduplicated when it is the same absolute path as the
project candidate.

Unlike upstream, Rust does not walk above the opened workspace to a parent
`.git`: the directory descriptor selected by `--workspace` is the product's
authority boundary. Rust also treats a symlink candidate as unavailable. The
official provider follows final-component symlinks, but doing that here could
send an unrelated host file to the model from an untrusted checkout. These
differences affect discovery only; instruction text remains lower authority
than system, developer, and direct user instructions.

## Rendering, state, and event order

The complete rendered user-role message is capped at 65,536 UTF-8 bytes. It
uses the official `<system-reminder>` frame and escapes every literal
`</system-reminder>` in untrusted content. If all files do not fit, whole broad
files are dropped first; only the most-specific remaining file is truncated at
a UTF-8 boundary. A visible budget diagnostic names omitted and truncated
paths. An empty first baseline contributes no message.

The source record uses `kind: agent-instructions`, `form: instructions`, a
stable baseline identity, and bounded `{action, scope, path, digest}` changes.
SHA-1 is used only as the fixed upstream content identity, not for security.
For a new live baseline, the first step records:

1. `turn/start` and `step/start`;
2. the direct human `user/message`;
3. the workspace-instruction `user/message`;
4. the ordinary Provider request and the remaining step events.

The Agent owns pending injection so cancellation before the step cannot consume
the message. Once the instruction event commits, it is ordinary append-only
Session history and the Provider request is reconstructible from the surface.
The first non-empty ordinary or Goal input can carry the baseline; in either
case the input stays first and all later turns reuse the same visible history.

## Resume reconciliation

The loader derives effective instruction state only from messages still on the
current Session surface. A matching baseline identity plus matching scope/path
digests injects nothing. Confirmed current differences append one bounded
message: `set` for a newly visible candidate, `replace` for changed content,
and `remove` for confirmed disappearance or duplicate suppression. Unreadable,
invalid-UTF-8, oversized, or symlinked candidates are `unavailable`, not proof
of removal. An incompatible baseline identity appends a complete replacement
baseline and explicit removals for old scopes.

No historical event is edited. A later resume folds the latest visible
transitions and therefore does not duplicate an unchanged update.

## Failure, cancellation, and safety

Discovery runs in one bounded blocking task before Provider or tool work.
Cancellation is checked before every probe/read and after the task joins.
Missing files are normal. Source construction or task failure fails assembly
before a model request. Ordinary I/O failure skips that candidate and cannot
revoke a previously visible instruction.

The loader reads at most five candidates, at most 1 MiB each, and retains at
most 65,536 rendered bytes. It never logs absolute host paths or instruction
prose outside the intended durable user message. Terminal UI does not render
the injected message as a human prompt, and the text grants no file, Shell,
plugin, approval, Goal, or Plan authority.

## Verification and compatibility status

Unit fixtures cover candidate order, local overlays, duplicate collapse,
missing/unavailable/symlink/oversized/invalid sources, delimiter escaping,
UTF-8 truncation, broad-file omission, structured changes, unchanged resume,
and add/replace/remove reconciliation. An Agent test proves direct-prompt then
instruction event/request order and cancellation before entry. A real offline
CLI journey proves a workspace `AGENTS.md` reaches the first DeepSeek-shaped
request and durable JSONL exactly once.

The compatibility row remains `partial`: nested tool-touch discovery,
same-process compaction rearming, parent `.git` traversal, configurable names,
and upstream symlink following are absent or intentionally different, and no
generated cross-language oracle exists yet.
