# Phase 29 local validation — 2026-08-29

## Result

Phase 29 is complete under the requested local-only, necessary-check gate. The
real CLI now accepts one to four queries in `web_search` and exposes an
anonymous `web_fetch` for one explicitly named public HTTP(S) page. Neither
tool opens an approval prompt. Search failures cancel and drain sibling
queries; fetch blocks local/private destinations before connection and keeps
all returned content bounded and labelled untrusted.

## Evidence

- The fixed commit
  `47f943859bef60e4160492346772ded9b24f765a` was rechecked for its disabled
  fetch provider, which explicitly lacked private-network/SSRF protection.
  Latest inspected master
  `cd5ef8148158c3a752a658978873241fdf8e2bbc` supplied the multi-query merge,
  public-address resolution, DNS pinning, DNS64/NAT64 checks, redirect, body,
  rendering, and standard-preset behavior recorded in `docs/upstream.md`.
- `web_search` now validates exactly one `queries` array of one to four bounded
  strings, removes exact duplicates, starts remaining DeepSeek requests
  concurrently, cancels/drains siblings on the first failure, and fairly
  round-robin merges and URL-deduplicates at most eight sources.
- `web_fetch` validates one HTTP(S) URL of at most 2,048 UTF-8 bytes and rejects
  embedded credentials. It rejects a complete DNS answer set if any address is
  non-public, classifies IPv4-mapped IPv6, discovers DNS64 prefixes, rejects a
  private translated IPv4 destination, and pins the HTTP client to only the
  checked socket addresses while preserving the hostname for Host/TLS.
- Fetch disables proxy inheritance, redirects, and retries in the HTTP client.
  Its explicit redirect loop allows at most five same-origin hops and performs
  a new resolution, validation, and pinned request at every hop. One 30-second
  deadline covers DNS, request, redirect, and body reads; Ctrl+C cancellation
  wins at every awaited boundary.
- Declared response size above 5,000,000 bytes fails before collection; streamed
  bytes, decoded characters, supported text content types/charsets, and final
  64 KiB encoded tool output are bounded. Non-2xx text remains a result. HTML
  conversion removes active/hidden elements, stops at depth 512, and substitutes
  a fixed omission marker rather than returning unsafe raw markup.
- Registry tests use fake providers to prove both schemas and the normal
  no-approval preparation path. Direct loopback transport tests prove a
  non-resolving hostname connects only to the supplied pinned address and keeps
  the hostname in the request. Policy tests prove the production provider
  refuses literal loopback before transport.
- A real `dsh --prompt` journey issues two concurrent native DeepSeek-shaped
  search requests and continues with one merged result. A second real journey
  asks `web_fetch` for a live loopback sentinel, proves no socket was accepted,
  returns `WEB_BLOCKED_URL` to the next model request, and finishes normally.
- The complete local all-target suite passed: 872 library tests, 34 script CLI
  journeys, 115 enhanced/linear real-PTY journeys, and all remaining Agent,
  Provider, file, Shell, plugin, persistence, resume, release, and example
  targets. Formatting, compilation, Clippy with warnings denied, and diff
  whitespace checks also passed.

## Local commands

```console
cargo test --lib web_ -- --nocapture
cargo test --lib provider::web_fetch::tests -- --nocapture
cargo test --test cli_smoke real_script_web_ -- --nocapture
cargo test --all-targets --quiet
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

Tests ran locally on macOS arm64 with Rust 1.85.0. They use fake providers,
temporary workspaces, local subprocesses/PTYs, fake credentials, an injected
test resolver, and loopback HTTP servers. No public-network request, live web
page, real DeepSeek search, API charge, remote CI, or additional platform
matrix was used.

## Known limitations

- This is an inspected current-master extension. The fixed baseline's model
  schema has one `query`, while Rust now exposes latest master's `queries`.
- HTML conversion is intentionally conservative rather than full Turndown/GFM.
  It can lose unusual layout or tables. Supported declared non-UTF-8 charsets
  are limited to ASCII and ISO-8859-1/Windows-1252 labels.
- `web_fetch` is anonymous text retrieval, not a browser: it has no JavaScript,
  cookies, login state, forms, custom headers, downloads, proxy, or cross-origin
  automatic redirect. Some legitimate special-purpose IP ranges are refused.
- Rust uses ordinary durable `tool/call`/`tool/result` facts and the generic
  terminal card rather than upstream's dedicated web request events and Web
  cards. A generated cross-language oracle is absent, so compatibility remains
  `partial`.
- No live public fetch/search was run, so current remote availability, TLS/DNS
  behavior outside the tested policy seams, entitlement, latency, and billing
  are not claimed by this local record.
