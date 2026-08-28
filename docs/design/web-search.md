# Phase 28 bounded `web_search` design

This design is based on DeepSeek Harness commit
`47f943859bef60e4160492346772ded9b24f765a`. Current master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` is also inspected for later
multi-query and untrusted-content hardening. Exact paths are listed in
`docs/upstream.md`.

## Problem and scope

The fixed official composition gives every normal coding agent a search-only
`web_search`; dsh-rs currently cannot retrieve current information except by
asking the model to run an unrelated Shell command. Phase 28 adds the fixed
tool contract and DeepSeek search provider. It excludes `web_fetch`, arbitrary
URL access, cookies, proxy credentials, browser state, provider selection,
multiple search providers, multi-query batching, and a specialized terminal
card.

## Ownership and flow

`tools::web_search` owns the strict query parser, source types, rendering, and
the provider trait used by deterministic tests. Its production provider owns
the separate HTTPS route and secret. `LocalToolRegistry` owns one provider
handle and advertises the schema; the Agent remains the sole owner of call,
result, timeout, cancellation, and Session ordering.

```text
tool/call(query) committed
  -> validate exactly { query: nonblank string }
  -> resolve DEEPSEEK_API_KEY
  -> POST {DEEPSEEK_SEARCH_BASE_URL}/messages
  -> parse native web_search_tool_result blocks and URL citations
  -> deduplicate/cap/render sources
  -> tool/result success or structured error committed
```

The fixed schema has one required `query`. The runtime additionally caps it at
4,096 UTF-8 bytes and rejects control characters. At most eight unique sources
survive. URL, title, snippet, and publication-date fields are individually and
collectively bounded before a result reaches the Agent's existing 64 KiB tool
content limit. Search-result text is explicitly labelled external untrusted
data, adopting current master's later prompt-injection hardening without
changing the fixed request shape.

## Network, secret, and cancellation rules

Production search reuses only `DEEPSEEK_API_KEY`. Its default base is
`https://api.deepseek.com/anthropic/v1`; only
`DEEPSEEK_SEARCH_BASE_URL` may override it. Base validation requires one
absolute HTTPS URL without userinfo, query, or fragment; HTTP is accepted only
on loopback for offline tests. Redirects and ambient proxies are disabled. Requests carry both
`x-api-key` and `Authorization: Bearer`, but errors, Debug output, Session
events, and results never include either header or its value.

One operation has a 60-second deadline covering send and bounded response-body
collection. Caller cancellation wins before credential resolution, before
send, during response, and before result acceptance. The client owns no
background worker; dropping its future cancels the request. The response body
and nesting are bounded before normalization. HTTP/provider details are
reduced to stable secret-free tool errors.

## Session and approval semantics

`tool/call` already records the exact query before execution and the ordinary
Agent dispatch barrier completes before the executor is polled, satisfying
intent-before-network-side-effect. The fixed upstream also appends a dedicated
secret-free `web/deepseek-search-llm-request` record containing route and body.
Rust does not add a one-provider event in this phase because the exact external
input is already derivable from the durable call and fixed provider recipe;
this remains a documented compatibility difference.

Search is read-only and follows upstream by requiring no human approval. It
still passes through schema validation, the unified tool preparation path,
timeout/cancellation, result normalization, and Session recording. It cannot
read workspace files or inherit Shell environment beyond the named API key and
base override.

## Failure behavior

An invalid endpoint or unusable HTTPS client fails assembly before a model
request. Missing/invalid credentials, transport failure, timeout,
cancellation, non-success HTTP status, oversized or malformed JSON,
and absence of native search-result blocks all return one correlated error
result. No prose-only provider response is trusted as search output. Partial
sources are never published after a failed parse. A zero-result native block is
a successful `No results found.` result.

## Verification

Focused tests cover exact arguments, result mapping/deduplication/rendering,
secret redaction, HTTP status and malformed/oversized bodies, redirects,
timeout and cancellation, registry schema/dispatch, Agent call-before-network
and result ordering, plus one real CLI journey against a loopback
DeepSeek-shaped server. No test reads a real API key or uses the public network.
