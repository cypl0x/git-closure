# AGENTS_SPRINT.md — git-closure Sprint Execution Protocol

**This file is the operational guide for any LLM-based coding agent executing the
`git-closure` post-sprint backlog. Read it completely before touching any source file.**

---

## 0. Who you are and what you are doing

You are a senior Rust engineer executing a well-scoped engineering backlog for
`git-closure`, a deterministic source-code snapshot tool. The backlog is in
`BACKLOG.md` (the normative worklist). Your job is to implement every
item in that backlog to its stated Definition of Done, in the correct order,
using test-driven development.

You are not a fast typist racing to a green test suite. You are a careful
engineer who researches before writing, reflects after testing, and only commits
when genuinely satisfied that the solution is correct, complete, and honest.

---

## 1. Non-negotiable process rules

These rules are not suggestions. Violating any of them is a sprint failure.

### 1.1 Research before every task

Before writing a single line of implementation code or a single test for any
task, you must:

1. **Read the relevant source files in full.** Use `bat`, `cat`, or editor tools.
   Never assume a function's behavior from its name or a partial reading.
2. **Read the relevant crate documentation.** Use `nix develop -c cargo doc --open`
   or fetch docs from `docs.rs` directly. If a crate's behavior on an edge case
   is unclear, read its source.
3. **Run small experiments to verify your understanding.** Write a small `main.rs`
   in a tempdir, or use `nix shell nixpkgs#...` to get a tool. Run it. Observe.
   If you are uncertain whether `lexpr` parses a particular S-expression form,
   write a 10-line test and run it before writing the real test.
4. **Read the relevant Rust reference or RFC if a language feature's semantics
   are in play.** This especially applies to: `Path::components()` behavior on
   edge cases, `#[non_exhaustive]` implications for pattern matching, `TempDir`
   drop semantics, `rename` atomicity guarantees on Linux vs macOS.
5. **For GitHub API and HTTP work specifically:** Read the official GitHub REST
   API documentation at `https://docs.github.com/en/rest/repos/contents` and the
   tarball archive endpoint docs before writing any HTTP code. Verify redirect
   behavior, authentication headers, and Content-Type expectations empirically.

**You must never assume and hope.** If you are about to write `to_be_bytes()`
because you think SHA-256 pre-image construction conventionally uses big-endian,
verify that by checking what the _existing codebase_ does, what the spec says,
and then reconcile. The endianness bug in RT-13 is a concrete example of what
happens when an assumption goes unverified.

### 1.2 Test-driven development, strictly

The sequence for every task is:

```
research → write failing tests → confirm they fail → implement → confirm they
pass → reflect → improve → confirm still passing → commit
```

**Write the tests first.** The tests are your specification in executable form.
If you find yourself writing implementation before tests, stop. Go back to
research.

A test is failing for the right reason when:

- its panic or assertion message names the specific semantic property being violated
- it would not pass trivially (a test that always passes is not a test)
- it fails for the precise reason the backlog describes, not a compile error

### 1.3 Reflect after every green suite

When all tests pass and `clippy` is clean, do not immediately commit. Pause and
ask yourself:

- **Coverage:** Are there edge cases the backlog mentioned that are not covered
  by a test? Are there edge cases that the backlog _didn't_ mention but that are
  real, given what I now know from research?
- **Specification completeness:** Does the implementation satisfy every
  sub-bullet in the Definition of Done? Not "does it work" but "does it satisfy
  the contract as written"?
- **Correctness of the tests themselves:** Could a broken implementation pass
  these tests? If so, the test is weak and needs strengthening.
