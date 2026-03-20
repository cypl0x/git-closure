/// Snapshot construction from a source directory.
///
/// Entry points: [`build_snapshot`], [`build_snapshot_with_options`],
/// [`build_snapshot_from_source`], [`build_snapshot_from_provider`].
use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::path::{Component, Path};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use ignore::WalkBuilder;

use crate::error::GitClosureError;
use crate::git::{
    ensure_git_source_is_clean, is_within_prefix, tracked_paths_from_index,
    untracked_paths_from_status, GitRepoContext,
};
use crate::providers::{fetch_source, Provider, ProviderKind};
use crate::utils::io_error_with_path;

use crate::providers::run_command_output;

use super::hash::{compute_snapshot_hash, sha256_hex};
use super::serial::serialize_snapshot;
use super::{BuildOptions, Result, SnapshotFile, SnapshotHeader};

// ── Public API ────────────────────────────────────────────────────────────────

/// Builds a snapshot of `source` using default options.
pub fn build_snapshot(source: &Path, output: &Path) -> Result<()> {
    build_snapshot_with_options(source, output, &BuildOptions::default())
}

/// Builds a snapshot from a URL or source specifier, fetching it via `provider_kind`.
pub fn build_snapshot_from_source(
    source: &str,
    output: &Path,
    options: &BuildOptions,
    provider_kind: ProviderKind,
) -> Result<()> {
    let fetched = fetch_source(source, provider_kind)?;
    build_snapshot_with_options(&fetched.root, output, options)
}

/// Builds a snapshot using a caller-supplied [`Provider`] implementation.
pub fn build_snapshot_from_provider<P: Provider>(
    provider: &P,
    source: &str,
    output: &Path,
    options: &BuildOptions,
) -> Result<()> {
    let fetched = provider.fetch(source)?;
    build_snapshot_with_options(&fetched.root, output, options)
}

/// Core build function: collects, sorts, hashes, and serializes all files.
pub fn build_snapshot_with_options(
    source: &Path,
    output: &Path,
    options: &BuildOptions,
) -> Result<()> {
    let source = fs::canonicalize(source).map_err(|err| io_error_with_path(err, source))?;

    if !source.is_dir() {
        return Err(GitClosureError::Parse(format!(
            "source is not a directory: {}",
            source.display()
        )));
    }

    let mut files = collect_files(&source, options)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let snapshot_hash = compute_snapshot_hash(&files);
    let (git_rev, git_branch) = read_git_metadata(&source);
    let header = SnapshotHeader {
        snapshot_hash,
        file_count: files.len(),
        git_rev,
        git_branch,
        extra_headers: Vec::new(),
    };
    let serialized = serialize_snapshot(&files, &header);

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|err| io_error_with_path(err, parent))?;
    }

    let mut writer = fs::File::create(output).map_err(|err| io_error_with_path(err, output))?;
    writer.write_all(serialized.as_bytes())?;

    Ok(())
}

// ── File collection ───────────────────────────────────────────────────────────

pub(crate) fn collect_files(root: &Path, options: &BuildOptions) -> Result<Vec<SnapshotFile>> {
    if let Some(repo_context) = GitRepoContext::discover(root)? {
        return collect_files_from_git_repo(&repo_context, options);
    }

    collect_files_from_ignore_walk(root)
}

// ── T-21: Consolidated file-attribute helper (eliminates 6-tuple duplication) ─

/// Resolved attributes for a single filesystem entry.
///
/// Unifies the previously duplicated symlink/regular-file classification logic
/// that appeared identically in both `collect_files_from_git_repo` and
/// `collect_files_from_ignore_walk`.
pub(crate) struct FileAttributes {
    pub(crate) sha256: String,
    pub(crate) mode: String,
    pub(crate) size: u64,
    pub(crate) encoding: Option<String>,
    pub(crate) symlink_target: Option<String>,
    pub(crate) content: Vec<u8>,
}

