/// Snapshot consumption: verification and filesystem materialization.
use std::fs;
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::error::GitClosureError;
use crate::gcl::hash::{compute_snapshot_hash, sha256_hex};
use crate::gcl::serial::parse_snapshot;
use crate::gcl::{Result, SnapshotFile, SnapshotHeader, VerifyReport};
use crate::utils::{
    ensure_no_symlink_ancestors, io_error_with_path, lexical_normalize, reject_if_symlink,
};

// ── Public API ────────────────────────────────────────────────────────────────

const VERIFY_SYNTHETIC_ROOT: &str = "/gcl-verify-root";

/// Policy profiles for snapshot materialization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MaterializePolicy {
    /// Preserve current strict behavior:
    /// - output directory must be empty
    /// - symlink entries are allowed (platform permitting)
    #[default]
    Strict,
    /// Allow materializing into a non-empty output directory.
    ///
    /// Existing files may be overwritten if the snapshot contains matching
    /// paths.
    ///
    /// This weakens the single-process safety properties provided by the
    /// strict empty-directory precondition. On shared/network filesystems,
    /// callers should ensure exclusive access to the output directory.
    TrustedNonempty,
    /// Reject snapshots that contain symlink entries.
    ///
    /// Useful for environments that cannot or must not create symlinks.
    NoSymlink,
}

/// Options controlling materialization behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterializeOptions {
    pub policy: MaterializePolicy,
}

/// Verifies the structural and content integrity of a snapshot file.
///
/// Checks performed:
/// 1. `snapshot-hash` header matches recomputed hash over file metadata.
/// 2. Each regular file's `:sha256` matches `SHA-256(content)`.
/// 3. Each regular file's `:size` matches `content.len()`.
/// 4. Each path is safe (no `..`, no absolute paths).
/// 5. Each mode string is valid octal.
///
/// Symlink target containment is checked against a fixed synthetic root
/// (`/gcl-verify-root`). For output-root-faithful containment checks, use
/// [`verify_snapshot_with_root`].
pub fn verify_snapshot(snapshot: &Path) -> Result<VerifyReport> {
    let (header, files) = parse_snapshot_file(snapshot)?;
    verify_snapshot_parsed(&header, &files)
}

/// Verifies snapshot integrity and checks symlink containment against `root`.
///
/// This provides a root-anchored check aligned with materialization semantics.
/// `root` is canonicalized first, so it must exist.
pub fn verify_snapshot_with_root(snapshot: &Path, root: &Path) -> Result<VerifyReport> {
    let canonical_root = fs::canonicalize(root).map_err(|err| io_error_with_path(err, root))?;
    let (header, files) = parse_snapshot_file(snapshot)?;
    verify_snapshot_parsed_against_root(&header, &files, &canonical_root)
}

/// Verifies an already parsed snapshot header + entries.
///
/// By default, this runs both structural integrity checks (`snapshot-hash`,
/// `file-count`) and per-entry checks (`sha256`, `size`, path/mode validity,
/// symlink target containment).
///
/// Note: `file-count` consistency is enforced here (verify layer), not in
/// `parse_snapshot` itself. This keeps parsing focused on syntax/shape and keeps
/// semantic integrity checks centralized in verification entry points.
///
/// Symlink containment is evaluated against the same synthetic root used by
/// [`verify_snapshot`]. Call [`verify_snapshot_with_root`] when a real output
/// root is available and root-anchored containment semantics are required.
pub fn verify_snapshot_parsed(
    header: &SnapshotHeader,
    files: &[SnapshotFile],
) -> Result<VerifyReport> {
    verify_snapshot_parsed_against_root(header, files, Path::new(VERIFY_SYNTHETIC_ROOT))
}

fn parse_snapshot_file(snapshot: &Path) -> Result<(SnapshotHeader, Vec<SnapshotFile>)> {
    let text = fs::read_to_string(snapshot).map_err(|err| io_error_with_path(err, snapshot))?;
    parse_snapshot(&text)
}

