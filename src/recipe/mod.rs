//! Declarative recipe frontend for the compile and build paths.
//!
//! A [`Recipe`] is a persistent description of a single snapshot target:
//! what source to snapshot, what output file to produce, which backend to
//! use, and which execution mode to apply.
//!
//! - `mode = "compile"` (default): routes through [`crate::compile::compile_source`]
//!   (provenance-light, supports `gcl` and `nar` output).
//! - `mode = "build"`: routes through [`crate::gcl::build::build_snapshot_from_source`],
//!   using the git-aware build path and recording git metadata where available
//!   (`gcl` output only in Phase 7).
//!
//! # Path semantics
//!
//! [`from_file`] canonicalizes the recipe file path and resolves paths for
//! stable recipe-relative execution:
//!
//! - `output`: if relative, resolved against the recipe file's parent directory.
//! - `source`: resolved by category —
//!   - remote/hosted (`gh:`, `gl:`, `https://`, `git@`, `github:`, `gitlab:`,
//!     `sourcehut:`, `git+`, `tarball+`): preserved unchanged.
//!   - `.git`-suffixed values that start with a URL scheme or `git@` are covered
//!     by the remote prefix rules above; plain `.git`-suffixed paths (e.g.
//!     `./repo.git`) fall through to the filesystem resolution rule below.
//!   - `nix:path:` with a relative payload: resolved recipe-file-relative
//!     (`nix:path:./flake` → `nix:path:/abs/base/flake`).
//!   - `nix:path:` with an absolute payload: preserved as-is.
//!   - other `nix:` forms (`nix:github:`, `nix:gitlab:`, registry refs, etc.): preserved unchanged.
//!   - `path:` with a relative payload: payload resolved recipe-file-relative
//!     (`path:./flake` → `path:/abs/base/flake`).
//!   - `path:` with an absolute payload: preserved as-is.
//!   - `file+` local-path forms: rejected with a clear error (Phase 6).
//!   - plain relative or absolute filesystem paths: resolved or preserved as-is.
//!
//! This guarantees that the same committed recipe file behaves identically
//! regardless of where it is invoked from.
//!
//! Use [`from_file`] to get correctly resolved paths.
//! Use [`from_str`] for testing or for recipes where all paths are absolute.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::compile::{compile_source, CompileFormat};
use crate::error::GitClosureError;
use crate::gcl::build::build_snapshot_from_source;
use crate::gcl::BuildOptions;
use crate::providers::ProviderKind;

/// A declarative snapshot target.
///
/// Unknown fields are rejected at parse time (`deny_unknown_fields`) so that
/// typos like `provdier`, `formta`, or `outputs` produce a clear error instead
/// of silently falling back to defaults.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub source: String,
    pub output: String,
    #[serde(default)]
    pub format: RecipeFormat,
    #[serde(default)]
    pub provider: RecipeProvider,
    #[serde(default)]
    pub mode: RecipeMode,
}

/// Output format for a recipe.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RecipeFormat {
    #[default]
    Gcl,
    Nar,
}

/// Provider kind for a recipe.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RecipeProvider {
    #[default]
    Auto,
    Local,
    GitClone,
    Nix,
    GithubApi,
}

/// Execution mode for a recipe.
///
/// - `Compile` (default): routes through [`crate::compile::compile_source`]
///   (provenance-light, supports `gcl` and `nar` output).
/// - `Build`: routes through [`crate::gcl::build::build_snapshot_from_source`],
///   using the git-aware build path and recording git metadata where available.
///   Only `gcl` output is supported in Phase 7.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RecipeMode {
    #[default]
    Compile,
    Build,
}

impl RecipeMode {
    /// Returns the canonical lowercase string name, matching the serde wire format.
    pub fn as_str(&self) -> &'static str {
        match self {
            RecipeMode::Compile => "compile",
            RecipeMode::Build => "build",
        }
    }
}

impl RecipeFormat {
    /// Returns the canonical lowercase string name, matching the serde wire format.
    pub fn as_str(&self) -> &'static str {
        match self {
            RecipeFormat::Gcl => "gcl",
            RecipeFormat::Nar => "nar",
        }
    }
}

/// A compact, ordered projection of manifest targets for discovery and reporting.
///
/// Produced by [`Manifest::summary`]. Fields are ordered by target name (deterministic,
/// inheriting [`BTreeMap`] iteration order from [`Manifest::targets`]).
///
/// Fields included: `default_target`, per-target `name`/`mode`/`format`/`is_default`.
/// Fields excluded: `source`, `output`, `provider` (path-resolved execution details).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ManifestSummary {
    pub default_target: Option<String>,
    pub targets: Vec<TargetSummary>,
}

