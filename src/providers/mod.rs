use std::path::PathBuf;

use tempfile::TempDir;

use crate::error::GitClosureError;

mod command;
mod git_clone;
mod github_api;
mod local;
mod nix;

pub use crate::source::SourceSpec;
pub use git_clone::GitCloneProvider;
pub use github_api::GithubApiProvider;
pub use local::LocalProvider;
pub use nix::NixProvider;

pub(crate) use command::run_command_output;

pub(crate) type Result<T> = std::result::Result<T, GitClosureError>;

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

    /// Creates a fetched source backed by a temporary directory.
    ///
    /// The `TempDir` is held for the lifetime of the `FetchedSource` so the
    /// temporary directory is not deleted while `root` is still in use.
    ///
    /// `root` must be a path within the managed `tempdir` directory. Passing an
    /// unrelated path is a logic error because the returned source would then
    /// point outside the owned temporary tree.
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

pub(crate) fn choose_provider(spec: &SourceSpec, requested: ProviderKind) -> Result<ProviderKind> {
    if requested != ProviderKind::Auto {
        return Ok(requested);
    }

    let selected = match spec {
        SourceSpec::LocalPath(_) => ProviderKind::Local,
        SourceSpec::NixFlakeRef(_) => ProviderKind::Nix,
        SourceSpec::GitHubRepo { .. } => ProviderKind::GithubApi,
        SourceSpec::GitLabRepo { .. } | SourceSpec::GitRemoteUrl(_) => ProviderKind::GitClone,
        SourceSpec::Unknown(value) => {
            return Err(GitClosureError::Parse(format!(
                "unsupported source syntax for auto provider: {value}"
            )));
        }
    };
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::{choose_provider, fetch_source, ProviderKind, SourceSpec};
    use crate::error::GitClosureError;

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
            ProviderKind::GithubApi
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
    fn auto_provider_github_repo_routes_to_github_api() {
        let gh = SourceSpec::parse("gh:owner/repo").expect("parse gh source");
        assert_eq!(
            choose_provider(&gh, ProviderKind::Auto).expect("choose provider"),
            ProviderKind::GithubApi
        );
    }

    #[test]
    fn auto_provider_github_https_routes_to_github_api() {
        let gh = SourceSpec::parse("https://github.com/owner/repo").expect("parse github https");
        assert_eq!(
            choose_provider(&gh, ProviderKind::Auto).expect("choose provider"),
            ProviderKind::GithubApi
        );
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