/// Reads a single filesystem entry and computes all snapshot attributes.
///
/// `path` must point to the actual file (or symlink) on disk.  `metadata` must
/// be obtained via `symlink_metadata` so that symlinks are not followed.
pub(crate) fn collect_file_attributes(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<FileAttributes> {
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        let target = target
            .to_str()
            .ok_or_else(|| {
                GitClosureError::Parse(format!("non-UTF-8 symlink target: {}", path.display()))
            })?
            .to_string();
        Ok(FileAttributes {
            sha256: String::new(),
            mode: "120000".to_string(),
            size: 0,
            encoding: None,
            symlink_target: Some(target),
            content: Vec::new(),
        })
    } else {
        let bytes = fs::read(path)?;
        let sha256 = sha256_hex(&bytes);
        #[cfg(unix)]
        let mode = format!("{:o}", metadata.permissions().mode() & 0o777);
        #[cfg(not(unix))]
        let mode = "644".to_string();
        let size = bytes.len() as u64;
        let encoding = if std::str::from_utf8(&bytes).is_ok() {
            None
        } else {
            Some("base64".to_string())
        };
        Ok(FileAttributes {
            sha256,
            mode,
            size,
            encoding,
            symlink_target: None,
            content: bytes,
        })
    }
}

fn collect_files_from_git_repo(
    context: &GitRepoContext,
    options: &BuildOptions,
) -> Result<Vec<SnapshotFile>> {
    if options.require_clean {
        ensure_git_source_is_clean(context)?;
    }

    let mut repo_relative_paths = tracked_paths_from_index(context)?;
    if options.include_untracked {
        let untracked = untracked_paths_from_status(context)?;
        repo_relative_paths.extend(untracked);
    }

    repo_relative_paths.sort();
    repo_relative_paths.dedup();

    let mut files = Vec::new();
    let source_root = context.workdir.join(&context.source_prefix);
    for repo_relative in repo_relative_paths {
        if !is_within_prefix(&repo_relative, &context.source_prefix) {
            continue;
        }

        let absolute = context.workdir.join(&repo_relative);
        let metadata = match fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };

        if !metadata.is_file() && !metadata.file_type().is_symlink() {
            continue;
        }

        let relative = absolute.strip_prefix(&source_root).map_err(|err| {
            GitClosureError::Parse(format!(
                "failed to create source-relative path for git entry: {} ({err})",
                absolute.display(),
            ))
        })?;

        let normalized = normalize_relative_path(relative)?;
        let attrs = collect_file_attributes(&absolute, &metadata)?;

        files.push(SnapshotFile {
            path: normalized,
            sha256: attrs.sha256,
            mode: attrs.mode,
            size: attrs.size,
            encoding: attrs.encoding,
            symlink_target: attrs.symlink_target,
            content: attrs.content,
        });
    }

    Ok(files)
}

