# Fixed-upstream `write` and `edit` tools

Status: Phase 34 implemented design, 2026-08-29.

## Problem and scope

The fixed official code preset exposes ordinary `write` and `edit` names, while
dsh-rs currently asks the model to use `apply_patch` or
`str_replace_editor`. Adding the official names makes common create, complete
rewrite and literal replacement tasks direct and improves compatibility with
the model's expected tool vocabulary.

This phase adds only UTF-8 text `write` and `edit`. It does not add
`read_image`, arbitrary host paths, multi-file transactions, sandbox
escalation arguments, a hidden filesystem policy plugin, background work or a
second publication implementation.

## Upstream basis

The semantic baseline is commit
`47f943859bef60e4160492346772ded9b24f765a`, especially:

- `packages/fs/tool-fs/src/{write,edit,diff,error,session-cwd}.ts`;
- `packages/fs/tool-fs/tests/{tools,integration}.spec.ts`;
- `packages/fs/{fs,fs-local,fs-observation-policy}`;
- `apps/cli/config/agent-presets/code/agent.cordis.yml`.

Latest inspected master `cd5ef8148158c3a752a658978873241fdf8e2bbc`
retains the schemas and model-facing result vocabulary.

## Inputs, outputs and event order

`write` accepts exactly `file_path` and `content`. Empty content is valid. At
preparation it resolves the path inside the retained workspace and chooses
create when absent or update when one regular file exists. Its successful model
text is the official `<path>`, `<type>file</type>`, and `Created file` or
`Updated file` envelope.

`edit` accepts exactly `file_path`, non-empty `old_string`, `new_string`, and
optional boolean `replace_all` defaulting to false. Equal old/new strings are
invalid. Default mode requires exactly one non-overlapping match; explicit all
mode replaces every match and still fails when there is none. Its two success
sentences follow the fixed official wording.

Both are exclusive tools. Their durable order is unchanged:

```text
assistant tool call -> tool/call -> optional approval -> filesystem publish -> tool/result
```

The ordinary result keeps the bounded success text plus existing replayable
mutation metadata. Full before/after text is used only during bounded
preparation and is not copied into Session.

## Failure, rejection, cancellation and timeout

Unknown fields, wrong types, blank paths, empty old strings and identical
old/new pairs fail before approval. Missing edit targets, binary/unsafe text,
no match, ambiguous default match, over-limit content, outside paths, links and
non-regular targets produce model-facing errors with no mutation.

Policy denial or human rejection creates no file change. Cancellation before
publication returns the existing aborted result; cancellation after the atomic
publication boundary cannot relabel a committed change as absent. An external
change between preparation and approval is detected by the existing late
identity/content check. File tools have the Agent's bounded action lifetime but
no model-configurable timeout.

## Ownership and side effects

One small `write_edit` module owns schema-specific parsing and candidate
generation. `patch::prepare_text_mutation` and a narrow create-or-update helper
continue to own validation, preview, approval material, result construction and
publication. `Workspace` remains the sole filesystem capability and conflict
owner. TUI consumes the same canonical mutation metadata and needs no new
filesystem access.

## Recovery and replay

The call arguments, approval pair and committed/not-committed result are
ordinary append-only Session facts. Recovery never replays either mutation.
Successful results retain the same trusted workspace-touch fact used to refresh
nested project instructions. No observation cache or unpublished baseline is
required for replay.

## Security and resource limits

Paths remain inside the opened workspace; symbolic links, hard links and unsafe
parent changes fail closed. Existing files and candidates are at most 16 MiB,
100,000 lines and 1 MiB per line, with safe LF text for new content and
preservation of a uniform existing LF/CRLF style. The complete approval diff
must fit its existing canonical budget. Tool arguments and result events retain
their Agent/Session caps.

The official default observation policy requires a prior `read` before
overwrite/edit. Rust intentionally relies on its complete preparation baseline,
default human approval and late exact-baseline revalidation instead. This
allows a human-approved blind overwrite but avoids a second hidden freshness
cache and keeps one mutation authority. Explicit `auto-edit` therefore carries
the documented risk of replacing a complete file without a prior model read.

## Tests and compatibility

A source-attributed fixture records schemas, exact normal result strings,
replace-all behavior and stable error codes. Unit and real-Agent tests cover
create, overwrite, empty content, unique edit, deletion, replace-all, invalid
arguments, no match, ambiguity, denial, rejection, cancellation, stale
conflict, links and limits. A real CLI journey proves the normal approval and
semantic diff card path. Local full gates protect existing mutation behavior.

The claim remains `partial`: the fixture is manually transcribed, Rust does not
implement the default observation cache or unconstrained filesystem provider,
and its mutation/resource policy is intentionally stricter.
