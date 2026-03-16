# AGENTS.md - git-closure

This file orients coding agents to the current repository shape and extension
patterns. For sprint execution process and backlog discipline, also read
`AGENTS_SPRINT.md`.

## Project Overview

`git-closure` is a Rust CLI/library for deterministic `.gcl` source snapshots.
It supports building, verifying, diffing, formatting, rendering, and
materializing snapshots with strong integrity and path-safety guarantees.

## Current CLI Commands

- `build` (`b`)
- `materialize` (`m`)
- `verify` (`v`)
- `list` (`l`)
- `diff` (`d`)
- `fmt` (`f`)
- `render` (`r`)
- `completion` (`c`)

Deprecated/planned names like `explode`, `watch`, `query`, and `export` are not
part of the current CLI surface.

## Module Graph

Core dependency flow:

`error -> utils -> providers -> git/snapshot/* -> materialize -> lib`

Entry points:

- `src/main.rs` - clap CLI, output rendering, exit policy
- `src/lib.rs` - public API re-exports and integration tests

Core modules:

- `src/error.rs` - typed error taxonomy
- `src/utils.rs` - shared utility helpers
- `src/providers/mod.rs` - source parsing + provider dispatch/fetch
- `src/git.rs` - git-mode source selection and cleanliness checks
- `src/snapshot/build.rs` - filesystem scan + snapshot assembly
- `src/snapshot/hash.rs` - `snapshot-hash` and SHA-256 helpers
- `src/snapshot/serial.rs` - parse/serialize/list/fmt for `.gcl`
- `src/snapshot/diff.rs` - structural snapshot diff model/algorithm
- `src/snapshot/render.rs` - markdown/html/json report rendering
- `src/materialize.rs` - verify/materialize logic + path safety

## Testing Conventions

- Unit/integration tests live in module tests and `src/lib.rs` integration block.
- CLI contract tests use `trycmd` in `tests/cli.rs` and fixtures under
  `tests/cli/` (including `tests/cli/README/` for README examples).
- Golden fixtures in `tests/fixtures/` lock byte-level external format behavior.
- Property tests use `proptest` in existing test modules.
- Fuzz targets live in separate `fuzz/` crate (`cargo-fuzz`).

## Provider Extension Pattern

When adding or changing source support:

1. Extend `SourceSpec` parsing in `src/providers/mod.rs`.
2. Keep provider selection in `choose_provider` grammar/semantics-driven.
3. Add parse/dispatch tests before implementation changes.
4. Preserve explicit `--provider` semantics even when auto behavior differs.
5. Add CLI fixture coverage for user-visible behavior and errors.

## Format Extension Pattern

When changing `.gcl` semantics or serialization:

1. Update model/parser/serializer (`snapshot/mod.rs`, `snapshot/serial.rs`).
2. Preserve forward compatibility (unknown headers/plist keys).
3. Update `SPEC.md` and `README.md` in the same change.
4. Update golden fixtures under `tests/fixtures/` when bytes change.
5. Add/adjust `trycmd` fixtures for CLI-visible behavior changes.

## Build / Quality Commands

```bash
cargo test --locked
cargo clippy --locked -- -D warnings
cargo fmt --check
cargo build --release
```

## Git Conventions

- Use conventional commits (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`).
- Keep commits logically scoped.
- Keep quality gates green before committing.
