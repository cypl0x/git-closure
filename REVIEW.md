# git-closure Deep Technical Review

- Date: 2026-03-20
- HEAD: `e6677b6`
- `nix develop -c cargo test --locked`: 151 tests passed (unit/integration/cli), 0 failed
- `nix develop -c cargo clippy --locked -- -D warnings`: 0 warnings

## 1. Codebase Orientation

The codebase has a clear core pipeline and a mostly acyclic module graph. The effective runtime path for most commands is:

`main.rs` (CLI parse/dispatch) -> `lib.rs` re-exported APIs -> `snapshot/build.rs` / `snapshot/serial.rs` / `materialize.rs` / `snapshot/diff.rs` / `snapshot/render.rs`.

Load-bearing correctness modules:

- `src/snapshot/serial.rs` (format parser/serializer): this is the protocol boundary and the single most correctness-critical parser.
- `src/snapshot/hash.rs` + hash call sites in build/verify/materialize: defines the deterministic identity model.
- `src/materialize.rs`: responsible for integrity enforcement and filesystem safety.
- `src/providers/mod.rs` (especially tarball extraction): network-adjacent fetch/extract logic and path traversal hardening.
- `src/snapshot/build.rs`: collection semantics, mode/content detection, path normalization.

Incidental/non-load-bearing (important but lower correctness blast radius):

- `src/main.rs` printing and exit-code plumbing.
- `src/snapshot/render.rs` report formatting and escaping.
- `src/utils.rs` error-message quality helpers.

Estimated complexity hotspots:

- `parse_files_value` in `src/snapshot/serial.rs`: many format branches and deferred invariant checks.
- `extract_github_tarball` in `src/providers/mod.rs`: archive path/symlink handling with security constraints.
- `materialize_snapshot` in `src/materialize.rs`: combines integrity checks, path checks, symlink safety, and writes.
- `compute_diff` in `src/snapshot/diff.rs`: rename heuristics and deterministic ordering logic.

## 2. Bug Report

### BR-01: Duplicate snapshot paths are accepted and can silently overwrite output

Severity: `wrong-output` (can manifest as effective data loss at materialization time)

Reproduction path:

1. Construct a `.gcl` with two entries with the same `:path` and different content.
2. `verify_snapshot` succeeds if each entry is internally consistent.
3. `materialize_snapshot` writes both entries in order; later entry overwrites earlier entry at same destination.

Current pattern:

[`src/snapshot/serial.rs`, `parse_files_value`]

```rust
files.sort_by(|a, b| a.path.cmp(&b.path));
Ok(files)
```

Fix sketch:

- In `parse_files_value`, after sorting, reject duplicate `path` values with `GitClosureError::Parse`.
- Add regression tests for duplicate regular paths and duplicate regular/symlink mixed paths.

### BR-02: Symlink modifications are reported as `Modified` with empty hashes

Severity: `wrong-output`

Reproduction path:

1. Create two snapshots where `link` points to different targets.
2. Run `diff`.
3. Output includes `Modified` with empty `old_sha256`/`new_sha256`, giving no meaningful change payload.

Current pattern:

[`src/snapshot/diff.rs`, `compute_diff`]

```rust
modified.push(DiffEntry::Modified {
    path: lf.path.clone(),
    old_sha256: lf.sha256.clone(),
    new_sha256: rf.sha256.clone(),
});
```

Fix sketch:

- Add a dedicated `DiffEntry::SymlinkTargetChanged { path, old_target, new_target }`.
- Update CLI/JSON rendering and tests to represent symlink diffs explicitly.
- Keep existing `Modified` semantics for regular files only.

### BR-03: `materialize.rs` is Unix-only in implementation, causing non-Unix build failure

Severity: `crash` (build-time failure on non-Unix targets)

Reproduction path:

1. Build on non-Unix target.
2. `materialize_snapshot` uses Unix-only APIs unguarded in function body.

Current pattern:

[`src/materialize.rs`, `materialize_snapshot`]

```rust
if let Some(target) = &file.symlink_target {
    // ...
    symlink(target_path, &destination)?;
}
// ...
let permissions = fs::Permissions::from_mode(mode);
```

Fix sketch:

- Gate symlink creation and permission setting with `#[cfg(unix)]`/`#[cfg(not(unix))]` branches.
- On non-Unix: either return explicit `Parse`/`Unsupported` errors or provide platform-specific equivalents.
- Add target-specific tests or compile checks in CI matrix.

## 3. Latent Risk Register

### LR-01: Symlink target safety is enforced in materialize, but not in verify

Current pattern:

[`src/materialize.rs`, `verify_snapshot`]

```rust
if file.symlink_target.is_some() {
    continue;
}
```

What would have to change to become a bug:

