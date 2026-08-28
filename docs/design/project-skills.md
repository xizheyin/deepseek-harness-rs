# Project-local Skills design

Status: implemented and locally validated in Phase 35.

## Problem and scope

An Agent repeatedly needs task-specific instructions that are too specialized
for every request. Phase 35 discovers real project Markdown Skills, advertises a
small name/description catalog to the model, and loads one full body only when
the model calls `skill`.

In scope are `.dsh/skills` and `.agents/skills` beneath the already opened
workspace, directory and flat Markdown entries, a durable catalog, the closed
model tool, current-body reload, cancellation and bounded refresh. This phase
does not execute a Skill's scripts, grant permissions, scan outside the
workspace, watch files in the background, or add remote providers, UI menus or
direct `/name` invocation.

## Upstream basis

The semantic baseline is fixed commit
`47f943859bef60e4160492346772ded9b24f765a`. Exact packages and tests are listed
in `docs/upstream.md`; the checked latest reference is
`cd5ef8148158c3a752a658978873241fdf8e2bbc`.

The fixed behavior used here is: project `.dsh` precedes project `.agents`,
names use lowercase kebab-case, discovery publishes only model-invocable
summaries, `skill` accepts exactly one name, loading rereads the file, and the
model receives canonical catalog and `<skill_content>` frames.

## Ownership and data flow

`SkillRuntime` owns the retained read-only workspace capability and all
discovery/rendering rules. `LocalToolRegistry` borrows a clone for the `skill`
schema and execution. `AgentLoop` borrows another clone to prepare catalog
context before a turn and after a completed tool step.

```text
workspace roots -> bounded discovery -> catalog context -> Session user/message
model skill call -> tool/call -> current bounded reread -> tool/result -> next step
```

The catalog source records `kind: skill-catalog`, `form: catalog`, the complete
published entries and whether it is a replacement. The file path and body are
not stored in the catalog. A loaded body becomes ordinary retained tool-result
content, so replay and resume use the same facts as the next Provider request.

## Parsing and priority

Each root is scanned one level deep, with at most 64 retained skills overall:

1. `.dsh/skills` (rank 100);
2. `.agents/skills` (rank 200).

An entry is either `<entry>/SKILL.md` or one flat `<entry>.md`. Direct and
intermediate symlinks, special files, non-UTF-8 names/content, files above 256
KiB, invalid frontmatter and invalid names are skipped. An unexpected I/O error
makes the observation incomplete, so it cannot publish a misleading deletion.
The first candidate by rank then lexical path wins a duplicate declared name.

Frontmatter is delimited by exact `---` lines. The bounded parser accepts
plain, single-quoted or JSON-style double-quoted scalar values for required
`name` and `description`, optional `whenToUse`, plus the official common boolean
spellings for `disable-model-invocation` and `user-invocable`. Unknown scalar
fields are ignored; duplicate or structured interpreted fields reject that
entry. The body is trimmed and may be empty.

## Catalog and result

Descriptions collapse whitespace and retain at most 500 Unicode scalar values.
The catalog uses the fixed `<system-reminder>` / `<available_skills>` wording,
escapes framing characters in descriptions, and stays below 64 KiB. No initial
message is emitted for an empty catalog. An unchanged visible catalog is not
duplicated; a changed or no-longer-visible catalog emits a complete replacement.

The tool schema is closed: `{ "name": string }`. On success, the tool returns
the fixed `<skill_content>` frame with provider `filesystem`, an absolute base
directory hint rooted in the retained workspace, and current body bytes.
Failures are structured as `SKILL_INVALID`, `SKILL_UNKNOWN`, `SKILL_DISABLED`,
`SKILL_UNAVAILABLE`, or ordinary cancellation; no partial body is returned.

## Failure, cancellation and security

- Discovery and loading are read-only: no approval and no side effect exists.
- Work runs on a blocking worker because host filesystem calls are blocking.
- Cancellation is checked before scheduling, between roots/entries, before and
  after every bounded read, and before a catalog or result is returned.
- The opened directory descriptor is authority. Display paths never authorize a
  read, and symlinks cannot redirect it.
- Catalog/file/entry/name/description/result limits prevent unbounded memory or
  context growth. Malformed files disappear from the catalog rather than
  becoming permissive.
- A file changing between catalog discovery and tool load is reread and must
  still declare the requested name and allow model invocation. Otherwise the
  call fails; old history is never rewritten.

Normal completion records `tool/call` before the read result and then the normal
step/turn closure. Invalid/unknown/disabled/unavailable calls produce a truthful
model-facing error result. Cancellation closes the call, step and turn through
the existing Agent machinery. Resume reconstructs prior catalogs/results from
the journal and rescans before new Provider work; it never replays a Skill call.

## Tests and intentional differences

The source fixture fixes schema, priority, catalog/result text and stable error
codes. Unit tests cover format, duplicate priority, flat/directory resources,
disabled and malformed entries, limits, symlinks, refresh and cancellation. A
real Agent test and enhanced PTY journey cover catalog -> call -> result -> next
request, Session order and resume refresh.

Unlike official dsh, Rust does not automatically read `$DSH_HOME`, `~/.agents`,
custom/bundled roots or follow symlinks, and it does not run watchers or direct
human `/name` injection. Its scalar frontmatter parser is not a general YAML
implementation. These choices keep the first capability project-scoped,
auditable and dependency-free; users see fewer Skills than official dsh, but no
ambient file becomes model-visible without being inside the opened workspace.
