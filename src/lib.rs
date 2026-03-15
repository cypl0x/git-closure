use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

pub mod providers;

use providers::{fetch_source, Provider, ProviderKind};

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotFile {
    path: String,
    sha256: String,
    mode: String,
    size: u64,
    encoding: Option<String>,
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
    let source = fs::canonicalize(source)
        .with_context(|| format!("failed to canonicalize source path: {}", source.display()))?;

    if !source.is_dir() {
        bail!("source is not a directory: {}", source.display());
    }

    let mut files = collect_files(&source, options)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let snapshot_hash = compute_snapshot_hash(&files);
    let serialized = serialize_snapshot(&files, &snapshot_hash);

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent directory: {}", parent.display()))?;
    }

    let mut writer = fs::File::create(output)
        .with_context(|| format!("failed to create output file: {}", output.display()))?;
    writer
        .write_all(serialized.as_bytes())
        .with_context(|| format!("failed to write output file: {}", output.display()))?;

    Ok(())
}

pub fn materialize_snapshot(snapshot: &Path, output: &Path) -> Result<()> {
    let text = fs::read_to_string(snapshot)
        .with_context(|| format!("failed to read snapshot: {}", snapshot.display()))?;

    let (header, files) = parse_snapshot(&text)?;

    let recomputed = compute_snapshot_hash(&files);
    if recomputed != header.snapshot_hash {
        bail!(
            "snapshot hash mismatch: expected {}, got {}",
            header.snapshot_hash,
            recomputed
        );
    }

    fs::create_dir_all(output)
        .with_context(|| format!("failed to create output directory: {}", output.display()))?;

    let output_abs = fs::canonicalize(output).with_context(|| {
        format!(
            "failed to canonicalize output directory: {}",
            output.display()
        )
    })?;

    for file in files {
        let relative = sanitized_relative_path(&file.path)?;
        let destination = output_abs.join(relative);

        if !destination.starts_with(&output_abs) {
            bail!("refusing to write outside output directory: {}", file.path);
        }

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent directory: {}", parent.display())
            })?;
        }

        let digest = sha256_hex(&file.content);
        if digest != file.sha256 {
            bail!(
                "content hash mismatch for {}: expected {}, got {}",
                file.path,
                file.sha256,
                digest
            );
        }

        fs::write(&destination, &file.content)
            .with_context(|| format!("failed to write file: {}", destination.display()))?;

        let mode = u32::from_str_radix(&file.mode, 8)
            .with_context(|| format!("invalid octal mode for {}: {}", file.path, file.mode))?;
        let permissions = fs::Permissions::from_mode(mode);
        fs::set_permissions(&destination, permissions)
            .with_context(|| format!("failed to set permissions: {}", destination.display()))?;
    }

    Ok(())
}

