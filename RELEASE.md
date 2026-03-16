# Release Checklist

This checklist is specific to `git-closure`'s known failure modes and must be
completed before tagging a release.

## 1) Format Consistency Gate

- If `.gcl` bytes or `snapshot-hash` behavior changed, update all of:
  - `SPEC.md`
  - `README.md`
  - golden fixtures in `tests/fixtures/`
- Confirm implementation/spec consistency for hash framing and endianness.
- Ensure commit message explicitly calls out format change when applicable.

## 2) README Contract Gate

- Run README example fixtures:
  - `cargo test --locked --test cli`
- Confirm examples under `tests/cli/README/` still pass.
- Remove or mark `[planned]` any non-shipped command references.

## 3) Golden Fixture Gate

- Run golden tests:
  - `cargo test --locked --test golden`
  - `cargo test --locked --test rt13_spec_and_fixture`
- Verify fixture updates are intentional and documented.

## 4) Exit-Code Taxonomy Gate

- Verify documented exit semantics by exercising:
  - `diff` identical vs changed
  - `fmt --check` canonical/noncanonical/hash-mismatch/parse-error
  - operational error path (missing file or provider/subprocess failure)
- Ensure README exit-code table matches observed behavior.

## 5) Quality Gate

- `cargo test --locked`
- `cargo clippy --locked -- -D warnings`
- `cargo fmt --check`
- `cargo build --locked --release`

## 6) CI/Workflow Gate

- Ensure CI matrix is green on Linux and macOS.
- Ensure release workflow definition is present for `v*` tags.
