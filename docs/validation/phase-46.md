# Phase 46 validation — title-enriched prior-Session search

Date: 2026-08-29

## Delivered behavior

- Every `session_search` hit carries the latest title from the same authorized,
  closed and strictly validated journal metadata observation.
- The model-facing heading is `Session <id> — <title>`; missing or unavailable
  title metadata becomes `untitled` without removing the base match.
- Ranking, filters, snippets, same-workspace authorization, caller/busy
  exclusion, cancellation and result limits are unchanged.
- At most 20 already-normalized 80-byte titles are added, and the maximum-result
  regression remains below the ordinary tool-output budget.

## Local evidence

- Source-attributed fixture:
  `tests/fixtures/tools/upstream_phase46_session_search_titles.json`.
- Unit coverage proves real closed-journal title propagation, titled/untitled
  rendering and the maximum rendered result bound.
- The existing real two-process CLI journey now asserts that a Phase 45
  fallback title appears in the next model request's `session_search` result.
- Required local commands passed:

```console
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets -q
cargo clippy --all-targets -- -D warnings
git diff --check
```

No remote CI, network model request or additional stress matrix was run.

## Known limits

Event search/read/trace and Session lineage trace headers are not title-enriched.
Rust does not distinguish an actually untitled journal from an isolated title
inspection failure, so both display `untitled`; the base search result remains
available in either case.
