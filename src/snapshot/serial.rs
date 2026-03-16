/// S-expression serialization and deserialization for `.gcl` snapshot files.
use std::fs;
use std::path::Path;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;

use crate::error::GitClosureError;
use crate::utils::io_error_with_path;

use super::{ListEntry, Result, SnapshotFile, SnapshotHeader};

// ── Serialization ─────────────────────────────────────────────────────────────

/// Serializes `files` into the canonical `.gcl` S-expression format.
///
/// `files` must be in lexicographic path order (the caller is responsible).
/// `header.git_rev` and `header.git_branch` are emitted as informational
/// comments but are **not** included in the structural `snapshot_hash`.
pub(crate) fn serialize_snapshot(files: &[SnapshotFile], header: &SnapshotHeader) -> String {
    let mut output = String::new();

    output.push_str(";; git-closure snapshot v0.1\n");
    output.push_str(&format!(";; snapshot-hash: {}\n", header.snapshot_hash));
    output.push_str(&format!(";; file-count: {}\n", files.len()));
    if let Some(rev) = &header.git_rev {
        output.push_str(&format!(";; git-rev: {rev}\n"));
    }
    if let Some(branch) = &header.git_branch {
        output.push_str(&format!(";; git-branch: {branch}\n"));
    }
    for (key, value) in &header.extra_headers {
        output.push_str(&format!(";; {key}: {value}\n"));
    }
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

/// Serializes a lexpr `Value` as a quoted S-expression string.
pub(crate) fn quote_string(input: &str) -> String {
    lexpr::to_string(&lexpr::Value::string(input))
        .expect("lexpr string serialization should not fail")
}

// ── Deserialization ───────────────────────────────────────────────────────────

/// Parses the full text of a `.gcl` snapshot into a header and file list.
///
/// Files in the returned vector are guaranteed to be in lexicographic path
/// order.  The `header.file_count` is cross-checked against the number of
/// parsed entries.
pub(crate) fn parse_snapshot(input: &str) -> Result<(SnapshotHeader, Vec<SnapshotFile>)> {
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
    let mut git_rev = None;
    let mut git_branch = None;
    let mut extra_headers = Vec::new();
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
            if let Some(value) = line.strip_prefix(";; git-rev:") {
                git_rev = Some(value.trim().to_string());
            }
            if let Some(value) = line.strip_prefix(";; git-branch:") {
                git_branch = Some(value.trim().to_string());
            }
            if let Some(rest) = line.strip_prefix(";; ") {
                if let Some((raw_key, raw_value)) = rest.split_once(':') {
                    let key = raw_key.trim();
                    if key != "snapshot-hash"
                        && key != "file-count"
                        && key != "git-rev"
                        && key != "git-branch"
                        && key != "format-hash"
                        && !key.is_empty()
                    {
                        extra_headers.push((key.to_string(), raw_value.trim().to_string()));
                    }
                }
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
            git_rev,
            git_branch,
            extra_headers,
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
                _other => {
                    // Unknown keys are intentionally ignored for forward compatibility.
                    // README: "unknown plist keys are silently ignored by any conformant reader."
                    // A future version of git-closure may emit :mtime, :git-object-id, etc.
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
                GitClosureError::Parse(format!("invalid base64 content for {path}: {err}"))
            })?,
            Some(other) => {
                return Err(GitClosureError::Parse(format!(
                    "unsupported encoding for {path}: {other}"
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

// ── Public high-level operations ─────────────────────────────────────────────

/// Parses a `.gcl` snapshot file and returns a `ListEntry` for each recorded
/// file, in lexicographic path order.
pub fn list_snapshot(snapshot: &Path) -> Result<Vec<ListEntry>> {
    let text = fs::read_to_string(snapshot).map_err(|err| io_error_with_path(err, snapshot))?;
    let (_header, files) = parse_snapshot(&text)?;
    Ok(files
        .into_iter()
        .map(|f| ListEntry {
            is_symlink: f.symlink_target.is_some(),
            symlink_target: f.symlink_target,
            sha256: f.sha256,
            mode: f.mode,
            size: f.size,
            path: f.path,
        })
        .collect())
}

/// Reads a `.gcl` snapshot file and returns its canonical serialized form.
///
/// The result is byte-identical to what [`crate::build_snapshot`] would
/// produce for the same content — modulo the structural hash which is
/// recomputed from the parsed file list.  Use `--check` mode in the `fmt`
/// subcommand to detect snapshots that are not yet in canonical form.
#[derive(Debug, Clone, Copy, Default)]
pub struct FmtOptions {
    pub repair_hash: bool,
}

pub fn fmt_snapshot(snapshot: &Path) -> Result<String> {
    fmt_snapshot_with_options(snapshot, FmtOptions::default())
}

pub fn fmt_snapshot_with_options(snapshot: &Path, options: FmtOptions) -> Result<String> {
    let text = fs::read_to_string(snapshot).map_err(|err| io_error_with_path(err, snapshot))?;
    let (mut header, mut files) = parse_snapshot(&text)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let computed_hash = super::hash::compute_snapshot_hash(&files);
    if header.snapshot_hash != computed_hash && !options.repair_hash {
        return Err(GitClosureError::HashMismatch {
            expected: header.snapshot_hash,
            actual: computed_hash,
        });
    }
    header.snapshot_hash = computed_hash;
    header.file_count = files.len();
    Ok(serialize_snapshot(&files, &header))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::hash::compute_snapshot_hash;

    /// Build a minimal SnapshotHeader (no git metadata) for use in tests.
    fn make_header(files: &[SnapshotFile]) -> SnapshotHeader {
        SnapshotHeader {
            snapshot_hash: compute_snapshot_hash(files),
            file_count: files.len(),
            git_rev: None,
            git_branch: None,
            extra_headers: Vec::new(),
        }
    }

    fn make_text_file(path: &str, content: &str) -> SnapshotFile {
        use crate::snapshot::hash::sha256_hex;
        let bytes = content.as_bytes().to_vec();
        SnapshotFile {
            path: path.to_string(),
            sha256: sha256_hex(&bytes),
            mode: "644".to_string(),
            size: bytes.len() as u64,
            encoding: None,
            symlink_target: None,
            content: bytes,
        }
    }

    #[test]
    fn serialize_then_parse_roundtrip_single_text_file() {
        let file = make_text_file("readme.txt", "hello\n");
        let files_arr = [file.clone()];
        let header = make_header(&files_arr);
        let text = serialize_snapshot(&files_arr, &header);
        let expected_hash = header.snapshot_hash.clone();
        let (header, files) = parse_snapshot(&text).expect("parse serialized snapshot");
        assert_eq!(header.snapshot_hash, expected_hash);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, file.path);
        assert_eq!(files[0].content, file.content);
    }

    #[test]
    fn serialize_then_parse_roundtrip_binary_file() {
        use crate::snapshot::hash::sha256_hex;
        let bytes: Vec<u8> = (0u8..=255).collect();
        let file = SnapshotFile {
            path: "all-bytes.bin".to_string(),
            sha256: sha256_hex(&bytes),
            mode: "644".to_string(),
            size: bytes.len() as u64,
            encoding: Some("base64".to_string()),
            symlink_target: None,
            content: bytes.clone(),
        };
        let files_arr = [file];
        let header = make_header(&files_arr);
        let text = serialize_snapshot(&files_arr, &header);
        let (_, files) = parse_snapshot(&text).expect("parse binary snapshot");
        assert_eq!(files[0].content, bytes);
    }

    #[test]
    fn parse_snapshot_unknown_plist_key_is_ignored() {
        let file = make_text_file("a.txt", "hi");
        let files_arr = [file];
        let header = make_header(&files_arr);
        let text = serialize_snapshot(&files_arr, &header);
        // Inject a future unknown key.
        let modified = text.replace(":mode ", ":future-key \"v\"\n     :mode ");
        let (_, files) = parse_snapshot(&modified).expect("unknown key must be silently ignored");
        assert_eq!(files[0].path, "a.txt");
    }

    #[test]
    fn parse_snapshot_rejects_legacy_format_hash_header() {
        let input = ";; format-hash: abc\n;; file-count: 0\n\n()\n";
        let err = parse_snapshot(input).expect_err("legacy header must be rejected");
        assert!(matches!(err, GitClosureError::LegacyHeader));
    }

    #[test]
    fn quote_string_matches_lexpr_printer() {
        let sample = "line1\nline2\u{0000}\u{fffd}\u{1f642}\\\"";
        let expected = lexpr::to_string(&lexpr::Value::string(sample)).expect("print with lexpr");
        assert_eq!(quote_string(sample), expected);
    }

    // ── list_snapshot tests ───────────────────────────────────────────────────

    #[test]
    fn list_snapshot_returns_entries_in_path_order() {
        use std::fs;
        use tempfile::TempDir;

        let file_b = make_text_file("b.txt", "b");
        let file_a = make_text_file("a.txt", "a");
        // Intentionally unsorted to verify output is sorted.
        let mut files = vec![file_b.clone(), file_a.clone()];
        files.sort_by(|x, y| x.path.cmp(&y.path));
        let header = make_header(&files);
        let text = serialize_snapshot(&files, &header);

        let dir = TempDir::new().unwrap();
        let snap = dir.path().join("snap.gcl");
        fs::write(&snap, text.as_bytes()).unwrap();

        let entries = list_snapshot(&snap).expect("list_snapshot must succeed");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "a.txt");
        assert_eq!(entries[1].path, "b.txt");
        assert!(!entries[0].is_symlink);
        assert_eq!(entries[0].size, 1);
    }

    #[test]
    fn list_snapshot_symlink_entry_has_correct_fields() {
        use crate::snapshot::hash::sha256_hex;
        use std::fs;
        use tempfile::TempDir;

        let symlink_file = SnapshotFile {
            path: "link".to_string(),
            sha256: String::new(),
            mode: "120000".to_string(),
            size: 0,
            encoding: None,
            symlink_target: Some("target.txt".to_string()),
            content: Vec::new(),
        };
        let regular = make_text_file("target.txt", "content");
        let files = vec![symlink_file, regular];
        let header = make_header(&files);
        let text = serialize_snapshot(&files, &header);

        let dir = TempDir::new().unwrap();
        let snap = dir.path().join("snap.gcl");
        fs::write(&snap, text.as_bytes()).unwrap();

        let entries = list_snapshot(&snap).expect("list_snapshot must succeed");
        let link_entry = entries.iter().find(|e| e.path == "link").unwrap();
        assert!(link_entry.is_symlink);
        assert_eq!(link_entry.symlink_target.as_deref(), Some("target.txt"));
        assert_eq!(link_entry.sha256, "");
        assert_eq!(link_entry.size, 0);

        // Suppress unused import warning in non-unix builds.
        let _ = sha256_hex;
    }

    // ── fmt_snapshot tests ────────────────────────────────────────────────────

    #[test]
    fn fmt_snapshot_is_idempotent() {
        use std::fs;
        use tempfile::TempDir;

        let file = make_text_file("src/lib.rs", "fn main() {}\n");
        let files_arr = [file];
        let header = make_header(&files_arr);
        let original = serialize_snapshot(&files_arr, &header);

        let dir = TempDir::new().unwrap();
        let snap = dir.path().join("snap.gcl");
        fs::write(&snap, original.as_bytes()).unwrap();

        let formatted = fmt_snapshot(&snap).expect("fmt_snapshot must succeed");
        assert_eq!(
            formatted, original,
            "fmt_snapshot on already-canonical snapshot must be idempotent"
        );

        // Write the formatted version and format again — must still be equal.
        fs::write(&snap, formatted.as_bytes()).unwrap();
        let formatted2 = fmt_snapshot(&snap).expect("second fmt_snapshot must succeed");
        assert_eq!(formatted2, formatted);
    }

    #[test]
    fn fmt_snapshot_sorts_files_canonically() {
        use std::fs;
        use tempfile::TempDir;

        let file_z = make_text_file("z.txt", "z");
        let file_a = make_text_file("a.txt", "a");
        // Build with files in reverse order (z before a) to create an out-of-order snapshot.
        let mut files_sorted = vec![file_z.clone(), file_a.clone()];
        files_sorted.sort_by(|x, y| x.path.cmp(&y.path));
        let header = make_header(&files_sorted);
        let canonical = serialize_snapshot(&files_sorted, &header);

        let dir = TempDir::new().unwrap();
        let snap = dir.path().join("snap.gcl");
        fs::write(&snap, canonical.as_bytes()).unwrap();

        let formatted = fmt_snapshot(&snap).expect("fmt_snapshot must succeed");
        // Paths must appear in order in the formatted output.
        let a_pos = formatted.find("\"a.txt\"").unwrap();
        let z_pos = formatted.find("\"z.txt\"").unwrap();
        assert!(
            a_pos < z_pos,
            "a.txt must appear before z.txt in canonical output"
        );
    }

    #[test]
    fn fmt_snapshot_preserves_unknown_headers_in_order() {
        use std::fs;
        use tempfile::TempDir;

        let file = make_text_file("a.txt", "a");
        let files = vec![file];
        let header = make_header(&files);
        let mut text = serialize_snapshot(&files, &header);
        text = text.replacen(
            ";; file-count: 1\n",
            ";; file-count: 1\n;; source-uri: gh:owner/repo@main\n;; x-custom: abc\n",
            1,
        );

        let dir = TempDir::new().unwrap();
        let snap = dir.path().join("snap.gcl");
        fs::write(&snap, text.as_bytes()).unwrap();

        let formatted = fmt_snapshot(&snap).expect("fmt_snapshot must succeed");
        let source_pos = formatted
            .find(";; source-uri: gh:owner/repo@main")
            .expect("source-uri header retained");
        let custom_pos = formatted
            .find(";; x-custom: abc")
            .expect("x-custom header retained");
        assert!(
            source_pos < custom_pos,
            "unknown headers must keep input order"
        );

        fs::write(&snap, formatted.as_bytes()).unwrap();
        let formatted_again = fmt_snapshot(&snap).expect("second fmt_snapshot must succeed");
        assert_eq!(formatted_again, formatted, "fmt(fmt(x)) must be idempotent");
    }

    #[test]
    fn fmt_snapshot_rejects_hash_mismatch_by_default() {
        use std::fs;
        use tempfile::TempDir;

        let file = make_text_file("a.txt", "a");
        let mut header = make_header(std::slice::from_ref(&file));
        header.snapshot_hash =
            "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        let text = serialize_snapshot(std::slice::from_ref(&file), &header);

        let dir = TempDir::new().unwrap();
        let snap = dir.path().join("tampered.gcl");
        fs::write(&snap, text.as_bytes()).unwrap();

        let err = fmt_snapshot(&snap).expect_err("fmt must reject hash mismatch by default");
        assert!(matches!(err, GitClosureError::HashMismatch { .. }));
    }

    #[test]
    fn fmt_snapshot_repair_hash_allows_recanonicalization() {
        use std::fs;
        use tempfile::TempDir;

        let file = make_text_file("a.txt", "a");
        let mut header = make_header(std::slice::from_ref(&file));
        header.snapshot_hash =
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_string();
        let text = serialize_snapshot(std::slice::from_ref(&file), &header);

        let dir = TempDir::new().unwrap();
        let snap = dir.path().join("repair.gcl");
        fs::write(&snap, text.as_bytes()).unwrap();

        let repaired = fmt_snapshot_with_options(&snap, FmtOptions { repair_hash: true })
            .expect("fmt --repair-hash should succeed");
        assert!(
            !repaired.contains("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
            "repaired output must contain a recomputed hash"
        );
    }

    // ── git metadata header tests (T-32) ─────────────────────────────────────

    #[test]
    fn serialize_with_git_metadata_emits_header_comments() {
        let file = make_text_file("src/lib.rs", "fn main() {}\n");
        let files = [file];
        let hash = compute_snapshot_hash(&files);
        let header = SnapshotHeader {
            snapshot_hash: hash,
            file_count: files.len(),
            git_rev: Some("deadbeef1234567890abcdef1234567890abcdef".to_string()),
            git_branch: Some("main".to_string()),
            extra_headers: Vec::new(),
        };
        let text = serialize_snapshot(&files, &header);
        assert!(
            text.contains(";; git-rev: deadbeef1234567890abcdef1234567890abcdef\n"),
            "serialized text must contain git-rev comment, got: {text}"
        );
        assert!(
            text.contains(";; git-branch: main\n"),
            "serialized text must contain git-branch comment, got: {text}"
        );
    }

    #[test]
    fn git_metadata_not_included_in_snapshot_hash() {
        let file = make_text_file("src/lib.rs", "fn main() {}\n");
        let files = [file];
        let hash = compute_snapshot_hash(&files);

        let header_without_meta = SnapshotHeader {
            snapshot_hash: hash.clone(),
            file_count: files.len(),
            git_rev: None,
            git_branch: None,
            extra_headers: Vec::new(),
        };
        let header_with_meta = SnapshotHeader {
            snapshot_hash: hash.clone(),
            file_count: files.len(),
            git_rev: Some("abc123".to_string()),
            git_branch: Some("feature-branch".to_string()),
            extra_headers: Vec::new(),
        };

        let text_without = serialize_snapshot(&files, &header_without_meta);
        let text_with = serialize_snapshot(&files, &header_with_meta);

        // The snapshot-hash comment must be identical in both.
        let hash_line = format!(";; snapshot-hash: {hash}\n");
        assert!(
            text_without.contains(&hash_line),
            "snapshot without meta must contain hash line"
        );
        assert!(
            text_with.contains(&hash_line),
            "snapshot with meta must contain same hash line"
        );

        // The two serializations must differ only in the metadata lines.
        assert_ne!(
            text_without, text_with,
            "snapshots with and without git metadata must differ in text"
        );
    }

    #[test]
    fn git_metadata_roundtrips_through_parse() {
        use std::fs;
        use tempfile::TempDir;

        let file = make_text_file("readme.txt", "hello\n");
        let files = [file];
        let hash = compute_snapshot_hash(&files);
        let header = SnapshotHeader {
            snapshot_hash: hash,
            file_count: files.len(),
            git_rev: Some("cafebabe".to_string()),
            git_branch: Some("release/v1".to_string()),
            extra_headers: Vec::new(),
        };
        let text = serialize_snapshot(&files, &header);

        let dir = TempDir::new().unwrap();
        let snap = dir.path().join("snap.gcl");
        fs::write(&snap, text.as_bytes()).unwrap();

        // Parse the file back; metadata fields must survive the round-trip.
        let (parsed_header, _) = parse_snapshot(&text).expect("parse must succeed");
        assert_eq!(parsed_header.git_rev.as_deref(), Some("cafebabe"));
        assert_eq!(parsed_header.git_branch.as_deref(), Some("release/v1"));

        // fmt_snapshot must preserve metadata.
        let formatted = fmt_snapshot(&snap).expect("fmt_snapshot must succeed");
        assert!(
            formatted.contains(";; git-rev: cafebabe\n"),
            "fmt_snapshot must preserve git-rev, got: {formatted}"
        );
        assert!(
            formatted.contains(";; git-branch: release/v1\n"),
            "fmt_snapshot must preserve git-branch, got: {formatted}"
        );
    }
}
