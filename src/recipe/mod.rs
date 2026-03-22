//! Declarative recipe frontend for the compile path.
//!
//! A [`Recipe`] is a persistent description of a single compile target:
//! what source to snapshot, what output file to produce, and which backend
//! to use. It routes through [`crate::compile::compile_source`] (provenance-light).
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

use std::path::Path;

use serde::Deserialize;

use crate::compile::{compile_source, CompileFormat};
use crate::error::GitClosureError;
use crate::providers::ProviderKind;

/// A declarative compile target.
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
}

/// Output format for a recipe.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
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
        recipe.output = normalize_path(&base.join(&recipe.output)).to_string_lossy().into_owned();
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
    Ok(normalize_path(&base.join(source)).to_string_lossy().into_owned())
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
/// Routes through [`compile_source`] (provenance-light).
pub fn execute(recipe: &Recipe) -> Result<(), GitClosureError> {
    compile_source(
        &recipe.source,
        Path::new(&recipe.output),
        recipe.format.clone().into(),
        recipe.provider.clone().into(),
    )
}
