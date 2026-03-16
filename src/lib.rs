use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

pub mod error;
pub mod providers;

pub use error::GitClosureError;
use providers::{fetch_source, run_command_output, truncate_stderr, Provider, ProviderKind};

type Result<T> = std::result::Result<T, GitClosureError>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotFile {
    path: String,
    sha256: String,
    mode: String,
    size: u64,
    encoding: Option<String>,
    symlink_target: Option<String>,
    content: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildOptions {
    pub include_untracked: bool,
    pub require_clean: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub file_count: usize,
}

pub fn build_snapshot(source: &Path, output: &Path) -> Result<()> {
    build_snapshot_with_options(source, output, &BuildOptions::default())
}

pub fn build_snapshot_from_source(
    source: &str,
    output: &Path,
    options: &BuildOptions,
    provider_kind: ProviderKind,
) -> Result<()> {
    let fetched = fetch_source(source, provider_kind)?;
    build_snapshot_with_options(&fetched.root, output, options)
}

pub fn build_snapshot_from_provider<P: Provider>(
    provider: &P,
    source: &str,
    output: &Path,
    options: &BuildOptions,
) -> Result<()> {
    let fetched = provider.fetch(source)?;
    build_snapshot_with_options(&fetched.root, output, options)
}

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
    let serialized = serialize_snapshot(&files, &snapshot_hash);

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|err| io_error_with_path(err, parent))?;
    }

    let mut writer = fs::File::create(output).map_err(|err| io_error_with_path(err, output))?;
    writer.write_all(serialized.as_bytes())?;

    Ok(())
}

pub fn materialize_snapshot(snapshot: &Path, output: &Path) -> Result<()> {
    let text = fs::read_to_string(snapshot).map_err(|err| io_error_with_path(err, snapshot))?;

    let (header, files) = parse_snapshot(&text)?;

    let recomputed = compute_snapshot_hash(&files);
    if recomputed != header.snapshot_hash {
        return Err(GitClosureError::HashMismatch {
            expected: header.snapshot_hash,
            actual: recomputed,
        });
    }

    fs::create_dir_all(output).map_err(|err| io_error_with_path(err, output))?;

    let output_abs = fs::canonicalize(output).map_err(|err| io_error_with_path(err, output))?;

    // Safety invariant: `materialize` writes a complete, deterministic tree.
    // Materializing into a non-empty directory risks TOCTOU-style attacks where
    // a pre-planted symlink under output_abs can redirect writes outside the
    // sandbox (lexical containment check passes but fs traversal follows the
    // symlink).  An empty-directory precondition eliminates this attack surface.
    let is_empty = output_abs
        .read_dir()
        .map_err(|err| io_error_with_path(err, &output_abs))?
        .next()
        .is_none();
    if !is_empty {
        return Err(GitClosureError::Parse(format!(
            "output directory must be empty: {}",
            output_abs.display()
        )));
    }

    for file in files {
        let relative = sanitized_relative_path(&file.path)?;
        let destination = output_abs.join(relative);

        if !destination.starts_with(&output_abs) {
            return Err(GitClosureError::UnsafePath(file.path));
        }

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|err| io_error_with_path(err, parent))?;
        }

        if let Some(target) = &file.symlink_target {
            let target_path = Path::new(target);
            let effective_target = if target_path.is_absolute() {
                target_path.to_path_buf()
            } else {
                destination
                    .parent()
                    .unwrap_or(&output_abs)
                    .join(target_path)
            };
            let normalized_target = lexical_normalize(&effective_target)?;
            if !normalized_target.starts_with(&output_abs) {
                return Err(GitClosureError::UnsafePath(format!(
                    "symlink target escapes output directory for {}: {}",
                    file.path, target
                )));
            }
            symlink(target_path, &destination)?;
            continue;
        }

        let digest = sha256_hex(&file.content);
        if digest != file.sha256 {
            return Err(GitClosureError::ContentHashMismatch {
                path: file.path,
                expected: file.sha256,
                actual: digest,
            });
        }

        fs::write(&destination, &file.content)
            .map_err(|err| io_error_with_path(err, &destination))?;

        let mode = u32::from_str_radix(&file.mode, 8).map_err(|err| {
            GitClosureError::Parse(format!(
                "invalid octal mode for {}: {} ({err})",
                file.path, file.mode
            ))
        })?;
        let permissions = fs::Permissions::from_mode(mode);
        fs::set_permissions(&destination, permissions)
            .map_err(|err| io_error_with_path(err, &destination))?;
    }

    Ok(())
}

pub fn verify_snapshot(snapshot: &Path) -> Result<VerifyReport> {
    let text = fs::read_to_string(snapshot).map_err(|err| io_error_with_path(err, snapshot))?;

    let (header, files) = parse_snapshot(&text)?;

    let recomputed = compute_snapshot_hash(&files);
    if recomputed != header.snapshot_hash {
        return Err(GitClosureError::HashMismatch {
            expected: header.snapshot_hash,
            actual: recomputed,
        });
    }

    for file in &files {
        let _ = sanitized_relative_path(&file.path)?;

        if file.symlink_target.is_some() {
            continue;
        }

        let digest = sha256_hex(&file.content);
        if digest != file.sha256 {
            return Err(GitClosureError::ContentHashMismatch {
                path: file.path.clone(),
                expected: file.sha256.clone(),
                actual: digest,
            });
        }

        if file.content.len() as u64 != file.size {
            return Err(GitClosureError::SizeMismatch {
                path: file.path.clone(),
                expected: file.size,
                actual: file.content.len() as u64,
            });
        }

        u32::from_str_radix(&file.mode, 8).map_err(|err| {
            GitClosureError::Parse(format!(
                "invalid octal mode for {}: {} ({err})",
                file.path, file.mode
            ))
        })?;
    }

    Ok(VerifyReport {
        file_count: files.len(),
    })
}

fn collect_files(root: &Path, options: &BuildOptions) -> Result<Vec<SnapshotFile>> {
    if let Some(repo_context) = GitRepoContext::discover(root)? {
        return collect_files_from_git_repo(&repo_context, options);
    }

    collect_files_from_ignore_walk(root)
}

struct GitRepoContext {
    workdir: PathBuf,
    source_prefix: PathBuf,
}

impl GitRepoContext {
    fn discover(source: &Path) -> Result<Option<Self>> {
        let output = run_command_output("git", &["rev-parse", "--show-toplevel"], Some(source))?;
        if !output.status.success() {
            return Ok(None);
        }

        let workdir = String::from_utf8(output.stdout)
            .map_err(|err| {
                GitClosureError::Parse(format!("git returned non-UTF-8 repository root: {err}"))
            })?
            .trim()
            .to_string();
        let workdir = PathBuf::from(workdir);

        if !source.starts_with(&workdir) {
            return Ok(None);
        }

        let source_prefix = source
            .strip_prefix(&workdir)
            .map_err(|err| {
                GitClosureError::Parse(format!(
                    "failed to determine source prefix under git workdir: {} ({err})",
                    source.display(),
                ))
            })?
            .to_path_buf();

        Ok(Some(Self {
            workdir,
            source_prefix,
        }))
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

        let relative = absolute
            .strip_prefix(context.workdir.join(&context.source_prefix))
            .map_err(|err| {
                GitClosureError::Parse(format!(
                    "failed to create source-relative path for git entry: {} ({err})",
                    absolute.display(),
                ))
            })?;

        let normalized = normalize_relative_path(relative)?;
        let (sha256, mode, size, encoding, symlink_target, content) =
            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&absolute)?;
                let target = target
                    .to_str()
                    .ok_or_else(|| {
                        GitClosureError::Parse(format!(
                            "non-UTF-8 symlink target: {}",
                            absolute.display()
                        ))
                    })?
                    .to_string();
                (
                    String::new(),
                    "120000".to_string(),
                    0,
                    None,
                    Some(target),
                    Vec::new(),
                )
            } else {
                let bytes = fs::read(&absolute)?;
                let sha256 = sha256_hex(&bytes);
                let mode = format!("{:o}", metadata.permissions().mode() & 0o777);
                let size = bytes.len() as u64;
                let encoding = if std::str::from_utf8(&bytes).is_ok() {
                    None
                } else {
                    Some("base64".to_string())
                };
                (sha256, mode, size, encoding, None, bytes)
            };

        files.push(SnapshotFile {
            path: normalized,
            sha256,
            mode,
            size,
            encoding,
            symlink_target,
            content,
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

        let (sha256, mode, size, encoding, symlink_target, content) = if metadata
            .file_type()
            .is_symlink()
        {
            let target = fs::read_link(path)?;
            let target = target
                .to_str()
                .ok_or_else(|| {
                    GitClosureError::Parse(format!("non-UTF-8 symlink target: {}", path.display()))
                })?
                .to_string();
            (
                String::new(),
                "120000".to_string(),
                0,
                None,
                Some(target),
                Vec::new(),
            )
        } else {
            let bytes = fs::read(path)?;

            let sha256 = sha256_hex(&bytes);
            let mode = format!("{:o}", metadata.permissions().mode() & 0o777);
            let size = bytes.len() as u64;
            let encoding = if std::str::from_utf8(&bytes).is_ok() {
                None
            } else {
                Some("base64".to_string())
            };
            (sha256, mode, size, encoding, None, bytes)
        };

        collected.push(SnapshotFile {
            path: normalized,
            sha256,
            mode,
            size,
            encoding,
            symlink_target,
            content,
        });
    }

    Ok(collected)
}

