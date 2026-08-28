# Draft-preserving user-question pager

## Scope and upstream basis

Phase 22 adds only backward/forward navigation inside the existing bounded
`ask_user_question` batch. It follows fixed upstream commit
`47f943859bef60e4160492346772ded9b24f765a`, specifically
`QuestionComposer.tsx` and its client pager test. Latest inspected master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` keeps the same behavior. It does not
add plan review, subagent routing, a second answerer, or new model-visible data.

## State and ordering

The terminal UI owns a fixed-length draft vector, one entry per validated
question. Each entry retains ordered selected indices, bounded custom text, and
an explicit skipped flag. The current page index is separate from answer
completeness. Previous/Next changes the page and emits no tool result.

Single selection, multi submission, custom submission, and skip still advance
normally. On the last page, submission scans drafts in request order. If one is
incomplete, the UI returns to the first missing page with retry feedback. Only
a complete vector is converted to the existing response variants and sent once
through the capacity-one broker.

## Terminal and Composer ownership

Enhanced choice screens use `[` and `]`; enhanced free-text editing uses Ctrl+P
and Ctrl+N. Linear mode accepts `[` or `]` as a complete line. Leaving a custom
page first stores its bounded Composer text; returning restores that text for
editing. The ordinary next-turn draft and history cursor remain in the existing
exclusive overlay and are restored when the question editor closes.

Escape cancels the whole question batch, Ctrl+C still cancels the turn, and EOF
keeps its existing cleanup path. Navigation never grants approval, starts a
tool, writes a Session event, or performs an external side effect.

## Failure, limits, and tests

Draft allocation happens before the envelope becomes active. Invalid page
transitions fail closed, invalid custom text remains local for correction, and
no partial answer is published. Existing limits remain: at most three
questions, four options, and 4,096 custom UTF-8 bytes.

Focused state tests cover draft retention, editing, first-missing return, and
all-or-nothing publication. A real enhanced PTY journey covers forward and
backward navigation across choice, Unicode custom, and multi-select pages,
including fast text-plus-control-key input and restoration of retained drafts.
This terminal key mapping is an intentional presentation difference; the
model-visible ordered answer is unchanged.
