# Phase 29 multi-query search and `web_fetch` design

This design retains the fixed DeepSeek Harness baseline
`47f943859bef60e4160492346772ded9b24f765a` and treats inspected master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` as a post-baseline product
extension. Exact upstream paths and tests are recorded in `docs/upstream.md`.

## Problem and scope

Phase 28 can search one query but cannot batch related searches or retrieve a
source page. Current upstream accepts one to four search queries and enables a
separately hardened `web_fetch` in the standard coding preset. Phase 29 adds
those two user-visible abilities. It does not add a browser, JavaScript
execution, form submission, cookies, authentication, arbitrary headers,
downloads, caching, proxy inheritance, cross-origin redirect following, or a
specialized terminal card.

The fixed fetch provider is useful evidence for URL, redirect, content-type,
charset, timeout, and size behavior, but its own source warns that it has no
private-network/SSRF protection. SSRF means tricking the Agent into making a
request to a machine or service that the user did not mean to expose, commonly
`localhost` or an internal cloud endpoint. Rust therefore follows the later
master hardening rather than enabling the unsafe fixed transport.

## Ownership and flow

`tools::web_search` continues to own the model schema, input parsing, result
merge, rendering, and provider-neutral search trait. It accepts exactly
`{queries: string[1..4]}`, rejects a blank/control/oversized member, removes
exact duplicate strings after validating the array, runs the remaining
queries concurrently, and round-robin merges results by rank. URL
deduplication and the combined eight-source cap apply after the fair merge.
The first provider failure cancels siblings and waits for them to settle before
returning one correlated error.

`tools::web_fetch` owns the exact `{url}` schema, provider-neutral result/error
types, external-content rendering, HTML conversion, metadata, and error
normalization. `provider::web_fetch` owns URL transport policy, DNS resolution,
address pinning, redirects, HTTP/body limits, content classification, and
decoding. A small resolver trait exists only because policy and real-loopback
tests need deterministic DNS answers; the production resolver uses Tokio's
system lookup.

```text
tool/call({url}) committed
  -> validate URL syntax/length/scheme/no embedded credentials
  -> resolve hostname once; validate every answer as public
  -> build one no-proxy/no-redirect client pinned to those addresses
  -> GET anonymously with a product User-Agent
  -> on redirect: require same origin, then resolve/validate/pin again
  -> classify, byte-cap, decode, character-cap
  -> convert bounded HTML to safe Markdown-like text
  -> tool/result success or structured error committed
```

The Agent and existing unified tool pipeline remain the sole owners of the
durable call/result order, cancellation, step closure, and recovery behavior.
Neither web tool writes Session facts directly.

## URL and network safety

Input is one absolute HTTP(S) URL of at most 2,048 UTF-8 bytes, without a user
name or password. Literal destinations and every resolver answer are checked.
If any answer is malformed or non-public, the whole request is refused rather
than choosing a convenient public answer. IPv4-mapped IPv6 is classified by
its embedded IPv4 address. IPv4 loopback, private, link-local, carrier-grade
NAT, documentation, benchmarking, multicast, reserved and broadcast ranges are
blocked. IPv6 unspecified, loopback, private, link-local, documentation,
multicast, transition and non-global ranges are blocked. DNS64/NAT64 discovery
uses `ipv4only.arpa`; a discovered translation to a blocked IPv4 address is
also refused.

After validation, `reqwest::ClientBuilder::resolve_to_addrs` receives only the
validated socket addresses. The URL hostname stays unchanged for the HTTP Host
header and TLS SNI, but the connector cannot perform a second DNS lookup and
switch to an internal address. Each same-origin redirect repeats resolution,
validation, and pinning. Cross-origin redirects fail and require a new explicit
tool call. At most five redirects are followed.

The client disables redirects, retries, and ambient proxy configuration. It
sends only `User-Agent` and a fixed `Accept`; there are no cookies,
authorization values, browser headers, or DeepSeek credential. A 30-second
deadline covers DNS, requests, redirects, and body collection. Caller
cancellation wins at all awaits.

## Body, rendering, and resource limits

`Content-Length` above 5,000,000 bytes fails before body collection. A chunked
body that crosses that cap keeps a bounded prefix and marks it truncated; an
exactly-at-cap body is not falsely labelled. Decoded input is capped at 100,000
characters. Accepted types are HTML/XHTML, `text/*`, JSON, XML, and structured
`+json`/`+xml`; missing or binary content types fail. UTF-8 is the default.
Rust supports UTF-8/ASCII and the common ISO-8859-1/Windows-1252 labels in this
phase and fails other declared charsets instead of returning mojibake. This
narrower charset set is an explicit implementation difference from the
platform-wide `TextDecoder` used upstream.

Plain text passes through after normalization. HTML uses a bounded lexical
converter with a maximum nesting depth of 512. It removes script, style,
noscript, template, iframe, object, embed, hidden elements, and hidden inputs;
renders common headings, paragraphs, lists, links, quotes, code and line breaks;
and decodes bounded common/numeric entities. Malformed or over-depth HTML
returns a fixed omission marker, never raw markup. This is deliberately more
conservative and less presentation-complete than upstream Turndown/GFM: some
tables or unusual markup lose formatting, but hostile active HTML never becomes
model-visible raw markup.

The final result starts with the final URL and HTTP status, then the standard
external-untrusted-data notice. Complete output is capped by the existing
64 KiB encoded tool-result limit, with a stable truncation footer. Metadata
contains only final URL, status, and the effective truncation flag.

## Failure, cancellation, approval, and recovery

Invalid or blocked URLs, DNS faults, provider faults, cross-origin/excessive
redirects, unsupported types/charsets, oversized declared bodies, timeout, and
cancellation map to stable secret-free `WebError` codes. Non-2xx HTTP responses
remain successful fetch results so the model can inspect bounded public error
text. No partial content is published after a policy or decoding failure.

Both tools are anonymous/read-only and, matching upstream, require no approval.
They still use the exact same validation → policy → execution → normalization
→ Session-result path as other tools. A model-generated URL is untrusted; page
text is also untrusted data and never an instruction. Recovery cannot replay an
unknown old call because the existing Session recovery rule closes it as
indeterminate before any new side effect.

## Verification and compatibility

Focused tests cover the new query schema, exact deduplication, concurrency,
first-failure sibling cancellation/drain, fair merge, and eight-source cap.
Fetch tests cover URL policy, representative IPv4/IPv6 blocks, mixed DNS
answers, DNS64 translation, pinned connection, redirects, type/charset/byte/
character caps, HTML omission and conversion, timeout, and cancellation. One
real script-mode CLI journey uses a loopback server reachable only through an
injected already-validated test resolver, asserts no approval, verifies the
second model request sees the correlated bounded page text, and checks the
durable call-before-result order.

Tests never use public DNS, a real URL, a real DeepSeek API key, or remote CI.
The fixed baseline remains pinned. Multi-query/fetch are recorded as inspected
current-master extensions, not silently claimed as fixed-commit compatibility.
Dedicated upstream web request events and its specialized Web card remain
unimplemented; ordinary durable tool call/result facts and the generic terminal
card remain the Rust behavior.
