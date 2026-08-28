# Bounded interactive user question

## Problem and first boundary

The fixed official Harness exposes `ask_user_question`: a model tool can pause
its current tool call, ask the human for a choice, and feed the answer back as
an ordinary tool result. This is useful when a coding decision genuinely cannot
be inferred from the workspace.

Phase 17 delivers the smallest useful terminal slice. One call contains exactly
one single-select question with two to four labelled options. The terminal shows
the optional heading, question, option labels and descriptions, then accepts the
corresponding number. Escape cancels only the question and returns a model-
visible tool error; Ctrl+C keeps its existing meaning and cancels the whole
turn.

This phase does not add free-form answers, multiple questions in one call,
multi-select, plan-review presentation, subagents, Web UI, or a general UI
plugin seam. The schema states these tighter limits so the model is not invited
to use behavior the CLI cannot collect.

## Official evidence

Semantic baseline: `deepseek-ai/deepseek-harness` commit
`47f943859bef60e4160492346772ded9b24f765a`.

- `packages/interaction/tool-ask-user/src/index.ts` defines the tool name,
  argument vocabulary, ordinary awaited execution, and compact JSON result.
- `packages/interaction/tool-ask-user/tests/tool-ask-user.spec.ts` fixes schema,
  option-label preservation, cancellation propagation, structured errors, and
  model-facing answer text.
- `packages/interaction/user-questions/src/{index,types}.ts` and
  `tests/user-questions.spec.ts` fix question/answer ownership, no-provider and
  cancellation failures, and the rule that pending UI activity is not separate
  model context.

Latest `master` was also inspected at
`cd5ef8148158c3a752a658978873241fdf8e2bbc`. The tool contract is unchanged;
the service provider changed from one registered provider to a scoped answerer
waterfall and normalized in-flight cancellation. Rust has one terminal answerer,
so reproducing the Cordis waterfall would add machinery without changing this
CLI's observable behavior.

## Inputs, outputs, and event order

The accepted model arguments are:

```json
{
  "questions": [{
    "id": "stable-id",
    "header": "Choose mode",
    "question": "Which mode should I use?",
    "options": [
      { "label": "Safe (Recommended)", "description": "Keep current checks." },
      { "label": "Fast", "description": "Run only focused checks." }
    ],
    "multi_select": false
  }]
}
```

`header` and `multi_select: false` are optional. Unknown fields, a different
question count, fewer than two or more than four options, duplicate labels,
control characters, and `multi_select: true` fail before UI dispatch.

A successful selection renders the official compact shape:

```json
{"answers":[{"id":"stable-id","selected":["Fast"]}]}
```

The Session order remains the ordinary tool order:

```text
assistant message with call -> tool/call -> wait for human -> tool/result
-> next Provider request
```

There is no separate question/answer Session event in the fixed official seam.
The assistant arguments preserve what was asked and the correlated tool result
preserves the accepted answer.

## State ownership and interfaces

`user_question` owns bounded validated request/answer values and a capacity-one
broker. A broker is a small channel: the tool sends one request to the terminal
and awaits its one-shot response. The CLI owns the only receiver and the active
question UI state. `LocalToolRegistry` only parses the model arguments and calls
the broker; it never reads terminal input.

The Agent continues to own event order, cancellation, tool correlation, and
result publication. A sealed `UserQuestion` claim lets the human wait use the
already bounded turn deadline rather than the ordinary 30-second tool deadline.
It does not grant file, Shell, plugin, Goal, approval, or Session authority.

Script mode has no human answerer and therefore does not advertise the tool.
Interactive assembly advertises it only after creating the terminal broker.

## Failure, cancellation, and recovery

- Invalid arguments become a normal model-facing `INVALID_TOOL_ARGUMENTS`
  result without opening the terminal question.
- A full, closed, dropped, or mismatched response channel becomes a bounded
  `UserQuestionError` result; it never silently chooses an option.
- Escape answers with `ASK_CANCELLED`, allowing the model to react in the next
  step. Ctrl+C cancels the owning turn through the existing cancellation token.
- EOF or terminal/output failure cancels the turn and closes the broker wait.
- A pending question cannot survive process exit. Recovery sees an unresolved
  old tool call and follows the existing no-replay repair policy; it never asks
  or selects again automatically.
- The ordinary 30-minute turn deadline is the hard waiting limit. This differs
  from the fixed package's unbounded UI wait, and prevents an abandoned terminal
  from owning resources forever.

## Side effects and security

The feature writes only terminal presentation and existing Session tool facts.
It starts no process, performs no network request, reads no file, and changes no
approval decision. Question strings are untrusted model output: byte/count
limits are checked before allocation-heavy presentation, control characters are
rejected, and the renderer uses the existing visible-text path.

Input is fenced after the complete question frame is displayed: buffered bytes
from before the question are discarded, so stale typing cannot choose an
option. Only a displayed numeric choice can create a successful answer.

## Tests and compatibility status

Focused tests cover the exact schema, every argument bound, broker laziness,
selection correlation, Escape, cancellation priority, full/closed channels,
the longer Agent wait classification, and compact JSON output. A real fake-
Provider PTY journey must prove question display, numeric answer, durable tool
result visibility in the second request, final assistant continuation, and
clean exit. A second focused path covers cancellation with no fabricated
selection.

The compatibility row remains `partial`: the official fixed contract supports
question batches, optional choices, custom answers, and multi-select, while
this first terminal slice intentionally accepts one closed single-choice form.
Local format, all-target compilation, focused tests, Clippy with warnings denied,
and whitespace checks are the Phase 17 gate. Remote CI, the full repository
suite, and cross-platform reruns are omitted under the user-selected fast local
validation boundary.
