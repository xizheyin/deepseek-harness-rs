# Phase 13 validation

Status: `complete`

Phase 13 implements the durable Goal boundary frozen in
`docs/design/goal-persistence.md`. The semantic baseline is DeepSeek Harness
commit `47f943859bef60e4160492346772ded9b24f765a`; latest `master` is reference
input only.

## User-selected gate

The user requested local necessary validation and rapid progress. This phase
therefore requires focused Goal codec/replay/tool tests, one real-binary
save/resume/rearm journey, format, all-target compilation, Clippy with warnings
denied, and `git diff --check`. Remote and full cross-platform matrices do not
block this checkpoint.

## Delivered evidence

- `src/goal.rs` owns the version-1 full-snapshot/clear payload, strict fold,
  opaque IDs, revision/phase/timestamp/round checks, used-ID set, two-phase
  runtime mutation, 32-round cap, and process-local activation.
- `src/session/{event,codec,projection,observer}.rs` recognizes
  `goal/change`, keeps it off the model-visible surface, advances rounds only
  from exact `{kind,goalId,revision,round}` user sources, and exposes the
  replayed facts to assembly.
- `/goal` materializes a deferred Session when necessary and commits the event
  before updating its local cache. Ctrl+C closes the round and commits a pause
  event. A resumed Session restores the Goal disarmed; `/goal resume` records a
  new revision before any automatic request.
- `create_goal` and `update_goal` use a sealed prepared mutation. The real
  journal assertion fixes `tool/call -> goal/change -> tool/result`; plugins
  cannot construct this carrier.
- Unit coverage rejects stale reapplication, malformed event kind, and cleared
  ID reuse; codec replay proves rounds and disarmed activation survive.
- The real enhanced-PTY journey creates a Goal, starts and cancels round 1,
  exits, resumes the stored Session, shows `paused`/`disarmed`, explicitly
  rearms, starts round 2, completes through `update_goal`, and checks journal
  order.

## Local verification

Run on macOS in `/Users/xizheyin/workspace/ds-harness-rs`:

```console
cargo check --all-targets
cargo test goal --lib
cargo test --test interactive_cli goal_ -- --nocapture
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
git diff --check
```

Results: all-target compilation passed; 9 focused library tests passed; 3 real
Goal PTY tests passed; format, Clippy with warnings denied, and whitespace
checks passed. In line with the user-selected fast local gate, the unrelated
full repository suite, remote CI, and cross-platform reruns were not repeated.

## Remaining gaps

Compatibility remains `partial`: Rust keeps 32 rather than 256 automatic
rounds, does not expose the official per-caller Goal-tool authority split or
per-Goal cap configuration, accepts no Goal image attachments, and has no
background autonomous worker. Resumed work is intentionally safer than an
automatic restart because activation is always disarmed.
