/// S-expression serialization and deserialization for `.gcl` snapshot files.
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;

use crate::error::GitClosureError;

use super::{Result, SnapshotFile, SnapshotHeader};

// ── Serialization ─────────────────────────────────────────────────────────────

/// Serializes `files` into the canonical `.gcl` S-expression format.
/// `files` must be in lexicographic path order (the caller is responsible).
pub(crate) fn serialize_snapshot(files: &[SnapshotFile], snapshot_hash: &str) -> String {
    let mut output = String::new();

    output.push_str(";; git-closure snapshot v0.1\n");
    output.push_str(&format!(";; snapshot-hash: {snapshot_hash}\n"));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::hash::compute_snapshot_hash;

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
        let hash = compute_snapshot_hash(&[file.clone()]);
        let text = serialize_snapshot(&[file.clone()], &hash);
        let (header, files) = parse_snapshot(&text).expect("parse serialized snapshot");
        assert_eq!(header.snapshot_hash, hash);
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
        let hash = compute_snapshot_hash(&[file.clone()]);
        let text = serialize_snapshot(&[file], &hash);
        let (_, files) = parse_snapshot(&text).expect("parse binary snapshot");
        assert_eq!(files[0].content, bytes);
    }

    #[test]
    fn parse_snapshot_unknown_plist_key_is_ignored() {
        let file = make_text_file("a.txt", "hi");
        let hash = compute_snapshot_hash(&[file.clone()]);
        let text = serialize_snapshot(&[file], &hash);
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
}
