# Durable safe permission presets

## Problem and scope

Repeated file-edit confirmations make an interactive coding session slow. Phase
53 adds one idle local command that can keep the conservative behavior or allow
ordinary file-edit tools without asking every time:

```text
/permission
/permission ask
/permission auto-edit
```

This phase does not add an operating-system sandbox, unrestricted host access,
automatic Shell approval, automatic plugin approval, a global settings file,
or a way to change an already running turn.

## Upstream basis

The semantic baseline remains
`47f943859bef60e4160492346772ded9b24f765a`. The fixed permission-preset
service and tests establish a log-only `permission/preset` event, last-value
replay, selection-before-mechanism ordering, no-op re-selection, current-session
durability, and model invisibility. The fixed client surface establishes
`/permission` as the current-session write path and keeps future-session
settings separate. Fresh `origin/master` at
`cd5ef8148158c3a752a658978873241fdf8e2bbc` retains those rules.

Exact source paths and the focused source-derived fixture are recorded in
`docs/upstream.md` and
`tests/fixtures/tools/upstream_phase53_permission_presets.json`.

## Rust presets and intentional safety difference

Official presets bundle a real sandbox mode with a whole-session approval
policy. This Rust CLI has path confinement and approval checks, but no proven
OS sandbox. It therefore must not advertise official names such as
`workspace-write` or `danger-full-access`.

Rust exposes two narrower presets:

| Preset | File-changing tools | Shell | Subprocess plugins |
| --- | --- | --- | --- |
| `ask` | ask | ask | ask |
| `auto-edit` | allow | ask | ask |

Read-only tools retain their existing automatic behavior. Exact Shell commands
already allowed for the current process retain their existing narrow grant;
the preset does not create a new Shell grant. This split is the central safety
difference: `auto-edit` removes the frequent edit prompt without turning off
confirmation for command execution or plugin side effects.

## State, precedence, and event order

`AgentLoop` owns the effective file-change policy. `SessionState` folds the
last `permission/preset` event so resume and fork use the same fact. With no
event, the effective preset is `ask`.

Startup precedence is:

```text
explicit --approval-mode
        ↓ otherwise
latest Session permission/preset
        ↓ otherwise
ask
```

An explicit startup value that changes the effective preset is durably appended
before Provider setup, tool construction, or any turn. An omitted flag never
overwrites the Session value. A runtime switch follows this order:

```text
parse one closed preset name
verify the Agent is idle
append permission/preset
change the in-memory file policy
show the effective preset
```

The append happens before any later file side effect. If it fails, the runtime
policy stays unchanged. Selecting the effective preset again appends nothing.
The event is log-only: it does not create a turn, user message, Provider
request, tool call, approval, or model-visible content.

## Failure, cancellation, and recovery

Unknown names, case variants, extra arguments and command-like prefixes are
local usage errors. The command is accepted only while idle. During an active
enhanced turn, it is consumed locally with a busy notice rather than queued as
a model prompt.

The switch performs one bounded local append and starts no network request or
child process. A storage failure leaves the old policy installed and uses the
existing Agent error path. Strict Session recovery rejects malformed preset
payloads before a new model request or tool side effect. A valid last preset is
installed before the resumed Agent can execute a tool.

## Resource and security bounds

The payload is a closed two-value enum, not an arbitrary policy expression.
There is no path, command, environment value, credential, or free-form string
in the event. Existing parameter validation, intent-before-side-effect Session
records, conflict checks, output limits, cancellation, and process cleanup are
unchanged.

## Tests

Deterministic Session tests cover codec round-trip, last-value replay,
model-surface exclusion, no-op selection and malformed-value rejection. Agent
tests cover append-before-policy-change, append failure preserving the old
policy, and the exact file/Shell/plugin policy split. CLI parser and palette
tests cover the closed command grammar and busy handling.

Loopback PTY journeys cover enhanced switching and automatic edit, Shell still
asking, switching back to `ask`, durable resume, explicit startup override, and
zero-ANSI linear reporting. They use a fake local DeepSeek endpoint and no real
credential or public network.

## Known limits

There is no deployment-wide default editor, Web picker, arbitrary custom
preset, sandbox-mode event, or approval-policy event. A legacy Session with no
preset continues to mean `ask`; unlike official composition, Rust does not
retroactively append default mechanism events. These limits keep the feature
truthful for the current single-process terminal architecture.
