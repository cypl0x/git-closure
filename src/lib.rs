<<<<<<< HEAD
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotFile {
    path: String,
    sha256: String,
    mode: String,
    size: u64,
    encoding: Option<String>,
    content: Vec<u8>,
}

pub fn build_snapshot(source: &Path, output: &Path) -> Result<()> {
    let source = fs::canonicalize(source)
        .with_context(|| format!("failed to canonicalize source path: {}", source.display()))?;

    if !source.is_dir() {
        bail!("source is not a directory: {}", source.display());
    }

    let mut files = collect_files(&source)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let format_hash = compute_format_hash(&files);
    let serialized = serialize_snapshot(&files, &format_hash);

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

    let recomputed = compute_format_hash(&files);
    if recomputed != header.format_hash {
        bail!(
            "format hash mismatch: expected {}, got {}",
            header.format_hash,
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

pub fn verify_snapshot(snapshot: &Path) -> Result<()> {
    let text = fs::read_to_string(snapshot)
        .with_context(|| format!("failed to read snapshot: {}", snapshot.display()))?;

    let (header, files) = parse_snapshot(&text)?;

    let recomputed = compute_format_hash(&files);
    if recomputed != header.format_hash {
        bail!(
            "format hash mismatch: expected {}, got {}",
            header.format_hash,
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

    Ok(())
}

fn collect_files(root: &Path) -> Result<Vec<SnapshotFile>> {
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

fn compute_format_hash(files: &[SnapshotFile]) -> String {
    let mut hasher = Sha256::new();
    for file in files {
        hasher.update(&file.content);
    }
    format!("{:x}", hasher.finalize())
}

fn serialize_snapshot(files: &[SnapshotFile], format_hash: &str) -> String {
    let mut output = String::new();

    output.push_str(";; git-closure snapshot v0.1\n");
    output.push_str(&format!(";; format-hash: {}\n", format_hash));
    output.push_str(&format!(";; file-count: {}\n", files.len()));
    output.push('\n');
    output.push_str("(\n");

    for file in files {
        output.push_str("  ((:path ");
        output.push_str(&quote_string(&file.path));
        output.push_str(" :sha256 ");
        output.push_str(&quote_string(&file.sha256));
        output.push_str(" :mode ");
        output.push_str(&quote_string(&file.mode));
        output.push_str(" :size ");
        output.push_str(&file.size.to_string());
        if let Some(encoding) = &file.encoding {
            output.push_str(" :encoding ");
            output.push_str(&quote_string(encoding));
        }
        output.push_str(") ");

        let content_string = if file.encoding.as_deref() == Some("base64") {
            BASE64_STANDARD.encode(&file.content)
        } else {
            String::from_utf8_lossy(&file.content).to_string()
        };

        output.push_str(&quote_string(&content_string));
        output.push_str(")\n");
    }

    output.push_str(")\n");
    output
}

#[derive(Debug)]
struct SnapshotHeader {
    format_hash: String,
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
    let mut format_hash = None;
    let mut file_count = None;
    let mut body_start = None;
    let mut cursor = 0usize;

    for line in input.lines() {
        let line_len = line.len();
        if line.starts_with(";;") {
            if let Some(value) = line.strip_prefix(";; format-hash:") {
                format_hash = Some(value.trim().to_string());
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

    let format_hash = format_hash.ok_or_else(|| anyhow!("missing format-hash header"))?;
    let file_count = file_count.ok_or_else(|| anyhow!("missing file-count header"))?;
    let body_start = body_start.ok_or_else(|| anyhow!("missing S-expression body"))?;

    let body = &input[body_start..];

    Ok((
        SnapshotHeader {
            format_hash,
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
            '\n' => output.push_str("\\n"),
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
    use super::{build_snapshot, materialize_snapshot, verify_snapshot};
    use std::fs;
    use std::io::Write;

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
            ";; git-closure snapshot v0.1\n;; format-hash: {digest}\n;; file-count: 1\n\n(\n  ((:path \"../escape.txt\" :sha256 \"{digest}\" :mode \"644\" :size 1) \"x\")\n)\n"
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

        verify_snapshot(&snapshot).expect("verify should pass");
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
            ";; git-closure snapshot v0.1\n;; format-hash: deadbeef\n;; file-count: 1\n\n(\n  ((:path \"x.txt\" :sha256 \"{digest}\" :mode \"644\" :size 1) \"x\")\n)\n"
        );
        fs::write(&snapshot, snapshot_text).expect("write invalid snapshot");

        let result = verify_snapshot(&snapshot);
        assert!(result.is_err(), "verify should reject bad format hash");
    }
}
||||||| parent of 8191579 (feat: add deterministic build and materialize commands)
=======
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotFile {
    path: String,
    sha256: String,
    mode: String,
    size: u64,
    encoding: Option<String>,
    content: Vec<u8>,
}

pub fn build_snapshot(source: &Path, output: &Path) -> Result<()> {
    let source = fs::canonicalize(source)
        .with_context(|| format!("failed to canonicalize source path: {}", source.display()))?;

    if !source.is_dir() {
        bail!("source is not a directory: {}", source.display());
    }

    let mut files = collect_files(&source)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let format_hash = compute_format_hash(&files);
    let serialized = serialize_snapshot(&files, &format_hash);

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

    let recomputed = compute_format_hash(&files);
    if recomputed != header.format_hash {
        bail!(
            "format hash mismatch: expected {}, got {}",
            header.format_hash,
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

pub fn verify_snapshot(snapshot: &Path) -> Result<()> {
    let text = fs::read_to_string(snapshot)
        .with_context(|| format!("failed to read snapshot: {}", snapshot.display()))?;

    let (header, files) = parse_snapshot(&text)?;

    let recomputed = compute_format_hash(&files);
    if recomputed != header.format_hash {
        bail!(
            "format hash mismatch: expected {}, got {}",
            header.format_hash,
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

    Ok(())
}

fn collect_files(root: &Path) -> Result<Vec<SnapshotFile>> {
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

fn compute_format_hash(files: &[SnapshotFile]) -> String {
    let mut hasher = Sha256::new();
    for file in files {
        hasher.update(&file.content);
    }
    format!("{:x}", hasher.finalize())
}

fn serialize_snapshot(files: &[SnapshotFile], format_hash: &str) -> String {
    let mut output = String::new();

    output.push_str(";; git-closure snapshot v0.1\n");
    output.push_str(&format!(";; format-hash: {}\n", format_hash));
    output.push_str(&format!(";; file-count: {}\n", files.len()));
    output.push('\n');
    output.push_str("(\n");

    for file in files {
        output.push_str("  ((:path ");
        output.push_str(&quote_string(&file.path));
        output.push_str(" :sha256 ");
        output.push_str(&quote_string(&file.sha256));
        output.push_str(" :mode ");
        output.push_str(&quote_string(&file.mode));
        output.push_str(" :size ");
        output.push_str(&file.size.to_string());
        if let Some(encoding) = &file.encoding {
            output.push_str(" :encoding ");
            output.push_str(&quote_string(encoding));
        }
        output.push_str(") ");

        let content_string = if file.encoding.as_deref() == Some("base64") {
            BASE64_STANDARD.encode(&file.content)
        } else {
            String::from_utf8_lossy(&file.content).to_string()
        };

        output.push_str(&quote_string(&content_string));
        output.push_str(")\n");
    }

    output.push_str(")\n");
    output
}

#[derive(Debug)]
struct SnapshotHeader {
    format_hash: String,
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
    let mut format_hash = None;
    let mut file_count = None;
    let mut body_start = None;
    let mut cursor = 0usize;

    for line in input.lines() {
        let line_len = line.len();
        if line.starts_with(";;") {
            if let Some(value) = line.strip_prefix(";; format-hash:") {
                format_hash = Some(value.trim().to_string());
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

    let format_hash = format_hash.ok_or_else(|| anyhow!("missing format-hash header"))?;
    let file_count = file_count.ok_or_else(|| anyhow!("missing file-count header"))?;
    let body_start = body_start.ok_or_else(|| anyhow!("missing S-expression body"))?;

    let body = &input[body_start..];

    Ok((
        SnapshotHeader {
            format_hash,
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
            '\n' => output.push_str("\\n"),
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
    use super::{build_snapshot, materialize_snapshot, verify_snapshot};
    use std::fs;
    use std::io::Write;

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
            ";; git-closure snapshot v0.1\n;; format-hash: {digest}\n;; file-count: 1\n\n(\n  ((:path \"../escape.txt\" :sha256 \"{digest}\" :mode \"644\" :size 1) \"x\")\n)\n"
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

        verify_snapshot(&snapshot).expect("verify should pass");
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
            ";; git-closure snapshot v0.1\n;; format-hash: deadbeef\n;; file-count: 1\n\n(\n  ((:path \"x.txt\" :sha256 \"{digest}\" :mode \"644\" :size 1) \"x\")\n)\n"
        );
        fs::write(&snapshot, snapshot_text).expect("write invalid snapshot");

        let result = verify_snapshot(&snapshot);
        assert!(result.is_err(), "verify should reject bad format hash");
    }
}
>>>>>>> 8191579 (feat: add deterministic build and materialize commands)
