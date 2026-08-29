# Current Session log export

## Problem and scope

People need a simple way to keep or inspect the exact Session record without
finding the private application-state directory. Phase 50 adds an idle,
pathless `/export` command for the current Session. It produces one raw JSONL
copy in the opened workspace. It does not export another Session, import a log,
register the copy for resume, include descendants/media, or build a general
archive service.

## Upstream basis

The semantic baseline remains
`47f943859bef60e4160492346772ded9b24f765a`. The focused evidence is:

- `packages/session-query/session-log-export/src/index.ts` and
  `tests/command.client.spec.ts` for the pathless `/export` command, local
  acknowledgement and zero model effect;
- `packages/host/apiproxy/src/session-export.ts` and
  `tests/session-export.spec.ts` for flush-before-read, byte-identical root
  artifacts, failure, cancellation, bounded streaming and safe filenames;
- `packages/session-query/session-log-export/README.md` for the shipped Web
  lifecycle and deferred Host-path behavior.

Fresh `origin/master` at
`cd5ef8148158c3a752a658978873241fdf8e2bbc` moves the archive and authenticated
GET/HEAD route into `packages/session-query/session-log-export`, but retains the
same pathless command, flush, exact raw entry and cancellation contract.

## Input, output and operation order

`/export` accepts no arguments and only runs while the terminal Agent is idle.
The terminal reserves a new root-level workspace filename:

```text
dsh-session-<safe-session-id>.jsonl
dsh-session-<safe-session-id>-2.jsonl
...
```

At most 100 candidates are tried. Each file is created with `create_new` and
mode `0600`; no existing path is followed or overwritten. The transaction is:

```text
validate idle and cancellation
materialize the current durable Session if necessary
collect an already-finished title result
flush the authoritative Session barrier
copy exactly the durable byte prefix in 64 KiB chunks
flush the destination file
publish success with the generated filename, workspace root and byte count
```

The copy uses positional reads, so it never moves the journal writer's append
cursor. The bytes are not parsed or encoded again. No Session event, model
message, Provider request, tool call or approval event is created.

## Ownership and recovery

`WorkspaceAuthority` owns the retained directory capability used to create and
remove the generated output. A small terminal-side export target owns the
reserved leaf until success; dropping or failing it attempts to unlink the
partial file. `Session` owns the durability barrier. `JournalWriter` owns the
source descriptor and the bounded positional copy, so no ambient path re-open
or competing lock is needed.

The export file is outside the Session store and is not discovered by
`--resume`. A resumed Session can run `/export` normally because its recovered
writer owns the same validated raw journal. No interrupted export is replayed.

## Failure, cancellation and timeout

A pre-cancelled request creates no durable Session fact and returns cancelled.
`Ctrl+C` propagates one cancellation token to the writer; the copy checks it
before every 64 KiB chunk and before destination synchronization. The terminal
waits for the owned operation to settle and then deletes the incomplete file.

Workspace admission exhaustion, unsafe destination metadata and destination
write/sync failures return a bounded export failure while leaving the Session
usable. A source read or durability failure is an Agent storage failure because
the authoritative journal can no longer be trusted. There is no separate time
deadline: the finite 512 MiB journal bound and cancellation provide the limit,
while an arbitrary short timeout could corrupt the usefulness of large exports.

## Side effects, safety and resource limits

The only new side effect is one explicitly requested file inside the retained
workspace. Direct human invocation is the authority; the model cannot call the
command and the tool approval pipeline is not involved. The output is private
to its owner initially, never overwrites a file, never follows a user path and
uses a generated ASCII leaf. The terminal prints the generated leaf and says
that it is in the workspace root; errors do not expose private storage paths.

Memory use stays one 64 KiB buffer plus fixed bookkeeping. Disk output cannot
exceed the already accepted durable journal prefix. A Session log can contain
prompts, tool output and other sensitive data, so README must tell users to
protect or delete exported files.

## Tests and compatibility

The source-attributed fixture fixes the pathless command, exact root bytes,
flush order, failure and cancellation. Writer tests cover byte identity,
append-cursor preservation, destination failure and cancellation cleanup.
Target tests cover safe names, collision suffixes, permissions and no
overwrite. Agent tests cover memory rejection, pre-cancellation and idle-only
admission. Real enhanced and linear PTY tests cover actual file creation,
argument rejection, exact raw bytes and a second non-overwriting export.

The compatibility row remains `partial`: Rust preserves the current root log's
observable safety semantics but emits one `.jsonl`, not the official ZIP with
subagent descendants and media.

## Intentional differences

Official `/export` is mounted only by the Web bundle and asks the browser to
download a ZIP; browser download settings choose the destination. Rust has no
Web client, attachment store or product subagents, so the terminal writes one
generated, owner-only raw JSONL file to the already-authorized workspace. It
does not accept a path either. Rust also omits upstream `command/run` and
`command/done` facts until local commands share one designed command-event
schema, so the exported bytes end at the last ordinary Session fact rather than
including an export acknowledgement pair.
