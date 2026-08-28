# Titles across historical Session tools

## Scope and upstream basis

Phase 47 completes title headings across the four historical tools left out of
Phase 46: event search, exact event read, Session lineage trace and event trace.
The fixed upstream evidence is
`packages/session-query/tool-session-query/src/{operations,presentation,workspace-access}.ts`
and its title, authorization, failure and trace tests at commit
`47f943859bef60e4160492346772ded9b24f765a`. Current master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` retains the behavior.

## State and data flow

Every existing search candidate already owns `SessionMetadata` containing the
optional latest validated title. Targeted event operations copy that value into
their outcome. Lineage scanning copies it into each `SessionLineageRecord`, so
target, ancestors and descendants stay bound to the same journal observation
used for identity and parent facts. Renderers share one `Session <id> — title`
heading rule and use `— title` in lineage rows.

## Failure, safety and resources

No new file is opened and no additional scan, network call or approval occurs.
Missing/unavailable title metadata becomes `untitled`. Existing cancellation,
deadline, workspace identity, caller/busy exclusion, strict validation and
output limits remain authoritative. Titles are already normalized and capped
at 80 bytes before entering the journal.

## Tests and differences

Tests cover actual closed-journal propagation for targeted event operations,
Session lineage and event traces, plus titled rendering for all four outputs.
Official tooling can attach a sanitized per-title backend failure code; Rust
still folds absent and unavailable titles into `untitled`. Live sources,
projection caching and title-only batch APIs remain absent.