fn tracked_paths_from_index(context: &GitRepoContext) -> Result<Vec<PathBuf>> {
    git_ls_files(context, false)
}

fn untracked_paths_from_status(context: &GitRepoContext) -> Result<Vec<PathBuf>> {
    git_ls_files(context, true)
}

fn ensure_git_source_is_clean(context: &GitRepoContext) -> Result<()> {
    let output = run_command_output(
        "git",
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        Some(&context.workdir),
    )?;

    if !output.status.success() {
        return Err(GitClosureError::CommandExitFailure {
            command: "git",
            status: output.status.to_string(),
            stderr: truncate_stderr(&output.stderr),
        });
    }

    evaluate_git_status_porcelain(&output.stdout, &context.source_prefix)
}

fn evaluate_git_status_porcelain(stdout: &[u8], source_prefix: &Path) -> Result<()> {
    let mut chunks = stdout.split(|b| *b == 0u8);
    while let Some(entry) = chunks.next() {
        if entry.is_empty() {
            continue;
        }

        let (xy, path) = parse_porcelain_entry(entry)?;

        let repo_relative = Path::new(path);
        if is_within_prefix(repo_relative, source_prefix) {
            return Err(GitClosureError::Parse(format!(
                "source tree is dirty at {} (use --include-untracked or clean working tree)",
                path
            )));
        }

        if matches!(xy[0], b'R' | b'C') || matches!(xy[1], b'R' | b'C') {
            let source_path_bytes = chunks.next().ok_or_else(|| {
                GitClosureError::Parse(
                    "git status rename/copy entry missing source path chunk".to_string(),
                )
            })?;
            if source_path_bytes.is_empty() {
                return Err(GitClosureError::Parse(
                    "git status rename/copy source path is empty".to_string(),
                ));
            }
            let source_path = std::str::from_utf8(source_path_bytes).map_err(|err| {
                GitClosureError::Parse(format!("git status produced non-UTF-8 path: {err}"))
            })?;
            if is_within_prefix(Path::new(source_path), source_prefix) {
                return Err(GitClosureError::Parse(format!(
                    "source tree is dirty at {} (use --include-untracked or clean working tree)",
                    source_path
                )));
            }
        }
    }

    Ok(())
}

fn parse_porcelain_entry(entry: &[u8]) -> Result<([u8; 2], &str)> {
    if entry.len() < 4 || entry[2] != b' ' {
        return Err(GitClosureError::Parse(format!(
            "git status produced malformed porcelain entry: {:?}",
            entry
        )));
    }

    let xy = [entry[0], entry[1]];
    let path = std::str::from_utf8(&entry[3..]).map_err(|err| {
        GitClosureError::Parse(format!("git status produced non-UTF-8 path: {err}"))
    })?;
    Ok((xy, path))
}

fn git_ls_files(context: &GitRepoContext, include_untracked: bool) -> Result<Vec<PathBuf>> {
    let mut args = vec!["ls-files", "-z", "--cached"];
    if include_untracked {
        args.extend(["--others", "--exclude-standard"]);
    }

    let output = run_command_output("git", &args, Some(&context.workdir))?;

    if !output.status.success() {
        return Err(GitClosureError::CommandExitFailure {
            command: "git",
            status: output.status.to_string(),
            stderr: truncate_stderr(&output.stderr),
        });
    }

    let mut paths = Vec::new();
    for chunk in output.stdout.split(|b| *b == 0u8) {
        if chunk.is_empty() {
            continue;
        }
        let path = std::str::from_utf8(chunk).map_err(|err| {
            GitClosureError::Parse(format!("git ls-files produced non-UTF-8 path: {err}"))
        })?;
        paths.push(PathBuf::from(path));
    }

    Ok(paths)
}

fn is_within_prefix(path: &Path, prefix: &Path) -> bool {
    if prefix.as_os_str().is_empty() {
        return true;
    }
    path.starts_with(prefix)
}

