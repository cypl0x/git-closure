use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use tempfile::TempDir;

use crate::error::GitClosureError;

type Result<T> = std::result::Result<T, GitClosureError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Auto,
    Local,
    GitClone,
    Nix,
    GithubApi,
}

pub struct FetchedSource {
    pub root: PathBuf,
    _tempdir: Option<TempDir>,
}

impl FetchedSource {
    pub fn local(root: PathBuf) -> Self {
        Self {
            root,
            _tempdir: None,
        }
    }

    pub fn temporary(root: PathBuf, tempdir: TempDir) -> Self {
        Self {
            root,
            _tempdir: Some(tempdir),
        }
    }
}

pub trait Provider {
    fn fetch(&self, source: &str) -> Result<FetchedSource>;
}

pub fn fetch_source(source: &str, provider_kind: ProviderKind) -> Result<FetchedSource> {
    let local = LocalProvider;
    let git = GitCloneProvider;
    let nix = NixProvider;
    let github_api = GithubApiProvider;

    match provider_kind {
        ProviderKind::Local => local.fetch(source),
        ProviderKind::GitClone => git.fetch(source),
        ProviderKind::Nix => nix.fetch(source),
        ProviderKind::GithubApi => github_api.fetch(source),
        ProviderKind::Auto => {
            if Path::new(source).exists() {
                return local.fetch(source);
            }

            if looks_like_nix_flake_ref(source) {
                return nix.fetch(source);
            }

            if looks_like_github_source(source) {
                return github_api.fetch(source);
            }

            git.fetch(source)
        }
    }
}

pub struct LocalProvider;

impl Provider for LocalProvider {
    fn fetch(&self, source: &str) -> Result<FetchedSource> {
        let path = Path::new(source);
        if !path.exists() {
            return Err(GitClosureError::Parse(format!(
                "local source path does not exist: {source}"
            )));
        }
        let absolute = fs::canonicalize(path)?;
        Ok(FetchedSource::local(absolute))
    }
}

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
                stderr: truncate_stderr(&clone_output.stderr),
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
                    stderr: truncate_stderr(&fetch_output.stderr),
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
                    stderr: truncate_stderr(&checkout_output.stderr),
                });
            }
        }

        Ok(FetchedSource::temporary(checkout, tempdir))
    }
}

pub struct NixProvider;

impl Provider for NixProvider {
    fn fetch(&self, source: &str) -> Result<FetchedSource> {
        let normalized = source.strip_prefix("nix:").unwrap_or(source);
        let output = run_command_output("nix", &["flake", "metadata", normalized, "--json"], None)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GitClosureError::Parse(format!(
                "nix flake metadata failed: {stderr}"
            )));
        }

        let path = parse_nix_metadata_path(&output.stdout)?;
        if !path.is_dir() {
            return Err(GitClosureError::Parse(format!(
                "nix flake metadata path is not a directory: {}",
                path.display()
            )));
        }

        Ok(FetchedSource::local(path))
    }
}

pub struct GithubApiProvider;

impl Provider for GithubApiProvider {
    fn fetch(&self, source: &str) -> Result<FetchedSource> {
        eprintln!(
            "warning: --provider github-api is not yet implemented; falling back to git clone"
        );
        // TODO: implement GitHub tarball fetch via GET /repos/{owner}/{repo}/tarball/{ref}.
        let git_provider = GitCloneProvider;
        git_provider.fetch(source)
    }
}

#[derive(Debug, serde::Deserialize)]
struct NixFlakeMetadata {
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedGitSource {
    url: String,
    reference: Option<String>,
}

fn parse_git_source(source: &str) -> Result<ParsedGitSource> {
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

fn split_repo_ref(input: &str) -> (&str, Option<String>) {
    if let Some((repo, reference)) = input.rsplit_once('@') {
        if !repo.is_empty() && !reference.is_empty() {
            return (repo, Some(reference.to_string()));
        }
    }
    (input, None)
}

fn looks_like_github_source(source: &str) -> bool {
    source.starts_with("gh:") || source.contains("github.com/") || source.starts_with("github:")
}

fn looks_like_nix_flake_ref(source: &str) -> bool {
    source.starts_with("nix:")
        || source.starts_with("github:")
        || source.starts_with("gitlab:")
        || source.starts_with("sourcehut:")
        || source.starts_with("git+")
        || source.starts_with("path:")
        || source.starts_with("tarball+")
        || source.starts_with("file+")
}

fn parse_nix_metadata_path(output: &[u8]) -> Result<PathBuf> {
    let metadata: NixFlakeMetadata = serde_json::from_slice(output).map_err(|err| {
        GitClosureError::Parse(format!("failed to parse nix flake metadata JSON: {err}"))
    })?;
    Ok(PathBuf::from(metadata.path))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn run_command_status(
    command: &'static str,
    args: &[&str],
    current_dir: Option<&Path>,
) -> Result<ExitStatus> {
    let mut cmd = Command::new(command);
    cmd.args(args);
    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }
    cmd.status()
        .map_err(|source| GitClosureError::CommandSpawnFailed { command, source })
}

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

fn truncate_stderr(bytes: &[u8]) -> String {
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
    use super::{
        parse_git_source, parse_nix_metadata_path, run_command_output, run_command_status,
        split_repo_ref, truncate_stderr, GitCloneProvider, Provider,
    };
    use crate::error::GitClosureError;
    use std::io::ErrorKind;

    #[test]
    fn split_repo_ref_parses_optional_reference() {
        assert_eq!(split_repo_ref("owner/repo"), ("owner/repo", None));
        assert_eq!(
            split_repo_ref("owner/repo@main"),
            ("owner/repo", Some("main".to_string()))
        );
    }

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
    fn parse_nix_metadata_extracts_store_path() {
        let json = br#"{ "path": "/nix/store/abc123-source", "locked": { "rev": "deadbeef" } }"#;
        let path = parse_nix_metadata_path(json).expect("parse nix metadata JSON");
        assert_eq!(path, std::path::PathBuf::from("/nix/store/abc123-source"));
    }

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
                assert_eq!(command, "git");
                assert!(!stderr.is_empty(), "stderr payload should be captured");
            }
            other => panic!("expected CommandExitFailure, got {other:?}"),
        }
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
