# git-closure

Deterministic, self-describing, verifiable source-tree snapshots.

`git-closure` builds `.gcl` files (S-expressions) that can be checked into git,
emailed, archived, diffed, and materialized back into a filesystem tree.

## Current CLI Surface (v0.1)

`git-closure` currently ships these subcommands:

- `build` (`b`) - build a snapshot from a local or remote source
- `materialize` (`m`) - restore a snapshot into a directory
- `verify` (`v`) - verify structural and per-file integrity
- `list` (`l`) - list recorded entries
- `diff` (`d`) - compare snapshots or snapshot-vs-directory (`text`, `--json`, `--stat`)
- `fmt` (`f`) - canonicalize snapshot formatting
- `render` (`r`) - render Markdown, HTML, or JSON reports
- `summary` (`s`) - print compact snapshot metadata (`text` or `--json`)
- `completion` (`c`) - generate shell completions (bash/zsh)

## Quick Start

```bash
# Build
cargo build --release

# Run
cargo run -- --help

# Quality gates
cargo test --locked
cargo clippy --locked -- -D warnings
cargo fmt --check
```

## Core Examples

These examples are machine-validated by `trycmd` fixtures under
`tests/cli/README/`.

```bash
# Snapshot a local directory
git-closure build repo -o repo.gcl

# Verify integrity
git-closure verify repo.gcl

# Count-only diff summary (exit 1 when differences exist)
git-closure diff empty.gcl repo.gcl --stat

# Snapshot vs working tree diff
git-closure diff repo.gcl ./repo

# Snapshot summary
git-closure summary repo.gcl --json
```

## Sources and Providers

`build` accepts local paths and remote source syntaxes. In auto mode, source
classification is grammar-driven:

- Local existing path -> `local`
- Nix flake references (`nix:`, `github:`, `gitlab:`, `path:`, `tarball+`, `file+`, `git+`, `sourcehut:`) -> `nix`
- GitHub shorthand/HTTPS repo (`gh:owner/repo[@ref]`, `https://github.com/owner/repo[@ref]`) -> `github-api`
- GitLab shorthand and other remotes -> `git-clone`

Provider behavior:

- `local`: snapshots a local directory directly
- `git-clone`: shallow clone (`--depth 1 --no-tags`) then snapshot checkout
- `nix`: `nix flake metadata <ref> --json`, then snapshot returned store path
- `github-api`: download GitHub tarball, strip top-level archive directory, snapshot extracted tree

GitHub authentication/rate limits:

- Set `GCL_GITHUB_TOKEN` for private repositories or higher API limits.
- GitHub tarball downloads are capped at 512 MiB by default; override with `GCL_GITHUB_TARBALL_MAX_BYTES`.
- This network tarball cap is separate from parse-time snapshot limits (`ParseLimits` in the library API).

Important syntax distinction:

- `gh:owner/repo` = GitHub shorthand (auto -> `github-api`)
- `github:owner/repo` = Nix flake ref (auto -> `nix`)

You can force a provider explicitly:

```bash
git-closure build gh:owner/repo@main -o repo.gcl --provider github-api
git-closure build gh:owner/repo@main -o repo.gcl --provider git-clone
git-closure build github:NixOS/nixpkgs -o nixpkgs.gcl --provider nix
```

## Output Naming and Build Notice

When `--output` is omitted, `build` derives a filename:

- `gh:owner/repo@main` -> `repo@main.gcl`
- `gl:group/project@v1.2` -> `project@v1.2.gcl`
- `.` -> `<current-directory-name>.gcl`
- fallback -> `snapshot.gcl`

When auto-deriving output, `build` emits exactly one stderr note:

`note: writing snapshot to <path>`

No note is emitted when `--output` is explicitly provided.

## File Selection Semantics

In a git repository, `build` follows git-tracked semantics by default:

- includes tracked files
- excludes untracked files unless `--include-untracked`
- still excludes ignored files when `--include-untracked` is set
- with `--require-clean`, fails if selected source scope has uncommitted changes

## Format and Integrity

`.gcl` snapshots contain:

- `;; snapshot-hash` (structural hash)
- `;; file-count`
- optional `;; git-rev`, `;; git-branch`
- optional provenance headers `;; source-uri`, `;; source-provider` for remote builds
- S-expression entries for files/symlinks

