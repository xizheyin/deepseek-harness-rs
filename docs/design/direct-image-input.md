# Direct terminal image input

## Problem and scope

Phase 54 lets the model discover and read an image through a tool, but a human
cannot attach an image to the first request. Phase 55 adds that missing entry:
repeatable `--image <PATH>` in script mode and an interactive `/image <PATH>`
draft consumed by the next ordinary prompt. `/image` shows the draft and
`/image clear` removes it.

The phase accepts PNG, JPEG, WebP and GIF files inside the retained workspace.
It does not add clipboard/drop handling, arbitrary files, outside-workspace
approval, image-only submission, persistent drafts, previews, or image-bearing
Goal/Plan/local commands. Existing `@relative/path ` completion remains literal
text and never becomes an implicit read.

## Upstream evidence

The fixed baseline is
`47f943859bef60e4160492346772ded9b24f765a`. The relevant behavior is in:

- `packages/host/apiproxy/src/api-proxy.ts`, especially
  `durablePromptContent` and `sessions.prompt`;
- `packages/host/apiproxy/tests/api-proxy-models.spec.ts`;
- `packages/attachment/attachment/README.md` and the attachment service/store;
- `packages/client/ui-conversation/src/client/service.ts` and its input tests.

The fixed Host rejects an image-incompatible model before promoting uploads,
checks message count and aggregate bytes, validates the complete image batch,
saves each accepted image, and only then creates one user message. Browser
drafts remain temporary until submission. Latest master
`cd5ef8148158c3a752a658978873241fdf8e2bbc` retains that host boundary and adds
image-aware command submission; Rust keeps Goal/Plan command images out of
this small phase.

## Input, state, and order

`--image` may occur at most four times. Interactive `/image <PATH>` appends one
bounded path to a process-local ordered draft; it performs no file I/O and
writes no Session event. The next ordinary prompt runs this sequence:

1. Snapshot the Agent's exact current model. Anything except
   `deepseek-v4-flash-vision-exp` fails before path resolution or reads.
2. Enforce four paths, supported filename extensions, regular non-symlink
   workspace paths, 4 MiB per image, and 4 MiB total raw bytes.
3. Fully decode and validate every image, including extension/byte agreement
   and the existing 40-million-pixel bound. Nothing is persisted during this
   validation pass.
4. Save the validated members in order through the existing immutable
   content-addressed store.
5. Construct one user message with image blocks in path order followed by the
   exact prompt text. The normal Agent Loop then records that message before
   its Provider request.
6. Clear the interactive draft only after the user message is observed as
   committed. Rejection keeps both the text draft and image paths.

The attachment runtime owns validation and immutable objects. A small CLI
image-input adapter owns path admission and batch choreography. Session remains
the only owner of message/event order; the Provider still sees only durable
references resolved through the verified attachment cache.

## Failure, cancellation, and safety

- Unsupported routes fail before workspace I/O or attachment writes.
- Unsupported extensions, missing/non-regular/symlink paths, oversized files,
  aggregate overflow, malformed rasters and type mismatch fail before any user
  message or Provider request.
- All members validate before the first save, so one malformed member cannot
  strand earlier valid members. A storage failure or cancellation during the
  later save loop can leave a private unreferenced content-addressed object;
  it can never become model-visible or be replayed without a Session reference.
- Reads and decode/storage work are bounded and cooperative cancellation waits
  for any blocking worker to finish; no background task is detached.
- Paths and raw bytes never enter Session events. Only the clean basename and
  verified attachment metadata are logged.
- The operation is read-only with respect to the workspace and requires no
  approval. It grants no Shell, network, or outside-workspace authority.

Normal Agent errors, timeout and Ctrl+C after message admission keep the
existing truthful turn/step closure. Cancellation before admission creates no
turn. Resume reuses Phase 54 verification and request reconstruction.

## Intentional differences

Official dsh receives temporary browser files through paste/drop and may send
an image-only prompt or attach images to selected commands. Rust exposes
explicit terminal paths, requires accompanying ordinary text, rejects final
symlinks, and does not attach images to Goal/Plan/local commands. This fits a
path-oriented CLI and avoids inventing a terminal clipboard protocol. The
observable effect is that some official Web compositions must be expressed as
`/image <PATH>` followed by text in Rust.

## Tests

The source-derived fixture fixes capability-first admission, full-batch
validation, save-before-message order and content order. Unit tests cover
repeatable argument limits, command parsing/draft behavior, route-before-I/O,
path/type/count/aggregate failures, malformed-later-member atomic validation,
ordered success and cancellation. One real script request proves direct image
wire content and durable Session order; one linear PTY journey proves `/image`
staging, text-route retention, vision send and zero ANSI. Acceptance is local
`fmt`, all-target check/test/Clippy and `git diff --check`; no live API, remote
CI, stress run or extra platform matrix is required.
