# Phase 54 validation — workspace image reading and DeepSeek vision

Date: 2026-08-29

Tested tree: Phase 54 working tree immediately before its green commit.

Environment: local macOS arm64, Rust 1.85.0
(`aarch64-apple-darwin`). Per the requested fast local scope, no remote CI,
real DeepSeek credential, live API, public-network request, or extra stress
matrix was used.

## Delivered behavior

- The production local registry advertises `read_image` with one closed
  `file_path` argument. It accepts case-insensitive PNG/JPEG/WebP/GIF
  extensions and checks the exact calling route before workspace file I/O.
- Only `deepseek-v4-flash-vision-exp` can execute the tool. `/model` shows it
  as a suggestion; the existing fixed-baseline model catalog remains unchanged
  so the Phase 2 oracle still compares exactly.
- One image is limited to 4 MiB and 40 million decoded pixels. Admission probes
  and fully decodes the raster, verifies the declared media type, and runs on a
  bounded blocking worker rather than a Tokio worker.
- Raw bytes are stored outside Session JSONL under the already-private Session
  root. Objects use `sha256:<64 lowercase hex>` identities, owner-only
  directories/files, create-new staging, hard-link publication, file/directory
  synchronization, deduplication verification, and bounded reads.
- The tool publishes storage before returning the upstream-style text envelope
  plus one image block. The ordinary Agent intent/result ordering and workspace
  path policy remain in force.
- Resume preloads only the newest request-retained objects and verifies digest,
  length, media type, and dimensions before Provider/tool side effects.
- DeepSeek vision requests preserve text/image order, include a stable handle,
  stream base64 data URLs without a second large string allocation, group
  tool-result images after their `role:tool` messages, reject unsupported
  roles/shapes, and omit oldest images beyond four occurrences or 4 MiB raw.
- `read_image` is exclusive in this first Rust slice so the 4 MiB/four-entry
  verified cache follows model order. Text reads and the existing safe tools
  keep their prior parallel behavior.

## Evidence

- Source-derived fixture:
  `tests/fixtures/tools/upstream_phase54_image_read.json`, SHA-256
  `40d0476921a7f27e1984d669b8ffb4fb1b095a8be808745bbd4299ce97f41138`.
- Attachment unit tests cover all four advertised formats, malformed input,
  SHA-256 identity, durable deduplication, resume preload, and corruption.
- Tool tests cover the closed extension surface, route-before-I/O refusal, and
  the two-block successful result.
- Provider tests cover grouped tool-result image order, exact inline data URL,
  and deterministic oldest-first offloading from five to four images.
- A real offline `cli_smoke` journey runs the installed test binary against a
  loopback DeepSeek-shaped server: request one calls `read_image`, request two
  contains the image data URL, and one durable object exists.
- The existing fixed Provider oracle remains green after keeping its two-model
  catalog unchanged.

The final local gates passed:

```console
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets -q
cargo clippy --all-targets -- -D warnings
git diff --check
```

The all-target suite passed 1,438 tests with no ignored tests. Focused tests
were used during implementation, followed by one ordinary repository-wide
run.

The new `image` 0.25.8 dependency is compiled with only PNG, JPEG, WebP, and
GIF features. It is used because header-only parsing would not prove that the
complete untrusted raster can decode; default codecs and unrelated formats are
disabled.

## Known limits

- Direct terminal upload, drag-and-drop, clipboard images, Goal/Plan image
  inputs, image editing, attachment export, remote storage, and DeepSeek Files
  API reuse are not implemented.
- Rust sends the original admitted raster inline; latest official master can
  normalize a separate request version and prefers reusable uploaded files.
- The 4 MiB/four-image request limits are intentionally much smaller than the
  latest official defaults. Oversized older occurrences become explicit text
  placeholders; an otherwise oversized complete JSON request still fails
  before HTTP under the existing 8 MiB wire cap.
- Attachment durability is best effort at the product's current Phase 8
  reliability level. Missing or corrupt retained objects fail closed rather
  than silently dropping an image.
