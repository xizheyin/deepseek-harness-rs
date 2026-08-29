# Phase 55 local validation

Date: 2026-08-29

Status: `in-progress`

## Scope

This checkpoint implements direct, bounded terminal image attachments for the
exact `deepseek-v4-flash-vision-exp` route. Script mode accepts repeatable
`--image <PATH>` arguments and interactive mode keeps `/image <PATH>` entries as
a process-local draft for the next ordinary prompt.

The implementation follows the fixed upstream baseline
`47f943859bef60e4160492346772ded9b24f765a` for prompt admission and durable
attachments, with the current upstream image command behavior at
`cd5ef8148158c3a752a658978873241fdf8e2bbc` inspected separately. The detailed
boundary and intentional terminal-specific differences are recorded in
`docs/design/direct-image-input.md` and `docs/compatibility.md`.

## Checks completed locally

- `cargo fmt --all` passed.
- `cargo check --all-targets` passed.
- Focused CLI library tests passed: 202 tests.
- The real script CLI direct-image journey passed and produced ordered image
  references followed by the exact prompt text.
- The real linear PTY journey passed: an image draft survived a text-model
  rejection, was sent after switching to the vision model, and then cleared.
- The command-palette PTY journey passed after updating the expected seventeenth
  entry.
- A repository-wide run passed all 1001 library tests, 48 CLI smoke tests and
  136 interactive CLI tests before an unrelated plugin cancellation test
  failed intermittently while reading its child PID; that exact test passed on
  immediate focused retry.

## Remaining gate

A subsequent repository-wide run was stopped at the user's request to push the
current implementation immediately. `cargo clippy --all-targets -- -D warnings`
was therefore not rerun after the final edits. This record does not claim a
green repository-wide gate, and Phase 55 remains `in-progress`.

All validation used local fake providers or loopback fixtures. No real DeepSeek
API request, remote CI run, or user credential was used.
