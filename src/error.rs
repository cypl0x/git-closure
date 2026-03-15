use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitClosureError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("snapshot hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("content hash mismatch for {path}: expected {expected}, got {actual}")]
    ContentHashMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("size mismatch for {path}: metadata {expected}, decoded {actual}")]
    SizeMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("unsafe path in snapshot: {0}")]
    UnsafePath(String),
    #[error("missing required header: {0}")]
    MissingHeader(&'static str),
    #[error("legacy format-hash header found; re-snapshot with current tool")]
    LegacyHeader,
    #[error("command '{command}' failed to spawn: {source}")]
    CommandSpawnFailed {
        command: &'static str,
        #[source]
        source: io::Error,
    },
    #[error(
        "command '{command}' exited with status {status}{stderr_suffix}",
        stderr_suffix = format_command_stderr(stderr)
    )]
    CommandExitFailure {
        command: &'static str,
        status: String,
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
