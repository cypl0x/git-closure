use std::io;
use std::path::Path;

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
