use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::error::GitClosureError;
use crate::utils::truncate_stderr;

type Result<T> = std::result::Result<T, GitClosureError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Auto,
    Local,
    GitClone,
    Nix,
    GithubApi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSpec {
    LocalPath(PathBuf),
    GitHubRepo {
        owner: String,
        repo: String,
        reference: Option<String>,
    },
    GitLabRepo {
        group: String,
        repo: String,
        reference: Option<String>,
    },
    NixFlakeRef(String),
    GitRemoteUrl(String),
    Unknown(String),
}

impl SourceSpec {
    pub fn parse(source: &str) -> Result<Self> {
        if source.trim().is_empty() {
            return Err(GitClosureError::Parse(
                "source must not be empty".to_string(),
            ));
        }

        if Path::new(source).exists() {
            return Ok(Self::LocalPath(PathBuf::from(source)));
        }

        if looks_like_nix_flake_ref(source) {
            return Ok(Self::NixFlakeRef(source.to_string()));
        }

        if let Some(rest) = source.strip_prefix("gh:") {
            return parse_hosted_repo(rest, "github", false).map(|(owner, repo, reference)| {
                Self::GitHubRepo {
                    owner,
                    repo,
                    reference,
                }
            });
        }

        if let Some(rest) = source.strip_prefix("gl:") {
            return parse_hosted_repo(rest, "gitlab", true).map(|(group, repo, reference)| {
                Self::GitLabRepo {
                    group,
                    repo,
                    reference,
                }
            });
        }

        if let Some(rest) = source.strip_prefix("https://github.com/") {
            if let Ok((owner, repo, reference)) = parse_hosted_repo(rest, "github", false) {
                return Ok(Self::GitHubRepo {
                    owner,
                    repo,
                    reference,
                });
            }
            return Ok(Self::Unknown(source.to_string()));
        }

        if let Some(rest) = source.strip_prefix("https://gitlab.com/") {
            if let Ok((group, repo, reference)) = parse_hosted_repo(rest, "gitlab", true) {
                return Ok(Self::GitLabRepo {
                    group,
                    repo,
                    reference,
                });
            }
            return Ok(Self::Unknown(source.to_string()));
        }

        if source.starts_with("http://")
            || source.starts_with("https://")
            || source.starts_with("git@")
            || source.ends_with(".git")
        {
            return Ok(Self::GitRemoteUrl(source.to_string()));
        }

        Ok(Self::Unknown(source.to_string()))
    }
}

pub struct FetchedSource {
    pub root: PathBuf,
    // TempDir does not implement Debug; keep field private and suppress the
    // derive — Debug is only needed for test assertions on the Err branch.
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
    let spec = SourceSpec::parse(source)?;
    let local = LocalProvider;
    let git = GitCloneProvider;
    let nix = NixProvider;
    let github_api = GithubApiProvider;

    let selected = choose_provider(&spec, provider_kind)?;

    match selected {
        ProviderKind::Local => local.fetch(source),
        ProviderKind::GitClone => git.fetch(source),
        ProviderKind::Nix => nix.fetch(source),
        ProviderKind::GithubApi => github_api.fetch(source),
        ProviderKind::Auto => unreachable!("auto is resolved by choose_provider"),
    }
}

