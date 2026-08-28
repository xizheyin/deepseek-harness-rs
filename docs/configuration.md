# Configuration

`dsh-rs` keeps configuration deliberately small. The installed command is
`dsh`; it reads command-line flags, process environment variables, and bounded
workspace instruction files. Phase 10 adds one explicit local tool-plugin file,
but there is still no general global profile or hot reload.

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

`web_search` uses the same `DEEPSEEK_API_KEY`, but it deliberately does not use
`DEEPSEEK_BASE_URL`: DeepSeek native search is a separate Anthropic-compatible
Messages API. Its default base is `https://api.deepseek.com/anthropic/v1`, and
`/messages` is appended. One search has a 60-second whole-operation limit,
returns at most eight sources, follows no redirects, and uses no ambient proxy.
The tool sends the model-provided query to DeepSeek without a separate approval;
it does not fetch arbitrary URLs or read browser cookies.

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

## Session locations

- macOS: `~/Library/Application Support/dsh/sessions`
- Linux with `XDG_STATE_HOME`: `$XDG_STATE_HOME/dsh/sessions`
- Linux fallback: `~/.local/state/dsh/sessions`

`DSH_SESSION_ROOT` is primarily useful for isolated tests and operator-managed
storage. It must be absolute. Session JSONL contains plaintext conversation and
tool history; see [SECURITY.md](../SECURITY.md) before retaining sensitive work.
