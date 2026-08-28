# Phase 47 validation — titles across historical tools

Date: 2026-08-29

## Delivered behavior

- `session_event_search`, `session_event_read` and `session_event_trace` carry
  the title from their exact already-authorized target candidate.
- `session_trace` carries title metadata in the target and every visible
  ancestor/descendant record from the same bounded lineage corpus.
- All headings use `Session <id> — <title>`; lineage rows use `<id> — <title>`;
  absent/unavailable values display `untitled` without losing the base result.
- No extra journal scan, model call, approval, ranking change or event payload
  change was introduced.

## Local evidence

- Source-attributed fixture:
  `tests/fixtures/tools/upstream_phase47_historical_tool_titles.json`.
- Actual closed-journal tests prove event search/read, lineage and event-trace
  title propagation.
- Renderer tests cover titled headings and lineage rows for all four tools.
- Local gates passed:

```console
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets -q
cargo clippy --all-targets -- -D warnings
git diff --check
```

No remote CI, live model request or additional stress matrix was run.

## Known limits

Rust has no live/current-session query source, projection cache, query cursor or
sanitized per-title unavailable code. Missing and unavailable titles are both
shown as `untitled`.
