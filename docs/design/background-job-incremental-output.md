# Phase 44 background-job incremental output design

## 1. Problem and scope

Phase 42 can detach Bash and Phase 43 reports completion, but `job_output`
returns no command bytes while work is live. This makes a model wait for a long
build or server even when useful diagnostics already exist. The fixed upstream
defines Bash as a stream job: every read consumes output produced since the
previous read, including after exit.

Phase 44 adds that single consuming cursor for the existing process-local Bash
producer. It does not add persisted jobs, terminal input, PTY sessions,
non-Shell producers or a general streaming tool API.

## 2. Upstream basis

The semantic baseline remains
`47f943859bef60e4160492346772ded9b24f765a`. Exact fixed sources and tests are
recorded in `docs/upstream.md`. Freshly fetched master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` changes only first-party prompt
section constants in the relevant tool packages; the read cursor and output
contract are unchanged.

## 3. Input, output, state and order

The public schema remains `job_output {job_id, wait?, timeout_ms?}`. Without
`wait`, it snapshots immediately. With `wait`, it first waits for settlement or
the existing bound, then performs the same consuming read.

Each Bash record owns one cursor with independent stdout and stderr offsets.
The process collector publishes a bounded tail snapshot whenever it observes a
pipe chunk. A read returns stdout since its offset, then a `[stderr]` section
when stderr has new bytes, and advances both offsets atomically. A second read
without new bytes returns `(no new output)` plus the current status. After
settlement, unread bytes remain available once; they are not re-delivered.

`job_list` and internal status snapshots never consume output. Tool call and
result ordering remains the ordinary unified pipeline:

```text
job_output tool/call → in-memory cursor read → bounded tool/result
```

## 4. Normal, failure, cancellation, timeout and suppression

- A running read returns the currently observed delta and `[status: running]`.
- A timed-out wait reads the current delta, leaves the process alive and keeps
  the normal running status.
- Cancelling a wait cancels only that tool call; unread output remains.
- A terminal read marks the job reported and suppresses an unclaimed completion
  notice, exactly as Phase 43.
- `job_kill` and teardown keep their current report/cleanup semantics. Output
  observed before cancellation remains readable until the registry exits.
- Spawn/ownership/normalization failures without pipe bytes expose one bounded
  fallback diagnostic, then consume it.
- UTF-8 decoding is lossy but never panics. A cursor that fell behind the
  retained window receives the retained tail and an explicit loss notice.

## 5. Side effects

Reading only mutates the in-memory cursor and the existing terminal `reported`
flag. It creates no process, file, network request or approval. The background
Shell still owns its existing private spill files. The new live snapshot is a
bounded duplicate of already observed bytes, not a new capture authority.

## 6. Ownership and interfaces

`tools::process` owns a shared read-only output tap attached before spawn. The
process runner is its sole writer. It retains at most 64,000 bytes per stream,
whole-stream offsets, final spill paths and an incomplete flag. `tools::jobs`
owns the one consumer cursor and renders the generic job result. Neither the
Agent nor TUI reads process pipes or job internals directly.

The tap uses a short synchronous mutex only for bounded memory copies. No lock
is held across `.await`, process observation, Session append or model work.

## 7. Recovery and replay

The cursor, tap and live process remain process-local and disappear on exit.
Every `job_output` call/result that reached Session is still durable evidence,
but resume never reconstructs a live cursor or replays a read. Old job ids stay
unknown, preserving the existing no-side-effect-replay boundary.

## 8. Resource and security limits

The tap keeps two exact 64,000-byte tails per active Bash job. Existing limits
still cap eight live jobs, 64 retained records, 8 MiB total observed process
output, 64 KiB encoded tool content, private spill files and the 295-second
command lifetime. Falling behind does not grow memory; it advances to the
retained window and reports loss. Spill locators are exposed only after the
collector has finalized them. A forced output-limit spill is described as
captured rather than complete output.

The command already required Shell approval. Incremental reads add no approval
because they only reveal bytes that the approved job has already emitted to
its owner.

## 9. Tests and comparison evidence

A fixed-source fixture records the stream cursor, stdout/stderr rendering,
terminal read, lossy read and non-consuming list semantics. Unit tests cover
tail offsets, loss, UTF-8 boundaries and finalization. Real process tests prove
output becomes readable before exit. Job tests cover repeated live reads,
stderr, timeout/cancellation, terminal one-shot delivery, fallback and notice
suppression. The real CLI background journey reads final output and proves a
second read returns no new bytes.

Acceptance is local-only: focused tests, one all-target run, formatting,
compiler, Clippy and `git diff --check`. No real DeepSeek call, public network,
remote CI or extra platform/stress matrix is required.

## 10. Intentional differences

The fixed upstream background Bash has no command timeout; Rust retains its
295-second safety cap. Upstream collector limits are configuration-owned; Rust
uses its existing fixed 64,000-byte per-stream tail and 8 MiB observation cap.
Rust advertises spill paths only after final flush, so a lossy live read may say
the missing output is unavailable until a later terminal read. Rust has only
one Agent-owned Bash producer and no owner replacement routing. These bounded
differences remain explicit and keep the compatibility row `partial`.
