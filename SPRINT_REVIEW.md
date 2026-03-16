# Sprint Review (Post-Sprint v0.3 Backlog)

This review records the final consistency and risk checks after implementing the
remaining backlog work.

## 1) Cross-Artifact Consistency

- CLI surface in `README.md`, `AGENTS.md`, and runtime `--help` is aligned to:
  `build`, `materialize`, `verify`, `list`, `diff`, `fmt`, `render`,
  `completion`.
- Deprecated/planned names (`query`, `watch`, `explode`, `export`) are no
  longer presented as shipped commands.
- Canonical metadata term is `git-rev` across implementation/docs/spec.
- Provider semantics now match implementation: `gh:` auto-routes to
  `github-api`, `github:` remains a Nix flake reference.

## 2) Exit-Code Taxonomy Audit

Representative command audit confirms:

- `diff` identical -> `0`
- `diff` changed -> `1`
- `fmt --check` canonical -> `0`
- `fmt --check` noncanonical -> `1`
- `fmt --check` hash mismatch -> `2`
- `fmt --check` parse error -> `3`
- operational failure path (`verify` missing file) -> `4`

## 3) Golden Fixture Verification

Validated in devshell:

- `nix develop -c cargo test --locked`
- `tests/golden.rs` passes
- `tests/rt13_spec_and_fixture.rs` passes

The golden fixture net remains authoritative for `.gcl` byte-level stability.

## 4) Dead Code Scan

`nix develop -c cargo check --locked` output scanned for `unused`/`dead_code`;
no new warnings were observed.

## 5) Dependency Duplication Audit

`cargo tree --duplicates` reports expected transitive duplication (notably
`getrandom` versions spanning runtime and dev/test stacks). No duplicate implies
a correctness issue in current scope.

## 6) Security Posture Review

- `materialize_snapshot` path and symlink containment guards remain intact.
- `GithubApiProvider` extraction now rejects writes through symlink ancestors and
  rejects duplicate file/symlink entry collisions.
- Redirect behavior for GitHub API tarball downloads is now covered by a mocked
  redirect test.

## 7) Remaining Risk Notes

- `github-api` still depends on live network/API conditions for end-to-end public
  repository behavior; mocked tests cover parser/status mapping/redirect/extract
  invariants, but not every live edge case.
- Transitive dependency duplicates remain acceptable for now; revisit only if
  binary size/startup profiling justifies consolidation work.