fn collect_files_from_ignore_walk(root: &Path) -> Result<Vec<SnapshotFile>> {
    let mut collected = Vec::new();

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .standard_filters(true)
        .follow_links(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker {
        let entry = entry.map_err(|err| {
            GitClosureError::Parse(format!("failed to walk source directory: {err}"))
        })?;
        let path = entry.path();

        if path == root {
            continue;
        }

        let metadata = fs::symlink_metadata(path)?;

        if !metadata.is_file() && !metadata.file_type().is_symlink() {
            continue;
        }

        let relative = path.strip_prefix(root).map_err(|err| {
            GitClosureError::Parse(format!(
                "failed to strip source prefix: {} ({err})",
                path.display()
            ))
        })?;

        let normalized = normalize_relative_path(relative)?;
        let attrs = collect_file_attributes(path, &metadata)?;

        collected.push(SnapshotFile {
            path: normalized,
            sha256: attrs.sha256,
            mode: attrs.mode,
            size: attrs.size,
            encoding: attrs.encoding,
            symlink_target: attrs.symlink_target,
            content: attrs.content,
        });
    }

    Ok(collected)
}

// ── Git metadata capture ──────────────────────────────────────────────────────

/// Attempts to read the current git revision and branch from `dir`.
/// Both fields are best-effort: failures (non-git directory, detached HEAD,
/// git not on PATH) silently return `None` — they must not abort the build.
fn read_git_metadata(dir: &Path) -> (Option<String>, Option<String>) {
    let rev = run_command_output("git", &["rev-parse", "HEAD"], Some(dir))
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let branch = run_command_output("git", &["symbolic-ref", "--short", "HEAD"], Some(dir))
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    (rev, branch)
}

// ── Path normalization ────────────────────────────────────────────────────────

/// Converts a relative filesystem path to a normalized forward-slash UTF-8
/// string suitable for inclusion in a snapshot.
///
/// Rejects absolute paths, `.`, `..`, and any non-UTF-8 component.
pub(crate) fn normalize_relative_path(path: &Path) -> Result<String> {
    if path.is_absolute() {
        return Err(GitClosureError::UnsafePath(path.display().to_string()));
    }

    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                if part == OsStr::new(".") || part == OsStr::new("..") {
                    return Err(GitClosureError::UnsafePath(path.display().to_string()));
                }
                components.push(
                    part.to_str()
                        .ok_or_else(|| {
                            GitClosureError::Parse(format!(
                                "non-UTF-8 path component: {}",
                                path.display()
                            ))
                        })?
                        .to_string(),
                );
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(GitClosureError::UnsafePath(path.display().to_string()));
            }
        }
    }

    if components.is_empty() {
        return Err(GitClosureError::UnsafePath(
            "empty relative path".to_string(),
        ));
    }

    Ok(components.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn normalize_relative_path_simple() {
        assert_eq!(
            normalize_relative_path(Path::new("src/lib.rs")).unwrap(),
            "src/lib.rs"
        );
    }

    #[test]
    fn normalize_relative_path_emits_forward_slashes() {
        let nested = Path::new("dir").join("sub").join("file.txt");
        let normalized = normalize_relative_path(&nested).expect("normalize nested path");
        assert_eq!(normalized, "dir/sub/file.txt");
        assert!(
            !normalized.contains('\\'),
            "snapshot path must not use backslash separators"
        );
    }

    #[test]
    fn normalize_relative_path_rejects_absolute() {
        assert!(normalize_relative_path(Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn normalize_relative_path_rejects_parent_traversal() {
        assert!(normalize_relative_path(Path::new("../etc/passwd")).is_err());
    }

    #[test]
    fn collect_file_attributes_regular_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("hello.txt");
        fs::write(&file, b"hello\n").unwrap();
        let meta = fs::symlink_metadata(&file).unwrap();
        let attrs = collect_file_attributes(&file, &meta).unwrap();
        assert!(attrs.symlink_target.is_none());
        assert_eq!(attrs.content, b"hello\n");
        assert_eq!(attrs.size, 6);
        assert!(
            attrs.encoding.is_none(),
            "UTF-8 file must not have base64 encoding"
        );
    }

    #[test]
    fn collect_file_attributes_binary_file_gets_base64_encoding() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("blob.bin");
        fs::write(&file, &[0u8, 159, 255]).unwrap();
        let meta = fs::symlink_metadata(&file).unwrap();
        let attrs = collect_file_attributes(&file, &meta).unwrap();
        assert_eq!(attrs.encoding.as_deref(), Some("base64"));
    }

    #[cfg(unix)]
    #[test]
    fn collect_file_attributes_symlink() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("target.txt"), b"x").unwrap();
        std::os::unix::fs::symlink("target.txt", dir.path().join("link")).unwrap();
        let link = dir.path().join("link");
        let meta = fs::symlink_metadata(&link).unwrap();
        let attrs = collect_file_attributes(&link, &meta).unwrap();
        assert_eq!(attrs.symlink_target.as_deref(), Some("target.txt"));
        assert_eq!(attrs.mode, "120000");
        assert!(attrs.content.is_empty());
    }

    #[test]
    fn collect_files_from_git_repo_precomputes_source_root_once() {
        let source = include_str!("build.rs");
        let legacy = [
            "strip_prefix(",
            "context.workdir.join(&context.source_prefix)",
            ")",
        ]
        .join("");
        assert!(
            !source.contains(&legacy),
            "collect_files_from_git_repo should avoid recomputing source root in loop"
        );
    }
}
