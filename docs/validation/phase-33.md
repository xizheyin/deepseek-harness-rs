# Phase 33 local validation — 2026-08-29

## Result

Phase 33 is complete under the requested local-only, necessary-check gate. When
one approved foreground Bash stream exceeds 64,000 bytes, dsh still returns its
bounded tail but also stores every byte actually captured in a random
owner-only temporary file. The model-facing result and final terminal card give
the locator. Small output creates no spill artifact.

## Evidence

- Fixed upstream commit
  `47f943859bef60e4160492346772ded9b24f765a` supplied the lazy per-stream spill,
  prior-tail replay, private modes, failure fallback, and exact normal-path
  `[output truncated; full output: PATH]` notice recorded in
  `docs/upstream.md`. Latest inspected master retains the behavior.
- `tests/fixtures/tools/upstream_phase33_shell_spill.json` records the source
  paths, 64,000-byte tail, 64 MiB official per-stream cap, normal notice, full
  byte count, and 0700/0600 expectations.
- The real collector creates one random private directory only after overflow,
  opens each stream file exclusively, writes the prior prefix before the
  overflowing chunk, awaits later writes, and flushes before publishing a
  locator. Write or flush failure removes the unpublished partial file when
  possible and safely falls back to the old tail-only result.
- Process tests cover exact-tail/no-file, uneven chunks, two independent
  streams, natural completion, caller cancellation, the existing 8 MiB output
  stop, byte counts, and cleanup. Renderer tests distinguish a proven full
  stream from the bounded prefix captured before a forced stop.
- The real Agent test verifies tail text, full file bytes, private permissions,
  result metadata and call correlation. The real CLI PTY journey confirms an
  approved Bash call returns the private locator to the next model step.
  Projector and timeline tests prove new paths render while old Session results
  without spill fields remain valid.
- The final local repository suite passed: 884 library tests, 16 real Shell
  integration tests, 118 enhanced/linear real-PTY journeys, and every remaining
  Agent, Provider, file, plugin, persistence, resume, release, and example
  target.

## Local commands

```console
cargo test --lib tools::process::spill
cargo test --lib tools::process::api_tests::combined_output_accepts_exactly_eight_mibibytes
cargo test --lib tools::process::api_tests::first_byte_over_the_combined_limit_forces_immediate_kill
cargo test --lib tools::process::api_tests::cancellation_flushes_output_spilled_before_group_cleanup
cargo test --lib tools::shell::tests::renderer_distinguishes_full_and_incomplete_spill_files
cargo test --lib tui::projector::tests::shell_metadata_accepts_legacy_results_and_only_complete_spill_extensions
cargo test --lib tui::timeline::tests::shell_card_exposes_private_spill_locators
cargo test --test shell_tools overflowing_shell_stdout_keeps_the_tail_and_exposes_the_private_full_stream
cargo test --test shell_tools rendered_shell_output_obeys_the_64_kib_compact_json_limit
cargo test --test interactive_cli foreground_shell_spills_full_output_and_returns_its_private_locator_to_the_model
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo fmt --all -- --check
git diff --check
```

Tests ran locally on macOS arm64 with locked dependencies. They use a fake
provider, temporary workspaces, local subprocesses/PTYs, and loopback HTTP. No
public-network request, real DeepSeek call, API charge, remote CI, or additional
platform matrix was used.

## Known limitations

- Rust deliberately retains its stricter 8 MiB combined observed-output stop,
  rather than the official 64 MiB per-stream spill. A forced-stop spill is an
  explicitly labelled captured prefix, not the full command output.
- Spill files are best-effort temporary artifacts, not Session storage or a
  backup. The OS or user may remove them, and resume/fork does not recreate or
  migrate their bytes.
- Spill content can contain secrets printed by the approved command. Directory
  mode 0700 and file mode 0600 protect against other ordinary local users, but
  they do not encrypt the data or protect it from the same account.
- The workspace-confined `read` tool cannot open these host-temporary paths.
  Retrieval requires another normally approved Bash call.
- Only foreground built-in Bash has this early spill path. There is no generic
  final-result spill policy, background-job support, spill browser, or automatic
  lifecycle cleanup after successful publication.
- The fixture is source-attributed but manually transcribed rather than emitted
  by a checked-in TypeScript producer; compatibility therefore remains
  `partial`, not `compatible`.
