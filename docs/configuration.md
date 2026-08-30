# Configuration

`dsh-rs` keeps configuration deliberately small. The installed command is
`dsh`; it reads command-line flags, process environment variables, and bounded
workspace instruction files. Explicit local files can enable tool plugins or
language servers, but there is still no general global profile or hot reload.

## Required credential

```console
export DEEPSEEK_API_KEY='your DeepSeek API key'
dsh --workspace .
```

The Provider reads `DEEPSEEK_API_KEY` for each request. It is not intentionally
persisted, but prompts, file contents, tool arguments, commands, and Session
events are model-visible. Do not put unrelated secrets in those values.

## Command-line settings

| Flag | Default | Meaning |
| --- | --- | --- |
| `--workspace <PATH>` | current directory for a new Session | Workspace retained by file tools and Shell startup |
| `--model <MODEL>` | `deepseek-v4-flash` for a new Session | DeepSeek model; resume otherwise reuses the stored model |
| `--prompt <TEXT>` | interactive terminal | Run one prompt and exit; write, Shell, and plugin approvals are denied |
| `--list-sessions` | off | List bounded local Session headers |
| `--resume <SESSION_ID>` | new Session | Continue one validated stored Session |
| `--plugin-config <PATH>` | no plugins | Start the explicitly configured local tool plugins for this process |
| `--lsp-config <PATH>` | no language server | Enable explicitly configured local stdio language servers for this process |
| `--time-zone <IANA_ZONE>` | no time context | Add one durable time reading before each entered model step |
| `--tui <MODE>` | `auto` | Choose `auto`, `enhanced`, or strict zero-ESC `linear` terminal presentation |
| `--no-color` | color when supported | Disable ANSI and force the linear terminal presentation |

Run `dsh --help` for exact syntax and mutually exclusive combinations.

## Environment variables

| Variable | Rule |
| --- | --- |
| `DEEPSEEK_API_KEY` | Required when a model request is made |
| `DEEPSEEK_BASE_URL` | Optional trusted base URL; HTTPS only, except loopback HTTP for offline tests |
| `DEEPSEEK_SEARCH_BASE_URL` | Optional DeepSeek Anthropic-compatible web-search base; HTTPS only, except loopback HTTP for offline tests |
| `DSH_SESSION_ROOT` | Optional absolute Session directory override |
| `DSH_HOME` | Optional absolute home for user-level `AGENTS.md`; defaults to `$HOME/.dsh` |
| `XDG_STATE_HOME` | Linux state base when `DSH_SESSION_ROOT` is absent |
| `NO_COLOR` | Presence disables ANSI and selects linear presentation |
| `TERM=dumb` | Also selects the linear presentation |

Auto selects the enhanced composer and inline Dock only for a colored
`TERM=xterm*` session, with no `TMUX`, `STY`, or `ZELLIJ` marker, whose initial
size is at least 44 columns by 12 rows. Unknown terminals, known multiplexers,
smaller windows, and color opt-outs start in the zero-ESC linear path. Explicit
`--tui enhanced` is the opt-in escape hatch for a known multiplexer, but it uses
the same initial geometry gate. Once enhanced is active it has a compact 12×5
resize rescue; below that it restores the terminal and fails closed.

The HTTP client does not follow redirects and ignores system proxy settings.
Choosing a custom HTTPS endpoint still grants that endpoint the API key and
model-visible request content.

## Per-step time context

Use a canonical IANA time-zone name when the model needs reliable local dates
or elapsed time:

```console
dsh --workspace . --time-zone Asia/Shanghai
```

The value is capped at 64 UTF-8 bytes and validated before Session creation,
credential access, local plugin/LSP startup, or network connection. Names are
case-sensitive and aliases are rejected with their canonical replacement; for
example, use `America/New_York`, not `america/NEW_YORK`.

Each entered model step gets one append-only snapshot containing the sampled
whole-second timestamp, numeric UTC offset, canonical zone, and elapsed time
since the preceding model-visible message or step reading. It is ordinary
model-visible Session history, so cold recovery and compaction retain old
readings. Pass `--time-zone` again with `--resume` to add fresh readings in the
new process; omitting it leaves history intact and disables new samples.

This setting neither asks for approval nor grants tool, file, Shell, process,
or network authority. It reads no browser zone, does not guess the host zone,
and starts no timer or background task. Its main cost is a small extra message
in every model step.

