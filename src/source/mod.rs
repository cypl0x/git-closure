//! Source classification for git-closure.
//!
//! This module answers "what IS this source?" — it classifies a raw source
//! string into a typed [`SourceSpec`] that the provider layer can dispatch on.
//!
//! Source transport ("HOW to fetch this source") lives in [`crate::providers`].

use std::path::{Path, PathBuf};

use crate::error::GitClosureError;

type Result<T> = std::result::Result<T, GitClosureError>;

/// Classification of a source specifier string.
///
/// A `SourceSpec` answers what kind of thing a source string refers to.
/// The provider layer ([`crate::providers`]) uses this to choose a concrete
/// fetch implementation.
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

/// Split a `<repo>[@<reference>]` string into its repo and optional reference parts.
///
/// Called by [`SourceSpec::parse`] and by `parse_git_source` in the provider
/// layer, which re-parses raw `gh:`/`gl:` strings to build git clone URLs.
pub(crate) fn split_repo_ref(input: &str) -> (&str, Option<String>) {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
