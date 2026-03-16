/// Core snapshot types shared across the build, serialization, hash, and
/// materialize layers.  Deliberately kept as a types-only module to keep
/// the dependency graph acyclic.
pub mod build;
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
/// `.gcl` file.  Only fields required for verification are stored here;
/// additional future fields are silently discarded by `split_header_body`.
#[derive(Debug)]
pub(crate) struct SnapshotHeader {
    pub(crate) snapshot_hash: String,
    pub(crate) file_count: usize,
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
