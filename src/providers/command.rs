//! Command execution utilities for providers.

use std::path::Path;
use std::process::Command;

use crate::error::GitClosureError;

pub(super) type Result<T> = std::result::Result<T, GitClosureError>;

pub(crate) fn run_command_output(
    command: &'static str,
    args: &[&str],
    current_dir: Option<&Path>,
) -> Result<std::process::Output> {
    let mut cmd = Command::new(command);
    cmd.args(args);
    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }
    cmd.output()
        .map_err(|source| GitClosureError::CommandSpawnFailed { command, source })
}

/// `run_command_status` is only used in tests (spawn/exit-code assertions).
/// Keeping it test-only avoids a `#[allow(dead_code)]` annotation on a
/// `pub(crate)` function that has no production call site.
#[cfg(test)]
pub(crate) fn run_command_status(
    command: &'static str,
    args: &[&str],
    current_dir: Option<&Path>,
) -> Result<std::process::ExitStatus> {
    let mut cmd = Command::new(command);
    cmd.args(args);
    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }
    cmd.status()
        .map_err(|source| GitClosureError::CommandSpawnFailed { command, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::GitClosureError;
    use crate::utils::truncate_stderr;
    use std::io::ErrorKind;

    #[test]
    fn missing_binary_maps_to_command_spawn_failed() {
        let err = run_command_status("__nonexistent_binary_for_testing__", &[], None)
            .expect_err("missing binary should produce spawn error");

        match err {
            GitClosureError::CommandSpawnFailed { command, source } => {
                assert_eq!(command, "__nonexistent_binary_for_testing__");
                assert_eq!(source.kind(), ErrorKind::NotFound);
            }
            other => panic!("expected CommandSpawnFailed, got {other:?}"),
        }
    }

    #[test]
    fn missing_binary_with_current_dir_maps_to_command_spawn_failed() {
        let dir = std::env::temp_dir();
        let err = run_command_status("__nonexistent_binary_for_testing__", &[], Some(&dir))
            .expect_err("missing binary should fail");
        assert!(
            matches!(
                err,
                GitClosureError::CommandSpawnFailed {
                    command: "__nonexistent_binary_for_testing__",
                    ..
                }
            ),
            "expected CommandSpawnFailed, got {err:?}"
        );
    }

    #[test]
    fn command_exit_failure_display_includes_stderr() {
        let output = run_command_output(
            "git",
            &["rev-parse", "--verify", "nonexistent-ref-xyz-abc"],
            None,
        )
        .expect("git command should execute");
        assert!(
            !output.status.success(),
            "rev-parse on nonexistent ref should fail"
        );

        let err = GitClosureError::CommandExitFailure {
            command: "git",
            status: output.status.to_string(),
            stderr: truncate_stderr(&output.stderr),
        };

        let display = err.to_string();
        assert!(
            display.contains("nonexistent-ref")
                || display.contains("fatal")
                || display.contains("unknown"),
            "error display must include stderr context, got: {display:?}"
        );
    }
}
