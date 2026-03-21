//! Git clone provider.

use tempfile::TempDir;

use crate::error::GitClosureError;
use crate::source::split_repo_ref;
use crate::utils::truncate_stderr;

use super::command::run_command_output;
use super::{FetchedSource, Provider, Result};

pub struct GitCloneProvider;

impl Provider for GitCloneProvider {
    fn fetch(&self, source: &str) -> Result<FetchedSource> {
        let parsed = parse_git_source(source)?;
        let tempdir = TempDir::new()?;
        let checkout = tempdir.path().join("repo");
        let checkout_str = checkout
            .to_str()
            .ok_or_else(|| GitClosureError::Parse("invalid checkout path".to_string()))?;

        let clone_output = run_command_output(
            "git",
            &[
                "clone",
                "--depth",
                "1",
                "--no-tags",
                &parsed.url,
                checkout_str,
            ],
            None,
        )?;

        if !clone_output.status.success() {
            return Err(GitClosureError::CommandExitFailure {
                command: "git",
                status: clone_output.status.to_string(),
                stderr: annotate_git_clone_stderr("clone failed", &clone_output.stderr),
            });
        }

        if let Some(reference) = parsed.reference {
            let fetch_output = run_command_output(
                "git",
                &[
                    "-C",
                    checkout_str,
                    "fetch",
                    "--depth",
                    "1",
                    "origin",
                    &reference,
                ],
                None,
            )?;

            if !fetch_output.status.success() {
                return Err(GitClosureError::CommandExitFailure {
                    command: "git",
                    status: fetch_output.status.to_string(),
                    stderr: annotate_git_clone_stderr(
                        "reference fetch failed",
                        &fetch_output.stderr,
                    ),
                });
            }

            let checkout_output = run_command_output(
                "git",
                &["-C", checkout_str, "checkout", "--detach", "FETCH_HEAD"],
                None,
            )?;

            if !checkout_output.status.success() {
                return Err(GitClosureError::CommandExitFailure {
                    command: "git",
                    status: checkout_output.status.to_string(),
                    stderr: annotate_git_clone_stderr("checkout failed", &checkout_output.stderr),
                });
            }
        }

        Ok(FetchedSource::temporary(checkout, tempdir))
    }
}

pub(super) fn annotate_git_clone_stderr(stage: &str, stderr: &[u8]) -> String {
    let detail = truncate_stderr(stderr);
    format!("git-clone: {stage}: {detail}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedGitSource {
    pub(super) url: String,
    pub(super) reference: Option<String>,
}

pub(super) fn parse_git_source(source: &str) -> Result<ParsedGitSource> {
    if let Some(rest) = source.strip_prefix("gh:") {
        let (repo, reference) = split_repo_ref(rest);
        return Ok(ParsedGitSource {
            url: format!("https://github.com/{repo}.git"),
            reference,
        });
    }

    if let Some(rest) = source.strip_prefix("gl:") {
        let (repo, reference) = split_repo_ref(rest);
        return Ok(ParsedGitSource {
            url: format!("https://gitlab.com/{repo}.git"),
            reference,
        });
    }

    Ok(ParsedGitSource {
        url: source.to_string(),
        reference: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::GitClosureError;

    #[test]
    fn parse_git_source_supports_gh_and_gl_shortcuts() {
        let gh = parse_git_source("gh:foo/bar@main").expect("parse gh source");
        assert_eq!(gh.url, "https://github.com/foo/bar.git");
        assert_eq!(gh.reference.as_deref(), Some("main"));

        let gl = parse_git_source("gl:foo/bar").expect("parse gl source");
        assert_eq!(gl.url, "https://gitlab.com/foo/bar.git");
        assert!(gl.reference.is_none());
    }

    #[test]
    fn git_clone_failure_maps_to_command_exit_failure() {
        let provider = GitCloneProvider;
        let err = match provider.fetch("::::") {
            Ok(_) => panic!("invalid git source should fail clone"),
            Err(err) => err,
        };

        match err {
            GitClosureError::CommandExitFailure {
                command, stderr, ..
            } => {
                assert_eq!(
                    command, "git",
                    "clone failure must identify the git command"
                );
                assert!(!stderr.is_empty(), "stderr payload should be captured");
                assert!(
                    stderr.contains("git-clone"),
                    "stderr payload should include git-clone prefix"
                );
            }
            other => panic!("expected CommandExitFailure, got {other:?}"),
        }
    }

    #[test]
    fn git_clone_failure_prefixes_include_operation_context() {
        assert_eq!(
            annotate_git_clone_stderr("clone failed", b"fatal: bad url"),
            "git-clone: clone failed: fatal: bad url"
        );
        assert_eq!(
            annotate_git_clone_stderr("reference fetch failed", b"fatal: no such ref"),
            "git-clone: reference fetch failed: fatal: no such ref"
        );
        assert_eq!(
            annotate_git_clone_stderr("checkout failed", b"fatal: checkout failed"),
            "git-clone: checkout failed: fatal: checkout failed"
        );
    }
}
