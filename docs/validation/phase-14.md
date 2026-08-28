# Phase 14 validation

Status: `complete`

Phase 14 implements the caller matrix in
`docs/design/goal-tool-authority.md` against fixed upstream commit
`47f943859bef60e4160492346772ded9b24f765a`.

## Delivered evidence

- Agent classifies validated turn input once as direct human, exact Goal round,
  or untrusted; direct human wins a mixed turn.
- The fact is retained inside the per-call sealed `ToolDispatchBinding`, not in
  model arguments or plugin-controlled data.
- `LocalToolRegistry` requires direct human for create/edit/pause/resume,
  permits complete/block for direct human or Goal round, and returns
  `GOAL_TOOL_AUTHORITY_REQUIRED` without preparing a mutation otherwise.
- The three-round block threshold moved out of the Goal event fold and into the
  Goal-round tool policy. Direct human can block earlier, matching upstream.
- Focused tests cover classification precedence, rejected Goal-round creation
  and edit, allowed Goal-round completion, early Goal-round block rejection,
  allowed direct-human early block, and unchanged Goal state after rejection.
- The three existing real Goal PTY journeys remain green, including automatic
  completion and durable recovery/rearm.

## Local verification

```console
cargo check --all-targets
cargo test goal --lib
cargo test --test interactive_cli goal_ -- --nocapture
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
git diff --check
```

Results: all-target compilation passed; 12 focused Goal/caller library tests
passed; 3 real Goal PTY tests passed; format, Clippy with warnings denied, and
whitespace checks passed. Per the user-selected gate, the unrelated full suite,
remote CI, and cross-platform reruns were not repeated.

## Remaining gaps

The Goal row remains `partial`: Rust still uses its older tool argument names,
does not require `goal_id`, cannot edit `max_goal_rounds`, does not accept a
model-provided `blocked_reason`, emits no official wrap-up context, caps at 32
rounds, and has no image or background-work support.
