//! Artifact backend abstraction for git-closure.
//!
//! An [`ArtifactBackend`] serializes a concrete [`crate::ir::Closure`] to a
//! file on disk.  The trait is the extension point for all output formats in
//! the new recipe pipeline.
//!
//! # Current backends
//!
//! | Backend | Module | Format | Status |
//! |---|---|---|---|
//! | [`nar::NarBackend`] | `nar` | NAR binary archive | active |
//! | [`gcl::GclBackend`] | `gcl` | `.gcl` human-readable S-expression snapshot | active (Phase 5) |
//!
//! # Architecture note
//!
//! The intentional asymmetry in Phase 2 — NAR as the first concrete backend,
//! `.gcl` still served by the v1 compatibility path — is deliberate sequencing,
//! not a design flaw.

use std::path::Path;

use crate::error::GitClosureError;
use crate::ir::Closure;

pub mod gcl;
pub mod nar;

pub(crate) type Result<T> = std::result::Result<T, GitClosureError>;

/// Serialize a [`Closure`] to an on-disk artifact file.
///
/// Implementors choose their own output format.  The IR ([`Closure`]) is
/// the sole input; no `.gcl`-specific types cross this boundary.
///
/// `name` and `extension` are used by the Phase 5 `compile` CLI subcommand
/// for format selection; they are not yet called in the current phase.
#[allow(dead_code)]
pub trait ArtifactBackend {
    /// Short lowercase name used for format selection (e.g. `"nar"`).
    fn name(&self) -> &'static str;
    /// Default file extension without the leading dot (e.g. `"nar"`).
    fn extension(&self) -> &'static str;
    /// Write `closure` to `output`, creating or truncating the file.
    fn write(&self, closure: &Closure, output: &Path) -> Result<()>;
}
