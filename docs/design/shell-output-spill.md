# Bounded Shell output spill design

Status: Phase 33 implemented design, 2026-08-29.

## Problem and scope

dsh-rs keeps only the last 64,000 bytes of each Shell stream. That makes the
result bounded, but an early compiler error or test failure can disappear when
a command prints a large tail. The fixed official Harness solves this by
spilling the complete captured stream to a private temporary file and returning
its path beside the tail.

This phase adds that foreground-Bash behavior. It does not add background jobs,
persistent Bash, a generic spill service for every tool, arbitrary host-file
access to `read`, a spill browser, or database-grade artifact retention.

## Upstream basis

The semantic baseline is commit
`47f943859bef60e4160492346772ded9b24f765a`, especially:

- `packages/subprocess/subprocess-local/src/spawn.ts` and
  `tests/spawn.spec.ts`;
- `packages/shell/bash-local/src/index.ts`, README, and executor tests;
- `packages/shell/tool-bash/src/render.ts` and `tests/tools.spec.ts`;
- `.agents/notes/implemented/architecture/2026-07-08-tool-output-spill-files.md`.

Latest inspected master `cd5ef8148158c3a752a658978873241fdf8e2bbc`
retains the same 64,000-byte tail, 67,108,864-byte official per-stream spill
default, locator rendering, and private-file hardening.

## Input, output, and event order

The Bash schema and approval request do not change. After an approved process
starts, stdout and stderr are captured independently. Each keeps a 64,000-byte
tail. On the first overflow, dsh lazily creates one private per-run directory,
opens a random per-stream file exclusively, writes the already retained prefix,
then appends later chunks with backpressure.

After process quiescence, a successful flush seals the locator. The ordinary
tool result contains the tail, a truncation notice and optional spill path. Its
metadata adds per-stream path-or-null and captured-byte counts. The existing
order remains:

```text
tool/call -> optional approval -> process side effect -> tool/result
```

The full spill bytes are not Session content. Only the bounded tail, locator,
counts, exit facts and existing error markers are logged.

## Failure, rejection, cancellation, and timeout

Rejection still starts no process and creates no spill directory. A directory,
file, write, or flush failure disables that stream's spill, removes any
unpublished partial file when possible, and returns the old tail-only notice.
It never changes the command's exit status or creates a false success/failure.

Cancellation and timeout keep their existing process-group cleanup. Bytes
observed before settlement can be spilled; the result remains cancelled or
timed out. Pipe failure or the 8 MiB output stop labels a retained locator as
captured/incomplete rather than full. Ownership loss never publishes a tool
result or locator; unpublished files are best-effort removed.

## Side effects and ownership

Only an approved, actually started Shell command can create spill artifacts.
`ProcessRunner` owns stream collection and an optional per-run spill directory.
The Shell renderer owns model text and metadata. Session and TUI only consume
the final bounded facts.

Directories use mode 0700; files use 0600 plus exclusive creation and random
names. Artifacts remain in the OS temporary area after a successful result so
the model or user can inspect them. They are not written inside the project.

## Recovery and replay

The locator is ordinary append-only tool-result data. Resume may show it again,
but the temporary file can already have expired; dsh never recreates it and
never replays the command. A fork can inherit the old locator without taking
ownership. No hidden in-memory map is needed to interpret the Session.

## Safety and resource limits

Rust deliberately keeps its current 8 MiB combined output ceiling and kills the
process group after the first byte beyond it. Consequently each spill is at
most that bounded observed amount, not the official 64 MiB per stream. File I/O
uses Tokio's blocking-file adapter and is awaited, so pipe reading cannot outrun
storage without bound. The in-memory tail remains 64,000 bytes per stream.

The locator can point outside the workspace only to dsh's random private temp
file. The built-in `read` authority remains unchanged. Retrieval uses a normal
approved Bash command. Shell output can contain secrets chosen by the command;
the owner-only artifact persists until OS/user cleanup, so the README must make
that privacy tradeoff visible.

## Tests and compatibility

A source-attributed fixture records the fixed normal overflow result, tail,
full file bytes and permission expectations. Focused tests cover exact-cap/no-
file, uneven chunks, stdout/stderr independence, natural overflow, file/setup/
flush failure fallback, output-limit wording, cancellation, cleanup, metadata,
renderer bounds, Session order, terminal presentation and a real CLI journey.

Rust matches the observable normal-path tail plus full-output locator shape but
keeps the smaller aggregate stop, uses one private directory per spilling run,
does not expose spill paths to workspace `read`, and has no generic Cordis spill
policy. These are documented intentional safety/product differences. Without a
generated cross-language producer, the compatibility claim remains `partial`.