fn choose_provider(spec: &SourceSpec, requested: ProviderKind) -> Result<ProviderKind> {
    if requested != ProviderKind::Auto {
        return Ok(requested);
    }

    let selected = match spec {
        SourceSpec::LocalPath(_) => ProviderKind::Local,
        SourceSpec::NixFlakeRef(_) => ProviderKind::Nix,
        SourceSpec::GitHubRepo { .. }
        | SourceSpec::GitLabRepo { .. }
        | SourceSpec::GitRemoteUrl(_) => ProviderKind::GitClone,
        SourceSpec::Unknown(value) => {
            return Err(GitClosureError::Parse(format!(
                "unsupported source syntax for auto provider: {value}"
            )));
        }
    };
    Ok(selected)
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
            return Err(GitClosureError::CommandExitFailure {
                command: "nix",
                status: output.status.to_string(),
                stderr: truncate_stderr(&output.stderr),
            });
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
    fn fetch(&self, _source: &str) -> Result<FetchedSource> {
        // TODO: implement GitHub tarball fetch via GET /repos/{owner}/{repo}/tarball/{ref}.
        Err(GitClosureError::Parse(
            "--provider github-api is not yet implemented; \
             use --provider git-clone or omit --provider for auto-detection"
                .to_string(),
        ))
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

fn parse_hosted_repo(
    source: &str,
    host: &str,
    allow_nested_group: bool,
) -> Result<(String, String, Option<String>)> {
    let (repo_part, reference) = split_repo_ref(source);
    let repo_part = repo_part.trim_end_matches(".git");
    let mut segments = repo_part.split('/').collect::<Vec<_>>();
    if segments.len() < 2 {
        return Err(GitClosureError::Parse(format!(
            "invalid {host} source, expected <owner>/<repo>: {source}"
        )));
    }
    if !allow_nested_group && segments.len() != 2 {
        return Err(GitClosureError::Parse(format!(
            "invalid {host} source, expected <owner>/<repo>: {source}"
        )));
    }

    let repo = segments.pop().unwrap().to_string();
    let owner_or_group = segments.join("/");
    if owner_or_group.is_empty() || repo.is_empty() {
        return Err(GitClosureError::Parse(format!(
            "invalid {host} source, expected <owner>/<repo>: {source}"
        )));
    }

    Ok((owner_or_group, repo, reference))
}

fn parse_nix_metadata_path(output: &[u8]) -> Result<PathBuf> {
    let metadata: NixFlakeMetadata = serde_json::from_slice(output).map_err(|err| {
        GitClosureError::Parse(format!("failed to parse nix flake metadata JSON: {err}"))
    })?;
    Ok(PathBuf::from(metadata.path))
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
    use super::{
        choose_provider, fetch_source, parse_git_source, parse_nix_metadata_path,
        run_command_output, run_command_status, split_repo_ref, GitCloneProvider, NixProvider,
        Provider, ProviderKind, SourceSpec,
    };
    use crate::error::GitClosureError;
    use crate::utils::truncate_stderr;
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
    fn source_spec_parse_documented_examples() {
        let gh = SourceSpec::parse("gh:owner/repo@main").expect("parse gh");
        assert!(matches!(
            gh,
            SourceSpec::GitHubRepo {
                owner,
                repo,
                reference: Some(reference)
            } if owner == "owner" && repo == "repo" && reference == "main"
        ));

        let gl = SourceSpec::parse("gl:group/project").expect("parse gl");
        assert!(matches!(
            gl,
            SourceSpec::GitLabRepo {
                group,
                repo,
                reference: None
            } if group == "group" && repo == "project"
        ));

        let nix = SourceSpec::parse("nix:github:NixOS/nixpkgs/nixos-unstable").expect("parse nix");
        assert!(matches!(nix, SourceSpec::NixFlakeRef(_)));

        let github_flake = SourceSpec::parse("github:owner/repo").expect("parse github flake ref");
        assert!(matches!(github_flake, SourceSpec::NixFlakeRef(_)));

        let https = SourceSpec::parse("https://github.com/owner/repo").expect("parse github https");
        assert!(matches!(https, SourceSpec::GitHubRepo { .. }));

        let archive = SourceSpec::parse("https://github.com/owner/repo/archive/main.tar.gz")
            .expect("parse github archive URL as unsupported");
        assert!(matches!(archive, SourceSpec::Unknown(_)));
    }

    #[test]
    fn choose_provider_auto_from_source_spec() {
        let local = SourceSpec::LocalPath(std::path::PathBuf::from("."));
        assert_eq!(
            choose_provider(&local, ProviderKind::Auto).expect("choose local"),
            ProviderKind::Local
        );

        let gh = SourceSpec::GitHubRepo {
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            reference: None,
        };
        assert_eq!(
            choose_provider(&gh, ProviderKind::Auto).expect("choose git clone for github"),
            ProviderKind::GitClone
        );

        let nix = SourceSpec::NixFlakeRef("github:owner/repo".to_string());
        assert_eq!(
            choose_provider(&nix, ProviderKind::Auto).expect("choose nix for flake refs"),
            ProviderKind::Nix
        );

        let unknown = SourceSpec::Unknown("wat://unknown".to_string());
        let err = choose_provider(&unknown, ProviderKind::Auto)
            .expect_err("unknown auto source should fail before subprocess");
        assert!(matches!(err, GitClosureError::Parse(_)));
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

    #[test]
    fn nix_provider_exit_failure_maps_to_command_exit_failure() {
        let provider = NixProvider;
        let err = match provider.fetch("path:/definitely/not/here") {
            Ok(_) => panic!("invalid local flake path should fail"),
            Err(err) => err,
        };

        // On systems without the `nix` binary the error is CommandSpawnFailed
        // (ENOENT).  On systems with `nix`, the path does not exist so it
        // exits non-zero → CommandExitFailure.  Both are acceptable outcomes
        // for this test; what we assert is that the error correctly identifies
        // the `nix` command and does not silently succeed.
        match err {
            GitClosureError::CommandExitFailure {
                command, stderr, ..
            } => {
                assert_eq!(command, "nix");
                assert!(
                    !stderr.is_empty(),
                    "stderr should be captured for nix exit failure"
                );
                let lowered = stderr.to_lowercase();
                assert!(
                    lowered.contains("does not exist")
                        || lowered.contains("while fetching the input")
                        || lowered.contains("nix"),
                    "stderr should include actionable nix context, got: {stderr:?}"
                );
            }
            GitClosureError::CommandSpawnFailed { command, .. } => {
                // nix binary is not installed; spawn failure is the expected path.
                assert_eq!(command, "nix");
            }
            other => panic!("expected CommandExitFailure or CommandSpawnFailed, got {other:?}"),
        }
    }

    #[test]
    fn github_api_provider_returns_not_implemented_error() {
        use super::GithubApiProvider;
        let provider = GithubApiProvider;
        let err = match provider.fetch("owner/repo") {
            Ok(_) => panic!("GithubApiProvider must return an error until implemented"),
            Err(e) => e,
        };
        assert!(
            matches!(err, GitClosureError::Parse(_)),
            "expected Parse(not-implemented), got {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("not implemented") || msg.contains("github-api"),
            "error message must mention 'not implemented' or 'github-api', got: {msg:?}"
        );
    }

    #[test]
    fn auto_provider_gh_shorthand_does_not_hit_unimplemented_github_api() {
        let err = match fetch_source("gh:owner/repo", ProviderKind::Auto) {
            Ok(_) => return,
            Err(err) => err,
        };

        match err {
            GitClosureError::CommandExitFailure { command, .. }
            | GitClosureError::CommandSpawnFailed { command, .. } => {
                assert_eq!(command, "git");
            }
            GitClosureError::Parse(msg) => {
                assert!(
                    !msg.contains("github-api") && !msg.contains("not implemented"),
                    "auto gh source must not route to unimplemented github-api: {msg:?}"
                );
            }
            other => panic!("unexpected error kind for auto gh source: {other:?}"),
        }
    }

    #[test]
    fn auto_provider_github_https_does_not_hit_unimplemented_github_api() {
        let err = match fetch_source("https://github.com/owner/repo", ProviderKind::Auto) {
            Ok(_) => return,
            Err(err) => err,
        };

        match err {
            GitClosureError::CommandExitFailure { command, .. }
            | GitClosureError::CommandSpawnFailed { command, .. } => {
                assert_eq!(command, "git");
            }
            GitClosureError::Parse(msg) => {
                assert!(
                    !msg.contains("github-api") && !msg.contains("not implemented"),
                    "auto github https source must not route to unimplemented github-api: {msg:?}"
                );
            }
            other => panic!("unexpected error kind for auto github https source: {other:?}"),
        }
    }

    #[test]
    fn auto_provider_github_prefix_is_still_treated_as_nix_flake_ref() {
        let err = match fetch_source("github:owner/repo", ProviderKind::Auto) {
            Ok(_) => return,
            Err(err) => err,
        };
        match err {
            GitClosureError::CommandExitFailure { command, .. }
            | GitClosureError::CommandSpawnFailed { command, .. } => {
                assert_eq!(command, "nix");
            }
            other => panic!("expected nix-command failure path for github: refs, got {other:?}"),
        }
    }
}
