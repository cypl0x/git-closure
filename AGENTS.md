# AGENTS.md - git-closure

## Project Overview

`git-closure` is a CLI tool to create deterministic, self-describing, verifiable snapshots of source code repositories. It produces `.gcl` files (S-expressions) that can be emailed, archived, versioned with git, or used for audits, reproducibility, and backup.

## Build / Run Commands

```bash
# Build
cargo build --release

# Run locally
cargo run -- --help

# Lint
cargo clippy

# Test
cargo test
```

### Running a Single Test

```bash
# Rust - run specific test
cargo test test_name

# Dart
dart test test/name_test.dart

# General - use --filter flag
cargo test --test integration_test
```

## Code Style Guidelines

### General Principles

- Write concise, readable code with minimal abstraction
- Prefer explicit over implicit
- Fail fast with clear error messages
- No comments unless explaining complex business logic

### Language-Specific Conventions

**If Rust:**
- Use `rustfmt` for formatting (default settings)
- Follow standard Rust naming: `snake_case` for functions/variables, `PascalCase` for types
- Use `Result<T, Error>` for error handling with `?` operator
- Prefer `anyhow` for application errors, `thiserror` for library errors
- Run `cargo clippy` before committing

**If Dart:**
- Use `dart format` for formatting
- Follow Dart style: `camelCase` for variables/functions, `PascalCase` for classes
- Use `try/catch` or `Result` type for error handling
- Enable strict typing: `dart analyze` should pass with no warnings

**If Shell:**
- Use `shellcheck` for linting
- Use `set -euo pipefail`
- Use `local` for all variables in functions
- Double-quote all variable expansions

### CLI Design

- Use a subcommand structure: `cr <command> [options]`
- Support both local paths and remote URLs as arguments
- Provide sensible defaults with `--output`, `--include-logs`, `--include-metadata` flags
- Show progress for long operations

### Output Format

The `.repo.txt` format should include:
1. Header comment with project metadata (name, git hash, author, dates, file count)
2. File tree structure
3. Each file preceded by a header comment with path and git metadata
4. File contents separated by headers

### Error Handling

- Exit with code 0 for success, 1 for errors
- Print errors to stderr
- Include helpful context in error messages (path, operation, reason)

### Testing

- Unit tests for core concatenation logic
- Integration tests for CLI commands
- Test both local paths and remote URLs
- Mock network calls in tests where possible

### File Organization (Example for Rust)

```
bin/git-closure.rs   # CLI entry point
src/
  commands/
    build.rs       # git-closure build subcommand
    watch.rs       # git-closure watch subcommand
    explode.rs     # git-closure explode subcommand
    verify.rs      # git-closure verify subcommand
    diff.rs        # git-closure diff subcommand
    query.rs       # git-closure query subcommand
    export.rs      # git-closure export subcommand
  lib.rs           # Library root
  concat.rs        # Core concatenation logic
  sexpr.rs         # S-expression serialization
  metadata.rs      # Git/file metadata extraction
  hash.rs          # SHA-256 hashing
  providers/
    github.rs      # GitHub API provider
    gitlab.rs      # GitLab API provider
    local.rs       # Local filesystem provider
tests/
  integration_test.rs
```

## Dependencies

- For remote repos: use GitHub CLI (`gh`) or raw HTTP requests
- Minimize external dependencies
- Use standard library where possible

## Git Conventions

- Use conventional commits: `feat:`, `fix:`, `refactor:`, `docs:`
- Keep commits atomic and small
- Run lint/test before committing
