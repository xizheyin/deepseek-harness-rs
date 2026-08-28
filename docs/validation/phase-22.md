# Phase 22 local validation — 2026-08-28

## Result

Phase 22 is complete under the user-requested local-only, necessary-check gate.
The production terminal can navigate a bounded question batch in both
directions, retain each question's draft, and return to the first missing draft
without publishing a partial model result.

## Evidence

- Fixed upstream `QuestionComposer.tsx` and its pager fixture were inspected;
  latest master `cd5ef8148158c3a752a658978873241fdf8e2bbc` keeps the contract.
- Two focused UI-state tests cover retained Unicode custom/multi drafts and
  final first-missing return.
- One real enhanced PTY journey crosses three question kinds in both directions,
  restores the saved custom and ordered multi-selection, fills the missing
  first question, and asserts the exact correlated JSON tool result.
- The decoder recognizes Ctrl+P/Ctrl+N even when terminal reads coalesce nearby
  Unicode text and the control key.

## Local commands

```console
cargo test cli::user_question::tests -- --nocapture
cargo test user_question_pager_retains_drafts_and_returns_to_the_missing_first_question --test interactive_cli -- --nocapture
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

The user explicitly asked to reduce checks and validate only on this machine.
The full repository test suite and remote/cross-platform CI were therefore not
repeated for this checkpoint.
