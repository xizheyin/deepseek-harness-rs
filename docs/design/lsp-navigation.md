# Configured stdio LSP navigation design

## Problem and scope

Text search is fast but ambiguous: a function name may appear in definitions,
calls, comments and generated files. Phase 37 adds one optional read-only `lsp`
tool so the model can ask a configured language server for an exact definition,
reference set, implementation or hover description.

This phase does not add diagnostics, rename, symbols, call hierarchy, code
actions, formatting, dynamic server installation, auto-detection, arbitrary
JSON-RPC, background jobs or server-initiated edits. It does not change file or
Shell approval policy.

## Upstream basis

The semantic baseline is DeepSeek Harness commit
`47f943859bef60e4160492346772ded9b24f765a`. The inspected paths are recorded in
`docs/upstream.md`, principally `packages/lsp/{lsp,lsp-stdio,tool-lsp}` and their
tests. Latest inspected master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` retains the model contract and stdio
lifecycle.

## Configuration and startup

LSP is absent unless the user passes `--lsp-config <PATH>`. The path resolves
against the process startup directory. Its JSON shape is:

```json
{
  "version": 1,
  "toolTimeoutMs": 60000,
  "servers": {
    "rust": {
      "command": "/absolute/path/to/rust-analyzer",
      "args": [],
      "extensionToLanguage": { ".rs": "rust" },
      "env": {},
      "initializationOptions": null,
      "configuration": null
    }
  }
}
```

The config is at most 64 KiB, must be an owner-only regular file, and must not
change while read. It contains at most eight servers, 32 total extension routes,
16 arguments per server and bounded strings/environment. `toolTimeoutMs` is a
user-controlled 100–295,000 ms process setting that defaults to 60,000 ms and
never enters model arguments. Unknown or duplicate
fields fail startup. IDs and extension routes are unique after lowercase and
leading-dot normalization.

`command` is an absolute, canonical, non-symlink regular executable owned by
the current user or root, without group/other writes, set-id bits or an unsafe
parent chain. The executable descriptor and metadata identity are retained and
rechecked immediately before every lazy spawn. This intentionally rejects PATH
lookup and shims such as the common `~/.cargo/bin/rust-analyzer` symlink; users
can put the stable path printed by `rustup which rust-analyzer` in the config.

Configuration only publishes the `lsp` schema after every server and extension
passes validation. It does not start a process. A resumed session must pass the
flag again; config and executable paths are never written into Session events.

## Model input, output and event order

The closed input object has four required fields:

- `operation`: `goToDefinition`, `findReferences`, `goToImplementation`, or
  `hover`;
- `file_path`: a workspace-contained source file;
- `line` and `character`: positive one-based integers, interpreted as UTF-16
  code-unit coordinates.

The ordinary Agent pipeline records `tool/call` before any file read or process
spawn. The tool converts the cursor to zero-based LSP coordinates. Navigation
results render as stable `path:line:character` lines, using relative paths for
workspace `file:` URIs. Empty navigation returns `No results.` Hover returns its
normalized Markdown/plain text or `No hover information.` The renderer keeps
at most 100 locations and 16,000 Unicode scalar values, including its omission
or truncation marker. The ordinary correlated `tool/result` follows.

The canonical server response is normalized before rendering: Location and
LocationLink become URI plus half-open range; MarkupContent and MarkedString
hover forms become one text string plus an optional range. Missing, negative,
fractional or structurally invalid coordinates fail rather than being silently
dropped.

## State ownership and process lifecycle

`LspHost` owns the static extension routes and one bounded actor per configured
server. Each actor owns at most one language-server process for this CLI's one
retained workspace. Its capacity-two queue serializes complete calls, including
source read completion, `didOpen`, request and `didClose`; different configured
servers may run independently.

The first matching call reads the source before spawning. It then starts the
server without a shell, initializes with the canonical workspace `file:` URI,
UTF-16/client capabilities and static initialization options, sends
`initialized`, and retains the process. A later query reuses it. If a selected
transport dies, the actor tears it down and retries that read-only query once
with a fresh process. Protocol or response-semantic errors are not retried.

On CLI shutdown every route stops accepting calls. Actors cancel queued/current
work, attempt bounded `shutdown` then `exit`, and finally use the existing
owned-process-group TERM→KILL cleanup and join their threads. No task or process
may outlive `ToolExecutor::shutdown`.

## Filesystem and environment authority

The existing `Workspace` owner resolves the path, rejects `..`, outside absolute
paths and every symbolic link, opens a regular file through the retained
directory capability, reads at most 4 MiB and verifies it did not change. Text
must be UTF-8. A canonical `file:` URI is derived from the already-opened
workspace plus the admitted relative path; a model string cannot choose a URI
or workspace.

The child receives the existing bounded allowlisted Shell environment snapshot,
then bounded config overrides replace matching names. Debug and model errors
never contain values. `NO_COLOR`, noninteractive pager and Git prompt overrides
remain. The configured executable is trusted local code and therefore is not a
sandbox; the documentation says this plainly.

## JSON-RPC and capability rules

Protocol stdout accepts only `Content-Length` framing. Headers are capped at 64
KiB, one body at 16 MiB and the existing process owner caps aggregate stdout at
32 MiB and stderr at 256 KiB. Response IDs must correlate with a pending numeric
request. Notifications are ignored after framing/JSON validation.

The host answers `workspace/configuration` by repeating the static configured
value for every item. `window/workDoneProgress/create`,
`client/registerCapability`, and `client/unregisterCapability` receive `null`.
`workspace/applyEdit` and every other server request receive a JSON-RPC method-
not-supported error. The host never runs commands or accepts dynamic authority.

Initialization accepts omitted or `utf-16` position encoding only. The server
must advertise the requested operation and transient open/close synchronization
(`textDocumentSync` 1/2 or `{openClose:true}`). `findReferences` always sends
`includeDeclaration:true`.

## Failure, timeout and cancellation

- Bad model fields: correlated `INVALID_ARGS`.
- No configured final-extension route: `LSP_UNAVAILABLE` without spawning.
- Unsafe/missing/outside/large/changing/non-text source: the existing safe file
  error, without spawning a fresh server.
- Unsupported server capability: `LSP_UNSUPPORTED_OPERATION`.
- Malformed frame/JSON/correlation/result: `LSP_MALFORMED_RESPONSE` or
  `LSP_PROTOCOL_ERROR`; the connection is terminated.
- Spawn/pipe/early-exit failure: `LSP_PROCESS_FAILED`, with no path, environment
  value or stderr exposed to the model.
- Capacity-two server queue full: `LSP_BUSY`.
- Every call, including queue and initialization, is capped at 60 seconds and
  returns `LSP_TIMEOUT` only after the owned process is quiescent or replaced.
- User/turn cancellation sends `$/cancelRequest` for an in-flight semantic
  request, waits 500 ms, then terminates an unresponsive connection; the normal
  correlated abort result closes the call.

## Side effects, security and recovery

The intended side effects are reading one workspace file and starting a trusted
configured local process. No file write, network operation by dsh, approval
decision, Session mutation outside ordinary call/result facts, or process is
hidden. The language server itself runs as the user and may have capabilities
outside dsh; explicit config is the authorization and is documented as such.

Server output is untrusted protocol data. It cannot choose a local executable,
change the workspace, request edits, exceed fixed presentation bounds or place
diagnostics directly into Session. Session recovery sees only ordinary complete
tool facts and never restarts/replays an old LSP call. A new/resumed CLI starts
with no process and lazily builds one only after a new recorded call.

## Tests and compatibility status

A fixed-source fixture records the schema, prompt, operation-method mapping,
coordinate conversion, small normal renders and stable error names. Pure tests
cover config/route admission, framing fragmentation and limits, capability and
response normalization, URI rendering and result caps. Runtime tests use a real
local fixture executable for initialize/open/query/close, server requests,
transport restart, timeout, cancellation, queue bounds, output limits and
process-group cleanup. One real script CLI journey proves the schema, protocol
lifecycle, call/result order and second Provider request.

The compatibility row remains `partial`: the four-operation model surface and
real stdio lifecycle exist, but Rust has stricter executable/config/environment
bounds, one workspace, no generated TypeScript oracle and no wider LSP feature
set.