fn normalize_relative_path(path: &Path) -> Result<String> {
    // `.gcl` path canonicalization is host-independent: snapshot paths are always
    // forward-slash-separated UTF-8 regardless of platform separator semantics.
    // Any Windows port must normalize `\` to `/` at this boundary.
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

/// Canonical snapshot hash input format:
///
/// For each file in lexicographic path order:
///   [entry_type_len: u64 be] [entry_type: UTF-8]   ("regular" or "symlink")
///   [path_len: u64 be]       [path: UTF-8]
///
/// For "regular" entries additionally:
///   [mode_len: u64 be]       [mode: UTF-8]
///   [sha256_len: u64 be]     [sha256_hex: UTF-8]
///
/// For "symlink" entries additionally:
///   [target_len: u64 be]     [target: UTF-8]
///
/// NOTE: v0.1 intentionally omits a content-only `payload_hash` aggregate
/// (SHA-256 over file bytes without path/mode metadata). Add it only when a
/// feature requires content-addressable or dedup-focused workflows (e.g. diff/CAS).
fn compute_snapshot_hash(files: &[SnapshotFile]) -> String {
    let mut hasher = Sha256::new();
    for file in files {
        if let Some(target) = &file.symlink_target {
            hash_length_prefixed(&mut hasher, b"symlink");
            hash_length_prefixed(&mut hasher, file.path.as_bytes());
            hash_length_prefixed(&mut hasher, target.as_bytes());
        } else {
            hash_length_prefixed(&mut hasher, b"regular");
            hash_length_prefixed(&mut hasher, file.path.as_bytes());
            hash_length_prefixed(&mut hasher, file.mode.as_bytes());
            hash_length_prefixed(&mut hasher, file.sha256.as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn serialize_snapshot(files: &[SnapshotFile], snapshot_hash: &str) -> String {
    let mut output = String::new();

    output.push_str(";; git-closure snapshot v0.1\n");
    output.push_str(&format!(";; snapshot-hash: {}\n", snapshot_hash));
    output.push_str(&format!(";; file-count: {}\n", files.len()));
    output.push('\n');
    output.push_str("(\n");

    for file in files {
        output.push_str("  (\n");
        output.push_str("    (:path ");
        output.push_str(&quote_string(&file.path));
        if let Some(target) = &file.symlink_target {
            output.push('\n');
            output.push_str("     :type ");
            output.push_str(&quote_string("symlink"));
            output.push('\n');
            output.push_str("     :target ");
            output.push_str(&quote_string(target));
            output.push_str(")\n");
            output.push_str("\"\"\n");
            output.push_str("  )\n");
            continue;
        }
        output.push('\n');
        output.push_str("     :sha256 ");
        output.push_str(&quote_string(&file.sha256));
        output.push('\n');
        output.push_str("     :mode ");
        output.push_str(&quote_string(&file.mode));
        output.push('\n');
        output.push_str("     :size ");
        output.push_str(&file.size.to_string());
        if let Some(encoding) = &file.encoding {
            output.push('\n');
            output.push_str("     :encoding ");
            output.push_str(&quote_string(encoding));
        }
        output.push_str(")\n");

        let content_string = if file.encoding.as_deref() == Some("base64") {
            BASE64_STANDARD.encode(&file.content)
        } else {
            // INVARIANT: files without base64 encoding were validated as valid UTF-8
            // during collection via `std::str::from_utf8` in collect_file_attributes.
            // `from_utf8_lossy` would silently corrupt data by substituting U+FFFD —
            // an undetectable data-loss bug.  Panic loudly instead so the invariant
            // violation is surfaced immediately during development/testing.
            String::from_utf8(file.content.clone())
                .expect("non-base64 file content must be valid UTF-8 (invariant violated)")
        };

        output.push_str(&quote_string(&content_string));
        output.push('\n');
        output.push_str("  )\n");
    }

    output.push_str(")\n");
    output
}

#[derive(Debug)]
struct SnapshotHeader {
    snapshot_hash: String,
    file_count: usize,
}

fn parse_snapshot(input: &str) -> Result<(SnapshotHeader, Vec<SnapshotFile>)> {
    let (header, body) = split_header_body(input)?;
    let parsed = lexpr::from_str(body).map_err(|err| {
        GitClosureError::Parse(format!("failed to parse S-expression body: {err}"))
    })?;
    let files = parse_files_value(&parsed)?;

    if files.len() != header.file_count {
        return Err(GitClosureError::Parse(format!(
            "file count mismatch: header says {}, parsed {}",
            header.file_count,
            files.len()
        )));
    }

    Ok((header, files))
}

fn split_header_body(input: &str) -> Result<(SnapshotHeader, &str)> {
    let mut snapshot_hash = None;
    let mut file_count = None;
    let mut body_start = None;
    let mut cursor = 0usize;

    for line in input.lines() {
        let line_len = line.len();
        if line.starts_with(";;") {
            if line.strip_prefix(";; format-hash:").is_some() {
                return Err(GitClosureError::LegacyHeader);
            }
            if let Some(value) = line.strip_prefix(";; snapshot-hash:") {
                snapshot_hash = Some(value.trim().to_string());
            }
            if let Some(value) = line.strip_prefix(";; file-count:") {
                file_count = Some(value.trim().parse::<usize>().map_err(|err| {
                    GitClosureError::Parse(format!("invalid file-count header: {err}"))
                })?);
            }
            cursor += line_len + 1;
            continue;
        }

        if line.trim().is_empty() {
            cursor += line_len + 1;
            continue;
        }

        body_start = Some(cursor);
        break;
    }

    let snapshot_hash = snapshot_hash.ok_or(GitClosureError::MissingHeader("snapshot-hash"))?;
    let file_count = file_count.ok_or(GitClosureError::MissingHeader("file-count"))?;
    let body_start = body_start.ok_or(GitClosureError::MissingHeader("S-expression body"))?;

    let body = &input[body_start..];

    Ok((
        SnapshotHeader {
            snapshot_hash,
            file_count,
        },
        body,
    ))
}

fn parse_files_value(value: &lexpr::Value) -> Result<Vec<SnapshotFile>> {
    let root = value
        .to_ref_vec()
        .ok_or_else(|| GitClosureError::Parse("snapshot body must be a list".to_string()))?;

    let mut files = Vec::with_capacity(root.len());

    for entry in root {
        let pair = entry.to_ref_vec().ok_or_else(|| {
            GitClosureError::Parse("each entry must be a 2-item list".to_string())
        })?;
        if pair.len() != 2 {
            return Err(GitClosureError::Parse(
                "each entry must contain plist and content".to_string(),
            ));
        }

        let plist = pair[0]
            .to_ref_vec()
            .ok_or_else(|| GitClosureError::Parse("entry plist must be a list".to_string()))?;

        let content_field = pair[1]
            .as_str()
            .ok_or_else(|| GitClosureError::Parse("entry content must be a string".to_string()))?;

        let mut path = None;
        let mut sha256 = None;
        let mut mode = None;
        let mut size = None;
        let mut encoding = None;
        let mut entry_type = None;
        let mut target = None;

        if plist.len() % 2 != 0 {
            return Err(GitClosureError::Parse(
                "plist key/value pairs are malformed".to_string(),
            ));
        }

        let mut idx = 0usize;
        while idx < plist.len() {
            let key = if let Some(keyword) = plist[idx].as_keyword() {
                keyword
            } else if let Some(symbol) = plist[idx].as_symbol() {
                symbol.strip_prefix(':').ok_or_else(|| {
                    GitClosureError::Parse("plist symbol keys must start with ':'".to_string())
                })?
            } else {
                return Err(GitClosureError::Parse(
                    "plist keys must be keywords or :symbol values".to_string(),
                ));
            };
            let value = &plist[idx + 1];

            match key {
                "path" => {
                    path = Some(
                        value
                            .as_str()
                            .ok_or_else(|| {
                                GitClosureError::Parse(":path must be a string".to_string())
                            })?
                            .to_string(),
                    );
                }
                "sha256" => {
                    sha256 = Some(
                        value
                            .as_str()
                            .ok_or_else(|| {
                                GitClosureError::Parse(":sha256 must be a string".to_string())
                            })?
                            .to_string(),
                    );
                }
                "mode" => {
                    mode = Some(
                        value
                            .as_str()
                            .ok_or_else(|| {
                                GitClosureError::Parse(":mode must be a string".to_string())
                            })?
                            .to_string(),
                    );
                }
                "size" => {
                    size = Some(value.as_u64().ok_or_else(|| {
                        GitClosureError::Parse(":size must be a u64".to_string())
                    })?);
                }
                "encoding" => {
                    encoding = Some(
                        value
                            .as_str()
                            .ok_or_else(|| {
                                GitClosureError::Parse(":encoding must be a string".to_string())
                            })?
                            .to_string(),
                    );
                }
                "type" => {
                    entry_type = Some(
                        value
                            .as_str()
                            .ok_or_else(|| {
                                GitClosureError::Parse(":type must be a string".to_string())
                            })?
                            .to_string(),
                    );
                }
                "target" => {
                    target = Some(
                        value
                            .as_str()
                            .ok_or_else(|| {
                                GitClosureError::Parse(":target must be a string".to_string())
                            })?
                            .to_string(),
                    );
                }
                other => {
                    // Unknown keys are intentionally ignored for forward compatibility.
                    // README: "unknown plist keys are silently ignored by any conformant reader."
                    // A future version of git-closure may emit `:mtime`, `:git-object-id`, etc.
                    // Rejecting them here would be a silent format-versioning break.
                    let _ = other;
                    idx += 2;
                    continue;
                }
            }

            idx += 2;
        }

        let path = path.ok_or_else(|| GitClosureError::Parse("missing :path".to_string()))?;
        if entry_type.as_deref() == Some("symlink") {
            let target = target
                .ok_or_else(|| GitClosureError::Parse("missing :target for symlink".to_string()))?;
            files.push(SnapshotFile {
                path,
                sha256: String::new(),
                mode: "120000".to_string(),
                size: 0,
                encoding: None,
                symlink_target: Some(target),
                content: Vec::new(),
            });
            continue;
        }

        let sha256 = sha256.ok_or_else(|| GitClosureError::Parse("missing :sha256".to_string()))?;
        let mode = mode.ok_or_else(|| GitClosureError::Parse("missing :mode".to_string()))?;
        let size = size.ok_or_else(|| GitClosureError::Parse("missing :size".to_string()))?;

        let content = match encoding.as_deref() {
            Some("base64") => BASE64_STANDARD.decode(content_field).map_err(|err| {
                GitClosureError::Parse(format!("invalid base64 content for {}: {err}", path))
            })?,
            Some(other) => {
                return Err(GitClosureError::Parse(format!(
                    "unsupported encoding for {}: {}",
                    path, other
                )));
            }
            None => content_field.as_bytes().to_vec(),
        };

        if content.len() as u64 != size {
            return Err(GitClosureError::SizeMismatch {
                path,
                expected: size,
                actual: content.len() as u64,
            });
        }

        files.push(SnapshotFile {
            path,
            sha256,
            mode,
            size,
            encoding,
            symlink_target: None,
            content,
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn quote_string(input: &str) -> String {
    lexpr::to_string(&lexpr::Value::string(input))
        .expect("lexpr string serialization should not fail")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn io_error_with_path(err: io::Error, path: &Path) -> io::Error {
    io::Error::new(err.kind(), format!("{}: {}", path.display(), err))
}

fn sanitized_relative_path(path: &str) -> Result<PathBuf> {
    if path.is_empty() {
        return Err(GitClosureError::UnsafePath("path is empty".to_string()));
    }

    let candidate = Path::new(path);

    if candidate.is_absolute() {
        return Err(GitClosureError::UnsafePath(path.to_string()));
    }

    let mut clean = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(GitClosureError::UnsafePath(path.to_string()));
            }
        }
    }

    if clean.as_os_str().is_empty() {
        return Err(GitClosureError::UnsafePath(format!(
            "path normalizes to empty path: {}",
            path
        )));
    }

    Ok(clean)
}

fn lexical_normalize(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    let mut has_root = false;

    for component in path.components() {
        match component {
            Component::Prefix(_) => {
                return Err(GitClosureError::UnsafePath(format!(
                    "unsupported path prefix: {}",
                    path.display()
                )));
            }
            Component::RootDir => {
                normalized.push(Path::new("/"));
                has_root = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                // POSIX path resolution keeps root stable (`/..` is `/`).
                // So once a lexical root has been seen, additional parent
                // components must not error and must not escape above `/`.
                if !normalized.pop() && !has_root {
                    return Err(GitClosureError::UnsafePath(format!(
                        "path escapes lexical root: {}",
                        path.display()
                    )));
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::{
        build_snapshot, build_snapshot_from_provider, build_snapshot_with_options,
        compute_snapshot_hash, ensure_git_source_is_clean, evaluate_git_status_porcelain,
        git_ls_files, materialize_snapshot, parse_porcelain_entry, verify_snapshot, BuildOptions,
        GitRepoContext, SnapshotFile,
    };
    use crate::error::GitClosureError;
    use crate::providers::{FetchedSource, Provider};
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn round_trip_is_byte_identical() {
        let source = TempDir::new().expect("create source tempdir");
        let restored = TempDir::new().expect("create restored tempdir");

        let alpha_path = source.path().join("alpha.txt");
        fs::write(&alpha_path, b"alpha\n").expect("write alpha.txt");

        #[cfg(unix)]
        symlink("alpha.txt", source.path().join("link-to-alpha")).expect("create fixture symlink");

        let nested_dir = source.path().join("nested");
        fs::create_dir_all(&nested_dir).expect("create nested directory");
        let script_path = nested_dir.join("script.sh");
        fs::write(&script_path, b"#!/usr/bin/env sh\necho hi\n").expect("write script.sh");

        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(0o755);
            fs::set_permissions(&script_path, perms).expect("set script permissions");
        }

        let binary_path = source.path().join("payload.bin");
        let mut binary_file = fs::File::create(&binary_path).expect("create payload.bin");
        binary_file
            .write_all(&[0, 159, 255, 1, 2, 3])
            .expect("write payload.bin bytes");

        let snapshot_a = source.path().join("snapshot-a.gcl");
        let snapshot_b = source.path().join("snapshot-b.gcl");

        build_snapshot(source.path(), &snapshot_a).expect("build first snapshot");
        materialize_snapshot(&snapshot_a, restored.path()).expect("materialize snapshot");
        build_snapshot(restored.path(), &snapshot_b).expect("build second snapshot");

        #[cfg(unix)]
        {
            let restored_link = restored.path().join("link-to-alpha");
            assert!(
                restored_link.exists(),
                "round-trip fixture must include a materialized symlink"
            );
            let target = fs::read_link(&restored_link).expect("read materialized fixture symlink");
            assert_eq!(target, std::path::PathBuf::from("alpha.txt"));
        }

        let a = fs::read(&snapshot_a).expect("read snapshot-a");
        let b = fs::read(&snapshot_b).expect("read snapshot-b");
        assert_eq!(a, b, "round trip snapshots differ");
    }

    #[cfg(unix)]
    #[test]
    fn round_trip_includes_symlink() {
        let source = TempDir::new().expect("create source tempdir");
        let restored = TempDir::new().expect("create restored tempdir");

        fs::write(source.path().join("alpha.txt"), b"alpha\n").expect("write alpha");
        std::os::unix::fs::symlink("alpha.txt", source.path().join("link-to-alpha"))
            .expect("create symlink");

        let snapshot_a = source.path().join("snap-a.gcl");
        let snapshot_b = source.path().join("snap-b.gcl");

        build_snapshot(source.path(), &snapshot_a).expect("build snapshot");
        materialize_snapshot(&snapshot_a, restored.path()).expect("materialize");
        build_snapshot(restored.path(), &snapshot_b).expect("rebuild");

        assert_eq!(
            fs::read(&snapshot_a).expect("read snap-a"),
            fs::read(&snapshot_b).expect("read snap-b"),
            "symlink round-trip must be byte-identical"
        );

        let link = restored.path().join("link-to-alpha");
        assert!(link.exists(), "symlink must exist after materialize");
        assert_eq!(
            fs::read_link(&link).expect("read link"),
            std::path::PathBuf::from("alpha.txt")
        );
    }

    #[test]
    fn materialize_rejects_parent_traversal_path() {
        let temp = TempDir::new().expect("create tempdir");
        let snapshot = temp.path().join("evil.gcl");
        let output = temp.path().join("out");

        let content = "x";
        let digest = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(content.as_bytes());
            format!("{:x}", hasher.finalize())
        };

        let snapshot_text = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {digest}\n;; file-count: 1\n\n(\n  ((:path \"../escape.txt\" :sha256 \"{digest}\" :mode \"644\" :size 1) \"x\")\n)\n"
        );
        fs::write(&snapshot, snapshot_text).expect("write malicious snapshot");

        let result = materialize_snapshot(&snapshot, &output);
        assert!(result.is_err(), "materialize should reject traversal path");
    }

    #[test]
    fn verify_accepts_valid_snapshot() {
        let source = TempDir::new().expect("create source tempdir");
        fs::write(source.path().join("ok.txt"), b"ok\n").expect("write source file");

        let snapshot = source.path().join("snapshot.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");

        let report = verify_snapshot(&snapshot).expect("verify should pass");
        assert_eq!(report.file_count, 1);
    }

    #[test]
    fn verify_missing_file_returns_io_error_variant() {
        let path = Path::new("/nonexistent/path/snapshot.gcl");
        let err = verify_snapshot(path).expect_err("verify should fail for missing file");
        assert!(
            matches!(err, GitClosureError::Io(_)),
            "expected Io variant, got: {err:?}"
        );
    }

    #[test]
    fn materialize_missing_output_parent_returns_io_error_variant() {
        let temp = TempDir::new().expect("create tempdir");
        let snapshot = temp.path().join("empty.gcl");
        let blocking_parent = temp.path().join("not-a-directory");
        fs::write(&blocking_parent, b"file").expect("create blocking file");

        fs::write(
            &snapshot,
            ";; git-closure snapshot v0.1\n;; snapshot-hash: e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n;; file-count: 0\n\n()\n",
        )
        .expect("write empty snapshot");

        let output = blocking_parent.join("child");
        let err = materialize_snapshot(&snapshot, &output)
            .expect_err("materialize should fail when output parent is not a directory");
        assert!(
            matches!(err, GitClosureError::Io(_)),
            "expected Io variant, got: {err:?}"
        );
    }

    #[test]
    fn io_error_display_includes_snapshot_path() {
        let path = std::path::Path::new("/nonexistent/path/my-snapshot.gcl");
        let err = verify_snapshot(path).expect_err("should fail on missing file");

        assert!(
            matches!(err, GitClosureError::Io(_)),
            "expected Io variant, got: {err:?}"
        );

        let msg = err.to_string();
        assert!(
            msg.contains("my-snapshot.gcl") || msg.contains("nonexistent"),
            "error message must contain path context, got: {msg:?}"
        );
    }

    #[test]
    fn io_error_display_includes_output_path_on_missing_dir() {
        let source = TempDir::new().expect("create source tempdir");
        fs::write(source.path().join("ok.txt"), b"ok\n").expect("write file");
        let snapshot = source.path().join("snap.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");

        let blocked_parent = source.path().join("blocked-parent");
        fs::write(&blocked_parent, b"not a directory").expect("create blocking file");
        let bad_output = blocked_parent.join("output-dir");
        let err = materialize_snapshot(&snapshot, &bad_output)
            .expect_err("should fail on non-directory parent");

        assert!(
            matches!(err, GitClosureError::Io(_)),
            "expected Io variant, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("output-dir") || msg.contains("blocked-parent"),
            "error message must contain output path context, got: {msg:?}"
        );
    }

    #[test]
    fn io_error_display_includes_build_output_path() {
        let source = TempDir::new().expect("create source tempdir");
        fs::write(source.path().join("ok.txt"), b"ok\n").expect("write source file");

        let blocked_parent = source.path().join("blocked-parent");
        fs::write(&blocked_parent, b"not a directory").expect("create blocking file");

        let output = blocked_parent.join("child").join("snap.gcl");
        let err = build_snapshot(source.path(), &output).expect_err("build should fail");

        assert!(
            matches!(err, GitClosureError::Io(_)),
            "expected Io variant, got: {err:?}"
        );

        let msg = err.to_string();
        assert!(
            msg.contains("blocked-parent") || msg.contains("child"),
            "error message must include failing output path context, got: {msg:?}"
        );
    }

    #[test]
    fn io_error_display_includes_build_source_path_on_canonicalize_failure() {
        let missing = Path::new("/nonexistent/path/missing-source-dir");
        let output = Path::new("/tmp/unused-output.gcl");
        let err =
            build_snapshot(missing, output).expect_err("build should fail for missing source");

        assert!(
            matches!(err, GitClosureError::Io(_)),
            "expected Io variant, got: {err:?}"
        );

        let msg = err.to_string();
        assert!(
            msg.contains("missing-source-dir") || msg.contains("nonexistent"),
            "error message must include source path context, got: {msg:?}"
        );
    }

    #[test]
    fn verify_rejects_bad_format_hash() {
        let temp = TempDir::new().expect("create tempdir");
        let snapshot = temp.path().join("invalid.gcl");

        let digest = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(b"x");
            format!("{:x}", hasher.finalize())
        };

        let snapshot_text = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: deadbeef\n;; file-count: 1\n\n(\n  ((:path \"x.txt\" :sha256 \"{digest}\" :mode \"644\" :size 1) \"x\")\n)\n"
        );
        fs::write(&snapshot, snapshot_text).expect("write invalid snapshot");

        let result = verify_snapshot(&snapshot);
        assert!(result.is_err(), "verify should reject bad format hash");
    }

    #[test]
    fn verify_odd_length_plist_returns_parse_error() {
        let temp = TempDir::new().expect("create tempdir");
        let snapshot = temp.path().join("malformed-plist.gcl");

        let snapshot_text = ";; git-closure snapshot v0.1\n;; snapshot-hash: deadbeef\n;; file-count: 1\n\n(\n  ((:path \"x.txt\" :sha256) \"x\")\n)\n";
        fs::write(&snapshot, snapshot_text).expect("write malformed snapshot");

        let err = verify_snapshot(&snapshot).expect_err("odd-length plist should fail parse");
        assert!(matches!(err, GitClosureError::Parse(_)));
        let msg = err.to_string();
        assert!(
            msg.contains("plist")
                || msg.contains("malformed")
                || msg.contains("parse")
                || msg.contains("x.txt"),
            "parse error should include contextual detail, got: {msg:?}"
        );
    }

    #[test]
    fn collision_regression_same_content_different_path() {
        let left = TempDir::new().expect("create left tempdir");
        let right = TempDir::new().expect("create right tempdir");

        fs::write(left.path().join("a.txt"), b"same\n").expect("write left file");
        fs::write(right.path().join("b.txt"), b"same\n").expect("write right file");

        let left_snapshot = left.path().join("left.gcl");
        let right_snapshot = right.path().join("right.gcl");

        build_snapshot(left.path(), &left_snapshot).expect("build left snapshot");
        build_snapshot(right.path(), &right_snapshot).expect("build right snapshot");

        let left_hash = read_snapshot_hash(&left_snapshot);
        let right_hash = read_snapshot_hash(&right_snapshot);

        assert_ne!(
            left_hash, right_hash,
            "snapshot hash must differ when path differs"
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_hash_protocol_is_consistent_across_entry_types() {
        let source = TempDir::new().expect("create source tempdir");
        fs::write(source.path().join("regular.txt"), b"hello\n").expect("write regular file");
        symlink("regular.txt", source.path().join("link")).expect("create symlink");

        let snapshot = source.path().join("mixed.gcl");
        build_snapshot(source.path(), &snapshot).expect("build mixed snapshot");

        let hash = read_snapshot_hash(&snapshot);
        assert_eq!(hash.len(), 64, "snapshot hash should be 64 hex chars");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "snapshot hash should be lowercase hex"
        );

        verify_snapshot(&snapshot).expect("verify should accept mixed entry types");
    }

    #[test]
    fn snapshot_hash_uses_length_prefix_not_null_termination() {
        let files = vec![
            SnapshotFile {
                path: "alpha.txt".to_string(),
                sha256: "a".repeat(64),
                mode: "644".to_string(),
                size: 1,
                encoding: None,
                symlink_target: None,
                content: vec![b'x'],
            },
            SnapshotFile {
                path: "sym".to_string(),
                sha256: String::new(),
                mode: "120000".to_string(),
                size: 0,
                encoding: None,
                symlink_target: Some("../target.txt".to_string()),
                content: Vec::new(),
            },
        ];

        let actual = compute_snapshot_hash(&files);
        let expected = manual_snapshot_hash_with_length_prefix(&files);
        assert_eq!(
            actual, expected,
            "snapshot hash must match documented length-prefixed protocol"
        );
    }

    #[cfg(unix)]
    #[test]
    fn collision_regression_same_path_different_mode() {
        let left = TempDir::new().expect("create left tempdir");
        let right = TempDir::new().expect("create right tempdir");

        let left_file = left.path().join("run.sh");
        let right_file = right.path().join("run.sh");

        fs::write(&left_file, b"echo hi\n").expect("write left file");
        fs::write(&right_file, b"echo hi\n").expect("write right file");

        fs::set_permissions(&left_file, fs::Permissions::from_mode(0o644))
            .expect("set left permissions");
        fs::set_permissions(&right_file, fs::Permissions::from_mode(0o755))
            .expect("set right permissions");

        let left_snapshot = left.path().join("left.gcl");
        let right_snapshot = right.path().join("right.gcl");

        build_snapshot(left.path(), &left_snapshot).expect("build left snapshot");
        build_snapshot(right.path(), &right_snapshot).expect("build right snapshot");

        let left_hash = read_snapshot_hash(&left_snapshot);
        let right_hash = read_snapshot_hash(&right_snapshot);

        assert_ne!(
            left_hash, right_hash,
            "snapshot hash must differ when mode differs"
        );
    }

    #[test]
    fn verify_rejects_legacy_format_hash_header() {
        let temp = TempDir::new().expect("create tempdir");
        let snapshot = temp.path().join("legacy.gcl");

        let digest = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(b"x");
            format!("{:x}", hasher.finalize())
        };

        let snapshot_text = format!(
            ";; git-closure snapshot v0.1\n;; format-hash: deadbeef\n;; file-count: 1\n\n(\n  ((:path \"x.txt\" :sha256 \"{digest}\" :mode \"644\" :size 1) \"x\")\n)\n"
        );
        fs::write(&snapshot, snapshot_text).expect("write legacy snapshot");

        let err = verify_snapshot(&snapshot).expect_err("legacy format hash must be rejected");
        let message = format!("{err:#}");
        assert!(
            (message.contains("format-hash") || message.contains("snapshot-hash"))
                && message.contains("re-snapshot"),
            "error should mention legacy header migration: {message}"
        );
    }

    #[test]
    fn verify_legacy_header_maps_to_typed_error() {
        let temp = TempDir::new().expect("create tempdir");
        let snapshot = temp.path().join("legacy.gcl");
        fs::write(
            &snapshot,
            ";; git-closure snapshot v0.1\n;; format-hash: deadbeef\n;; file-count: 0\n\n()\n",
        )
        .expect("write legacy snapshot");

        let err = verify_snapshot(&snapshot).expect_err("legacy header should fail");
        assert!(matches!(err, GitClosureError::LegacyHeader));
    }

    #[test]
    fn materialize_path_traversal_maps_to_typed_error() {
        let temp = TempDir::new().expect("create tempdir");
        let snapshot = temp.path().join("evil.gcl");
        let output = temp.path().join("out");

        let content = "x";
        let digest = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(content.as_bytes());
            format!("{:x}", hasher.finalize())
        };

        let snapshot_hash = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update((b"regular".len() as u64).to_be_bytes());
            hasher.update(b"regular");
            hasher.update(("../escape.txt".len() as u64).to_be_bytes());
            hasher.update(b"../escape.txt");
            hasher.update((b"644".len() as u64).to_be_bytes());
            hasher.update(b"644");
            hasher.update((digest.len() as u64).to_be_bytes());
            hasher.update(digest.as_bytes());
            format!("{:x}", hasher.finalize())
        };

        let snapshot_text = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {snapshot_hash}\n;; file-count: 1\n\n(\n  ((:path \"../escape.txt\" :sha256 \"{digest}\" :mode \"644\" :size 1) \"x\")\n)\n"
        );
        fs::write(&snapshot, snapshot_text).expect("write malicious snapshot");

        let err = materialize_snapshot(&snapshot, &output).expect_err("materialize should fail");
        assert!(matches!(err, GitClosureError::UnsafePath(_)));
    }

    #[test]
    fn collision_regression_rebuild_is_byte_identical() {
        let source = TempDir::new().expect("create source tempdir");
        let snapshots = TempDir::new().expect("create snapshot tempdir");
        fs::write(source.path().join("a.txt"), b"alpha\n").expect("write a.txt");
        fs::create_dir_all(source.path().join("bin")).expect("create bin directory");
        let script = source.path().join("bin").join("run.sh");
        fs::write(&script, b"#!/bin/sh\necho hi\n").expect("write script");

        #[cfg(unix)]
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("set script mode");

        let first = snapshots.path().join("first.gcl");
        let second = snapshots.path().join("second.gcl");
        build_snapshot(source.path(), &first).expect("build first snapshot");
        build_snapshot(source.path(), &second).expect("build second snapshot");

        let a = fs::read(first).expect("read first snapshot");
        let b = fs::read(second).expect("read second snapshot");
        assert_eq!(a, b, "snapshot output must be deterministic");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_survives_round_trip() {
        let source = TempDir::new().expect("create source tempdir");
        let restored = TempDir::new().expect("create restored tempdir");

        fs::write(source.path().join("target.txt"), b"payload\n").expect("write target file");
        symlink("target.txt", source.path().join("result")).expect("create source symlink");

        let snapshot = source.path().join("snapshot.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");
        materialize_snapshot(&snapshot, restored.path()).expect("materialize snapshot");

        let restored_link = restored.path().join("result");
        assert!(
            restored_link.exists(),
            "materialized symlink path should exist"
        );
        let target = fs::read_link(&restored_link).expect("read materialized symlink target");
        assert_eq!(target, std::path::PathBuf::from("target.txt"));

        let snapshot_b = restored.path().join("snapshot-b.gcl");
        build_snapshot(restored.path(), &snapshot_b).expect("rebuild from materialized snapshot");

        let a_bytes = fs::read(&snapshot).expect("read original snapshot");
        let b_bytes = fs::read(&snapshot_b).expect("read rebuilt snapshot");
        assert_eq!(
            a_bytes, b_bytes,
            "rebuild from materialized symlink snapshot must be byte-identical"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_changes_snapshot_hash() {
        let left = TempDir::new().expect("create left tempdir");
        let right = TempDir::new().expect("create right tempdir");

        symlink("one.txt", left.path().join("result")).expect("create left symlink");
        symlink("two.txt", right.path().join("result")).expect("create right symlink");

        let left_snapshot = left.path().join("left.gcl");
        let right_snapshot = right.path().join("right.gcl");

        build_snapshot(left.path(), &left_snapshot).expect("build left snapshot");
        build_snapshot(right.path(), &right_snapshot).expect("build right snapshot");

        let left_hash = read_snapshot_hash(&left_snapshot);
        let right_hash = read_snapshot_hash(&right_snapshot);
        assert_ne!(
            left_hash, right_hash,
            "symlink target must affect snapshot hash"
        );
    }

    #[cfg(unix)]
    #[test]
    fn materialize_rejects_absolute_symlink_target_outside_output() {
        let temp = TempDir::new().expect("create tempdir");
        let snapshot = temp.path().join("escape.gcl");
        let output = temp.path().join("out");

        let path = "result";
        let target = "/etc/passwd";
        let snapshot_hash = symlink_snapshot_hash(path, target);

        let snapshot_text = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {snapshot_hash}\n;; file-count: 1\n\n(\n  ((:path \"{path}\" :type \"symlink\" :target \"{target}\") \"\")\n)\n"
        );
        fs::write(&snapshot, snapshot_text).expect("write symlink snapshot");

        let err = materialize_snapshot(&snapshot, &output)
            .expect_err("absolute symlink target outside output must fail");
        let message = format!("{err:#}");
        assert!(
            message.contains("symlink") && message.contains("escapes output directory"),
            "error should explain unsafe absolute symlink target: {message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn materialize_rejects_relative_symlink_traversal() {
        let temp = TempDir::new().expect("create tempdir");
        let snapshot = temp.path().join("escape-relative.gcl");
        let output = temp.path().join("out");

        let path = "foo/link";
        let target = "../../etc/passwd";
        let snapshot_hash = symlink_snapshot_hash(path, target);
        let snapshot_text = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {snapshot_hash}\n;; file-count: 1\n\n(\n  ((:path \"{path}\" :type \"symlink\" :target \"{target}\") \"\")\n)\n"
        );
        fs::write(&snapshot, snapshot_text).expect("write symlink snapshot");

        let err = materialize_snapshot(&snapshot, &output)
            .expect_err("relative traversal symlink must be rejected");
        assert!(matches!(err, GitClosureError::UnsafePath(_)));
    }

    #[test]
    fn lexical_normalize_posix_root_parent_stays_at_root() {
        let normalized = super::lexical_normalize(Path::new("/../..")).expect("normalize root");
        assert_eq!(normalized, std::path::PathBuf::from("/"));
    }

    #[cfg(unix)]
    #[test]
    fn materialize_rejects_symlink_whose_effective_target_is_root() {
        let temp = TempDir::new().expect("create tempdir");
        let snapshot = temp.path().join("root-target.gcl");
        let output = temp.path().join("out");

        let path = "link";
        let target = "/../..";
        let snapshot_hash = symlink_snapshot_hash(path, target);
        let snapshot_text = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {snapshot_hash}\n;; file-count: 1\n\n(\n  ((:path \"{path}\" :type \"symlink\" :target \"{target}\") \"\")\n)\n"
        );
        fs::write(&snapshot, snapshot_text).expect("write symlink snapshot");

        let err = materialize_snapshot(&snapshot, &output)
            .expect_err("symlink resolving to root must be rejected");
        assert!(matches!(err, GitClosureError::UnsafePath(_)));
    }

    #[cfg(unix)]
    #[test]
    fn materialize_accepts_valid_relative_symlink() {
        let temp = TempDir::new().expect("create tempdir");
        let snapshot = temp.path().join("valid-relative.gcl");
        let output = temp.path().join("out");

        let path = "subdir/link";
        let target = "../sibling.txt";
        let snapshot_hash = symlink_snapshot_hash(path, target);
        let snapshot_text = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {snapshot_hash}\n;; file-count: 1\n\n(\n  ((:path \"{path}\" :type \"symlink\" :target \"{target}\") \"\")\n)\n"
        );
        fs::write(&snapshot, snapshot_text).expect("write symlink snapshot");

        materialize_snapshot(&snapshot, &output).expect("safe relative symlink should materialize");

        let link = output.join("subdir/link");
        let actual_target = fs::read_link(&link).expect("read materialized symlink");
        assert_eq!(actual_target, std::path::PathBuf::from(target));
    }

    #[cfg(unix)]
    #[test]
    fn materialize_accepts_deeply_nested_relative_symlink() {
        let temp = TempDir::new().expect("create tempdir");
        let snapshot = temp.path().join("valid-deep-relative.gcl");
        let output = temp.path().join("out");

        let path = "a/b/c/link";
        let target = "../../d/target.txt";
        let snapshot_hash = symlink_snapshot_hash(path, target);
        let snapshot_text = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {snapshot_hash}\n;; file-count: 1\n\n(\n  ((:path \"{path}\" :type \"symlink\" :target \"{target}\") \"\")\n)\n"
        );
        fs::write(&snapshot, snapshot_text).expect("write symlink snapshot");

        materialize_snapshot(&snapshot, &output)
            .expect("nested safe relative symlink should materialize");

        let link = output.join("a/b/c/link");
        let actual_target = fs::read_link(&link).expect("read materialized symlink");
        assert_eq!(actual_target, std::path::PathBuf::from(target));
    }

    #[test]
    fn remote_build_round_trip_with_mock_provider() {
        let fixture = TempDir::new().expect("create fixture tempdir");
        fs::write(fixture.path().join("a.txt"), b"hello\n").expect("write fixture file");
        fs::create_dir_all(fixture.path().join("nested")).expect("create nested fixture dir");
        fs::write(fixture.path().join("nested").join("b.txt"), b"world\n")
            .expect("write nested fixture file");

        let provider = MockProvider {
            root: fixture.path().to_path_buf(),
        };

        let work = TempDir::new().expect("create working tempdir");
        let restored = TempDir::new().expect("create restored tempdir");

        let snapshot_a = work.path().join("remote-a.gcl");
        let snapshot_b = work.path().join("remote-b.gcl");

        build_snapshot_from_provider(
            &provider,
            "mock://example/repo",
            &snapshot_a,
            &BuildOptions::default(),
        )
        .expect("build snapshot from mock provider");
        materialize_snapshot(&snapshot_a, restored.path()).expect("materialize mock snapshot");
        build_snapshot(restored.path(), &snapshot_b)
            .expect("build local snapshot after materialize");

        let a = fs::read(&snapshot_a).expect("read remote snapshot");
        let b = fs::read(&snapshot_b).expect("read rebuilt local snapshot");
        assert_eq!(a, b, "remote->materialize->local snapshots differ");
    }

    #[test]
    fn git_mode_excludes_untracked_by_default() {
        let repo = TempDir::new().expect("create temp repo");
        init_git_repo(repo.path());

        fs::write(repo.path().join("tracked.txt"), b"tracked\n").expect("write tracked");
        run_git(repo.path(), &["add", "tracked.txt"]);
        run_git(repo.path(), &["commit", "-m", "initial"]);

        fs::write(repo.path().join("untracked.txt"), b"untracked\n").expect("write untracked");

        let snapshot = repo.path().join("snapshot.gcl");
        build_snapshot(repo.path(), &snapshot).expect("build snapshot");

        let text = fs::read_to_string(snapshot).expect("read snapshot");
        assert!(text.contains("\"tracked.txt\""));
        assert!(!text.contains("\"untracked.txt\""));
    }

    #[test]
    fn git_mode_include_untracked_respects_gitignore() {
        let repo = TempDir::new().expect("create temp repo");
        init_git_repo(repo.path());

        fs::write(repo.path().join("tracked.txt"), b"tracked\n").expect("write tracked");
        fs::write(repo.path().join(".gitignore"), b"ignored.txt\n").expect("write gitignore");
        run_git(repo.path(), &["add", "tracked.txt", ".gitignore"]);
        run_git(repo.path(), &["commit", "-m", "initial"]);

        fs::write(repo.path().join("ignored.txt"), b"ignored\n").expect("write ignored");
        fs::write(repo.path().join("new.txt"), b"new\n").expect("write new");

        let snapshot = repo.path().join("snapshot.gcl");
        build_snapshot_with_options(
            repo.path(),
            &snapshot,
            &BuildOptions {
                include_untracked: true,
                require_clean: false,
            },
        )
        .expect("build snapshot");

        let text = fs::read_to_string(snapshot).expect("read snapshot");
        assert!(text.contains("\"tracked.txt\""));
        assert!(text.contains("\"new.txt\""));
        assert!(!text.contains("\"ignored.txt\""));
    }

    #[test]
    fn git_mode_require_clean_rejects_dirty_tree() {
        let repo = TempDir::new().expect("create temp repo");
        init_git_repo(repo.path());

        fs::write(repo.path().join("tracked.txt"), b"tracked\n").expect("write tracked");
        run_git(repo.path(), &["add", "tracked.txt"]);
        run_git(repo.path(), &["commit", "-m", "initial"]);

        fs::write(repo.path().join("tracked.txt"), b"changed\n").expect("modify tracked");

        let snapshot = repo.path().join("snapshot.gcl");
        let result = build_snapshot_with_options(
            repo.path(),
            &snapshot,
            &BuildOptions {
                include_untracked: false,
                require_clean: true,
            },
        );
        assert!(
            result.is_err(),
            "dirty tree should fail with --require-clean"
        );
    }

    #[test]
    fn git_mode_require_clean_rejects_staged_changes() {
        let repo = TempDir::new().expect("create temp repo");
        init_git_repo(repo.path());

        fs::write(repo.path().join("tracked.txt"), b"tracked\n").expect("write tracked");
        run_git(repo.path(), &["add", "tracked.txt"]);
        run_git(repo.path(), &["commit", "-m", "initial"]);

        fs::write(repo.path().join("staged.txt"), b"staged\n").expect("write staged");
        run_git(repo.path(), &["add", "staged.txt"]);

        let snapshot = repo.path().join("snapshot.gcl");
        let result = build_snapshot_with_options(
            repo.path(),
            &snapshot,
            &BuildOptions {
                include_untracked: false,
                require_clean: true,
            },
        );
        assert!(result.is_err(), "staged change should fail require_clean");
    }

    #[test]
    fn git_mode_require_clean_rejects_rename_inside_source_to_outside() {
        let repo = TempDir::new().expect("create temp repo");
        init_git_repo(repo.path());

        let source_dir = repo.path().join("src");
        fs::create_dir_all(&source_dir).expect("create source dir");
        fs::write(source_dir.join("tracked.txt"), b"tracked\n").expect("write tracked");
        run_git(repo.path(), &["add", "src/tracked.txt"]);
        run_git(repo.path(), &["commit", "-m", "initial"]);

        run_git(repo.path(), &["mv", "src/tracked.txt", "moved.txt"]);

        let snapshot = repo.path().join("snapshot.gcl");
        let result = build_snapshot_with_options(
            &source_dir,
            &snapshot,
            &BuildOptions {
                include_untracked: false,
                require_clean: true,
            },
        );
        assert!(
            result.is_err(),
            "rename moving file out of source prefix should fail require_clean"
        );
    }

    #[test]
    fn git_mode_require_clean_ignores_untracked_outside_source_prefix() {
        let repo = TempDir::new().expect("create temp repo");
        init_git_repo(repo.path());

        let source_dir = repo.path().join("src");
        fs::create_dir_all(&source_dir).expect("create source dir");
        fs::write(source_dir.join("tracked.txt"), b"tracked\n").expect("write tracked");
        run_git(repo.path(), &["add", "src/tracked.txt"]);
        run_git(repo.path(), &["commit", "-m", "initial"]);

        fs::write(repo.path().join("outside.txt"), b"outside\n").expect("write outside file");

        let snapshot = repo.path().join("snapshot.gcl");
        let result = build_snapshot_with_options(
            &source_dir,
            &snapshot,
            &BuildOptions {
                include_untracked: false,
                require_clean: true,
            },
        );
        assert!(
            result.is_ok(),
            "untracked file outside source prefix should not fail require_clean"
        );
    }

    #[test]
    fn git_mode_require_clean_rejects_unmerged_conflict() {
        let repo = TempDir::new().expect("create temp repo");
        init_git_repo(repo.path());
        let base_branch = current_git_branch(repo.path());

        fs::write(repo.path().join("conflict.txt"), b"base\n").expect("write base");
        run_git(repo.path(), &["add", "conflict.txt"]);
        run_git(repo.path(), &["commit", "-m", "base"]);

        run_git(repo.path(), &["checkout", "-b", "feature"]);
        fs::write(repo.path().join("conflict.txt"), b"feature\n").expect("write feature");
        run_git(repo.path(), &["commit", "-am", "feature"]);

        run_git(repo.path(), &["checkout", &base_branch]);
        fs::write(repo.path().join("conflict.txt"), b"main\n").expect("write main");
        run_git(repo.path(), &["commit", "-am", "main"]);

        let merge_status = Command::new("git")
            .args(["merge", "feature"])
            .current_dir(repo.path())
            .status()
            .expect("run merge");
        assert!(!merge_status.success(), "merge should produce conflict");

        let snapshot = repo.path().join("snapshot.gcl");
        let result = build_snapshot_with_options(
            repo.path(),
            &snapshot,
            &BuildOptions {
                include_untracked: false,
                require_clean: true,
            },
        );
        assert!(
            result.is_err(),
            "unmerged conflict should fail require_clean"
        );
    }

    #[test]
    fn parse_porcelain_entry_rejects_too_short() {
        let err = parse_porcelain_entry(b"M").expect_err("short entry should fail");
        assert!(matches!(err, GitClosureError::Parse(_)));
    }

    #[test]
    fn parse_porcelain_entry_rejects_missing_xy_separator() {
        let err = parse_porcelain_entry(b"MMfile.txt").expect_err("missing separator should fail");
        assert!(matches!(err, GitClosureError::Parse(_)));
    }

    #[test]
    fn parse_porcelain_entry_accepts_valid_record() {
        let (xy, path) = parse_porcelain_entry(b" M file.txt").expect("valid entry");
        assert_eq!(xy, [b' ', b'M']);
        assert_eq!(path, "file.txt");
    }

    #[test]
    fn evaluate_git_status_porcelain_rejects_copy_source_within_prefix() {
        let stdout = b"C  copied.txt\0src/original.txt\0";
        let err = evaluate_git_status_porcelain(stdout, Path::new("src"))
            .expect_err("copy source under prefix should fail");
        assert!(matches!(err, GitClosureError::Parse(_)));
    }

    #[test]
    fn evaluate_git_status_porcelain_consumes_copy_source_chunk() {
        let stdout = b"C  outside/new.txt\0outside/original.txt\0";
        evaluate_git_status_porcelain(stdout, Path::new("src"))
            .expect("copy outside prefix should not fail and source chunk must be consumed");
    }

    #[test]
    fn ensure_git_source_is_clean_non_repo_returns_command_exit_failure() {
        let temp = TempDir::new().expect("create tempdir");
        let context = GitRepoContext {
            workdir: temp.path().to_path_buf(),
            source_prefix: PathBuf::new(),
        };

        let err =
            ensure_git_source_is_clean(&context).expect_err("git status in non-repo should fail");
        match err {
            GitClosureError::CommandExitFailure {
                command, stderr, ..
            } => {
                assert_eq!(command, "git");
                assert!(!stderr.is_empty(), "stderr should be captured");
            }
            other => panic!("expected CommandExitFailure, got {other:?}"),
        }
    }

    #[test]
    fn git_ls_files_non_repo_returns_command_exit_failure() {
        let temp = TempDir::new().expect("create tempdir");
        let context = GitRepoContext {
            workdir: temp.path().to_path_buf(),
            source_prefix: PathBuf::new(),
        };

        let err = git_ls_files(&context, false).expect_err("git ls-files in non-repo should fail");
        match err {
            GitClosureError::CommandExitFailure {
                command, stderr, ..
            } => {
                assert_eq!(command, "git");
                assert!(!stderr.is_empty(), "stderr should be captured");
            }
            other => panic!("expected CommandExitFailure, got {other:?}"),
        }
    }

    fn init_git_repo(path: &Path) {
        run_git(path, &["init"]);
        run_git(path, &["config", "user.name", "git-closure-test"]);
        run_git(
            path,
            &["config", "user.email", "git-closure-test@example.com"],
        );
    }

    fn run_git(path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("failed to run git command");
        assert!(status.success(), "git command failed: git {:?}", args);
    }

    fn current_git_branch(path: &Path) -> String {
        let output = Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .current_dir(path)
            .output()
            .expect("failed to read current git branch");
        assert!(output.status.success(), "failed to resolve current branch");
        String::from_utf8(output.stdout)
            .expect("branch output should be UTF-8")
            .trim()
            .to_string()
    }

    fn read_snapshot_hash(snapshot: &Path) -> String {
        let text = fs::read_to_string(snapshot).expect("read snapshot text");
        for line in text.lines() {
            if let Some(value) = line.strip_prefix(";; snapshot-hash:") {
                return value.trim().to_string();
            }
            if let Some(value) = line.strip_prefix(";; format-hash:") {
                return value.trim().to_string();
            }
        }
        panic!("missing snapshot hash header");
    }

    #[cfg(unix)]
    fn symlink_snapshot_hash(path: &str, target: &str) -> String {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update((b"symlink".len() as u64).to_be_bytes());
        hasher.update(b"symlink");
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path.as_bytes());
        hasher.update((target.len() as u64).to_be_bytes());
        hasher.update(target.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    fn manual_snapshot_hash_with_length_prefix(files: &[SnapshotFile]) -> String {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        for file in files {
            if let Some(target) = &file.symlink_target {
                update_length_prefixed(&mut hasher, b"symlink");
                update_length_prefixed(&mut hasher, file.path.as_bytes());
                update_length_prefixed(&mut hasher, target.as_bytes());
            } else {
                update_length_prefixed(&mut hasher, b"regular");
                update_length_prefixed(&mut hasher, file.path.as_bytes());
                update_length_prefixed(&mut hasher, file.mode.as_bytes());
                update_length_prefixed(&mut hasher, file.sha256.as_bytes());
            }
        }

        format!("{:x}", hasher.finalize())
    }

    fn update_length_prefixed(hasher: &mut sha2::Sha256, bytes: &[u8]) {
        use sha2::Digest;
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }

    #[test]
    fn serialization_round_trips_all_byte_values() {
        let source = TempDir::new().expect("create source tempdir");
        let restored = TempDir::new().expect("create restored tempdir");

        let payload: Vec<u8> = (0u8..=255u8).collect();
        fs::write(source.path().join("all-bytes.bin"), &payload).expect("write all-bytes file");

        let snapshot = source.path().join("snapshot.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");
        verify_snapshot(&snapshot).expect("verify snapshot");
        materialize_snapshot(&snapshot, restored.path()).expect("materialize snapshot");

        let restored_payload =
            fs::read(restored.path().join("all-bytes.bin")).expect("read restored all-bytes file");
        assert_eq!(restored_payload, payload);
    }

    #[test]
    fn serialization_round_trips_unicode_edge_cases() {
        let source = TempDir::new().expect("create source tempdir");
        let restored = TempDir::new().expect("create restored tempdir");

        let content = ["", "\u{feff}", "\u{0000}", "\u{fffd}", "\u{1f642}"].join("|");
        let expected = content.as_bytes().to_vec();
        fs::write(source.path().join("unicode.txt"), expected.clone()).expect("write unicode file");

        let snapshot = source.path().join("snapshot.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");
        verify_snapshot(&snapshot).expect("verify snapshot");
        materialize_snapshot(&snapshot, restored.path()).expect("materialize snapshot");

        let restored_bytes =
            fs::read(restored.path().join("unicode.txt")).expect("read restored unicode file");
        assert_eq!(restored_bytes, expected);
    }

    #[test]
    fn serialization_round_trips_special_character_paths() {
        let source = TempDir::new().expect("create source tempdir");
        let restored = TempDir::new().expect("create restored tempdir");

        let special_dir = source.path().join("dir with spaces");
        fs::create_dir_all(&special_dir).expect("create special directory");
        let special_path = special_dir.join("file \"quoted\" [x].txt");
        let expected = b"special path content\n";
        fs::write(&special_path, expected).expect("write special path file");

        let snapshot = source.path().join("snapshot.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");
        verify_snapshot(&snapshot).expect("verify snapshot");
        materialize_snapshot(&snapshot, restored.path()).expect("materialize snapshot");

        let restored_bytes = fs::read(
            restored
                .path()
                .join("dir with spaces/file \"quoted\" [x].txt"),
        )
        .expect("read restored special path file");
        assert_eq!(restored_bytes, expected);
    }

    #[test]
    fn quote_string_matches_lexpr_printer() {
        let sample = "line1\nline2\u{0000}\u{fffd}\u{1f642}\\\"";
        let expected = lexpr::to_string(&lexpr::Value::string(sample)).expect("print with lexpr");
        assert_eq!(super::quote_string(sample), expected);
    }

    #[test]
    fn serialize_symlink_type_field_uses_quote_string() {
        assert_eq!(super::quote_string("symlink"), "\"symlink\"");

        let source = TempDir::new().expect("create tempdir");
        let target_path = source.path().join("target.txt");
        fs::write(&target_path, b"payload\n").expect("write target");

        #[cfg(unix)]
        std::os::unix::fs::symlink("target.txt", source.path().join("link"))
            .expect("create symlink");

        let snapshot = source.path().join("snap.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");

        let text = fs::read_to_string(&snapshot).expect("read snapshot");
        assert!(
            text.contains(":type \"symlink\""),
            "serialized snapshot must contain :type with quoted string, got:\n{}",
            text
        );

        verify_snapshot(&snapshot).expect("verify must pass after serialization fix");
    }

    #[test]
    #[should_panic(expected = "MockProvider called with unexpected source")]
    fn mock_provider_panics_on_wrong_source() {
        let provider = MockProvider {
            root: std::path::PathBuf::new(),
        };
        let _ = provider.fetch("wrong://source");
    }

    struct MockProvider {
        root: std::path::PathBuf,
    }

    impl Provider for MockProvider {
        fn fetch(&self, source: &str) -> std::result::Result<FetchedSource, GitClosureError> {
            if source != "mock://example/repo" {
                panic!("MockProvider called with unexpected source: {source}");
            }
            Ok(FetchedSource::local(self.root.clone()))
        }
    }

    // ── T-19: Forward compatibility — unknown plist keys ──────────────────────

    /// A snapshot emitted by a *future* version of git-closure that adds a new
    /// `:mtime` key must be accepted by the current parser.  The README promises
    /// "unknown plist keys are silently ignored by any conformant reader."
    #[test]
    fn parse_snapshot_silently_ignores_unknown_plist_key() {
        let source = TempDir::new().expect("create source tempdir");
        fs::write(source.path().join("hello.txt"), b"hello\n").expect("write hello.txt");

        let snapshot = source.path().join("snap.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");

        // Inject an unknown future key into the plist section.
        // The snapshot-hash is computed over path/mode/sha256 — not over the
        // plist text — so adding an extra key does not invalidate the hash.
        let text = fs::read_to_string(&snapshot).expect("read snapshot");
        let modified = text.replace(":mode ", ":mtime \"1234567890\"\n     :mode ");

        let modified_snap = source.path().join("modified.gcl");
        fs::write(&modified_snap, modified).expect("write modified snapshot");

        // Must succeed — T-19 regression guard.
        verify_snapshot(&modified_snap)
            .expect("snapshot with unknown plist key must verify successfully");
    }

    /// Unknown keys must be ignored for *all* three public entry points
    /// (verify, materialize).  Materialize additionally must restore the
    /// actual file content correctly.
    #[test]
    fn materialize_snapshot_silently_ignores_unknown_plist_key() {
        let source = TempDir::new().expect("create source tempdir");
        fs::write(source.path().join("data.txt"), b"payload\n").expect("write data.txt");

        let snapshot = source.path().join("snap.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");

        let text = fs::read_to_string(&snapshot).expect("read snapshot");
        // Inject two unknown keys of different positions.
        let modified = text
            .replace(":path ", ":x-future-key \"v\"\n     :path ")
            .replace(":sha256 ", ":x-other \"42\"\n     :sha256 ");

        let modified_snap = source.path().join("modified.gcl");
        fs::write(&modified_snap, modified).expect("write modified snapshot");

        let restored = TempDir::new().expect("create restored tempdir");
        materialize_snapshot(&modified_snap, restored.path())
            .expect("materialize with unknown keys must succeed");

        let bytes = fs::read(restored.path().join("data.txt")).expect("read restored data.txt");
        assert_eq!(bytes, b"payload\n");
    }

    /// Forward-compat round-trip: a snapshot written with extra keys can be
    /// parsed, re-serialized (via materialize + rebuild), and produce an
    /// identical snapshot-hash as the original.
    #[test]
    fn snapshot_with_unknown_key_roundtrip_preserves_hash() {
        let source = TempDir::new().expect("create source tempdir");
        fs::write(source.path().join("a.txt"), b"round\n").expect("write a.txt");

        let snap_orig = source.path().join("orig.gcl");
        build_snapshot(source.path(), &snap_orig).expect("build original snapshot");

        let text = fs::read_to_string(&snap_orig).expect("read original snapshot");
        let modified = text.replace(":size ", ":git-object-id \"deadbeef\"\n     :size ");

        let snap_future = source.path().join("future.gcl");
        fs::write(&snap_future, modified).expect("write future snapshot");

        let restored = TempDir::new().expect("create restored tempdir");
        materialize_snapshot(&snap_future, restored.path()).expect("materialize future snapshot");

        let snap_rebuilt = source.path().join("rebuilt.gcl");
        build_snapshot(restored.path(), &snap_rebuilt).expect("rebuild snapshot");

        let hash_orig = read_snapshot_hash(&snap_orig);
        let hash_rebuilt = read_snapshot_hash(&snap_rebuilt);
        assert_eq!(
            hash_orig, hash_rebuilt,
            "snapshot-hash must be identical after round-trip through future-format snapshot"
        );
    }

    // ── T-26b: materialize must reject non-empty output directories ───────────

    /// Materializing into a directory that already contains files must fail
    /// with a clear diagnostic — this prevents TOCTOU-style symlink-escalation
    /// attacks via pre-planted symlinks in the output directory.
    #[test]
    fn materialize_into_non_empty_directory_fails_with_clear_error() {
        let source = TempDir::new().expect("create source tempdir");
        fs::write(source.path().join("a.txt"), b"content\n").expect("write a.txt");

        let snapshot = source.path().join("snap.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");

        let output = TempDir::new().expect("create output tempdir");
        fs::write(
            output.path().join("existing_file.txt"),
            b"I was here first\n",
        )
        .expect("write pre-existing file");

        let err = materialize_snapshot(&snapshot, output.path())
            .expect_err("materialize into non-empty directory must fail");

        match err {
            GitClosureError::Parse(msg) => {
                assert!(
                    msg.contains("empty"),
                    "error message should mention 'empty', got: {msg}"
                );
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    /// An *existing but empty* directory is an acceptable target.  `create_dir_all`
    /// makes this scenario realistic (pre-created target dirs in scripts).
    #[test]
    fn materialize_into_existing_empty_directory_succeeds() {
        let source = TempDir::new().expect("create source tempdir");
        fs::write(source.path().join("a.txt"), b"content\n").expect("write a.txt");

        let snapshot = source.path().join("snap.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");

        let output = TempDir::new().expect("create output tempdir");
        // Directory pre-exists but is empty — must be accepted.
        materialize_snapshot(&snapshot, output.path())
            .expect("materialize into existing empty directory must succeed");

        let bytes = fs::read(output.path().join("a.txt")).expect("read materialized a.txt");
        assert_eq!(bytes, b"content\n");
    }

    /// A subdirectory structure already present in output must also be rejected.
    #[test]
    fn materialize_into_directory_with_subdirectory_fails() {
        let source = TempDir::new().expect("create source tempdir");
        fs::write(source.path().join("a.txt"), b"content\n").expect("write a.txt");

        let snapshot = source.path().join("snap.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");

        let output = TempDir::new().expect("create output tempdir");
        fs::create_dir(output.path().join("subdir")).expect("create subdir");

        let err = materialize_snapshot(&snapshot, output.path())
            .expect_err("materialize into directory with subdir must fail");
        assert!(
            matches!(err, GitClosureError::Parse(_)),
            "expected Parse error, got {err:?}"
        );
    }
}
