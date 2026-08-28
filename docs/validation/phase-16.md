# Phase 16 validation

Status: `complete`

Phase 16 implements `docs/design/goal-wrapup.md` against fixed upstream commit
`47f943859bef60e4160492346772ded9b24f765a`.

## Delivered evidence

- A detached bounded `GoalWrapup` renders separate `<goal_complete>` and
  `<goal_blocked>` instructions with JSON-quoted objective/blocker, grounding,
  direct-user wording, and the no-more-tools rule.
- Agent selects it only for a successful terminal mutation whose sealed caller
  is the exact Goal round. Direct-human completion/block injects nothing.
- At most one wrap-up is retained while the already-declared tool batch settles;
  Session appends it as a `tool-goal` plugin-notice user message after tool
  results and before the next Provider request.
- Unit tests cover both terminal tags, quoting, blocker text, summary, and the
  no-more-tools instruction.
- The real auto-complete and resumed-complete PTY journeys assert that the last
  user input of the post-tool Provider request is `<goal_complete>`. The durable
  journal asserts `tool/call -> goal/change -> tool/result -> user/message`.

## Local verification

```console
cargo check --all-targets
cargo test goal --lib -- --nocapture
cargo test --test interactive_cli goal_ -- --nocapture
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
git diff --check
```

Results: all-target compilation passed; 15 focused Goal/wrap-up library tests
passed; 3 real Goal PTY journeys passed; format, Clippy with warnings denied,
and whitespace checks passed. The unrelated full suite, remote CI, and cross-
platform reruns were omitted under the user-selected fast local gate.

## Remaining gaps

The Goal compatibility row remains `partial` because the default/cap range is
intentionally smaller, latest `master` image attachments are absent, Rust has
no product subagent graph or background Goal worker, and no generated cross-
runtime Goal oracle has been committed yet.