fn verify_snapshot_parsed_against_root(
    header: &SnapshotHeader,
    files: &[SnapshotFile],
    containment_root: &Path,
) -> Result<VerifyReport> {
    if header.file_count != files.len() {
        return Err(GitClosureError::Parse(format!(
            "file-count header mismatch: expected {}, got {}",
            header.file_count,
            files.len()
        )));
    }

    let recomputed = compute_snapshot_hash(files);
    if recomputed != header.snapshot_hash {
        return Err(GitClosureError::HashMismatch {
            expected: header.snapshot_hash.clone(),
            actual: recomputed,
        });
    }

    for file in files {
        let _ = sanitized_relative_path(&file.path)?;

        if let Some(target) = &file.symlink_target {
            let entry_parent = containment_root.join(
                Path::new(&file.path)
                    .parent()
                    .unwrap_or_else(|| Path::new("")),
            );
            let effective_target = if Path::new(target).is_absolute() {
                Path::new(target).to_path_buf()
            } else {
                entry_parent.join(target)
            };
            let normalized = lexical_normalize(&effective_target)?;
            if !normalized.starts_with(containment_root) {
                return Err(GitClosureError::UnsafePath(format!(
                    "symlink target would escape output root for {}: {}",
                    file.path, target
                )));
            }
            continue;
        }

        let digest = sha256_hex(&file.content);
        if digest != file.sha256 {
            return Err(GitClosureError::ContentHashMismatch {
                path: file.path.clone(),
                expected: file.sha256.clone(),
                actual: digest,
            });
        }

        if file.content.len() as u64 != file.size {
            return Err(GitClosureError::SizeMismatch {
                path: file.path.clone(),
                expected: file.size,
                actual: file.content.len() as u64,
            });
        }

        u32::from_str_radix(&file.mode, 8).map_err(|err| {
            GitClosureError::Parse(format!(
                "invalid octal mode for {}: {} ({err})",
                file.path, file.mode
            ))
        })?;
    }

    Ok(VerifyReport {
        file_count: files.len(),
        symlink_targets_checked: true,
    })
}

/// Materializes a snapshot into `output`, creating the directory tree and
/// restoring file contents and permissions.
///
/// **Preconditions:**
/// - `output` must be empty or newly created.  Materializing into a non-empty
///   directory is rejected to prevent TOCTOU-style symlink-escalation attacks
///   via pre-planted symlinks that bypass the lexical containment check.
/// - All paths in the snapshot must be safe (no `..`, no absolute paths).
/// - Symlink targets must not escape `output` when resolved lexically.
/// - On non-Unix platforms, mode parsing still occurs but applying POSIX
///   permissions is intentionally a no-op in v0.1.
pub fn materialize_snapshot(snapshot: &Path, output: &Path) -> Result<()> {
    materialize_snapshot_with_options(snapshot, output, &MaterializeOptions::default())
}