- If external tooling treats `verify_snapshot` as a full safety gate before allowing use, malicious symlink targets can pass verify and fail only later in materialize (or in downstream consumers).

### LR-02: Tarball extraction allows arbitrary symlink targets inside destination

Current pattern:

[`src/providers/mod.rs`, `extract_github_tarball`]

```rust
if entry_type.is_symlink() {
    // ...
    std::os::unix::fs::symlink(&target, &output_path)?;
}
```

What would have to change to become a bug:

- If later code starts following symlinks during scan/materialize, or another consumer reuses extracted trees with symlink-following operations, this can become an escape/write primitive.

### LR-03: Unbounded decode/allocation on untrusted snapshot contents

Current pattern:

[`src/snapshot/serial.rs`, `parse_files_value`]

```rust
Some("base64") => BASE64_STANDARD.decode(content_field).map_err(|err| {
    GitClosureError::Parse(format!("invalid base64 content for {path}: {err}"))
})?,
```

What would have to change to become a bug:

- Larger untrusted inputs (CI artifact ingestion, service deployment) can trigger high memory usage or OOM before policy checks.

### LR-04: Parser accepts contradictory symlink plist metadata

Current pattern:

[`src/snapshot/serial.rs`, `parse_files_value`]

```rust
if entry_type.as_deref() == Some("symlink") {
    let target = target.ok_or_else(|| GitClosureError::Parse("missing :target for symlink".to_string()))?;
    files.push(SnapshotFile { path, sha256: String::new(), mode: "120000".to_string(), ... });
    continue;
}
```

What would have to change to become a bug:

- If future metadata fields gain semantics, silently discarding conflicting symlink fields (`:sha256`, `:size`, `:encoding`) can hide malformed inputs and break round-trip expectations.

## 4. Test Coverage Gaps

Baseline command requested:

- `nix develop -c cargo test --locked 2>&1 | tail -5` output ends with doc-test summary (`0 passed`), so it does not reflect full suite size.
- Full run (`nix develop -c cargo test --locked`) reports 151 tests passed.

Per-module pub/pub(crate) function gaps (no direct test invocation):

### `src/git.rs`

- `GitRepoContext::discover` (used indirectly via build path; no unit test directly exercising discovery edge cases).
- `tracked_paths_from_index` and `untracked_paths_from_status` (only indirectly through `build_snapshot_with_options`).

### `src/snapshot/build.rs`

- `build_snapshot_from_source` (not directly invoked by Rust tests; mostly covered via CLI/provider integration).

### `src/materialize.rs`

- `sanitized_relative_path` (covered indirectly via verify/materialize and fuzz shim, but no direct unit test table).

### `src/providers/mod.rs`

- `FetchedSource::temporary` (constructed internally; no direct assertion-focused unit test).

### `src/lib.rs`

- `fuzz_parse_snapshot`, `fuzz_sanitized_relative_path`, `fuzz_lexical_normalize` (intended for fuzz targets, not standard test suite).

Coverage gaps intersecting bug/risk items:

- BR-01/LR-04: no direct tests rejecting duplicate or contradictory symlink plist metadata in `parse_files_value`.
- LR-01: no direct test asserting verify behavior for unsafe symlink targets (current behavior is permissive).

## 5. Performance & Allocation Review

Targeted hot paths reviewed: `serialize_snapshot`, `compute_snapshot_hash`, `parse_files_value`, `collect_files_from_git_repo`.

### `serialize_snapshot` (`src/snapshot/serial.rs`)

Clone found:

```rust
String::from_utf8(file.content.clone())
    .expect("non-base64 file content must be valid UTF-8 (invariant violated)")
```

- Avoidable: yes. Use `std::str::from_utf8(&file.content)` then `.to_owned()` only when needed by `quote_string`.
- 10k-file impact: for large UTF-8 files this adds full-buffer duplication per file during serialization, increasing peak RSS and allocator churn significantly.

### `compute_snapshot_hash` (`src/snapshot/hash.rs`)

- No `.clone()` in hash loop.
- Hot-path profile is dominated by hasher updates and string-byte traversal; clone pressure is minimal.

### `parse_files_value` (`src/snapshot/serial.rs`)

- Uses `.to_string()` for parsed fields (`path`, `mode`, `sha256`, etc.).
- Mostly unavoidable because `SnapshotFile` owns strings.
- 10k-file impact: linear string allocations; acceptable but sensitive to very long path/target/value strings.

### `collect_files_from_git_repo` (`src/snapshot/build.rs`)

- No obvious `.clone()` in per-file hot loop.
- Main cost is disk IO (`symlink_metadata`, `read`) and content hashing; allocation overhead is from collected file content, not cloning metadata strings.

## 6. Security Audit

### Surface A: `materialize_snapshot` (`src/materialize.rs`)

