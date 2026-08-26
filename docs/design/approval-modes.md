# Interactive approval modes

## Status and scope

This design freezes one narrow response to repeated interactive file-edit
prompts. It adds a process-local CLI choice:

- `--approval-mode ask` keeps the current behavior and remains the default;
- `--approval-mode auto-edit` automatically permits only the built-in
  workspace-confined `apply_patch` action.

Shell and configured plugin actions still use terminal approval in both modes.
For a fully prepared built-in Shell action only, the selector also offers an
explicit `Allow exact Shell for this process` choice. A clean first execution
can install one bounded process-local grant, and a later execution with the
same sealed execution identity can consume that grant without another prompt.
Read-only tools remain automatic in both modes. This slice does not add a
sandbox, command-risk classifier, persistent trust rule, per-directory
configuration, blanket “allow Shell” button, or unattended script mutation.

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
`apply_patch`. Its durable approval vocabulary also has no remembered exact
Shell outcome. These are therefore tested Rust product choices, not new
compatibility claims.

## Exact Shell process grant

The fourth choice is deliberately narrower than “allow for session.” It is
shown only for the sealed built-in Shell Action after argument validation,
workdir preparation, policy selection, and resource admission have succeeded.
The other choices remain `Allow once`, `Reject`, and `Cancel`; Reject remains
the initial selection. Existing `y`, `yes`, and `allow` input continues to mean
only `Allow once`.

The Agent loop is the sole owner of a cache with at most 64 entries. A grant is
not stored in Session, configuration, the environment, the workspace, or a
global variable. A new Agent/process, including resume, starts empty. Script
mode's `Deny`, plugin `Ask`, file policy, and ordinary one-shot answers take
precedence and can neither create nor consume this cache.

“Exact” is based on a sealed structured execution identity, never approval
preview text. Its versioned, length-prefixed input includes:

- the exact validated command bytes;
- the normalized timeout;
- the retained workspace and prepared workdir identities, including the
  normalized relative workdir and Unix device/inode facts;
- the exact fixed child-environment snapshot;
- the fixed `/bin/bash --noprofile --norc -c` launcher and Shell policy
  version.

Call ID is excluded because it changes on every invocation. Description is
also excluded: it is display-only text and cannot change the process that is
started. Command, timeout, directory identity, environment, launcher, or
policy changes do change the key. The cache stores only a process-keyed
HMAC-SHA256 digest, never raw command, path, or environment text. Entropy or
allocation failure disables remembering and falls back to normal `Ask`.

The cache uses a consume-and-reinsert rule. On a hit, the digest is removed
before the Action runs, so another call cannot borrow an in-flight grant. It is
inserted or reinserted only after all of these facts are true:

1. the user explicitly chose the process grant, or an existing grant was
   consumed;
2. the workdir was revalidated and the command started;
3. the entire owned process group was cleaned up and reaped;
4. the command ended with exit code 0, no signal, timeout, cancellation, or
   unknown ownership;
5. the authoritative correlated `tool/result` was committed.

Any failure simply leaves the digest absent, so the next matching call asks
again. Capacity exhaustion also keeps asking; it never evicts an unrelated
grant or grows without a bound. A cache hit skips only the human question. It
still records the new `tool/call`, revalidates every capability and budget,
runs the normal owned-process path, and records the new `tool/result`.

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

The exact Shell grant is likewise process-local. It needs no CLI flag, but it
can be created only by the human's explicit fourth selector choice and resets
on every new process/resume.

## State, event order, and side effects

`ApprovalMode` belongs to CLI admission and is passed once into assembly. For
an interactive assembly:

| Mode | File policy | Shell policy | Plugin policy |
| --- | --- | --- | --- |
| `ask` | `Ask` | `Ask` | `Ask` |
| `auto-edit` | `Allow` | `Ask` | `Ask` |

The terminal approval provider remains installed in both modes because Shell
and plugin calls can still ask. The bounded exact-Shell grant store is mutable
Agent-loop state, separate from static policy and durable Session state.

For an accepted auto-edit patch, the durable order remains:

```text
tool/call intent -> prepared action body -> external file publication -> tool/result
```

There is no invented `approval/asked` or `approval/decided` pair because no
question occurred. Argument, preparation, path, conflict, resource,
cancellation, timeout, Session-capacity, and commit failures retain their
existing truthful results. A cancelled turn never gains authority from the
mode, and an unresolved old call is never replayed on resume.

For the first exact-Shell grant, the durable order is the ordinary truthful
one-shot order:

```text
tool/call -> approval/asked -> approval/decided(allowed-once) -> process -> tool/result
```

The process scope is deliberately not a new durable approval outcome. It
becomes usable only after the successful `tool/result` commit. A later cache
hit has no approval pair because no question occurred:

```text
tool/call -> process -> tool/result
```

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
- Selector/render/provider failure cannot create a process grant.
- A cache-hit workdir replacement, cancellation, timeout, signal, non-zero
  exit, cleanup uncertainty, or result-commit failure consumes the grant and
  forces the next call to ask.
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
6. `Allow once` keeps asking, while the explicit exact-Shell choice skips only
   a later structurally identical built-in Shell prompt after a clean committed
   success;
7. command, timeout, workdir identity, environment, or launcher changes miss;
   description and call-ID changes do not affect execution identity;
8. rejection, cancellation, non-zero exit, signal, timeout, cleanup/result
   failure, cache capacity, and allocation/entropy failure never grant;
9. Shell `Deny`, configured plugins, files, and scripts cannot consume the
   Shell cache, including in `auto-edit` mode;
10. a resumed/new process resets both `ask` mode and the grant cache;
11. enhanced and zero-escape linear selectors keep fresh-input protection,
    Reject default, one-shot `y`, correct fourth-choice navigation, and
    terminal restoration;
12. the full repository formatting, all-target check/test, Clippy, whitespace,
   existing Phase 0–11 PTY, release, and plugin gates remain green.

The compatibility table remains `intentional-difference` for patch policy and
`partial` for the broader Phase 11 terminal experience. This checkpoint does
not complete Phase 11.
