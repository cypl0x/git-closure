use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::error::GitClosureError;

/// Wraps an [`io::Error`] with a human-readable path prefix so that low-level
/// OS errors carry actionable context ("Permission denied" → "/etc/foo: Permission denied").
pub(crate) fn io_error_with_path(err: io::Error, path: &Path) -> io::Error {
    io::Error::new(err.kind(), format!("{}: {}", path.display(), err))
}

/// Converts a raw stderr byte buffer to a UTF-8 string (lossy) trimmed and
/// capped at 512 bytes to prevent log flooding from verbose tools like `git`.
pub(crate) fn truncate_stderr(bytes: &[u8]) -> String {
    const MAX_BYTES: usize = 512;
    let trimmed = String::from_utf8_lossy(bytes).trim().to_string();
    if trimmed.len() <= MAX_BYTES {
        return trimmed;
    }

    let mut end = MAX_BYTES.saturating_sub(3);
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &trimmed[..end])
}

pub(crate) fn ensure_no_symlink_ancestors(
    root: &Path,
    target: &Path,
) -> Result<(), GitClosureError> {
    let relative = target.strip_prefix(root).map_err(|_| {
        GitClosureError::UnsafePath(format!(
            "target path escapes destination root: {}",
            target.display()
        ))
    })?;

    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        reject_if_symlink(&current)?;
    }

    Ok(())
}

pub(crate) fn reject_if_symlink(path: &Path) -> Result<(), GitClosureError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(GitClosureError::UnsafePath(format!(
            "path component is a symlink: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn lexical_normalize(path: &Path) -> Result<PathBuf, GitClosureError> {
    let mut normalized = PathBuf::new();
    let mut has_root = false;

    for component in path.components() {
        match component {
            Component::Prefix(_) => {
                return Err(GitClosureError::UnsafePath(format!(
                    "unsupported path prefix: {}",
                    path.display()
                )));
            }
            Component::RootDir => {
                normalized.push(Path::new("/"));
                has_root = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && !has_root {
                    return Err(GitClosureError::UnsafePath(format!(
                        "path escapes lexical root: {}",
                        path.display()
                    )));
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_stderr_short_string_unchanged() {
        let input = b"fatal: not a git repository\n";
        assert_eq!(truncate_stderr(input), "fatal: not a git repository");
    }

    #[test]
    fn truncate_stderr_long_string_gets_ellipsis() {
        let long = b"x".repeat(600);
        let result = truncate_stderr(&long);
        assert!(
            result.ends_with("..."),
            "truncated string must end with '...'"
        );
        assert!(result.len() <= 512, "result must not exceed 512 bytes");
    }

    #[test]
    fn truncate_stderr_empty_input() {
        assert_eq!(truncate_stderr(b""), "");
    }

    #[test]
    fn io_error_with_path_includes_path_in_message() {
        let err = io::Error::new(io::ErrorKind::NotFound, "No such file or directory");
        let annotated = io_error_with_path(err, Path::new("/foo/bar"));
        assert!(
            annotated.to_string().contains("/foo/bar"),
            "error message must contain the path"
        );
    }
}
