# Phase 51 validation — completed-turn current Session fork

Date: 2026-08-29

Tested tree: Phase 51 working tree immediately before its green commit.

Environment: macOS 27.0 arm64, Rust 1.85.0
(`4d91de4e48198da2e33413efdcd9cd2cc0c46688`).

## Delivered behavior

- Idle `/fork [EVENT_SEQ]` is available in enhanced and linear terminals, the
  fourteen-command palette and `/help`. The optional value is one canonical
  non-negative safe event sequence; malformed values show usage locally.
- Omitted and past-end anchors select the latest completed turn. An in-log
  anchor selects the first `turn/end` at or after it. Standalone facts after
  that boundary are inherited until the next `turn/start`; an unfinished or
  no-turn source fails without creating a child.
- The child header records a fresh id, the retained workspace, direct
  `parentSession` and exact `seedLength`. Selected source event rows are copied
  byte-for-byte, followed by `session/end-seed` only when the seed does not
  already end with that marker.
- An existing title receives a bounded user-owned fork suffix: ` (1)`, an
  incremented `(N)`, or an incremented full-width `（N）`. Decimal increment is
  string-based and cannot overflow an integer.
- The private store writes a locked `0600` non-canonical staging file, syncs
  it, and publishes the canonical child with an atomic no-replace rename.
  Cancellation/failure removes the matched staging or final inode; existing
  Session files are never overwritten.
- Success prints a copyable `dsh --resume <child-id>` command. The parent stays
  active, and fork creates no parent event, model request, tool or approval.

## Evidence

- Source-attributed fixture:
  `tests/fixtures/tools/upstream_phase51_session_fork.json`.
- Journal tests cover completed anchors, trailing facts, omitted/past-end
  fallback, open-turn rejection, exact copied rows, source cursor isolation
  and deterministic cancellation between 64 KiB chunks.
- Destination/title tests cover private staging, no pre-commit canonical name,
  atomic publication, collision refusal, failed-target cleanup, inherited
  end-seed de-duplication, both bracket styles, UTF-8 limits and unbounded
  decimal suffixes.
- Agent/recovery tests create a child, continue and rename the parent, strictly
  resume the child, verify parent/seed metadata, inherited model-visible
  history and the incremented title. Empty and pre-cancelled cases create no
  child.
- The enhanced real PTY journey forks after one turn, continues only in the
  parent, exits, resumes the child and proves the child request contains the
  pre-fork prompt but not the later parent prompt. The linear real PTY journey
  covers past-end fallback and zero ANSI output.
- Local gates passed:

```console
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets -q
cargo clippy --all-targets -- -D warnings
git diff --check
```

The all-target run passed 1,419 tests with no ignored tests. The first combined
gate reached green tests and then found one Clippy-only redundant binding; that
binding was removed and formatting, Clippy and diff checks were rerun green.
No network, real DeepSeek credential, remote CI or extra stress matrix was used.

Independent review was not separately delegated under the user-requested
local-minimal validation scope; compiler, Clippy, deterministic fixture, full
repository tests, strict recovery and real PTY journeys form the acceptance
evidence.

## Known limits

- `/fork` operates only on the current live Session. A closed Session must be
  resumed first; there is no direct cold-source terminal command.
- The parent remains current after creation. Rust prints a resume command
  instead of automatically switching or running a second Agent.
- Rust has no product subagent Workspace tree or attachment store, so ordinary
  fork copies only Session JSONL facts and cannot attach external media.
- The command requires a completed turn, following the fixed Host API rather
  than the lower-level core store's ability to fork an empty Session.