`snapshot-hash` uses SHA-256 over length-prefixed tuples with `u64` big-endian
length prefixes.

Canonical git revision field name is `git-rev` (not `git-commit`). The value is
captured from `git rev-parse HEAD` and is treated as informational metadata.

## materialize Safety Model

`materialize` has explicit safety constraints:

- output directory must be empty (or newly created)
- paths must be safe relative paths (no absolute paths, no `..` traversal)
- symlink targets are containment-checked lexically and cannot escape output root

These constraints prevent pre-planted symlink and path-traversal attacks during
reconstruction.

Library consumers can opt into alternate materialization profiles via
`MaterializeOptions`:

- `Strict` (default): requires empty output directory
- `TrustedNonempty`: allows overlay into non-empty output directories
- `NoSymlink`: rejects snapshots containing symlink entries

## diff Output Modes

- default text: path-level changes with identity detail
- `--json`: structured entries (`added`, `removed`, `modified`, `renamed`, `mode_changed`)
- `--stat`: deterministic counts only

The right-hand diff input is auto-detected:

- existing directory path -> compare snapshot against live source tree
- otherwise -> compare snapshot file vs snapshot file

`diff` exit behavior:

- exit `0` when identical
- exit `1` when differences exist

## fmt Behavior

- `git-closure fmt <file>` canonicalizes formatting
- by default, `fmt` rejects parseable files whose stored `snapshot-hash` does
  not match recomputed structure
- `--repair-hash` explicitly opts into hash repair/re-canonicalization

`fmt --check` exit behavior is split by condition (see Exit Codes).

## render Formats

```bash
git-closure render repo.gcl --format markdown
git-closure render repo.gcl --format html
git-closure render repo.gcl --format json -o report.json
```

- formats: `markdown`, `html`, `json`
- default output: stdout
- optional file output: `-o/--output`

## summary Output

```bash
git-closure summary repo.gcl
git-closure summary repo.gcl --json
```

Summary includes snapshot hash, entry counts, total bytes, git metadata, and
top-5 largest regular files.

Symlink rendering policy:

- Markdown/HTML: entry type shown as symlink, digest column shows `-> <target>`
- JSON: `type=symlink`, `mode="120000"`, `size=0`, `sha256=""`, and
  `symlink_target` populated

## Exit Codes

- `0` - success / no semantic differences
- `1` - semantic negative result (`diff` differences, `fmt --check` noncanonical-valid)
- `2` - integrity mismatch (`fmt --check` hash mismatch)
- `3` - parse failure (`fmt --check` parse error)
- `4` - operational/runtime failure (I/O, provider/subprocess, usage/runtime errors)

## CI Contract

CI validates both library and CLI contract behavior:

- `cargo test --locked` on `ubuntu-latest` and `macos-latest`
- both MSRV (`1.85`) and stable toolchains
- `cargo clippy --locked -- -D warnings`
- `cargo fmt --check`
- `cargo build --locked --release`
- tag-triggered `Release` workflow for `v*`

## Golden Fixtures

Byte-level format stability is locked by committed fixtures:

- `tests/fixtures/simple.gcl` (canonical snapshot bytes)
- `tests/fixtures/simple.render.json` (render JSON surface)

Intentional format changes must update fixtures in the same commit with an
explicit rationale.

## Shell Completions

Generate completion scripts:

```bash
git-closure completion bash
git-closure completion zsh
```

`clap_complete` is intentionally included unconditionally in v0.1 so completion
generation is always available in distributed binaries.

## Fuzzing

Fuzz targets are in `fuzz/`:

```bash
nix shell nixpkgs#cargo-fuzz -c cargo fuzz run fuzz_parse_snapshot
nix shell nixpkgs#cargo-fuzz -c cargo fuzz run fuzz_sanitized_relative_path
nix shell nixpkgs#cargo-fuzz -c cargo fuzz run fuzz_lexical_normalize
```

## Roadmap

- v0.1 (current): build/materialize/verify/list/diff/fmt/render/summary/completion
- post-v0.1 [planned]: richer remote providers, additional render/export targets,
  and further performance/security hardening

Commands like `query` and `watch` are not part of the current shipped CLI.

## License

MIT