/// Materializes a snapshot with explicit policy controls.
///
/// Under [`MaterializePolicy::Strict`], requiring an empty output directory is
/// the primary defense against symlink-race (TOCTOU) attacks in the write path.
/// [`MaterializePolicy::TrustedNonempty`] relaxes that invariant and assumes the
/// caller provides equivalent external guarantees (for example, exclusive
/// directory ownership).
pub fn materialize_snapshot_with_options(
    snapshot: &Path,
    output: &Path,
    options: &MaterializeOptions,
) -> Result<()> {
    let text = fs::read_to_string(snapshot).map_err(|err| io_error_with_path(err, snapshot))?;

    let (header, files) = parse_snapshot(&text)?;

    let recomputed = compute_snapshot_hash(&files);
    if recomputed != header.snapshot_hash {
        return Err(GitClosureError::HashMismatch {
            expected: header.snapshot_hash,
            actual: recomputed,
        });
    }

    fs::create_dir_all(output).map_err(|err| io_error_with_path(err, output))?;

    let output_abs = fs::canonicalize(output).map_err(|err| io_error_with_path(err, output))?;

    if options.policy != MaterializePolicy::TrustedNonempty {
        // Safety invariant: require an empty output directory.
        // See module-level doc comment for the security rationale.
        let is_empty = output_abs
            .read_dir()
            .map_err(|err| io_error_with_path(err, &output_abs))?
            .next()
            .is_none();
        if !is_empty {
            return Err(GitClosureError::Parse(format!(
                "output directory must be empty: {}",
                output_abs.display()
            )));
        }
    }

    for file in files {
        let relative = sanitized_relative_path(&file.path)?;
        let destination = output_abs.join(relative);

        if !destination.starts_with(&output_abs) {
            return Err(GitClosureError::UnsafePath(file.path));
        }

        if let Some(parent) = destination.parent() {
            ensure_no_symlink_ancestors(&output_abs, parent)?;
            fs::create_dir_all(parent).map_err(|err| io_error_with_path(err, parent))?;
        }

        if let Some(target) = &file.symlink_target {
            if options.policy == MaterializePolicy::NoSymlink {
                return Err(GitClosureError::Parse(format!(
                    "symlink entry is disallowed by materialize policy: {}",
                    file.path
                )));
            }
            let target_path = Path::new(target);
            let effective_target = if target_path.is_absolute() {
                target_path.to_path_buf()
            } else {
                destination
                    .parent()
                    .unwrap_or(&output_abs)
                    .join(target_path)
            };
            let normalized_target = lexical_normalize(&effective_target)?;
            if !normalized_target.starts_with(&output_abs) {
                return Err(GitClosureError::UnsafePath(format!(
                    "symlink target escapes output directory for {}: {}",
                    file.path, target
                )));
            }
            reject_if_symlink(&destination)?;
            #[cfg(unix)]
            {
                symlink(target_path, &destination)?;
                continue;
            }
            #[cfg(not(unix))]
            {
                return Err(GitClosureError::Parse(format!(
                    "symlink materialization is not supported on this platform: {}",
                    file.path
                )));
            }
        }

        let digest = sha256_hex(&file.content);
        if digest != file.sha256 {
            return Err(GitClosureError::ContentHashMismatch {
                path: file.path,
                expected: file.sha256,
                actual: digest,
            });
        }

        ensure_no_symlink_ancestors(&output_abs, &destination)?;
        fs::write(&destination, &file.content)
            .map_err(|err| io_error_with_path(err, &destination))?;

        let mode = u32::from_str_radix(&file.mode, 8).map_err(|err| {
            GitClosureError::Parse(format!(
                "invalid octal mode for {}: {} ({err})",
                file.path, file.mode
            ))
        })?;
        #[cfg(unix)]
        {
            let permissions = fs::Permissions::from_mode(mode);
            fs::set_permissions(&destination, permissions)
                .map_err(|err| io_error_with_path(err, &destination))?;
        }
    }

    Ok(())
}

// ── Path safety helpers ───────────────────────────────────────────────────────

/// Validates that a snapshot path is a safe, normalized relative path and
/// converts it to a `PathBuf` suitable for `output.join(path)`.
pub(crate) fn sanitized_relative_path(path: &str) -> Result<PathBuf> {
    if path.is_empty() {
        return Err(GitClosureError::UnsafePath("path is empty".to_string()));
    }

    let candidate = Path::new(path);

    if candidate.is_absolute() {
        return Err(GitClosureError::UnsafePath(path.to_string()));
    }

    let mut clean = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(GitClosureError::UnsafePath(path.to_string()));
            }
        }
    }

    if clean.as_os_str().is_empty() {
        return Err(GitClosureError::UnsafePath(format!(
            "path normalizes to empty path: {path}"
        )));
    }

    Ok(clean)
}
