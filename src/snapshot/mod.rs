/// Core snapshot types shared across the build, serialization, hash, and
/// materialize layers.  Deliberately kept as a types-only module to keep
/// the dependency graph acyclic.
pub mod build;
pub mod diff;
pub mod hash;
pub mod serial;

use crate::error::GitClosureError;

pub(crate) type Result<T> = std::result::Result<T, GitClosureError>;

/// An individual file record within a `.gcl` snapshot.
///
/// Invariants:
/// - `symlink_target.is_some()` ⟺ `sha256.is_empty() && content.is_empty() && mode == "120000"`
/// - `encoding == Some("base64")` ⟺ `content` contains non-UTF-8 bytes
/// - `content.len() as u64 == size` for all regular files
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotFile {
    pub(crate) path: String,
    pub(crate) sha256: String,
    pub(crate) mode: String,
    pub(crate) size: u64,
    pub(crate) encoding: Option<String>,
    pub(crate) symlink_target: Option<String>,
    pub(crate) content: Vec<u8>,
}

/// Parsed representation of the `;;`-comment header block at the top of a
/// `.gcl` file.
#[derive(Debug, Default)]
pub(crate) struct SnapshotHeader {
    pub(crate) snapshot_hash: String,
    pub(crate) file_count: usize,
    /// SHA-1 / SHA-256 revision captured from the source git repository at
    /// build time.  Informational only — not included in `snapshot_hash`.
    pub(crate) git_rev: Option<String>,
    /// Short branch name captured from the source git repository at build time.
    /// Informational only — not included in `snapshot_hash`.
    pub(crate) git_branch: Option<String>,
}

/// Options that influence which files are included in a snapshot build.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildOptions {
    /// Include files not tracked by git (mirrors `git ls-files --others`).
    pub include_untracked: bool,
    /// Abort the build if the working tree is dirty (uncommitted changes).
    pub require_clean: bool,
}

/// Summary returned by [`crate::verify_snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub file_count: usize,
}

/// A single entry returned by [`crate::list_snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    /// Slash-delimited path relative to the snapshot root.
    pub path: String,
    /// `true` for symlinks, `false` for regular files.
    pub is_symlink: bool,
    /// Symlink target string (only set when `is_symlink == true`).
    pub symlink_target: Option<String>,
    /// Hex-encoded SHA-256 of file content (empty for symlinks).
    pub sha256: String,
    /// Octal permission bits as a string (e.g. `"644"`, `"755"`, `"120000"`).
    pub mode: String,
    /// Byte size of file content (0 for symlinks).
    pub size: u64,
}
