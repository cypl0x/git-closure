/// Snapshot consumption: verification and filesystem materialization.
use std::fs;
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::error::GitClosureError;
use crate::snapshot::hash::{compute_snapshot_hash, sha256_hex};
use crate::snapshot::serial::parse_snapshot;
use crate::snapshot::{Result, VerifyReport};
use crate::utils::{io_error_with_path, lexical_normalize};

// ── Public API ────────────────────────────────────────────────────────────────

/// Verifies the structural and content integrity of a snapshot file.
///
/// Checks performed:
/// 1. `snapshot-hash` header matches recomputed hash over file metadata.
/// 2. Each regular file's `:sha256` matches `SHA-256(content)`.
/// 3. Each regular file's `:size` matches `content.len()`.
/// 4. Each path is safe (no `..`, no absolute paths).
/// 5. Each mode string is valid octal.
pub fn verify_snapshot(snapshot: &Path) -> Result<VerifyReport> {
    let text = fs::read_to_string(snapshot).map_err(|err| io_error_with_path(err, snapshot))?;

    let (header, files) = parse_snapshot(&text)?;

    let recomputed = compute_snapshot_hash(&files);
    if recomputed != header.snapshot_hash {
        return Err(GitClosureError::HashMismatch {
            expected: header.snapshot_hash,
            actual: recomputed,
        });
    }

    for file in &files {
        let _ = sanitized_relative_path(&file.path)?;

        if file.symlink_target.is_some() {
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
pub fn materialize_snapshot(snapshot: &Path, output: &Path) -> Result<()> {
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

    for file in files {
        let relative = sanitized_relative_path(&file.path)?;
        let destination = output_abs.join(relative);

        if !destination.starts_with(&output_abs) {
            return Err(GitClosureError::UnsafePath(file.path));
        }

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|err| io_error_with_path(err, parent))?;
        }

        if let Some(target) = &file.symlink_target {
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
            symlink(target_path, &destination)?;
            continue;
        }

        let digest = sha256_hex(&file.content);
        if digest != file.sha256 {
            return Err(GitClosureError::ContentHashMismatch {
                path: file.path,
                expected: file.sha256,
                actual: digest,
            });
        }

        fs::write(&destination, &file.content)
            .map_err(|err| io_error_with_path(err, &destination))?;

        let mode = u32::from_str_radix(&file.mode, 8).map_err(|err| {
            GitClosureError::Parse(format!(
                "invalid octal mode for {}: {} ({err})",
                file.path, file.mode
            ))
        })?;
        let permissions = fs::Permissions::from_mode(mode);
        fs::set_permissions(&destination, permissions)
            .map_err(|err| io_error_with_path(err, &destination))?;
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
