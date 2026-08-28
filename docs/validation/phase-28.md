# Phase 28 local validation — 2026-08-29

## Result

Phase 28 is complete under the requested local-only, necessary-check gate. The
real CLI now advertises and executes one search-only `web_search` without a
human approval prompt. It uses DeepSeek's separate Anthropic-compatible native
search route, returns a bounded source list, records the ordinary tool intent
before network execution, and continues the same Agent turn with the result.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` was inspected for the single
  `query` schema, search-only default composition, DeepSeek native request,
  result/citation mapping, eight-source cap, timeout, cancellation, and error
  behavior. Latest master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` was separately checked for its
  multi-query, untrusted-content, and `web_fetch` extensions.
- `LocalToolRegistry` advertises `web_search` only when CLI assembly installs
  the provider. It uses the ordinary preparation path rather than an approval
  Action, so argument validation, `tool/call`, execution, `tool/result`, and
  next-step replay keep their existing owners.
- The provider resolves `DEEPSEEK_API_KEY` per call, uses only
  `DEEPSEEK_SEARCH_BASE_URL` for its route, accepts HTTPS or loopback HTTP,
  disables redirects/proxy inheritance, and never includes the key in Debug,
  result, or Session data.
- One request is bounded to 4,096 query bytes, 60 seconds, a 2 MiB response,
  256 response blocks, 512 result/citation items, eight deduplicated sources,
  and individually bounded URL/title/snippet/date fields. Cancellation and the
  deadline cover partial response-body reads.
- A prose-only provider answer is rejected; only native
  `web_search_tool_result` blocks become sources. Output begins with an
  external-untrusted-data warning and asks the model to cite relevant URLs.
- A real `dsh --prompt` test runs two main-model requests and one separate
  search request against loopback servers. It checks the native wire shape,
  bounded source in the second model request, durable call-before-result order,
  absence of `approval/asked`, final answer, and clean shutdown.
- The complete local all-target suite passed, including 856 library tests, 115
  enhanced/linear real-PTY journeys, and all Agent, Provider, file, Shell,
  plugin, persistence, resume, release, and example targets.

## Local commands

```console
cargo test --lib web_search -- --nocapture
cargo test --test cli_smoke real_script_web_search_uses_the_separate_bounded_provider_and_continues -- --nocapture
cargo test --all-targets -- --test-threads=1
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Tests ran locally on macOS arm64 with Rust 1.85.0. They use fake providers,
temporary workspaces, local subprocesses/PTYS, fake credentials, and loopback
HTTP servers. No public-network request, real DeepSeek search, API charge,
remote CI, or additional platform matrix was used.

## Known limitations

- This phase follows the fixed commit's one-string `query`; current master can
  merge one to four queries.
- `web_fetch` is absent. Search cannot retrieve arbitrary URLs, browser cookies,
  signed-in pages, or full result bodies.
- Rust records the exact query in ordinary `tool/call` but does not add the
  fixed upstream's dedicated `web/deepseek-search-llm-request` event.
- The terminal uses its generic tool card rather than the upstream Web client's
  specialized source card.
- No live DeepSeek search was run, so endpoint availability, current model
  entitlement, latency, and billable behavior are not claimed by this local
  acceptance record.