What is protected:

- Snapshot integrity is checked before writes (`compute_snapshot_hash` vs header).
- Output directory must be empty (strong mitigation against pre-planted symlink path attacks).
- Relative path sanitization rejects absolute/`.`/`..` components.
- Symlink effective target is lexically normalized and constrained under output root.
- Regular-file content digest is verified before write.

Evidence excerpt:

```rust
let is_empty = output_abs.read_dir()?.next().is_none();
if !is_empty { return Err(GitClosureError::Parse(...)); }
// ...
let normalized_target = lexical_normalize(&effective_target)?;
if !normalized_target.starts_with(&output_abs) { return Err(...); }
```

What is not protected:

- No explicit non-Unix fallback path (portability/security parity issue).
- Verify phase does not enforce symlink-target policy.
- No explicit anti-TOCTOU file descriptor strategy (`O_NOFOLLOW` style), relying on empty-dir invariant.

Attack vector:

- A crafted snapshot with unsafe symlink target is rejected in materialize, but may pass verify and potentially mislead pipelines that treat verify as the sole guard.

### Surface B: `extract_github_tarball` (`src/providers/mod.rs`)

What is protected:

- Archive entries must remain under a single top-level directory.
- Path components are restricted to `Component::Normal` after stripping prefix.
- Writes through symlink ancestors are blocked via `ensure_no_symlink_ancestors` + `reject_if_symlink`.
- Duplicate file/symlink paths are rejected.
- Unsupported entry types are rejected.

Evidence excerpt:

```rust
let relative = strip_github_archive_prefix(entry_path.as_ref(), &mut top_level)?;
if let Some(parent) = output_path.parent() {
    ensure_no_symlink_ancestors(destination, parent)?;
}
```

What is not protected:

- Symlink target strings themselves are not constrained (can be absolute or traversal).
- No entry-count/size budget limits; large archives can cause resource pressure.

Attack vector:

- Malicious but valid archive can plant dangerous symlink targets; currently safe for extraction phase due to ancestor checks, but risky for any future symlink-following consumer.

### Surface C: `parse_files_value` (`src/snapshot/serial.rs`)

What is protected:

- Structural checks: body list shape, plist even key/value count, required keys for regular and symlink entries.
- Type checks for all known fields.
- Content decoding restricted to `base64` or raw UTF-8 string field.
- `size` must match decoded content length.

Evidence excerpt:

```rust
if plist.len() % 2 != 0 {
    return Err(GitClosureError::Parse("plist key/value pairs are malformed".to_string()));
}
// ...
if content.len() as u64 != size { return Err(GitClosureError::SizeMismatch { ... }); }
```

What is not protected:

- Duplicate `:path` entries are allowed.
- No format validation for `:sha256` shape at parse time.
- Symlink entries can include contradictory ignored fields.
- No hard limits for decoded payload sizes.

Attack vector:

- Crafted large or contradictory snapshots can pass parse and create ambiguous semantics or resource exhaustion.

## 7. API & Ergonomics

Public API surface in `src/lib.rs` is cohesive and practical for core workflows (`build`, `verify`, `materialize`, `list`, `diff`, `fmt`, `render`). Main ergonomics issues:

- Missing item-level rustdoc on several re-exported types/functions (module has top-level docs, but generated docs will still feel sparse for individual APIs).
- Naming consistency is mostly good; the one notable asymmetry is `build_snapshot_from_source` vs `build_snapshot_from_provider` where provider selection model is not mirrored by a convenience function returning inferred output path.
- Missing convenience functions likely expected by library consumers:
  - `parse_snapshot_bytes` / `verify_snapshot_text` (avoid forced filesystem round-trip).
  - `materialize_snapshot_with_policy` (toggle empty-dir requirement for trusted contexts).
  - `diff_snapshot_texts` for in-memory usage in CI tooling.

## 8. Prioritized Improvement Backlog

### IM-01: Reject duplicate file paths during parse
Priority: R0
Category: Bug
Effort: S

Current pattern:
[`src/snapshot/serial.rs`, `parse_files_value`]

```rust
files.sort_by(|a, b| a.path.cmp(&b.path));
Ok(files)
```

Proposed change:

- After sort, scan adjacent pairs; if equal path, return `GitClosureError::Parse` with conflicting path.

Rationale:

- Prevents ambiguous materialization semantics and eliminates overwrite-by-duplicate behavior.

### IM-02: Add explicit symlink diff variant
Priority: R1
Category: DX
Effort: M

Current pattern:
[`src/snapshot/diff.rs`, `compute_diff`]

```rust
DiffEntry::Modified { old_sha256: lf.sha256.clone(), new_sha256: rf.sha256.clone(), ... }
```

Proposed change:

- Introduce `DiffEntry::SymlinkTargetChanged` and update CLI/JSON output and tests.

