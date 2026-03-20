use std::io;

use thiserror::Error;

/// Canonical error type returned by `git-closure` library operations.
#[derive(Debug, Error)]
pub enum GitClosureError {
    /// Any filesystem or OS-level I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// Snapshot syntax or semantic parse error.
    #[error("parse error: {0}")]
    Parse(String),
    /// Top-level snapshot integrity mismatch.
    ///
    /// This compares the header `snapshot-hash` against the hash recomputed
    /// from all parsed entries.
    #[error("snapshot hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    /// Per-file content digest mismatch.
    ///
    /// Unlike [`Self::HashMismatch`], this indicates one specific file entry
    /// had bytes whose SHA-256 does not match its declared `:sha256` value.
    #[error("content hash mismatch for {path}: expected {expected}, got {actual}")]
    ContentHashMismatch {
        /// Snapshot-relative path of the failing file.
        path: String,
        /// Declared digest from snapshot metadata.
        expected: String,
        /// Digest recomputed from decoded file content.
        actual: String,
    },
    /// Per-file decoded byte-size mismatch.
    #[error("size mismatch for {path}: metadata {expected}, decoded {actual}")]
    SizeMismatch {
        /// Snapshot-relative path of the failing file.
        path: String,
        /// Declared `:size` metadata value.
        expected: u64,
        /// Actual decoded content length in bytes.
        actual: u64,
    },
    /// Path or symlink target failed safety checks.
    #[error("unsafe path in snapshot: {0}")]
    UnsafePath(String),
    /// Required snapshot header is missing.
    #[error("missing required header: {0}")]
    MissingHeader(&'static str),
    /// Legacy `format-hash` header encountered.
    #[error("legacy format-hash header found; re-snapshot with current tool")]
    LegacyHeader,
    /// External command could not be started.
    #[error("command '{command}' failed to spawn: {source}")]
    CommandSpawnFailed {
        /// Name of the command that failed.
        command: &'static str,
        /// Underlying process-spawn error.
        #[source]
        source: io::Error,
    },
    /// External command ran but returned a non-zero exit status.
    #[error(
        "command '{command}' exited with status {status}{stderr_suffix}",
        stderr_suffix = format_command_stderr(stderr)
    )]
    CommandExitFailure {
        /// Name of the command that failed.
        command: &'static str,
        /// Exit status rendered as text.
        status: String,
        /// Captured standard error output.
        stderr: String,
    },
}

fn format_command_stderr(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(":\n{stderr}")
    }
}
