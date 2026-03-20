# Sprint Review (Sprint B/C Completion)

This review captures the required end-of-sprint checks after completing Sprint B
and Sprint C items through `FR-04`, with `FR-05` intentionally deferred.

## 1) Cross-Artifact Consistency

- CLI surface is aligned across `README.md`, `AGENTS.md`, and `--help` output:
  `build`, `materialize`, `verify`, `list`, `diff`, `fmt`, `render`, `summary`,
  `completion`.
- `README.md` now documents:
  - snapshot-vs-directory diff auto-detection,
  - remote build provenance headers (`source-uri`, `source-provider`),
  - `summary` text/JSON modes,
  - materialize policy profiles (`Strict`, `TrustedNonempty`, `NoSymlink`).
- `SPEC.md` remains format-focused; no incompatible format changes were made in
  this sprint.

## 2) Exit-Code Taxonomy Audit

Verified via direct binary execution (`target/debug/git-closure`):

- `diff` identical -> `0`
- `diff` changed -> `1`
- `fmt --check` canonical -> `0`
- `fmt --check` noncanonical -> `1`
- `fmt --check` hash mismatch -> `2`
- `fmt --check` parse error -> `3`
- operational failures (`verify` missing file, provider rejection) -> `4`

## 3) Golden Fixture Verification

- `nix develop -c cargo test --locked --test golden` passed.
- `nix develop -c cargo test --locked --test rt13_spec_and_fixture` passed.
- Linux environment verified in this session; macOS verification not available
  in the current environment.

## 4) Dead Code Scan

- `nix develop -c cargo check --locked` completed cleanly; no new `unused` or
  `dead_code` warnings surfaced.

## 5) Dependency Duplication Audit

- `nix develop -c cargo tree --duplicates` reports expected transitive
  duplication (`getrandom` 0.2/0.3/0.4 and `webpki-roots` 0.26/1.0) driven by
  runtime plus test/tooling stacks.
- No new duplication introduced by Sprint B/C features.

## 6) Security Posture Review

- `materialize_snapshot_with_options` preserves strict-by-default behavior;
  `materialize_snapshot` remains a strict wrapper.
- `TrustedNonempty` is explicit opt-in; path containment and symlink-ancestor
  checks still run.
- `NoSymlink` fails fast with `Parse` on symlink entries.
- `GithubApiProvider` extraction continues to enforce path safety and symlink
  containment for archive materialization.

## 7) Deferred / Follow-up

- `FR-05` (signed snapshot headers) is correctly left deferred by backlog
  policy.
- Follow-up candidate: add a CLI flag surface for materialize policy selection
  if non-default library policies are expected to be used from shell workflows.
