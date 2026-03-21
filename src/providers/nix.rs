//! Nix flake provider.

use std::path::PathBuf;

use crate::error::GitClosureError;

use super::command::run_command_output;
use super::{FetchedSource, Provider, Result};

pub struct NixProvider;

impl Provider for NixProvider {
    fn fetch(&self, source: &str) -> Result<FetchedSource> {
        let normalized = source.strip_prefix("nix:").unwrap_or(source);
        let output = run_command_output("nix", &["flake", "metadata", normalized, "--json"], None)?;

        if !output.status.success() {
            return Err(GitClosureError::CommandExitFailure {
                command: "nix",
                status: output.status.to_string(),
                stderr: crate::utils::truncate_stderr(&output.stderr),
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

#[derive(Debug, serde::Deserialize)]
struct NixFlakeMetadata {
    path: String,
}

pub(super) fn parse_nix_metadata_path(output: &[u8]) -> Result<PathBuf> {
    let metadata: NixFlakeMetadata = serde_json::from_slice(output).map_err(|err| {
        GitClosureError::Parse(format!("failed to parse nix flake metadata JSON: {err}"))
    })?;
    Ok(PathBuf::from(metadata.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::GitClosureError;

    #[test]
    fn parse_nix_metadata_extracts_store_path() {
        let json = br#"{ "path": "/nix/store/abc123-source", "locked": { "rev": "deadbeef" } }"#;
        let path = parse_nix_metadata_path(json).expect("parse nix metadata JSON");
        assert_eq!(path, std::path::PathBuf::from("/nix/store/abc123-source"));
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
}