`web_search` uses the same `DEEPSEEK_API_KEY`, but it deliberately does not use
`DEEPSEEK_BASE_URL`: DeepSeek native search is a separate Anthropic-compatible
Messages API. Its default base is `https://api.deepseek.com/anthropic/v1`, and
`/messages` is appended. One tool call accepts one to four queries, executes
them concurrently under the ordinary turn limit, fairly merges at most eight
sources, follows no redirects, and uses no ambient proxy. Each provider request
has a 60-second whole-operation limit. The tool sends the model-provided queries
to DeepSeek without a separate approval.

`web_fetch` is separate from both DeepSeek endpoints and needs no environment
variable or credential. It anonymously retrieves one public HTTP(S) page after
validating every DNS answer and pins the connection to those addresses. It uses
no ambient proxy, cookies, browser state, or authentication; follows at most
five same-origin, revalidated redirects; and has a 30-second whole-operation
limit plus fixed response, decoded-text, and final-output caps. Loopback,
private, link-local, reserved, transition, multicast, and DNS64-translated
private destinations are refused without a separate approval.

## Workspace instructions

Before the first model request, `dsh` reads `$DSH_HOME/AGENTS.md` (or
`$HOME/.dsh/AGENTS.md`) and then `AGENTS.md`, `CLAUDE.md`, `AGENTS.local.md`,
and `CLAUDE.local.md` from the exact opened workspace root. The rendered
message is capped at 65,536 UTF-8 bytes and each source at 1 MiB. It is written
to Session history after the direct prompt; a resume reuses unchanged context
and appends confirmed additions, replacements, or removals.

`DSH_HOME` must be absolute to be used. Instruction files are guidance, not an
approval or sandbox mechanism. Rust deliberately refuses instruction-file
symlinks and never walks above `--workspace`. Nested instruction discovery
after a file-tool call is not implemented yet.

## Local subprocess tool plugins

Plugins are an experimental, tool-only Phase 10 extension. A plugin is a
trusted native executable started by `dsh`; it is **not sandboxed**. Passing a
config authorizes process startup and schema discovery. In interactive mode,
each valid model-requested plugin call still asks for approval. Script and piped
input modes deny plugin calls because no human can approve them.

Build the two no-side-effect examples from this repository:

```console
cargo +1.85.0 build --locked --examples
```

Create a private JSON file whose program paths are absolute, canonical paths:

```json
{
  "version": 1,
  "plugins": [
    {
      "id": "text-tools",
      "program": "/absolute/path/to/dsh-rs/target/debug/examples/text_stats_plugin",
      "args": []
    },
    {
      "id": "json-tools",
      "program": "/absolute/path/to/dsh-rs/target/debug/examples/json_format_plugin",
      "args": []
    }
  ]
}
```

Then restrict the file and launch it explicitly:

```console
chmod 600 /absolute/path/to/plugins.json
dsh --workspace . --plugin-config /absolute/path/to/plugins.json
```

The file is limited to eight plugins. IDs match
`[a-z][a-z0-9-]{0,31}`; programs must be regular executable files with no set-ID or writable
unsafe path component. Each plugin receives only `PATH=/usr/bin:/bin`,
`LANG=C`, `LC_ALL=C`, `DSH_PLUGIN_PROTOCOL=1`, and its `DSH_PLUGIN_ID`; it starts
in the program's parent directory. Its stdout is reserved for the bounded
version-1 NDJSON protocol and stderr is bounded diagnostics.

Configured `args` become ordinary operating-system process argv. They are not
automatically persisted into Session JSONL, but same-account process inspection
may expose them; never put an API key, password, or unrelated secret there.

Plugin configuration, executable paths, and configured program argv are not
automatically written into Session JSONL. Model-requested tool arguments and
results are recorded as normal Agent facts. Plugins are not restored
automatically: pass `--plugin-config` again with
`--resume` if that new process should expose the tools. An already recorded
unknown tool outcome is never replayed to a restarted plugin. The closed
protocol/schema limits are documented in
[the Phase 10 design](design/subprocess-tool-plugins.md).

### webClx terminal messaging adapter

