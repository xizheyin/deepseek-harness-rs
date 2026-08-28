# `str_replace_editor` design

Status: Phase 32 implementation design, 2026-08-29.

## Problem and scope

The fixed official CLI ships a small editor beside its filesystem tools. It is
easier for a model to make an exact local edit with `old_str`/`new_str` than to
construct a unified diff. dsh-rs currently exposes only `apply_patch` for file
changes. This phase adds the official four commands: `view`, `create`,
`str_replace`, and `insert`.

This is not a second filesystem authority, an undo stack, a binary editor, a
multi-file transaction, a watcher, or a way around approval. `apply_patch`
remains supported.

## Upstream basis

The semantic baseline is commit
`47f943859bef60e4160492346772ded9b24f765a`, especially:

- `packages/fs/tool-str-replace-editor/src/index.ts`;
- `packages/fs/tool-str-replace-editor/tests/tools.spec.ts`;
- its README and the base/minimal preset registration.

Latest inspected master `cd5ef8148158c3a752a658978873241fdf8e2bbc`
retains this default tool and adds no incompatible command to this surface.

## Input, output, and order

The closed schema accepts `command`, absolute `path`, and only the known
command-specific fields. `view` returns numbered UTF-8 text or a sorted shallow
directory tree. `create` requires `file_text`. `str_replace` requires one
non-empty, unique literal `old_str`; omitted `new_str` means deletion. `insert`
requires an integral boundary in `[0, line_count]` and `new_str`.

A view completes as an ordinary tool result. A mutation follows the existing
order:

```text
tool/call -> prepare candidate and preview -> optional approval facts
          -> atomic commit attempt -> tool/result
```

The success text follows the official editor wording. The existing structured
file metadata and workspace-touch fact remain the Rust source of truth for UI,
recovery, and nested instruction refresh.

## Failure, rejection, cancellation, and timeout

Missing, outside-workspace, relative, symlinked, non-file, non-UTF-8, NUL,
oversized, ambiguous, and stale inputs fail before publication. A rejected
approval produces the ordinary correlated denial result. Cancellation is
checked during view traversal, preparation, and commit. A conflict after
preparation fails without overwriting the changed file. The Agent's existing
turn deadline owns the whole call; no background work is introduced.

## Side effects and ownership

Only `create`, `str_replace`, and `insert` may change one regular workspace
file. `Workspace` owns path capabilities and bytes; the editor only derives a
candidate. The existing patch mutation finalizer owns approval, atomic
publication, result metadata, and truthful committed/not-committed outcomes.
Agent and Session ownership do not change.

## Recovery and replay

The model-requested call is durable before preparation. Approval and result
facts remain append-only. An unresolved old call is not replayed after resume.
The editor retains no private cross-call state, so reconstruction needs only the
existing Session and filesystem facts.

## Safety and resource limits

Paths must be absolute but resolve inside the exact startup workspace. The
existing no-symlink/no-hardlink mutation rules remain in force. Text files keep
the 16 MiB mutation limit, 100,000-line limit, and line-size limit. View output
clips at 16,000 characters; traversal has fixed depth, entry, and retained-path
caps. Parameters and normalized Session results remain bounded.

Default interactive mode asks before mutation. Explicit `auto-edit` applies the
existing allow policy only to these prepared built-in file changes; it still
does not authorize Shell or plugins.

## Tests and compatibility

A committed fixture records the exact fixed source paths, schema, canonical
results, range/insert semantics, unique-match errors, and directory exclusions.
Focused tests cover all commands, empty files, literal replacement, ambiguity,
range errors, clipping, cancellation, denial, auto-allow, conflict, path/link
confinement, and Agent event order. One loopback CLI journey proves the tool is
actually advertised and callable.

Rust intentionally keeps its stricter workspace and approval policy, existing
atomic publication, and bounded UTF-8/resource rules. It does not reproduce the
official observation-policy plugin or Cordis lifecycle. Character clipping can
differ around JavaScript UTF-16 surrogate accounting. Until a generated
cross-language oracle covers every error string, compatibility remains
`partial`, not `compatible`.