- **Missed implications:** Does this change have downstream effects that require
  a follow-up test? (Example: adding `ModeChanged` to `DiffEntry` without
  `#[non_exhaustive]` is a SemVer break. Did I add `#[non_exhaustive]`? Did I
  test that it's there?)
- **Cross-artifact consistency:** If I changed behavior, is the relevant spec,
  README, AGENTS.md, or CLI help text still accurate?

If any of these questions reveals a gap, fix it before committing. "The
program compiles and tests pass" is not the bar. The bar is the Definition of
Done.

### 1.4 Commit hygiene

- One logical change per commit.
- Conventional commit format: `fix:`, `feat:`, `refactor:`, `test:`, `docs:`,
  `chore:`.
- Reference the task ID in the body: `Closes RT-13`, `Addresses TD-01`.
- If a task is large, split it into logically ordered commits (smallest isolated
  unit first, largest conceptual change last — the same pattern as the T-20
  modularization series in the previous sprint).
- Never bundle a correctness fix and a feature addition in one commit.
- Never commit if `cargo test`, `cargo clippy -- -D warnings`, or
  `cargo fmt --check` is failing.

### 1.5 Dependency ordering constraints

These ordering constraints are not negotiable — implementing them in the wrong
order forces rework:

**RT-01 (GitHub auto-dispatch regression) must ship as a two-line hot fix
_before_ RT-02 (SourceSpec enum).** Do not begin the SourceSpec refactor until
the regression is committed and tested. The regression is a broken advertised
workflow. It cannot wait for a multi-day architectural refactor.

**TD-01 (SnapshotHeader refactor) must be completed before RT-09 (unknown
header preservation in fmt).** RT-09's implementation _is_ TD-01's refactor. If
you start RT-09 without first restructuring the header model, you will end up
writing the refactor twice or producing an incomplete implementation.

**RT-09 and RT-08 touch the same code path (`split_header_body`,
`serialize_snapshot`).** Implement them in the same batch (not the same commit,
but the same sitting). Implementing RT-08 first and then RT-09 is fine;
implementing them in separate sittings invites conflicting partial states.

**RT-03 (ModeChanged to DiffEntry) must note the SemVer implication.** Before
adding the `ModeChanged` variant, mark `DiffEntry` as `#[non_exhaustive]` in
the same commit. This is a one-line addition that prevents a breaking change for
any downstream library consumers. The backlog does not explicitly state this
but it follows directly from RT-03's change to a public enum.

**TD-04 (CLI integration tests) should be set up before the Sprint B features
land.** If you establish the `trycmd` harness early, each new behavioral change
in Sprint B can be accompanied by a `.toml` transcript test immediately rather
than retrofitted.

### 1.6 Tool and crate guidance

The backlog mentions several crate options for CLI testing. The correct choice
for this project is **`trycmd`**, not `assert_cmd` or `snapbox` alone. Reasons:

- `trycmd` snapshot-tests complete command transcripts (stdin, stdout, stderr,
  exit code) from `.toml` or `.txt` fixture files.
- It integrates with `snapbox`'s update-on-change workflow: run with
  `TRYCMD=overwrite` to regenerate snapshots after intentional behavior changes.
- It is the correct tool for catching output drift, exit code regressions, and
  help-text drift simultaneously.
- It produces readable diffs in CI when tests fail.

For property-based tests, use **`proptest`** (not `quickcheck`). Add it to
`[dev-dependencies]`. Property tests belong in the existing test modules next
to unit tests.

For fuzz testing, use **`cargo-fuzz`** with `libfuzzer-sys`. This requires a
separate `fuzz/` crate at the workspace root with its own `Cargo.toml`. Do not
confuse property tests (deterministic, in `dev-dependencies`) with fuzz targets
(non-deterministic, in a separate fuzz crate). Bootstrap the fuzz crate with:

```bash
nix shell nixpkgs#cargo-fuzz -c cargo fuzz init
nix shell nixpkgs#cargo-fuzz -c cargo fuzz add fuzz_parse_snapshot
```

---

## 2. Per-task research checklist

For each task below, specific research is required before implementation. This
is not exhaustive — you may find additional relevant material during research.

### RT-00 (already complete if SPEC.md says big-endian) / RT-13 (endianness)

Before touching SPEC.md:

- Read `src/snapshot/hash.rs` in full. Confirm `to_be_bytes()` is used throughout.
- Read the reference pseudocode in `SPEC.md §6`. Confirm it says `to_le_bytes()`.
- Read `README.md` — does it describe the endianness? Which way?
- Read the test helpers in `src/lib.rs` that manually compute hashes
  (`manual_snapshot_hash_with_length_prefix`, `symlink_snapshot_hash`).
  Confirm they also use big-endian.
- Write a minimal verification: compute the SHA-256 hash of a one-file snapshot
  manually with big-endian length prefixes and compare to the binary output.
  This becomes the locked fixture for TD-07.
- Only then edit SPEC.md. The fix is one word and one code-block line.

### RT-01 (GitHub auto-dispatch regression)

Before writing the fix:

- Read `fetch_source` in `src/providers/mod.rs` completely.
- Trace the exact code path for `ProviderKind::Auto` when the source string is
  `gh:owner/repo`.
- Confirm `looks_like_github_source` returns true for `gh:`.
- Confirm `GithubApiProvider::fetch` returns `Err(...)`.
- Write the test that demonstrates the failure before writing the fix.
- The fix is two lines in the `Auto` branch: route GitHub-like sources to
  `GitCloneProvider` if `GithubApiProvider` is not yet implemented.
- Verify `--provider github-api` still hard-errors after the fix.

### RT-02 (SourceSpec enum)

Before designing the enum:

- Read all source strings mentioned in `README.md`. Catalogue them.
- Read `looks_like_github_source` and `looks_like_nix_flake_ref` and map every
  input class to the current output.
- Check the Nix flake reference grammar at
  `https://nix.dev/manual/nix/stable/command-ref/new-cli/nix3-flake#types`.
  The `github:` prefix is a Nix flake ref, not a GitHub shorthand — this is
  intentional in the current codebase.
- Design the `SourceSpec` enum variants to cover every documented example.
- Write parse tests for each variant before writing the parser.
- The `choose_provider` function signature should take both a `&SourceSpec` and
  a `ProviderKind` so explicit `--provider` overrides remain possible.

### RT-03 (ModeChanged in DiffEntry)

Before writing the variant:

- Read `src/snapshot/diff.rs` completely. Understand `compute_diff`'s current
  `content_key` function.
- Understand what `content_key` returns for a file with mode `644` vs `755` but
  identical SHA-256. Confirm it returns the same key (the sha256 string) and
  therefore treats them as identical.
- Design the secondary pass: after the existing logic identifies unchanged files
  (same content_key), run a mode comparison pass over that set.
- Consider the interaction: can a file be simultaneously Renamed and
  ModeChanged? If the rename detection happens first (matching by content_key),
  a rename+mode-change would only report as Renamed. Is that the right
  semantics? Look at how `git diff` handles this. Decide explicitly and document
  in a comment.
- Before adding `ModeChanged` to `DiffEntry`, add `#[non_exhaustive]` to the
  enum in the same commit.
- Write tests for: `644→755`, `755→644`, identical modes, rename+mode-change,
  symlink type change (is that a ModeChanged or a Modified?).

### RT-08 + RT-09 + TD-01 (fmt safety and header preservation)

This is a coupled batch. Research all three before writing any of them.

- Read `split_header_body` in `src/snapshot/serial.rs` completely. Map exactly
  which header fields are currently captured and which are silently discarded.
- Read `serialize_snapshot`. Map exactly which fields are emitted.
- Understand the current `SnapshotHeader` struct in `src/snapshot/mod.rs`.
- Design the new header representation before writing any code. The correct
  design is:
  - Keep typed fields for `snapshot_hash`, `file_count`, `git_rev`,
    `git_branch` (these have semantic meaning to the tool).
  - Add `pub(crate) extra_headers: Vec<(String, String)>` (or `IndexMap` if
    ordering needs to be guaranteed after lookup — probably `Vec` is sufficient
    since we only need to preserve and re-emit, not look up by key).
  - `split_header_body` populates `extra_headers` with every `;;`-prefixed line
    whose key is not a known field.
  - `serialize_snapshot` emits known fields first (in canonical order), then
    `extra_headers` in the order they were found.
- For RT-08 (hash validation): after parsing, check if the stored
  `snapshot_hash` matches the recomputed hash _before_ any write operation.
  If they differ, return a `GitClosureError::HashMismatch` and refuse to write.
  The `--repair-hash` flag in `main.rs` can opt out of this check.
  Consider: should `fmt --check` _also_ report a hash mismatch as a distinct
  exit condition? The backlog says yes (exit-code distinction between noncanonical
  formatting and corrupted hash). Design this before implementing.

### TD-03 (GithubApiProvider real implementation)

This task requires the most upfront research. Do not write a single line of
HTTP code until you have answers to all of these.

Read the following documentation before implementation:

- GitHub REST API: `GET /repos/{owner}/{repo}/tarball/{ref}`
  at `https://docs.github.com/en/rest/repos/contents#download-a-repository-archive-tar`
- Understand the 302 redirect behavior: the endpoint returns a redirect to a
  time-limited S3 URL. Your HTTP client must follow it.
- Understand authentication: `Authorization: Bearer <token>` header.
  The token comes from `GCL_GITHUB_TOKEN` env var. Unauthenticated requests
  are rate-limited to 60/hour per IP for public repos.
- Understand tarball structure: GitHub archive tarballs have a single top-level
  directory named `{owner}-{repo}-{sha}/`. Your extraction must strip this
  prefix before passing the path to the snapshot builder.
- Understand rate limit response codes: 403 with `X-RateLimit-Remaining: 0`
  vs 401 for auth failure. Your error messages should distinguish them.

For the HTTP client: use **`ureq`** (synchronous, no Tokio, <200KB). Add it to
`[dependencies]` with `features = ["tls"]`. Do not use `reqwest` — it pulls in
Tokio which is heavyweight for a CLI tool.

For tarball extraction: use `flate2` + `tar`. Both are already common in the
Rust ecosystem and their docs are at `docs.rs`. Verify that the `tar` crate's
`Archive::entries()` correctly handles the top-level directory stripping pattern
before writing the main code.

Security: the tarball extraction must reject path traversal entries (entries
whose normalized path would escape the tempdir). The `tar` crate may or may not
do this automatically — verify explicitly with a crafted tarball in a test.

### TD-04 (CLI integration tests with trycmd)

Before writing any fixtures:

- Add `trycmd` to `[dev-dependencies]`.
- Create `tests/cli/` directory.
- Write a minimal `tests/cli.rs` that runs `trycmd::TestCases::new().case("tests/cli/*.toml")`.
- Understand the `.toml` fixture format: `cmd`, `stdin`, `stdout`, `stderr`,
  `status` (exit code). Read `trycmd`'s README at
  `https://docs.rs/trycmd/latest/trycmd/`.
- Build a `tests/cli/fixtures/` directory with small `.gcl` files needed by
  the tests. These can be generated via the binary itself during test setup, or
  committed directly.
- The first fixture tests to write: exit code for `diff` with no changes,
  exit code for `diff` with changes, `fmt --check` on a canonical file,
  `fmt --check` on a non-canonical file, `build` auto-output disclosure notice.

### TD-05 (property tests + fuzz)

For `proptest`:

- Read `https://docs.rs/proptest/latest/proptest/` before writing any strategy.
- The highest-value property to test is idempotence of canonicalization:
  `parse(serialize(build(files))) == files` for arbitrary valid file lists.
  Design the proptest strategy for `SnapshotFile` generation: random valid
  UTF-8 paths (no `..`, no leading `/`), random ASCII mode strings, random
  contents. Use `proptest::string::string_regex` to generate valid paths.
- The second highest-value property: `fmt(fmt(x)) == fmt(x)` for any valid
  snapshot string.

For `cargo-fuzz`:

- Bootstrap the fuzz crate as described in §1.6.
- Write `fuzz_parse_snapshot`: takes arbitrary bytes, calls `parse_snapshot`,
  asserts no panic (error is ok, panic is not).
- Write `fuzz_sanitized_relative_path`: takes arbitrary bytes as a path string,
  calls `sanitized_relative_path`, asserts no panic and that Ok results never
  contain `..` components.
- Write `fuzz_lexical_normalize`: arbitrary path strings, no panics.

### DOC-01 (README synchronization)

Before editing README:

- Run `git-closure --help` and compare every subcommand listed against the CLI
  source in `main.rs`.
- Run each documented example in a tempdir and verify it produces the described
  output. Any example that fails is either wrong or stale.
- Remove `query` and `watch` entirely from the CLI surface section.
- Update the provider table: `github-api` now hard-errors when requested
  explicitly; in auto mode it falls back to git-clone (after RT-01 is fixed).
- Update the roadmap to reflect: `diff`, `fmt`, `list`, `render` now exist.
- Add a brief "Exit codes" section (values from RT-10).

### DOC-02 (AGENTS.md synchronization)

> **Partially addressed** (NAR export sprint, 2026-03-21): `export` now exists as
> a real subcommand (`e`); it was added to `AGENTS.md`'s CLI list and the deprecated
> sentence was corrected.  `src/nar.rs` and `src/snapshot/export.rs` were added to
> the module graph.  Remaining: `explode`, `watch`, `query` still appear nowhere in
> the codebase — only remove them if they still appear in `AGENTS.md` after re-reading.

`AGENTS.md` is what future coding agents will read for project orientation.
Before updating it:

- Read it fully.
- Read the current `src/` directory structure.
- Update the module graph, command list, and extension points to reflect reality.
- Do not delete the "project overview" framing — that still applies.

---

## 3. End-of-sprint final review

After implementing all backlog items and reaching every Definition of Done:

**Do not immediately treat the sprint as complete.**

Run the following checks:

1. **Cross-artifact consistency sweep.** For every user-visible change made in
   this sprint, verify that `README.md`, `SPEC.md`, `AGENTS.md`, and CLI `--help`
   output all describe the same behavior. Use `git diff` against the pre-sprint
   state and read every changed prose section.

2. **Exit-code taxonomy audit.** Run `git-closure <subcommand>` for every
   subcommand with inputs designed to trigger: success, semantic-negative result,
   parse failure, IO failure, provider failure. Record the exit codes. Verify
   they match the documented taxonomy.

3. **Golden fixture verification.** Run the binary against the committed fixture
   tree (from TD-07). Verify byte-identical output on both Linux and macOS (or
   note explicitly if macOS is not available in the current environment).

4. **Dead code scan.** Run `cargo check 2>&1 | grep "unused\|dead_code"`. If
   anything appears that wasn't there before the sprint, investigate.

5. **Dependency audit.** Run `cargo tree --duplicates`. If new duplicates
   appeared (especially for `serde` or TLS stacks), evaluate whether they are
   acceptable.

6. **Security posture review.** Re-read `materialize_snapshot` and the
   `GithubApiProvider` tarball extraction. Does path traversal remain impossible?
   Does the empty-output-dir precondition still hold? Does the HTTP client
   follow redirects safely?

Then write the final review. The review must be:

- Honest. If a Definition of Done was not fully met, say so.
- Differentiated. Distinguish strong decisions from weak ones.
- Specific. Name files, functions, and line numbers when describing issues.
- Non-trivial. Do not list "tests pass, clippy clean" as findings. Those are
  the floor, not the ceiling.
- Architecturally aware. Describe the module dependency graph, the invariant
  model, and any remaining technical debt that did not fit in this sprint.
- Actionable. Every concern raised should have a recommended follow-up action
  with enough precision that it can become a backlog entry.

If the review reveals regressions, fix them before closing the sprint.
If the review reveals Definition of Done gaps, fix them before closing the sprint.
If the review reveals improvement opportunities that belong in a future sprint,
add them to the backlog as new entries with proper priority and DoD sections.

---

## 4. What you must not do

- **Never assume crate behavior. Always verify.**
- **Never write tests that trivially pass.** A test that passes without the
  feature implemented is not a test.
- **Never commit a failing test suite.** Not even with "will fix later."
- **Never silently change the format.** Any change to how `.gcl` files are
  serialized or how `snapshot-hash` is computed is a format change and must be
  treated as such, documented explicitly, and reflected in both SPEC.md and
  the golden fixtures.
- **Never let a README example survive that describes nonexistent behavior.**
  If you add a feature, update the README. If you change behavior, update the
  README. The README is a contract, not marketing copy.
- **Never bundle unrelated changes.** A commit that fixes RT-13 and RT-01
  simultaneously is not a commit — it's noise in `git log`.
- **Never conflate "locally plausible" with "specification-conformant."** Green
  tests written against your implementation do not prove the implementation
  satisfies the backlog's stated contract. Read the DoD. Check each point
  explicitly.

---

## 5. Environment

You are working in a Nix flake devshell. Use:

```bash
nix develop -c cargo test
nix develop -c cargo clippy -- -D warnings
nix develop -c cargo fmt --check
nix develop -c cargo build --release
nix develop -c cargo doc --no-deps
```

For tools not in the devshell:

```bash
nix shell nixpkgs#cargo-fuzz -c cargo fuzz ...
nix shell nixpkgs#hyperfine -c hyperfine ...
```

---

## 6. The bar

Any stronger solution than what the backlog proposes is welcome, provided:

- advertised workflows are not broken by default,
- canonical formatting does not silently launder corruption,
- forward-compatible metadata is not destroyed by benign tooling,
- source classification and provider dispatch are explicit and testable,
- CLI exit codes and examples are contractual, not accidental,
- documentation never promises behavior the binary does not actually provide,
- the `snapshot-hash` algorithm is byte-for-byte consistent between spec,
  README, implementation, and all test helpers.

That is the bar. Not green CI. That.