pub fn verify_snapshot(snapshot: &Path) -> Result<VerifyReport> {
    let text = fs::read_to_string(snapshot)
        .with_context(|| format!("failed to read snapshot: {}", snapshot.display()))?;

    let (header, files) = parse_snapshot(&text)?;

    let recomputed = compute_snapshot_hash(&files);
    if recomputed != header.snapshot_hash {
        bail!(
            "snapshot hash mismatch: expected {}, got {}",
            header.snapshot_hash,
            recomputed
        );
    }

    for file in &files {
        let _ = sanitized_relative_path(&file.path)?;

        let digest = sha256_hex(&file.content);
        if digest != file.sha256 {
            bail!(
                "content hash mismatch for {}: expected {}, got {}",
                file.path,
                file.sha256,
                digest
            );
        }

        if file.content.len() as u64 != file.size {
            bail!(
                "size mismatch for {}: metadata {}, decoded {}",
                file.path,
                file.size,
                file.content.len()
            );
        }

        u32::from_str_radix(&file.mode, 8)
            .with_context(|| format!("invalid octal mode for {}: {}", file.path, file.mode))?;
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
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(source)
            .output();

        let output = match output {
            Ok(output) if output.status.success() => output,
            _ => return Ok(None),
        };

        let workdir = String::from_utf8(output.stdout)
            .context("git returned non-UTF-8 repository root")?
            .trim()
            .to_string();
        let workdir = PathBuf::from(workdir);

        if !source.starts_with(&workdir) {
            return Ok(None);
        }

        let source_prefix = source
            .strip_prefix(&workdir)
            .with_context(|| {
                format!(
                    "failed to determine source prefix under git workdir: {}",
                    source.display()
                )
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
        let metadata = match fs::metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };

        if !metadata.is_file() {
            continue;
        }

        let relative = absolute
            .strip_prefix(context.workdir.join(&context.source_prefix))
            .with_context(|| {
                format!(
                    "failed to create source-relative path for git entry: {}",
                    absolute.display()
                )
            })?;

        let normalized = normalize_relative_path(relative)?;
        let bytes = fs::read(&absolute)
            .with_context(|| format!("failed to read file bytes: {}", absolute.display()))?;
        let sha256 = sha256_hex(&bytes);
        let mode = format!("{:o}", metadata.permissions().mode() & 0o777);
        let size = bytes.len() as u64;
        let encoding = if std::str::from_utf8(&bytes).is_ok() {
            None
        } else {
            Some("base64".to_string())
        };

        files.push(SnapshotFile {
            path: normalized,
            sha256,
            mode,
            size,
            encoding,
            content: bytes,
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
        let entry = entry.context("failed to walk source directory")?;
        let path = entry.path();

        if path == root {
            continue;
        }

        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata for: {}", path.display()))?;

        if !metadata.is_file() {
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("failed to strip source prefix: {}", path.display()))?;

        let normalized = normalize_relative_path(relative)?;
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read file bytes: {}", path.display()))?;

        let sha256 = sha256_hex(&bytes);
        let mode = format!("{:o}", metadata.permissions().mode() & 0o777);
        let size = bytes.len() as u64;
        let encoding = if std::str::from_utf8(&bytes).is_ok() {
            None
        } else {
            Some("base64".to_string())
        };

        collected.push(SnapshotFile {
            path: normalized,
            sha256,
            mode,
            size,
            encoding,
            content: bytes,
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
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(&context.workdir)
        .output()
        .context("failed to run git status for clean check")?;

    if !output.status.success() {
        bail!("git status failed during clean check");
    }

    let mut chunks = output.stdout.split(|b| *b == 0u8);
    while let Some(entry) = chunks.next() {
        if entry.is_empty() {
            continue;
        }

        if entry.len() < 4 {
            continue;
        }

        let path_bytes = &entry[3..];
        let path = std::str::from_utf8(path_bytes)
            .context("git status produced non-UTF-8 path")?
            .trim();

        let repo_relative = Path::new(path);
        if is_within_prefix(repo_relative, &context.source_prefix) {
            bail!(
                "source tree is dirty at {} (use --include-untracked or clean working tree)",
                path
            );
        }

        if entry.starts_with(b"R") || entry.starts_with(b"C") {
            let _ = chunks.next();
        }
    }

    Ok(())
}

fn git_ls_files(context: &GitRepoContext, include_untracked: bool) -> Result<Vec<PathBuf>> {
    let mut args = vec!["ls-files", "-z", "--cached"];
    if include_untracked {
        args.extend(["--others", "--exclude-standard"]);
    }

    let output = Command::new("git")
        .args(&args)
        .current_dir(&context.workdir)
        .output()
        .context("failed to run git ls-files")?;

    if !output.status.success() {
        bail!("git ls-files failed");
    }

    let mut paths = Vec::new();
    for chunk in output.stdout.split(|b| *b == 0u8) {
        if chunk.is_empty() {
            continue;
        }
        let path = std::str::from_utf8(chunk).context("git ls-files produced non-UTF-8 path")?;
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
    if path.is_absolute() {
        bail!("absolute paths are forbidden: {}", path.display());
    }

    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                if part == OsStr::new(".") || part == OsStr::new("..") {
                    bail!("invalid path component in: {}", path.display());
                }
                components.push(
                    part.to_str()
                        .ok_or_else(|| anyhow!("non-UTF-8 path component: {}", path.display()))?
                        .to_string(),
                );
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                bail!("non-normalized path component in: {}", path.display())
            }
        }
    }

    if components.is_empty() {
        bail!("empty relative path");
    }

    Ok(components.join("/"))
}

fn compute_snapshot_hash(files: &[SnapshotFile]) -> String {
    let mut hasher = Sha256::new();
    for file in files {
        hasher.update((file.path.len() as u64).to_be_bytes());
        hasher.update(file.path.as_bytes());
        hasher.update(file.mode.as_bytes());
        hasher.update([0x00]);
        hasher.update(file.sha256.as_bytes());
        hasher.update([0x00]);
    }
    format!("{:x}", hasher.finalize())
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
            String::from_utf8_lossy(&file.content).to_string()
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
    let parsed = lexpr::from_str(body).context("failed to parse S-expression body")?;
    let files = parse_files_value(&parsed)?;

    if files.len() != header.file_count {
        bail!(
            "file count mismatch: header says {}, parsed {}",
            header.file_count,
            files.len()
        );
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
                bail!("legacy format-hash header found; re-snapshot with current tool");
            }
            if let Some(value) = line.strip_prefix(";; snapshot-hash:") {
                snapshot_hash = Some(value.trim().to_string());
            }
            if let Some(value) = line.strip_prefix(";; file-count:") {
                file_count = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .context("invalid file-count header")?,
                );
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

    let snapshot_hash = snapshot_hash.ok_or_else(|| anyhow!("missing snapshot-hash header"))?;
    let file_count = file_count.ok_or_else(|| anyhow!("missing file-count header"))?;
    let body_start = body_start.ok_or_else(|| anyhow!("missing S-expression body"))?;

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
        .ok_or_else(|| anyhow!("snapshot body must be a list"))?;

    let mut files = Vec::with_capacity(root.len());

    for entry in root {
        let pair = entry
            .to_ref_vec()
            .ok_or_else(|| anyhow!("each entry must be a 2-item list"))?;
        if pair.len() != 2 {
            bail!("each entry must contain plist and content");
        }

        let plist = pair[0]
            .to_ref_vec()
            .ok_or_else(|| anyhow!("entry plist must be a list"))?;

        let content_field = pair[1]
            .as_str()
            .ok_or_else(|| anyhow!("entry content must be a string"))?;

        let mut path = None;
        let mut sha256 = None;
        let mut mode = None;
        let mut size = None;
        let mut encoding = None;

        if plist.len() % 2 != 0 {
            bail!("plist key/value pairs are malformed");
        }

        let mut idx = 0usize;
        while idx < plist.len() {
            let key = if let Some(keyword) = plist[idx].as_keyword() {
                keyword
            } else if let Some(symbol) = plist[idx].as_symbol() {
                symbol
                    .strip_prefix(':')
                    .ok_or_else(|| anyhow!("plist symbol keys must start with ':'"))?
            } else {
                bail!("plist keys must be keywords or :symbol values");
            };
            let value = &plist[idx + 1];

            match key {
                "path" => {
                    path = Some(
                        value
                            .as_str()
                            .ok_or_else(|| anyhow!(":path must be a string"))?
                            .to_string(),
                    );
                }
                "sha256" => {
                    sha256 = Some(
                        value
                            .as_str()
                            .ok_or_else(|| anyhow!(":sha256 must be a string"))?
                            .to_string(),
                    );
                }
                "mode" => {
                    mode = Some(
                        value
                            .as_str()
                            .ok_or_else(|| anyhow!(":mode must be a string"))?
                            .to_string(),
                    );
                }
                "size" => {
                    size = Some(
                        value
                            .as_u64()
                            .ok_or_else(|| anyhow!(":size must be a u64"))?,
                    );
                }
                "encoding" => {
                    encoding = Some(
                        value
                            .as_str()
                            .ok_or_else(|| anyhow!(":encoding must be a string"))?
                            .to_string(),
                    );
                }
                other => bail!("unknown metadata key: :{}", other),
            }

            idx += 2;
        }

        let path = path.ok_or_else(|| anyhow!("missing :path"))?;
        let sha256 = sha256.ok_or_else(|| anyhow!("missing :sha256"))?;
        let mode = mode.ok_or_else(|| anyhow!("missing :mode"))?;
        let size = size.ok_or_else(|| anyhow!("missing :size"))?;

        let content = match encoding.as_deref() {
            Some("base64") => BASE64_STANDARD
                .decode(content_field)
                .with_context(|| format!("invalid base64 content for {}", path))?,
            Some(other) => bail!("unsupported encoding for {}: {}", path, other),
            None => content_field.as_bytes().to_vec(),
        };

        if content.len() as u64 != size {
            bail!(
                "size mismatch for {}: metadata {}, decoded {}",
                path,
                size,
                content.len()
            );
        }

        files.push(SnapshotFile {
            path,
            sha256,
            mode,
            size,
            encoding,
            content,
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn quote_string(input: &str) -> String {
    let mut output = String::with_capacity(input.len() + 2);
    output.push('"');
    for ch in input.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push('\n'),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            c if c.is_control() => output.push_str(&format!("\\x{:02x};", c as u32)),
            c => output.push(c),
        }
    }
    output.push('"');
    output
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn sanitized_relative_path(path: &str) -> Result<PathBuf> {
    if path.is_empty() {
        bail!("path is empty");
    }

    let candidate = Path::new(path);

    if candidate.is_absolute() {
        bail!("absolute paths are forbidden: {}", path);
    }

    let mut clean = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                bail!("unsafe path in snapshot: {}", path)
            }
        }
    }

    if clean.as_os_str().is_empty() {
        bail!("path normalizes to empty path: {}", path);
    }

    Ok(clean)
}

#[cfg(test)]
mod tests {
    use super::{
        build_snapshot, build_snapshot_from_provider, build_snapshot_with_options,
        materialize_snapshot, verify_snapshot, BuildOptions,
    };
    use crate::providers::{FetchedSource, Provider};
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn round_trip_is_byte_identical() {
        let source = TempDir::new().expect("create source tempdir");
        let restored = TempDir::new().expect("create restored tempdir");

        let alpha_path = source.path().join("alpha.txt");
        fs::write(&alpha_path, b"alpha\n").expect("write alpha.txt");

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

        let a = fs::read(&snapshot_a).expect("read snapshot-a");
        let b = fs::read(&snapshot_b).expect("read snapshot-b");
        assert_eq!(a, b, "round trip snapshots differ");
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

    struct MockProvider {
        root: std::path::PathBuf,
    }

    impl Provider for MockProvider {
        fn fetch(&self, source: &str) -> anyhow::Result<FetchedSource> {
            if source != "mock://example/repo" {
                anyhow::bail!("unexpected mock source: {source}");
            }
            Ok(FetchedSource::local(self.root.clone()))
        }
    }
}
