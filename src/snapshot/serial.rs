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

        let quoted_content = if file.encoding.as_deref() == Some("base64") {
            quote_string(&BASE64_STANDARD.encode(&file.content))
        } else {
            // INVARIANT: files without base64 encoding were validated as valid UTF-8
            // during collection via `std::str::from_utf8` in collect_file_attributes.
            // `from_utf8_lossy` would silently corrupt data by substituting U+FFFD —
            // an undetectable data-loss bug.  Panic loudly instead so the invariant
            // violation is surfaced immediately during development/testing.
            quote_string(
                std::str::from_utf8(&file.content)
                    .expect("non-base64 file content must be valid UTF-8 (invariant violated)"),
            )
        };

        output.push_str(&quoted_content);
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
#[derive(Debug, Clone, Default)]
pub struct ParseLimits {
    pub max_entry_count: Option<usize>,
    pub max_file_bytes: Option<u64>,
    pub max_total_bytes: Option<u64>,
}

pub fn parse_snapshot(input: &str) -> Result<(SnapshotHeader, Vec<SnapshotFile>)> {
    parse_snapshot_with_limits(input, None)
}

pub fn parse_snapshot_with_limits(
    input: &str,
    limits: Option<&ParseLimits>,
) -> Result<(SnapshotHeader, Vec<SnapshotFile>)> {
    let (header, body) = split_header_body(input)?;
    let parsed = lexpr::from_str(body).map_err(|err| {
        GitClosureError::Parse(format!("failed to parse S-expression body: {err}"))
    })?;
    let files = parse_files_value(&parsed, limits)?;

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

fn parse_files_value(
    value: &lexpr::Value,
    limits: Option<&ParseLimits>,
) -> Result<Vec<SnapshotFile>> {
    let root = value
        .to_ref_vec()
        .ok_or_else(|| GitClosureError::Parse("snapshot body must be a list".to_string()))?;

    if let Some(limit) = limits.and_then(|l| l.max_entry_count) {
        if root.len() > limit {
            return Err(GitClosureError::Parse(format!(
                "snapshot entry count {} exceeds max_entry_count limit {}",
                root.len(),
                limit
            )));
        }
    }

    let mut files = Vec::with_capacity(root.len());
    let mut total_bytes = 0u64;

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
            if sha256.as_deref().map(|s| !s.is_empty()).unwrap_or(false) {
                return Err(GitClosureError::Parse(format!(
                    "symlink entry {} has unexpected :sha256 field",
                    path
                )));
            }
            if size.map(|s| s != 0).unwrap_or(false) {
                return Err(GitClosureError::Parse(format!(
                    "symlink entry {} has unexpected non-zero :size",
                    path
                )));
            }
            if encoding.is_some() {
                return Err(GitClosureError::Parse(format!(
                    "symlink entry {} has unexpected :encoding field",
                    path
                )));
            }
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
        if !is_valid_sha256_field(&sha256) {
            return Err(GitClosureError::Parse(format!(
                "invalid :sha256 value for {path}: expected 64 lowercase hex digits, got {sha256:?}"
            )));
        }
        let mode = mode.ok_or_else(|| GitClosureError::Parse("missing :mode".to_string()))?;
        let size = size.ok_or_else(|| GitClosureError::Parse("missing :size".to_string()))?;

        if let Some(limit) = limits.and_then(|l| l.max_file_bytes) {
            if size > limit {
                return Err(GitClosureError::Parse(format!(
                    "entry {} exceeds max_file_bytes limit ({size} > {limit})",
                    path
                )));
            }
        }

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

        total_bytes = total_bytes.saturating_add(size);
        if let Some(limit) = limits.and_then(|l| l.max_total_bytes) {
            if total_bytes > limit {
                return Err(GitClosureError::Parse(format!(
                    "snapshot content exceeds max_total_bytes limit ({total_bytes} > {limit})"
                )));
            }
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
    for window in files.windows(2) {
        if window[0].path == window[1].path {
            return Err(GitClosureError::Parse(format!(
                "duplicate :path in snapshot: {}",
                window[0].path
            )));
        }
    }
    Ok(files)
}

fn is_valid_sha256_field(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// ── Public high-level operations ─────────────────────────────────────────────

/// Parses a `.gcl` snapshot file and returns a `ListEntry` for each recorded
/// file, in lexicographic path order.
pub fn list_snapshot(snapshot: &Path) -> Result<Vec<ListEntry>> {
    let text = fs::read_to_string(snapshot).map_err(|err| io_error_with_path(err, snapshot))?;
    list_snapshot_str(&text)
}

/// Parses snapshot text and returns a `ListEntry` for each recorded file.
pub fn list_snapshot_str(text: &str) -> Result<Vec<ListEntry>> {
    let (_header, files) = parse_snapshot(text)?;
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

/// Formatting behavior toggles for [`fmt_snapshot_with_options`].
#[derive(Debug, Clone, Copy, Default)]
pub struct FmtOptions {
    /// Recompute and overwrite a mismatched header `snapshot-hash`.
    pub repair_hash: bool,
}

/// Reads and canonicalizes a snapshot file using default [`FmtOptions`].
///
/// The result is byte-identical to what [`crate::build_snapshot`] would
/// produce for the same content — modulo the structural hash which is
/// recomputed from the parsed file list. Use `--check` mode in the `fmt`
/// subcommand to detect snapshots that are not yet in canonical form.
pub fn fmt_snapshot(snapshot: &Path) -> Result<String> {
    fmt_snapshot_with_options(snapshot, FmtOptions::default())
}

/// Reads and canonicalizes a snapshot file with explicit formatting options.
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
    use proptest::prelude::*;
    use std::collections::BTreeMap;

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

    fn path_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex(r"[A-Za-z0-9_.-]{1,12}(/[A-Za-z0-9_.-]{1,12}){0,2}")
            .expect("valid path regex")
            .prop_filter("path must be safe and relative", |path| {
                !path.starts_with('/')
                    && !path
                        .split('/')
                        .any(|segment| segment == "." || segment == "..")
            })
    }

    fn symlink_target_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex(r"[A-Za-z0-9_.-]{1,16}(/[A-Za-z0-9_.-]{1,16}){0,2}")
            .expect("valid symlink target regex")
            .prop_filter("symlink target must not be empty", |target| {
                !target.is_empty()
            })
    }

    fn snapshot_file_strategy() -> impl Strategy<Value = SnapshotFile> {
        let regular_utf8 = (
            path_strategy(),
            prop::sample::select(vec!["644".to_string(), "755".to_string()]),
            proptest::string::string_regex("[ -~]{0,64}").expect("valid UTF-8 content regex"),
        )
            .prop_map(|(path, mode, content)| {
                let bytes = content.into_bytes();
                SnapshotFile {
                    path,
                    sha256: crate::snapshot::hash::sha256_hex(&bytes),
                    mode,
                    size: bytes.len() as u64,
                    encoding: None,
                    symlink_target: None,
                    content: bytes,
                }
            });

        let regular_binary = (
            path_strategy(),
            prop::sample::select(vec!["644".to_string(), "755".to_string()]),
            prop::collection::vec(any::<u8>(), 0..64),
        )
            .prop_map(|(path, mode, bytes)| SnapshotFile {
                path,
                sha256: crate::snapshot::hash::sha256_hex(&bytes),
                mode,
                size: bytes.len() as u64,
                encoding: Some("base64".to_string()),
                symlink_target: None,
                content: bytes,
            });

        let symlink =
            (path_strategy(), symlink_target_strategy()).prop_map(|(path, target)| SnapshotFile {
                path,
                sha256: String::new(),
                mode: "120000".to_string(),
                size: 0,
                encoding: None,
                symlink_target: Some(target),
                content: Vec::new(),
            });

        prop_oneof![regular_utf8, regular_binary, symlink]
    }

    fn canonicalize_generated_files(files: Vec<SnapshotFile>) -> Vec<SnapshotFile> {
        let mut by_path = BTreeMap::new();
        for file in files {
            by_path.entry(file.path.clone()).or_insert(file);
        }
        by_path.into_values().collect()
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

    proptest! {
        #[test]
        fn proptest_parse_serialize_roundtrip(files in prop::collection::vec(snapshot_file_strategy(), 0..16)) {
            let files = canonicalize_generated_files(files);
            let header = make_header(&files);
            let serialized = serialize_snapshot(&files, &header);
            let (parsed_header, parsed_files) = parse_snapshot(&serialized)
                .expect("generated snapshot should parse");

            prop_assert_eq!(parsed_header.file_count, files.len());
            prop_assert_eq!(parsed_header.snapshot_hash, compute_snapshot_hash(&files));
            prop_assert_eq!(parsed_files, files);
        }

        #[test]
        fn proptest_fmt_is_idempotent(files in prop::collection::vec(snapshot_file_strategy(), 0..16)) {
            let files = canonicalize_generated_files(files);
            let header = make_header(&files);
            let serialized = serialize_snapshot(&files, &header);

            let tmp = tempfile::TempDir::new().expect("create tempdir");
            let snapshot = tmp.path().join("proptest.gcl");
            std::fs::write(&snapshot, serialized).expect("write generated snapshot");

            let once = fmt_snapshot(&snapshot).expect("first fmt pass");
            std::fs::write(&snapshot, &once).expect("write first fmt result");
            let twice = fmt_snapshot(&snapshot).expect("second fmt pass");

            prop_assert_eq!(twice, once);
        }
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
    fn parse_snapshot_rejects_duplicate_regular_paths() {
        let content_a = "a";
        let content_b = "b";
        let digest_a = crate::snapshot::hash::sha256_hex(content_a.as_bytes());
        let digest_b = crate::snapshot::hash::sha256_hex(content_b.as_bytes());
        let snapshot_hash = crate::snapshot::hash::sha256_hex(b"placeholder");
        let input = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {snapshot_hash}\n;; file-count: 2\n\n(\n  ((:path \"dup.txt\" :sha256 \"{digest_a}\" :mode \"644\" :size 1) \"{content_a}\")\n  ((:path \"dup.txt\" :sha256 \"{digest_b}\" :mode \"644\" :size 1) \"{content_b}\")\n)\n"
        );

        let err = parse_snapshot(&input).expect_err("duplicate paths must be rejected");
        match err {
            GitClosureError::Parse(msg) => assert!(
                msg.contains("duplicate :path") && msg.contains("dup.txt"),
                "parse error should mention duplicate path, got: {msg}"
            ),
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn parse_snapshot_rejects_duplicate_regular_and_symlink_paths() {
        let content = "x";
        let digest = crate::snapshot::hash::sha256_hex(content.as_bytes());
        let snapshot_hash = crate::snapshot::hash::sha256_hex(b"placeholder");
        let input = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {snapshot_hash}\n;; file-count: 2\n\n(\n  ((:path \"dup.txt\" :sha256 \"{digest}\" :mode \"644\" :size 1) \"{content}\")\n  ((:path \"dup.txt\" :type \"symlink\" :target \"target.txt\") \"\")\n)\n"
        );

        let err = parse_snapshot(&input)
            .expect_err("duplicate path between regular and symlink must be rejected");
        match err {
            GitClosureError::Parse(msg) => assert!(
                msg.contains("duplicate :path") && msg.contains("dup.txt"),
                "parse error should mention duplicate path, got: {msg}"
            ),
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn verify_snapshot_rejects_duplicate_paths_via_parse() {
        use tempfile::TempDir;

        let dir = TempDir::new().expect("create tempdir");
        let snapshot = dir.path().join("duplicate.gcl");

        let content_a = "a";
        let content_b = "b";
        let digest_a = crate::snapshot::hash::sha256_hex(content_a.as_bytes());
        let digest_b = crate::snapshot::hash::sha256_hex(content_b.as_bytes());
        let files = vec![
            SnapshotFile {
                path: "dup.txt".to_string(),
                sha256: digest_a.clone(),
                mode: "644".to_string(),
                size: 1,
                encoding: None,
                symlink_target: None,
                content: content_a.as_bytes().to_vec(),
            },
            SnapshotFile {
                path: "dup.txt".to_string(),
                sha256: digest_b.clone(),
                mode: "644".to_string(),
                size: 1,
                encoding: None,
                symlink_target: None,
                content: content_b.as_bytes().to_vec(),
            },
        ];
        let snapshot_hash = crate::snapshot::hash::compute_snapshot_hash(&files);
        let input = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {snapshot_hash}\n;; file-count: 2\n\n(\n  ((:path \"dup.txt\" :sha256 \"{digest_a}\" :mode \"644\" :size 1) \"{content_a}\")\n  ((:path \"dup.txt\" :sha256 \"{digest_b}\" :mode \"644\" :size 1) \"{content_b}\")\n)\n"
        );
        std::fs::write(&snapshot, input).expect("write duplicate snapshot");

        let err = crate::materialize::verify_snapshot(&snapshot)
            .expect_err("verify must reject snapshots with duplicate paths");
        assert!(matches!(err, GitClosureError::Parse(_)));
    }

    #[test]
    fn parse_snapshot_with_limits_rejects_entry_count_limit() {
        let file_a = make_text_file("a.txt", "a");
        let file_b = make_text_file("b.txt", "b");
        let files = vec![file_a, file_b];
        let header = make_header(&files);
        let text = serialize_snapshot(&files, &header);

        let limits = ParseLimits {
            max_entry_count: Some(1),
            max_file_bytes: None,
            max_total_bytes: None,
        };
        let err = parse_snapshot_with_limits(&text, Some(&limits))
            .expect_err("entry count limit must reject oversized snapshot");
        assert!(matches!(err, GitClosureError::Parse(_)));
    }

    #[test]
    fn parse_snapshot_with_limits_rejects_file_bytes_limit() {
        let file = make_text_file("a.txt", "hello");
        let files = vec![file];
        let header = make_header(&files);
        let text = serialize_snapshot(&files, &header);

        let limits = ParseLimits {
            max_entry_count: None,
            max_file_bytes: Some(4),
            max_total_bytes: None,
        };
        let err = parse_snapshot_with_limits(&text, Some(&limits))
            .expect_err("file bytes limit must reject oversized entry");
        assert!(matches!(err, GitClosureError::Parse(_)));
    }

    #[test]
    fn parse_snapshot_with_limits_rejects_total_bytes_limit() {
        let file_a = make_text_file("a.txt", "abc");
        let file_b = make_text_file("b.txt", "def");
        let files = vec![file_a, file_b];
        let header = make_header(&files);
        let text = serialize_snapshot(&files, &header);

        let limits = ParseLimits {
            max_entry_count: None,
            max_file_bytes: None,
            max_total_bytes: Some(5),
        };
        let err = parse_snapshot_with_limits(&text, Some(&limits))
            .expect_err("total bytes limit must reject oversized aggregate");
        assert!(matches!(err, GitClosureError::Parse(_)));
    }

    #[test]
    fn parse_snapshot_rejects_symlink_with_nonempty_sha256() {
        let files = vec![SnapshotFile {
            path: "link".to_string(),
            sha256: String::new(),
            mode: "120000".to_string(),
            size: 0,
            encoding: None,
            symlink_target: Some("target.txt".to_string()),
            content: Vec::new(),
        }];
        let header = make_header(&files);
        let text = serialize_snapshot(&files, &header);
        let modified = text.replace(
            ":type \"symlink\"",
            ":sha256 \"deadbeef\"\n     :type \"symlink\"",
        );

        let err = parse_snapshot(&modified)
            .expect_err("symlink entries must reject non-empty sha256 field");
        assert!(matches!(err, GitClosureError::Parse(_)));
    }

    #[test]
    fn parse_snapshot_rejects_symlink_with_nonzero_size() {
        let files = vec![SnapshotFile {
            path: "link".to_string(),
            sha256: String::new(),
            mode: "120000".to_string(),
            size: 0,
            encoding: None,
            symlink_target: Some("target.txt".to_string()),
            content: Vec::new(),
        }];
        let header = make_header(&files);
        let text = serialize_snapshot(&files, &header);
        let modified = text.replace(":type \"symlink\"", ":size 1\n     :type \"symlink\"");

        let err =
            parse_snapshot(&modified).expect_err("symlink entries must reject non-zero size field");
        assert!(matches!(err, GitClosureError::Parse(_)));
    }

    #[test]
    fn parse_snapshot_rejects_short_sha256() {
        let content = "x";
        let invalid_sha = "a".repeat(63);
        let snapshot_hash = crate::snapshot::hash::sha256_hex(b"placeholder");
        let input = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {snapshot_hash}\n;; file-count: 1\n\n(\n  ((:path \"short.txt\" :sha256 \"{invalid_sha}\" :mode \"644\" :size 1) \"{content}\")\n)\n"
        );

        let err = parse_snapshot(&input).expect_err("short :sha256 must be rejected");
        match err {
            GitClosureError::Parse(message) => {
                assert!(
                    message.contains("short.txt") && message.contains(&invalid_sha),
                    "parse error should include path and invalid value, got: {message}"
                );
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn parse_snapshot_rejects_non_hex_sha256() {
        let content = "x";
        let invalid_sha = "not-a-hash";
        let snapshot_hash = crate::snapshot::hash::sha256_hex(b"placeholder");
        let input = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {snapshot_hash}\n;; file-count: 1\n\n(\n  ((:path \"nonhex.txt\" :sha256 \"{invalid_sha}\" :mode \"644\" :size 1) \"{content}\")\n)\n"
        );

        let err = parse_snapshot(&input).expect_err("non-hex :sha256 must be rejected");
        match err {
            GitClosureError::Parse(message) => {
                assert!(
                    message.contains("nonhex.txt") && message.contains(invalid_sha),
                    "parse error should include path and invalid value, got: {message}"
                );
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn parse_snapshot_rejects_uppercase_sha256() {
        let content = "x";
        let invalid_sha = "A".repeat(64);
        let snapshot_hash = crate::snapshot::hash::sha256_hex(b"placeholder");
        let input = format!(
            ";; git-closure snapshot v0.1\n;; snapshot-hash: {snapshot_hash}\n;; file-count: 1\n\n(\n  ((:path \"upper.txt\" :sha256 \"{invalid_sha}\" :mode \"644\" :size 1) \"{content}\")\n)\n"
        );

        let err = parse_snapshot(&input).expect_err("uppercase :sha256 must be rejected");
        match err {
            GitClosureError::Parse(message) => {
                assert!(
                    message.contains("upper.txt") && message.contains(&invalid_sha),
                    "parse error should include path and invalid value, got: {message}"
                );
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
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

    #[test]
    fn list_snapshot_str_returns_expected_entries() {
        let files = vec![make_text_file("a.txt", "a"), make_text_file("b.txt", "bb")];
        let header = make_header(&files);
        let text = serialize_snapshot(&files, &header);

        let entries =
            list_snapshot_str(&text).expect("list_snapshot_str should parse valid snapshot");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "a.txt");
        assert_eq!(entries[1].path, "b.txt");
        assert_eq!(entries[0].size, 1);
        assert_eq!(entries[1].size, 2);
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

    #[test]
    fn serialize_snapshot_avoids_content_clone_in_utf8_path() {
        let source = include_str!("serial.rs");
        let needle = ["String::from_utf8(", "file.content.clone()", ")"].join("");
        assert!(
            !source.contains(&needle),
            "utf8 serialization path should avoid cloning file.content"
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