/// A single target entry in a [`ManifestSummary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct TargetSummary {
    pub name: String,
    pub mode: RecipeMode,
    pub format: RecipeFormat,
    pub is_default: bool,
}

/// A manifest of named snapshot targets, loaded from a TOML file.
///
/// Supports two TOML formats:
/// - **Legacy flat** (Phase 6/7): top-level `source`/`output` fields — parsed as a
///   single target named `"default"` for backward compatibility.
/// - **Named targets**: `[targets.<name>]` sections — each section is a full [`Recipe`].
///
/// `targets` is a [`BTreeMap`] so iteration order is deterministic; this is important
/// for a durable public API type and ensures error messages (listing available target
/// names) are stable and sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub targets: BTreeMap<String, Recipe>,
    pub default_target: Option<String>,
}

impl Manifest {
    /// Produce a compact introspection projection of this manifest.
    ///
    /// Targets are listed in sorted name order (matching [`BTreeMap`] iteration order).
    /// `is_default` is set on the target named by `default_target`, if any.
    pub fn summary(&self) -> ManifestSummary {
        ManifestSummary {
            default_target: self.default_target.clone(),
            targets: self
                .targets
                .iter()
                .map(|(name, recipe)| TargetSummary {
                    name: name.clone(),
                    mode: recipe.mode.clone(),
                    format: recipe.format.clone(),
                    is_default: self.default_target.as_deref() == Some(name.as_str()),
                })
                .collect(),
        }
    }

    /// Select a target by name, applying default / single-target ergonomics when
    /// `name` is `None`.
    ///
    /// Rules:
    /// - `Some(n)` → look up by name; error (listing available names in sorted order)
    ///   if not found.
    /// - `None` + `default_target` set → use it; error if it names a nonexistent target.
    /// - `None` + exactly 1 target + no `default_target` → auto-select (covers both
    ///   legacy flat files and new single-target manifests without requiring `--target`).
    /// - `None` + 0 targets → error.
    /// - `None` + >1 targets + no `default_target` → error listing targets and
    ///   instructing the user to use `--target`.
    pub fn select(&self, name: Option<&str>) -> Result<&Recipe, GitClosureError> {
        match name {
            Some(n) => self.targets.get(n).ok_or_else(|| {
                let avail: Vec<&str> = self.targets.keys().map(|s| s.as_str()).collect();
                GitClosureError::Parse(format!(
                    "target {n:?} not found; available: {}",
                    avail.join(", ")
                ))
            }),
            None => {
                if let Some(d) = &self.default_target {
                    self.targets.get(d.as_str()).ok_or_else(|| {
                        GitClosureError::Parse(format!(
                            "default_target {d:?} is not defined in [targets.*]"
                        ))
                    })
                } else if self.targets.len() == 1 {
                    Ok(self.targets.values().next().unwrap())
                } else if self.targets.is_empty() {
                    Err(GitClosureError::Parse(
                        "manifest has no targets".to_string(),
                    ))
                } else {
                    let avail: Vec<&str> = self.targets.keys().map(|s| s.as_str()).collect();
                    Err(GitClosureError::Parse(format!(
                        "manifest has multiple targets but no default_target; \
                         use --target to select one. Available: {}",
                        avail.join(", ")
                    )))
                }
            }
        }
    }
}

// Private deserialization helper for the [targets.<name>] format.
// deny_unknown_fields catches top-level typos like `default_targte`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestToml {
    default_target: Option<String>,
    targets: BTreeMap<String, Recipe>,
}

/// Parse a [`Manifest`] from a TOML string.
///
/// Auto-detects format:
/// - If the top-level table contains `source`, treats it as a legacy single-target
///   flat recipe (backward compatible with Phase 6/7 files).
/// - If the top-level table contains `targets`, treats it as a named-target manifest.
/// - Both present → error.
/// - Neither present → error.
///
/// Paths are returned as-is. Use [`manifest_from_file`] for recipe-file-relative
/// path resolution.
pub fn manifest_from_str(text: &str) -> Result<Manifest, GitClosureError> {
    let value: toml::Value =
        toml::from_str(text).map_err(|e| GitClosureError::Parse(e.to_string()))?;
    let table = value
        .as_table()
        .ok_or_else(|| GitClosureError::Parse("manifest must be a TOML table".to_string()))?;

    match (table.contains_key("source"), table.contains_key("targets")) {
        (true, true) => Err(GitClosureError::Parse(
            "cannot mix top-level source/output fields with [targets.*] sections; \
             use one format or the other"
                .to_string(),
        )),
        (true, false) => {
            // Legacy single-target flat format.
            let recipe: Recipe =
                toml::from_str(text).map_err(|e| GitClosureError::Parse(e.to_string()))?;
            let mut targets = BTreeMap::new();
            targets.insert("default".to_string(), recipe);
            Ok(Manifest {
                targets,
                default_target: Some("default".to_string()),
            })
        }
        (false, true) => {
            let raw: ManifestToml =
                toml::from_str(text).map_err(|e| GitClosureError::Parse(e.to_string()))?;
            Ok(Manifest {
                targets: raw.targets,
                default_target: raw.default_target,
            })
        }
        (false, false) => Err(GitClosureError::Parse(
            "manifest must contain either top-level source/output fields \
             or [targets.*] sections"
                .to_string(),
        )),
    }
}

