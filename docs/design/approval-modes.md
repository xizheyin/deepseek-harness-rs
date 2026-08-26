# Interactive approval modes

## Status and scope

This design freezes one narrow response to repeated interactive file-edit
prompts. It adds a process-local CLI choice:

- `--approval-mode ask` keeps the current behavior and remains the default;
- `--approval-mode auto-edit` automatically permits only the built-in
  workspace-confined `apply_patch` action.

Shell and configured plugin actions still use the existing one-shot terminal
approval in both modes. Read-only tools remain automatic in both modes. This
slice does not add a sandbox, command-risk classifier, persistent trust rule,
per-directory configuration, “allow for session” button, or unattended script
mutation.

The fixed semantic baseline is upstream commit
`47f943859bef60e4160492346772ded9b24f765a`. Relevant sources and tests are:

- `packages/core/tools/src/index.ts` and `packages/core/tools/tests/tools.spec.ts`
  for the closed `allow` / `deny` / `ask` pre-tool decision and approval
  resolution;
- `packages/interaction/user-approval/src/{index,types,invariant}.ts` and
  `packages/interaction/user-approval/tests/{approval,invariant}.spec.ts` for
  one-shot outcomes, fail-closed audit pairing, and the separate session
  `ask` / `never` policy;
- `packages/fs/tool-fs/src/{write,edit,diff}.ts` and the Phase 5 oracle paths
  already recorded in `docs/upstream.md` for upstream's ordinarily allowed
  workspace file mutations.

Upstream does not have this CLI mode or Rust's strict single-file
`apply_patch`. This is therefore a tested Rust product choice, not a new
compatibility claim.

## Why auto-edit stops at apply_patch

The built-in patch path is narrow before policy is consulted: it validates one
closed argument shape, prepares one canonical diff without writing, confines
the target to the retained workspace capability, rejects unsafe links and
aliases, enforces resource limits, and rechecks conflicts before publication.
Selecting `auto-edit` changes only its final static policy from `Ask` to
`Allow`; it does not bypass any of those stages.

Foreground Shell is different. An approved `/bin/bash` command is arbitrary
native code with the user's operating-system authority. A textual allowlist is
not a security boundary: a build, test, script, alias, interpreter, redirection,
or command substitution can perform unrelated writes, access secrets, or start
network activity. Plugins are trusted local processes but have the same broad
side-effect problem. Until either capability has a verified sandbox or a
strictly typed low-risk action surface, this mode must keep them at `Ask`.

`auto-edit` can still create or substantially rewrite a regular file inside the
workspace. The CLI and README must say this plainly. It is explicit per-process
opt-in rather than a hidden default or stored trust decision.

## CLI and lifetime

The grammar is closed and case-sensitive:

```text
--approval-mode ask
--approval-mode=ask
--approval-mode auto-edit
--approval-mode=auto-edit
```

Missing, empty, duplicate, mixed-case, or unknown values are usage errors.
`--list-sessions` rejects the option. An explicit approval mode is valid only
when all three standard streams select the interactive terminal path; using it
with `--prompt` or piped input fails before workspace/session creation,
credentials, plugins, or network work. Script mode otherwise keeps its fixed
`Deny` policy for patch, Shell, and plugins.

The choice is not stored in Session, the workspace, environment variables, or
global configuration. A new process, including resume, uses `ask` unless the
flag is supplied again. The model cannot change the mode.

## State, event order, and side effects

`ApprovalMode` belongs to CLI admission and is passed once into assembly. For
an interactive assembly:

| Mode | File policy | Shell policy | Plugin policy |
| --- | --- | --- | --- |
| `ask` | `Ask` | `Ask` | `Ask` |
| `auto-edit` | `Allow` | `Ask` | `Ask` |

The terminal approval provider remains installed in both modes because Shell
and plugin calls can still ask. No mutable policy is added to the Agent loop.

For an accepted auto-edit patch, the durable order remains:

```text
tool/call intent -> prepared action body -> external file publication -> tool/result
```

There is no invented `approval/asked` or `approval/decided` pair because no
question occurred. Argument, preparation, path, conflict, resource,
cancellation, timeout, Session-capacity, and commit failures retain their
existing truthful results. A cancelled turn never gains authority from the
mode, and an unresolved old call is never replayed on resume.

## Failure and cancellation analysis

- Invalid CLI input is a usage failure with no workspace, Session, plugin,
  credential, or network side effect.
- Patch preparation failure produces no approval and no file write.
- Cancellation before commit prevents publication; cancellation after a
  definite commit records the committed fact through the existing owned-action
  cleanup path.
- A late conflict, unsafe link, output/resource limit, or Session failure keeps
  its current fail-closed behavior.
- Shell and plugin approval unavailability still denies their actions.
- Resume never treats an old unknown tool result as permission to rerun it.

## Verification plan

Default-enabled tests must prove:

1. exact CLI forms, default `ask`, duplicate/missing/invalid values, argument
   limits, help text, and list-session rejection;
2. explicit mode rejection for `--prompt` and piped input before workspace,
   Session, credentials, plugins, or network work;
3. default interactive patch behavior still asks and remains fail-closed;
4. a real interactive `auto-edit` patch commits without rendering or accepting
   an approval selector, records no approval pair, and reaches its correlated
   result;
5. malformed, outside-workspace, link, conflict, cancellation, and resource
   failures remain unable to publish a file;
6. Shell and configured plugin calls still ask in `auto-edit` mode;
7. a resumed process resets to `ask` unless the flag is supplied again;
8. the full repository formatting, all-target check/test, Clippy, whitespace,
   existing Phase 0–11 PTY, release, and plugin gates remain green.

The compatibility table remains `intentional-difference` for patch policy and
`partial` for the broader Phase 11 terminal experience. This checkpoint does
not complete Phase 11.
