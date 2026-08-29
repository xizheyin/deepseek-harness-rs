# Completed-turn current Session fork

## Problem and scope

People often want to try a different direction without losing the current
conversation. Phase 51 adds idle `/fork [EVENT_SEQ]` for making a separate,
resumable child Session from the current Session's completed history. The
optional event sequence anchors a turn; it is not a byte offset. The command
does not fork an arbitrary closed Session directly, run a second Agent, switch
the current terminal automatically, copy external attachments, or expose a
model tool.

## Upstream basis

The semantic baseline is
`47f943859bef60e4160492346772ded9b24f765a`. The exact paths and current-master
move are recorded in `docs/upstream.md`. The fixed core tests establish copied
seed events, lineage, end-seed and strict boundary invariants. The fixed Host
tests establish the user-facing completed-turn anchor. Latest master at
`cd5ef8148158c3a752a658978873241fdf8e2bbc` retains that behavior under Session
Controller and retains optional client-side fork-title increment.

## Input and boundary selection

The terminal accepts only these forms:

```text
/fork
/fork 123
```

The number must be a canonical non-negative safe integer. With no number, or
with a number greater than the last event sequence, the latest `turn/end` is
selected. For an in-log anchor, the first `turn/end` whose sequence is at least
the anchor is selected. If none exists, the anchor belongs to an unfinished
turn and the command fails rather than selecting an earlier turn. A Session
with no completed turn cannot be forked through this command.

After the selected `turn/end`, the cut includes consecutive standalone facts
until the next `turn/start`. This preserves a title or other log-only fact
published immediately after the turn while excluding the next turn.

## Operation order and output

```text
validate idle command and cancellation
materialize the parent if needed
collect already-finished automatic title work
flush the parent durability barrier
scan exact durable rows and select the completed cut
allocate a fresh child id and locked 0600 journal
write child header
copy the exact selected parent event-row bytes in 64 KiB chunks
append session/end-seed and an incremented user title when available
sync the child file and verify its canonical name/identity/size
publish success with child id and dsh --resume command
```

The child header records the same canonical workspace identity and cwd,
`parentSession` equal to the current Session id, and `seedLength` equal to the
number of copied parent events. Ordinary fork does not set `origin: subagent`.
The new marker sequence is exactly `seedLength`. A generated title event, when
present, follows the marker and uses user source with no message sequence.

The command creates no parent Session event, Agent turn, Provider request, tool
call or approval. The parent remains active and unchanged. The child is
discovered by `--list-sessions`, can be resumed with the printed command, and
inherits the selected request-header facts through its seed.

## Title behavior

If the current Session has a validated title, the child receives a bounded
version of the official client increment:

- `Work` becomes `Work (1)`;
- `Work (1)` becomes `Work (2)`;
- `Work（9）` becomes `Work（10）`.

Decimal increment is performed as a bounded string operation, so it cannot
overflow an integer. The prefix is shortened at a UTF-8 boundary when required
to keep the existing 80-byte canonical title limit. If no title exists, no
title event is invented.

## Ownership, failure and cancellation

`Session` owns the durability barrier and the authoritative logical event
count. `JournalWriter` owns positional source scanning and copying, so the
append cursor never moves. `SessionStore` owns destination capacity, canonical
name creation, the exclusive file lock, identity checks, directory sync and
failure cleanup. The terminal owns the fresh id and displays the result.

The scan and copy each check cancellation between fixed-size chunks. `Ctrl+C`
cancels the current stage, waits for its owned worker operation to settle and
removes the unaccepted child. Destination creation/write/sync failure leaves
the parent usable. Source read, JSONL-contiguity or inspected-snapshot mismatch
poisons the parent because its accepted journal no longer agrees with the
Session state. A missing completed turn is a normal unavailable result, not
storage corruption.

The source snapshot records its durable length and selected byte boundary.
Copy refuses if the journal changed between inspection and copying. The one
idle Agent and collected title task normally make that impossible; the check
keeps the storage seam safe if a future background producer is added.

## Side effects, security and resource bounds

The only accepted side effect is one new canonical JSONL file in the private
Session store. Creation first uses a non-canonical staging name with
`create_new`, mode `0600`, no symlink following, one link, the existing
128-session/256-entry store capacity and an exclusive advisory lock. After the
complete file is synchronized, a no-replace atomic rename publishes the
canonical name. Existing files are never overwritten. A failed target is
unlinked only after its opened inode is matched to its current name.

Scanning retains at most one bounded JSONL row (9 MiB maximum) and copies with
one 64 KiB buffer. The journal remains under the existing 512 MiB Session
limit. There is no arbitrary short timeout; cancellation and finite source
bounds prevent unowned work. Fork is not a backup or sandbox, and the copied
history can contain sensitive prompts and tool output.

## Recovery and tests

The child is accepted only after a complete sync. Normal recovery revalidates
its header, contiguous inherited events, end-seed and any title before a new
model request or tool side effect. No interrupted fork is replayed.

Tests cover fixed-source boundary cases, canonical input, exact row copying,
trailing facts, past-end fallback, unfinished/no-turn rejection, private
collision-safe creation, cancellation, destination/source failures, title
increment, parent usability, strict child resume, lineage/search visibility,
inherited model selection and enhanced/linear real terminal journeys.

## Intentional differences

Rust exposes a terminal-local current-Session command instead of the Web RPC.
It leaves the parent open and prints a resume command; official callers also
choose whether to open the newly addressable child. Rust cannot fork a cold
Session without first resuming it and has no product subagent workspace tree or
attachment store. It applies the official UI's requested title increment by
default so two entries are distinguishable in the terminal picker. These
differences affect presentation and available sources, not the selected seed,
lineage, end-seed or resumed model history.