/// Parse a [`Manifest`] from a TOML file on disk, resolving relative paths per-target.
///
/// Path resolution is applied to each target independently, using the manifest file's
/// parent directory as the base — identical semantics to [`from_file`] for a single recipe.
pub fn manifest_from_file(path: &Path) -> Result<Manifest, GitClosureError> {
    let canon = std::fs::canonicalize(path)?;
    let base = canon.parent().unwrap_or(Path::new("/"));
    let text = std::fs::read_to_string(&canon)?;
    let mut manifest = manifest_from_str(&text)?;
    for recipe in manifest.targets.values_mut() {
        if !Path::new(&recipe.output).is_absolute() {
            recipe.output = normalize_path(&base.join(&recipe.output))
                .to_string_lossy()
                .into_owned();
        }
        recipe.source = resolve_source(&recipe.source, base)?;
    }
    Ok(manifest)
}

impl From<RecipeFormat> for CompileFormat {
    fn from(f: RecipeFormat) -> Self {
        match f {
            RecipeFormat::Gcl => CompileFormat::Gcl,
            RecipeFormat::Nar => CompileFormat::Nar,
        }
    }
}

impl From<RecipeProvider> for ProviderKind {
    fn from(p: RecipeProvider) -> Self {
        match p {
            RecipeProvider::Auto => ProviderKind::Auto,
            RecipeProvider::Local => ProviderKind::Local,
            RecipeProvider::GitClone => ProviderKind::GitClone,
            RecipeProvider::Nix => ProviderKind::Nix,
            RecipeProvider::GithubApi => ProviderKind::GithubApi,
        }
    }
}

/// Parse a `Recipe` from a TOML string.
///
/// Paths are returned as-is (no resolution). Use [`from_file`] when loading
/// from disk to get recipe-file-relative path resolution.
pub fn from_str(text: &str) -> Result<Recipe, GitClosureError> {
    toml::from_str(text).map_err(|e| GitClosureError::Parse(e.to_string()))
}

/// Parse a `Recipe` from a TOML file on disk, resolving relative paths.
///
/// Path resolution rules:
/// - `output`: if relative, resolved against the recipe file's parent directory.
///   If absolute, preserved as-is.
/// - `source`: resolved via [`resolve_source`] — see its doc for full rules.
///
/// The recipe file path is canonicalized first, so the resulting `Recipe`
/// carries stable absolute paths that do not depend on the caller's CWD.
pub fn from_file(path: &Path) -> Result<Recipe, GitClosureError> {
    let canon = std::fs::canonicalize(path)?;
    let base = canon.parent().unwrap_or(Path::new("/"));
    let text = std::fs::read_to_string(&canon)?;
    let mut recipe = from_str(&text)?;

    // Always resolve output recipe-file-relative (output may not exist yet).
    if !Path::new(&recipe.output).is_absolute() {
        recipe.output = normalize_path(&base.join(&recipe.output))
            .to_string_lossy()
            .into_owned();
    }

    // Resolve source — remote/hosted refs preserved, local paths resolved recipe-relative.
    recipe.source = resolve_source(&recipe.source, base)?;

    Ok(recipe)
}

