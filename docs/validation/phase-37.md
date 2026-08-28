# Phase 37 local validation — 2026-08-29

## Result

Phase 37 is complete under the requested local-only, necessary-check gate. The
real CLI now accepts an explicit private `--lsp-config` and exposes one
read-only `lsp` tool with `goToDefinition`, `findReferences`,
`goToImplementation`, and `hover`. Each configured language server starts
lazily, remains owned by the CLI process, receives bounded requests, and is
shut down or killed as a process group when required. Model calls cannot choose
the executable, environment, timeout, workspace, or protocol method.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the model-facing tool,
  stdio framing, translation, lifecycle, capability and rendering behavior
  recorded in `docs/upstream.md`. Fetched latest master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` retains that contract.
- `tests/fixtures/tools/upstream_phase37_lsp.json` records the source paths,
  exact four-operation schema, prompt guidance, bounds and reduced expected
  result shape.
- Unit tests cover private configuration, stable executable validation,
  extension routing, JSON-RPC framing, fragmented input, capability admission,
  UTF-16 coordinate conversion, Location/LocationLink/hover normalization,
  output bounds and workspace symlink rejection.
- A deterministic real stdio fixture server verifies initialization,
  configuration replies, `didOpen` source text and language id, zero-based
  protocol coordinates, declaration-inclusive references, `didClose`, graceful
  shutdown and `exit`.
- Real script CLI tests cover the normal schema → initialize → query → result →
  next-model-request path without approval, one transport-crash restart,
  malformed-response correlation, configured timeout, caller cancellation and
  cleanup of a spawned descendant in the server process group.
- A clean serial all-target run passed every LSP, Agent, Provider, tool,
  persistence, plugin, CLI smoke, release and example target. Two earlier
  default-parallel attempts each exposed a different unrelated local harness
  transient: one unsupported idle-test PTY and one `WouldBlock` read in a fake
  Web server. Both exact standalone reruns passed without a code change or a
  disabled test; the clean all-target run then passed with one test thread.

## Local commands

```console
cargo test 'tools::lsp' --lib
cargo build --example lsp_fixture
cargo test --test cli_smoke 'lsp_' -- --nocapture
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
cargo test --test interactive_cli idle_hup_quit_and_term_use_stable_exit_codes -- --exact --nocapture
cargo test --test cli_smoke real_script_overlaps_independent_web_tool_calls_and_preserves_model_order -- --exact --nocapture
cargo test --all-targets --quiet -- --test-threads=1
cargo clippy --all-targets -- -D warnings
git diff --check
```

Tests ran locally on macOS arm64 with Rust 1.85.0 and locked dependencies. They
use fake providers, temporary workspaces, local subprocesses and loopback HTTP.
No real DeepSeek request, API credential, charge, remote CI, extra operating-
system matrix, public-network product test or real third-party language server
was used. Network access was used only for the separate upstream source fetch
already recorded in `docs/upstream.md`.

## Review

The implementation was reviewed locally for configuration privacy, executable
identity, workspace and symlink confinement, source and protocol bounds,
request/result correlation, call-before-result Session order, model-visible
error stability, cancellation, timeout, retry ownership, server-request
authority and descendant cleanup. The compiler, focused tests, all-target run
and Clippy provide the automated checks. No subagent was used because this
continuation was not authorized for delegation.

## Known limitations

- Configuration is explicit and process-local. Resume requires passing
  `--lsp-config` again, and an old unresolved LSP call is never replayed.
- The executable must be a canonical absolute regular file rather than a
  symbolic-link shim or mutable `PATH` lookup. The language server is trusted
  local software running with the current user's operating-system authority;
  this is not a sandbox.
- One CLI process owns one workspace. One actor serializes calls to each server,
  while different configured servers may be scheduled in parallel.
- Diagnostics, rename, symbols, call hierarchy, dynamic capability
  registration, automatic server discovery/installation and arbitrary
  JSON-RPC methods are intentionally absent.
- Source files are UTF-8, regular, non-symlink workspace files up to 4 MiB.
  Requests, responses, stderr, total protocol output, locations, rendered text,
  queue depth, timeout and cleanup grace all have fixed limits.
- The real wire path is tested with a deterministic Rust fixture server, not a
  generated TypeScript oracle or a third-party server matrix. The compatibility
  row therefore remains `partial`, not `compatible`.
