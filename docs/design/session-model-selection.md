# Idle Session model selection

## Problem and scope

`dsh` already accepts `--model` when it starts, but changing model mid-session
currently requires exiting and restarting. Phase 52 adds an idle local command
that changes the DeepSeek model and optional reasoning effort used by the next
request. It does not add another Provider, fetch a remote catalog, change a
running step, store a global default, or add image input.

## Upstream basis

The semantic baseline is
`47f943859bef60e4160492346772ded9b24f765a`. Exact fixed and current-master
paths are recorded in `docs/upstream.md`. The fixed Agent model-selection tests
establish an atomic prompt-assembly snapshot. The fixed Host tests establish
validation, advisory-unlisted model pass-through, default-effort resolution,
failure preservation and durability only through a later request header. Latest
master at `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains those rules in the
Session Controller.

## Command contract

The terminal accepts:

```text
/model
/model deepseek-v4-pro
/model deepseek-v4-pro max
```

The no-argument form reports the effective current model and effort, the two
built-in advisory model ids, and the supported efforts. A model id must be one
non-whitespace, control-free UTF-8 token of at most 256 bytes. The catalog is
advisory, so another valid model id passes through unchanged. The optional
effort is exactly `off`, `high` or `max`; case variants and extra arguments are
usage errors.

All forms are local and accepted only while the Agent is idle. During a turn,
the enhanced terminal consumes the command and displays a busy notice; it does
not queue the text for the model. Linear input is processed at the next idle
record boundary, as with existing idle metadata commands.

## State and event order

`AgentLoop` owns the selected `LlmCallConfig`, because the same owner snapshots
it with system text, tools and model-visible history. Selection performs this
order:

```text
parse and bound terminal arguments
construct the proposed DeepSeek call
ask the configured Provider to resolve and validate it without I/O
materialize the Provider's default effort when the command omitted one
replace the next-call selection atomically
mark the next request header as a configuration change when history exists
display the accepted effective model and effort
```

There is deliberately no Session append at selection time. If the process
exits before another prompt, the unconsumed choice disappears. On the next real
request, the normal Agent preflight appends `request/header` before dispatch:
`initial` when the Session has no earlier header and `change` after an earlier
request. That header is the durable selection used by resume and fork.

Repeated choices before a request replace the pending selection, so only the
last one is consumed. Re-selecting an already explicit identical choice is a
no-op. Selecting a model with its Provider default may still replace an older
adapter-default selection with an explicit effort, matching the official
selector's resolved result.

## Failure, cancellation and side effects

Malformed command input never reaches the Agent. Unsupported effort, wrong
Provider binding or invalid model configuration returns an unavailable result
and keeps the exact prior selection and request-header state. Selection is a
short synchronous operation with no network, file, process, Session or tool
side effect, so there is no asynchronous cancellation window or cleanup task.
Normal turn cancellation after selection follows the existing Agent behavior;
if preflight never appends the new header, the selection remains pending for a
later turn in the same process.

## Security and resource bounds

The command never reads credentials and `ModelProvider::prepare_call` must be
side-effect free. Model ids are bounded to the existing DeepSeek 256-byte
limit; effort comes from a closed three-value set. Unlisted ids are not trusted
as paths, shell text or endpoints: they are only copied into the authenticated
DeepSeek request's model field after the normal provider wire-size checks.

## Tests and recovery

Deterministic Agent tests cover default-effort resolution, no immediate event,
last-choice wins, invalid-effort preservation, first-request `initial`, later
request `change`, exact Provider wire selection and durable resume. Parser and
palette tests cover the closed command grammar. Real enhanced and zero-ANSI
linear PTY journeys exercise the shipped binary against a loopback fake
DeepSeek endpoint without credentials or public network.

## Intentional differences

Rust exposes a compact text command instead of the official Web popup and
serves only its one configured DeepSeek route. It displays the two built-in
advisory models rather than aggregating dynamic Provider catalogs, accepts only
token-shaped model ids, and does not save an accepted choice as a deployment
default. These differences narrow discovery and global configuration, but the
next-assembly snapshot, Provider validation, no-message behavior and
request-header durability remain aligned.