/// Resolve a `source` value for stable recipe-file-relative execution.
///
/// - Remote/hosted refs (`gh:`, `gl:`, `https://`, `git@`, `github:`, `gitlab:`,
///   `sourcehut:`, `git+`, `tarball+`): preserved unchanged.
///   `.git`-suffixed paths that start with a URL scheme or `git@` are already
///   caught by those prefix rules; bare `.git`-suffixed local paths (e.g.
///   `./repo.git`) fall through to the filesystem resolution rule.
/// - `nix:path:` with a relative payload: resolved recipe-file-relative
///   (`nix:path:./flake` → `nix:path:/base/flake`).
/// - `nix:path:` with an absolute payload: preserved as-is.
/// - other `nix:` forms (registry refs, `nix:github:`, etc.): preserved unchanged.
/// - `path:` with a relative payload: resolved recipe-file-relative.
/// - `path:` with an absolute payload: preserved as-is.
/// - `file+` refs: rejected — ambiguous local-path form not yet supported (Phase 6).
/// - plain relative filesystem path: resolved against `base`.
/// - plain absolute filesystem path: preserved as-is.
fn resolve_source(source: &str, base: &Path) -> Result<String, GitClosureError> {
    // Clearly remote/hosted — preserve unchanged.
    if is_remote_source(source) {
        return Ok(source.to_owned());
    }

    // nix: prefix — split on local path-bearing vs hosted/remote forms.
    if let Some(nix_payload) = source.strip_prefix("nix:") {
        // nix:path: with relative payload — resolve recipe-file-relative.
        if let Some(path_payload) = nix_payload.strip_prefix("path:") {
            let p = Path::new(path_payload);
            if p.is_absolute() {
                return Ok(source.to_owned());
            }
            let resolved = normalize_path(&base.join(p));
            return Ok(format!("nix:path:{}", resolved.display()));
        }
        // All other nix: forms (github:, gitlab:, sourcehut:, registry refs, etc.) — preserve.
        return Ok(source.to_owned());
    }

    // path: prefix — resolve payload if relative.
    if let Some(payload) = source.strip_prefix("path:") {
        let p = Path::new(payload);
        if p.is_absolute() {
            return Ok(source.to_owned());
        }
        let resolved = normalize_path(&base.join(p));
        return Ok(format!("path:{}", resolved.display()));
    }

    // file+ prefix — reject with clear error in Phase 6.
    if source.starts_with("file+") {
        return Err(GitClosureError::Parse(format!(
            "unsupported source syntax '{source}': \
             file+ local-path forms are not supported in Phase 6; \
             use a plain relative path or path: instead"
        )));
    }

    // Plain filesystem path — resolve if relative.
    let p = Path::new(source);
    if p.is_absolute() {
        return Ok(source.to_owned());
    }
    Ok(normalize_path(&base.join(source))
        .to_string_lossy()
        .into_owned())
}

/// Normalize a path by resolving `.` and `..` components without hitting the
/// filesystem (so it works for paths that do not yet exist).
fn normalize_path(path: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// Returns `true` if `source` uses a recognized remote/hosted syntax
/// that must be passed unchanged to the provider layer.
///
/// Notes:
/// - `nix:` is intentionally excluded — its sub-forms require finer-grained
///   handling in `resolve_source` (local `nix:path:` vs hosted `nix:github:` etc.).
/// - `.git`-suffix is intentionally NOT used as a blanket remote signal.
///   Remote git URLs ending in `.git` (e.g. `https://host/repo.git`,
///   `git@host:repo.git`) are already caught by the `https://` and `git@` prefix
///   rules. Plain `.git`-suffixed local paths (e.g. `./repo.git`, `repo.git`)
///   are local filesystem paths and must be resolved recipe-file-relative.
fn is_remote_source(source: &str) -> bool {
    const REMOTE_PREFIXES: &[&str] = &[
        "gh:",
        "gl:",
        "http://",
        "https://",
        "git@",
        "github:",
        "gitlab:",
        "sourcehut:",
        "git+",
        "tarball+",
    ];
    REMOTE_PREFIXES.iter().any(|p| source.starts_with(p))
}

/// Execute a recipe — fetch the source and write the output artifact.
///
/// Paths in `recipe` are used as-is. If loaded via [`from_file`], they
/// are already recipe-file-relative absolute paths.
///
/// Routing:
/// - [`RecipeMode::Compile`]: routes through [`compile_source`] (provenance-light,
///   supports `gcl` and `nar` output).
/// - [`RecipeMode::Build`]: routes through [`build_snapshot_from_source`], using
///   the git-aware build path and recording git metadata where available.
///   Only `gcl` output is supported in Phase 7; `nar` is rejected with a
///   clear validation error.
pub fn execute(recipe: &Recipe) -> Result<(), GitClosureError> {
    match recipe.mode {
        RecipeMode::Build => {
            if recipe.format == RecipeFormat::Nar {
                return Err(GitClosureError::Parse(
                    "build mode only supports gcl output; \
                     nar format is not available with mode = \"build\" in Phase 7"
                        .to_string(),
                ));
            }
            build_snapshot_from_source(
                &recipe.source,
                Path::new(&recipe.output),
                &BuildOptions::default(),
                recipe.provider.clone().into(),
            )
        }
        RecipeMode::Compile => compile_source(
            &recipe.source,
            Path::new(&recipe.output),
            recipe.format.clone().into(),
            recipe.provider.clone().into(),
        ),
    }
}
