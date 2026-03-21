//! Backend-agnostic compile path.
//!
//! [`compile_source`] takes a source string, fetches it via the provider layer,
//! converts the file tree to a [`crate::ir::Closure`], and writes it through the
//! chosen [`crate::backends::ArtifactBackend`].
//!
//! # Provenance
//!
//! This function does **not** read or inject git metadata (`git-rev`,
//! `git-branch`).  The resulting artifact carries content-addressed identity
//! only.  For git-aware snapshots with full provenance, use
//! [`crate::build_snapshot_from_source`].

use std::path::Path;

use crate::backends::ArtifactBackend;
use crate::error::GitClosureError;
use crate::gcl::build::collect_files;
use crate::gcl::hash::compute_snapshot_hash;
use crate::gcl::{BuildOptions, SnapshotHeader};
use crate::ir::Closure;
use crate::providers::{fetch_source, ProviderKind};

/// Output format for [`compile_source`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileFormat {
    /// Human-readable `.gcl` S-expression snapshot (provenance-light in Phase 5).
    Gcl,
    /// Binary NAR archive (Nix Archive format).
    Nar,
}

/// Compile a source into an artifact via the backend-agnostic IR pipeline.
///
/// # Provenance
///
/// This function does **not** read or inject git metadata (`git-rev`,
/// `git-branch`). The resulting artifact carries content-addressed identity
/// only. For git-aware snapshots with full provenance, use
/// [`crate::build_snapshot_from_source`].
pub fn compile_source(
    source: &str,
    output: &Path,
    format: CompileFormat,
    provider_kind: ProviderKind,
) -> Result<(), GitClosureError> {
    let fetched = fetch_source(source, provider_kind)?;

    let mut files = collect_files(&fetched.root, &BuildOptions::default())?;
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let snapshot_hash = compute_snapshot_hash(&files);
    let header = SnapshotHeader {
        snapshot_hash,
        file_count: files.len(),
        git_rev: None,
        git_branch: None,
        extra_headers: vec![],
    };
    let closure = Closure::from((header, files));

    match format {
        CompileFormat::Gcl => crate::backends::gcl::GclBackend.write(&closure, output),
        CompileFormat::Nar => crate::backends::nar::NarBackend.write(&closure, output),
    }
}
