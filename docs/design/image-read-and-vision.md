# Workspace image reading and DeepSeek vision

## Scope

Phase 54 lets the model call `read_image` for one PNG, JPEG, WebP, or GIF
inside the opened workspace. The tool validates and durably stores the image,
then returns a text envelope plus the existing provider-neutral image block.
`deepseek-v4-flash-vision-exp` serializes retained images as inline base64
`image_url` parts. Text-only routes reject the tool before reading the file.

Direct terminal uploads, drag-and-drop, clipboard images, the DeepSeek Files
API, image editing, and exporting attachment objects are outside this phase.
The attachment/runtime boundary is deliberately reusable by a later terminal
input phase.

## Evidence

The fixed baseline is commit
`47f943859bef60e4160492346772ded9b24f765a`. Its relevant sources are:

- `packages/fs/tool-fs/src/read-image.ts` and `tests/read-image.spec.ts`;
- `packages/attachment/attachment{,-local}/src/` and local-store tests;
- `packages/llm/llm-deepseek/src/serialize.ts`, which rejects images.

Latest master `cd5ef8148158c3a752a658978873241fdf8e2bbc` adds the
`deepseek-v4-flash-vision-exp` catalog route and ordered Files API/base64 image
serialization in `packages/llm/llm-deepseek/src/{index,serialize,adapter}.ts`.
The committed Phase 54 fixture records the fixed tool/store behavior and the
latest wire extension separately, so this does not relabel latest behavior as
fixed-baseline compatibility.

## State and order

1. The Agent records the validated `tool/call` intent through its existing
   pipeline.
2. `read_image` checks the exact calling provider/model and file extension
   before file I/O.
3. The workspace capability reads at most 4 MiB. A blocking worker fully
   decodes the bounded raster, verifies its declared type and enforces a
   40-million-pixel limit.
4. The attachment store hashes the bytes with SHA-256 and publishes one
   owner-only immutable object before returning its reference.
5. A bounded in-process cache receives the committed bytes. The tool returns
   the upstream text envelope and image reference; only then can the Agent
   append `tool/result`.
6. Before a provider request, the synchronous encoder selects newest image
   occurrences under four images and 4 MiB raw bytes. Older occurrences become
   deterministic text placeholders. Retained bytes come only from the verified
   cache and become ordered data URLs.
7. Resume preloads only the newest request-retained objects from the durable
   store, rechecking digest, media type, dimensions, and length.

## Failure, cancellation, and safety

- Wrong routes and unsupported extensions fail before workspace reads or
  attachment writes.
- Empty, malformed, mismatched, oversized, or excessive-pixel images fail
  without returning an image reference.
- Workspace reads keep the existing path and symlink policy. CPU-heavy decode
  and attachment filesystem work run on blocking workers.
- Cancellation is checked before and after bounded read/decode/store work. A
  decoder already running is allowed to finish inside the fixed byte/pixel
  bounds; no detached task remains.
- Object identity is `sha256:<64 lowercase hex>`. Existing objects are accepted
  only after their digest matches. Session logs contain metadata, never image
  bytes or ambient paths.
- A missing or corrupt retained attachment fails resume assembly before a new
  model request or tool side effect.

## Ownership and replay

The new attachment module owns validation, content-addressed storage, cache,
and reference verification. The tool owns workspace path resolution and its
model-facing envelope. The DeepSeek adapter owns route support, request
offloading, and inline wire encoding. Session remains the sole owner of the
ordered image reference facts.

The local store is placed below the already-private Session root as
`attachments-v1`, whereas official dsh uses `DSH_HOME/attachments/v1`. This is
an intentional Rust packaging difference: the CLI already has a capability-
checked private state root and needs no second ambient home-path policy. It
changes only the object location, not attachment IDs or logged references.

The first Rust slice uses inline base64 only. Latest official master prefers
the Files API and falls back to base64; omitting reusable uploads is simpler
and avoids another credentialed lifecycle, at the cost of larger repeated
requests. It also sends the already-validated original raster instead of
creating a resized/re-encoded request version. Rust's 4 MiB/four-image request
policy is therefore stricter and is covered as an intentional resource-bound
difference.

Although the fixed tool declares content-addressed reads concurrency-safe,
Rust schedules `read_image` exclusively in this first slice. That preserves
model-order cache eviction without retaining every decoded image in memory;
ordinary text reads and other existing safe tools remain parallel.

## Tests

Focused tests cover route-before-I/O gating, strict arguments, four formats,
type mismatch, malformed/byte/pixel bounds, content-addressed deduplication,
corruption/missing recovery failure, newest-first request offload, ordered
user/tool-result wire parts, request-size enforcement, and text-only rejection.
An Agent test proves the exact model route reaches tool execution. The normal
local repository gates remain the acceptance check; no live API or remote CI
is required.