Rationale:

- Removes misleading empty-hash modifications and makes symlink changes auditable.

### IM-03: Make materialization cross-platform explicit
Priority: R1
Category: Security
Effort: M

Current pattern:
[`src/materialize.rs`, `materialize_snapshot`]

```rust
symlink(target_path, &destination)?;
let permissions = fs::Permissions::from_mode(mode);
```

Proposed change:

- Add platform-gated branches with explicit unsupported errors or platform-native behavior.

Rationale:

- Avoids non-Unix build breakage and clarifies security behavior per target OS.

### IM-04: Enforce symlink policy in verify path
Priority: R1
Category: Security
Effort: S

Current pattern:
[`src/materialize.rs`, `verify_snapshot`]

```rust
if file.symlink_target.is_some() {
    continue;
}
```

Proposed change:

- Option A: add strict verify mode that applies symlink-target lexical checks against a hypothetical output root.
- Option B: document and surface `VerifyReport` flag indicating unchecked symlink target safety.

Rationale:

- Reduces policy mismatch between verify and materialize in automated pipelines.

### IM-05: Add parse-time resource limits for untrusted snapshots
Priority: R2
Category: Security
Effort: M

Current pattern:
[`src/snapshot/serial.rs`, `parse_files_value`]

```rust
BASE64_STANDARD.decode(content_field)
```

Proposed change:

- Introduce optional size/entry limits (e.g., max entry count, max decoded bytes per entry, total bytes cap) via parse options.

Rationale:

- Mitigates memory exhaustion from malicious or accidental giant snapshots.

### IM-06: Avoid UTF-8 content clone during serialization
Priority: R2
Category: Performance
Effort: S

Current pattern:
[`src/snapshot/serial.rs`, `serialize_snapshot`]

```rust
String::from_utf8(file.content.clone())
```

Proposed change:

- Replace with `std::str::from_utf8(&file.content)` and only allocate once for escaped output path.

Rationale:

- Cuts peak memory and allocator pressure in large snapshots (10k files with sizable text content).

## 9. Feature Recommendations

### FR-01: Incremental snapshot diff against working tree

- What it does: compare a `.gcl` snapshot directly against a live directory without rebuilding a second snapshot file.
- Builds on: `snapshot/build.rs` collection, `snapshot/diff.rs` compute logic.
- Implementation sketch: expose internal `compute_diff` on `Vec<SnapshotFile>` and add `diff_snapshot_to_source(snapshot, source, BuildOptions)`.
- New dependencies: no.

### FR-02: Signed snapshot headers (detached signature support)

- What it does: attach/verify detached signatures over canonical serialized bytes for provenance.
- Builds on: `snapshot/serial.rs` canonicalization + `verify_snapshot` pipeline.
- Implementation sketch: add optional `;; signature:` and CLI subcommands for sign/verify-signature using external tooling hooks.
- New dependencies: optional (could shell out to `gpg`/`age`; in-process crypto optional).

### FR-03: Policy profiles for materialization

- What it does: `strict` (current), `trusted-nonempty` (allow non-empty output with strong no-follow writes), and `no-symlink` profiles.
- Builds on: `materialize.rs` safety checks.
- Implementation sketch: add `MaterializeOptions` with explicit policy enum and enforce branches in write path.
- New dependencies: no (unless implementing hardened no-follow open wrappers cross-platform).

### FR-04: Snapshot summary command for CI metadata

- What it does: output compact machine-readable metadata (`hash`, counts, total bytes, git-rev/branch, largest files).
- Builds on: `snapshot/serial.rs` parse + `snapshot/render.rs` counting helpers.
- Implementation sketch: add `summary` command and public `summarize_snapshot` API.
- New dependencies: no.

### FR-05: Provider provenance headers

- What it does: persist source URI/provider/ref in `extra_headers` to improve auditability without affecting structural hash.
- Builds on: `providers/mod.rs` parse/fetch and `SnapshotHeader.extra_headers` serialization.
- Implementation sketch: populate `extra_headers` at build time with normalized source metadata.
- New dependencies: no.

## Executive Summary

`git-closure` has a strong architectural core: deterministic hashing, format canonicalization, and meaningful path-safety controls in materialization and tar extraction. The highest-priority correctness item is duplicate-path acceptance in `parse_files_value`, which can cause silent overwrite semantics and should be rejected at parse time. Security posture is generally good for traversal/symlink ancestor attacks, but verify/materialize policy mismatch and unbounded parsing allocations are notable hardening opportunities. Performance is healthy overall; the main low-effort gain is eliminating a full UTF-8 content clone in serialization. Recommended sprint focus: fix duplicate-path parsing, improve symlink diff semantics, and add explicit platform/policy behavior for materialization.