The `webclx_terminal_message_plugin` example exposes
`webclx_list_terminals` and `webclx_send_terminal_message` through the bounded
subprocess protocol. It lets a Rust `dsh` process cooperate with webClx-managed
Codex, Claude, and DeepSeek terminals. After building the example, create an
owner-private plugin config:

```json
{
  "version": 1,
  "plugins": [
    {
      "id": "webclx-terminal-message",
      "program": "/absolute/path/to/dsh-rs/target/debug/examples/webclx_terminal_message_plugin",
      "args": [
        "--base-url", "http://127.0.0.1:11111",
        "--sender", "RUST_DSH_TERMINAL",
        "--local-token-file", "/absolute/path/to/webclx-local-token"
      ]
    }
  ]
}
```

`--sender` must be the exact current webClx terminal name or stable session ID.
The plugin host clears ambient `WEBCLX_*` variables, so identity and the optional
local token file are explicit arguments. Omit `--local-token-file` when the
loopback API does not require it. For a cross-host return route, add
`--reply-url URL`. URLs reject credentials, query strings, and fragments;
redirects are not followed; and a local token is sent only to a loopback base
URL. Tool calls still use the normal interactive plugin approval policy.

After building the example, the repository acceptance script performs a real
read-only NDJSON handshake and calls the configured webClx session-list API:

```console
scripts/accept-webclx-terminal-message.sh
```

Set `DSH_WEBCLX_PLUGIN`, `WEBCLX_URL`, `WEBCLX_LOCAL_TOKEN_FILE`, or
`DSH_WEBCLX_EXPECTED_PATH` when the executable, API, token file, or expected
workspace differs from the defaults. Set `DSH_WEBCLX_ACCEPT_TIMEOUT_SECONDS`
to change the 15-second per-response timeout. The acceptance path never sends
terminal input.

## Local stdio language servers

`--lsp-config` enables the experimental read-only `lsp` tool. Passing the flag
authorizes `dsh` to start the listed native executables lazily; those programs
run with your account and are **not sandboxed**. The model can only choose one
of four queries—definition, references, implementation, or hover—and a source
cursor. It cannot choose the executable, environment, workspace, timeout or
protocol method, and server requests to edit files or run commands are refused.

Create an owner-private JSON file. This Rust boundary requires a canonical
absolute executable without a symbolic-link shim. For Rust installed by
`rustup`, copy the path printed by `rustup which rust-analyzer` rather than
`~/.cargo/bin/rust-analyzer`, which is normally a symlink:

```json
{
  "version": 1,
  "toolTimeoutMs": 60000,
  "servers": {
    "rust": {
      "command": "/absolute/toolchain/path/bin/rust-analyzer",
      "args": [],
      "extensionToLanguage": { ".rs": "rust" },
      "env": {},
      "initializationOptions": null,
      "configuration": null
    }
  }
}
```

```console
chmod 600 /absolute/path/to/lsp.json
dsh --workspace . --lsp-config /absolute/path/to/lsp.json
```

The config allows at most eight servers and 32 total unique final-extension
routes. `toolTimeoutMs` defaults to 60,000 and accepts 100–295,000 ms. A source
must be a non-symlink UTF-8 file inside the retained workspace and is capped at
4,000,000 bytes. Each server starts on its first matching query, is reused
serially, and is shut down and process-group reaped when `dsh` exits. One
message is capped at 16,000,000 bytes; presentation keeps at most 100 locations
and 16,000 characters.

The child gets the same bounded allowlisted environment used by built-in Shell,
then the config's `env` entries replace matching names. Values are not written
to Debug output or Session JSONL by configuration loading, but they are sent to
the trusted server process and may be visible to same-account process
inspection. Do not put unrelated credentials there. Pass `--lsp-config` again
on resume; old LSP calls are never replayed. Full protocol and intentional
differences are documented in [the Phase 37 design](design/lsp-navigation.md).

## Session locations

- macOS: `~/Library/Application Support/dsh/sessions`
- Linux with `XDG_STATE_HOME`: `$XDG_STATE_HOME/dsh/sessions`
- Linux fallback: `~/.local/state/dsh/sessions`

`DSH_SESSION_ROOT` is primarily useful for isolated tests and operator-managed
storage. It must be absolute. Session JSONL contains plaintext conversation and
tool history; see [SECURITY.md](../SECURITY.md) before retaining sensitive work.
