use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use tempfile::TempDir;

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
            bail!("local source path does not exist: {source}");
        }
        let absolute = fs::canonicalize(path)
            .with_context(|| format!("failed to canonicalize local source: {source}"))?;
        Ok(FetchedSource::local(absolute))
    }
}

pub struct GitCloneProvider;

impl Provider for GitCloneProvider {
    fn fetch(&self, source: &str) -> Result<FetchedSource> {
        let parsed = parse_git_source(source)?;
        let tempdir = TempDir::new().context("failed to create temporary directory")?;
        let checkout = tempdir.path().join("repo");

        let status = Command::new("git")
            .args([
                "clone",
                "--depth",
                "1",
                "--no-tags",
                &parsed.url,
                checkout
                    .to_str()
                    .ok_or_else(|| anyhow!("invalid checkout path"))?,
            ])
            .status()
            .context("failed to execute git clone")?;

        if !status.success() {
            bail!("git clone failed for source: {source}");
        }

        if let Some(reference) = parsed.reference {
            let fetch_status = Command::new("git")
                .args([
                    "-C",
                    checkout
                        .to_str()
                        .ok_or_else(|| anyhow!("invalid checkout path"))?,
                    "fetch",
                    "--depth",
                    "1",
                    "origin",
                    &reference,
                ])
                .status()
                .context("failed to execute git fetch for reference")?;

            if !fetch_status.success() {
                bail!("git fetch failed for reference '{reference}'");
            }

            let checkout_status = Command::new("git")
                .args([
                    "-C",
                    checkout
                        .to_str()
                        .ok_or_else(|| anyhow!("invalid checkout path"))?,
                    "checkout",
                    "--detach",
                    "FETCH_HEAD",
                ])
                .status()
                .context("failed to checkout fetched reference")?;

            if !checkout_status.success() {
                bail!("git checkout failed for reference '{reference}'");
            }
        }

        Ok(FetchedSource::temporary(checkout, tempdir))
    }
}

pub struct NixProvider;

impl Provider for NixProvider {
    fn fetch(&self, source: &str) -> Result<FetchedSource> {
        let normalized = source.strip_prefix("nix:").unwrap_or(source);
        let output = Command::new("nix")
            .args(["flake", "metadata", normalized, "--json"])
            .output()
            .context("failed to execute nix flake metadata")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("nix flake metadata failed: {stderr}");
        }

        let path = parse_nix_metadata_path(&output.stdout)?;
        if !path.is_dir() {
            bail!(
                "nix flake metadata path is not a directory: {}",
                path.display()
            );
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
    let metadata: NixFlakeMetadata =
        serde_json::from_slice(output).context("failed to parse nix flake metadata JSON")?;
    Ok(PathBuf::from(metadata.path))
}

#[cfg(test)]
mod tests {
    use super::{parse_git_source, parse_nix_metadata_path, split_repo_ref};

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
}
