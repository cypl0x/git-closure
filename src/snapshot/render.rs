/// Audit report rendering: Markdown, HTML, and JSON output from a snapshot.
use std::fs;
use std::path::Path;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use serde::Serialize;

use crate::utils::{io_error_with_path, sha256_prefix};

use super::serial::parse_snapshot;
use super::{Result, SnapshotFile, SnapshotHeader};

// ── Public types ──────────────────────────────────────────────────────────────

/// Output format for [`render_snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFormat {
    /// Render as a Markdown report.
    Markdown,
    /// Render as a standalone HTML page.
    Html,
    /// Render as pretty-printed JSON.
    Json,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Renders a snapshot file as an audit report in the given format.
pub fn render_snapshot(snapshot: &Path, format: RenderFormat) -> Result<String> {
    let text = fs::read_to_string(snapshot).map_err(|err| io_error_with_path(err, snapshot))?;
    let (header, files) = parse_snapshot(&text)?;
    match format {
        RenderFormat::Markdown => Ok(render_markdown(&header, &files)),
        RenderFormat::Html => Ok(render_html(&header, &files)),
        RenderFormat::Json => Ok(render_json(&header, &files)),
    }
}

// ── Markdown renderer ─────────────────────────────────────────────────────────

fn render_markdown(header: &SnapshotHeader, files: &[SnapshotFile]) -> String {
    let mut out = String::new();

    // YAML front matter — consumed by pandoc and other Markdown processors.
    out.push_str("---\n");
    out.push_str("title: \"Snapshot Audit Report\"\n");
    out.push_str(&format!(
        "snapshot-hash: {}\n",
        yaml_quote(&header.snapshot_hash)
    ));
    out.push_str(&format!("file-count: {}\n", header.file_count));
    if let Some(rev) = &header.git_rev {
        out.push_str(&format!("git-rev: {}\n", yaml_quote(rev)));
    }
    if let Some(branch) = &header.git_branch {
        out.push_str(&format!("git-branch: {}\n", yaml_quote(branch)));
    }
    out.push_str("---\n\n");

    out.push_str("# Snapshot Audit Report\n\n");
    out.push_str("## Metadata\n\n");
    out.push_str(&format!(
        "| Field | Value |\n|---|---|\n| Snapshot hash | `{}` |\n| File count | {} |\n",
        header.snapshot_hash, header.file_count
    ));
    if let Some(rev) = &header.git_rev {
        out.push_str(&format!("| Git revision | `{rev}` |\n"));
    }
    if let Some(branch) = &header.git_branch {
        out.push_str(&format!("| Git branch | `{branch}` |\n"));
    }

    let (regular_count, symlink_count) = count_file_types(files);
    let total_bytes: u64 = files.iter().map(|f| f.size).sum();
    out.push_str(&format!(
        "| Regular files | {regular_count} |\n| Symlinks | {symlink_count} |\n| Total bytes | {total_bytes} |\n"
    ));

    out.push_str("\n## Files\n\n");
    out.push_str("| Path | Type | Mode | Size | SHA-256 (prefix) |\n");
    out.push_str("|---|---|---|---|---|\n");

    for f in files {
        let is_symlink = f.symlink_target.is_some();
        let entry_type = if is_symlink { "symlink" } else { "file" };
        let sha256_display = if is_symlink {
            format!("→ {}", md_escape(f.symlink_target.as_deref().unwrap_or("")))
        } else {
            format!("`{}`", sha256_prefix(&f.sha256))
        };
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            md_escape(&f.path),
            entry_type,
            f.mode,
            f.size,
            sha256_display
        ));
    }

    out.push_str("\n## File Contents\n");
    for f in files {
        if f.symlink_target.is_some() {
            continue;
        }
        out.push_str(&format!("\n### `{}`\n\n", md_escape(&f.path)));
        if f.encoding.as_deref() == Some("base64") {
            out.push_str(&format!("```\n[binary content, {} bytes]\n```\n", f.size));
        } else {
            let text = std::str::from_utf8(&f.content)
                .expect("non-base64 content must be valid UTF-8 (invariant violated)");
            let lang = lang_hint(&f.path);
            out.push_str(&format!("```{lang}\n{text}\n```\n"));
        }
    }

    out
}

// ── HTML renderer ─────────────────────────────────────────────────────────────

fn render_html(header: &SnapshotHeader, files: &[SnapshotFile]) -> String {
    let mut out = String::new();

    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"UTF-8\">\n");
    out.push_str("<title>Snapshot Audit Report</title>\n");
    out.push_str("<style>body{font-family:monospace;max-width:1200px;margin:2em auto;padding:0 1em}table{border-collapse:collapse;width:100%}th,td{border:1px solid #ccc;padding:4px 8px;text-align:left}th{background:#f0f0f0}code{background:#f8f8f8;padding:2px 4px;border-radius:2px}pre{background:#f8f8f8;padding:1em;overflow-x:auto;white-space:pre-wrap}</style>\n");
    out.push_str("</head>\n<body>\n");
    out.push_str("<h1>Snapshot Audit Report</h1>\n");
    out.push_str("<h2>Metadata</h2>\n<table>\n");
    out.push_str(&format!(
        "<tr><th>Snapshot hash</th><td><code>{}</code></td></tr>\n",
        html_escape(&header.snapshot_hash)
    ));
    out.push_str(&format!(
        "<tr><th>File count</th><td>{}</td></tr>\n",
        header.file_count
    ));
    if let Some(rev) = &header.git_rev {
        out.push_str(&format!(
            "<tr><th>Git revision</th><td><code>{}</code></td></tr>\n",
            html_escape(rev)
        ));
    }
    if let Some(branch) = &header.git_branch {
        out.push_str(&format!(
            "<tr><th>Git branch</th><td><code>{}</code></td></tr>\n",
            html_escape(branch)
        ));
    }
    let (regular_count, symlink_count) = count_file_types(files);
    let total_bytes: u64 = files.iter().map(|f| f.size).sum();
    out.push_str(&format!(
        "<tr><th>Regular files</th><td>{regular_count}</td></tr>\n"
    ));
    out.push_str(&format!(
        "<tr><th>Symlinks</th><td>{symlink_count}</td></tr>\n"
    ));
    out.push_str(&format!(
        "<tr><th>Total bytes</th><td>{total_bytes}</td></tr>\n"
    ));
    out.push_str("</table>\n");

    out.push_str("<h2>Files</h2>\n<table>\n");
    out.push_str("<thead><tr><th>Path</th><th>Type</th><th>Mode</th><th>Size</th><th>SHA-256 (prefix)</th></tr></thead>\n<tbody>\n");

    for f in files {
        let is_symlink = f.symlink_target.is_some();
        let entry_type = if is_symlink { "symlink" } else { "file" };
        let sha256_display = if is_symlink {
            format!(
                "→ {}",
                html_escape(f.symlink_target.as_deref().unwrap_or(""))
            )
        } else {
            format!("<code>{}</code>", sha256_prefix(&f.sha256))
        };
        out.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            html_escape(&f.path),
            entry_type,
            f.mode,
            f.size,
            sha256_display
        ));
    }
    out.push_str("</tbody></table>\n");

    out.push_str("<h2>File Contents</h2>\n");
    for f in files {
        if f.symlink_target.is_some() {
            continue;
        }
        out.push_str(&format!("<h3><code>{}</code></h3>\n", html_escape(&f.path)));
        if f.encoding.as_deref() == Some("base64") {
            out.push_str(&format!("<pre>[binary content, {} bytes]</pre>\n", f.size));
        } else {
            let text = std::str::from_utf8(&f.content)
                .expect("non-base64 content must be valid UTF-8 (invariant violated)");
            out.push_str(&format!("<pre><code>{}</code></pre>\n", html_escape(text)));
        }
    }

    out.push_str("</body>\n</html>\n");
    out
}

// ── JSON renderer ─────────────────────────────────────────────────────────────

fn render_json(header: &SnapshotHeader, files: &[SnapshotFile]) -> String {
    let (regular_count, symlink_count) = count_file_types(files);
    let total_bytes: u64 = files.iter().map(|f| f.size).sum();

    let json_files: Vec<RenderJsonFile<'_>> = files
        .iter()
        .map(|f| {
            let is_symlink = f.symlink_target.is_some();
            let content = if is_symlink {
                None
            } else if f.encoding.as_deref() == Some("base64") {
                Some(BASE64_STANDARD.encode(&f.content))
            } else {
                Some(
                    std::str::from_utf8(&f.content)
                        .expect("non-base64 content must be valid UTF-8 (invariant violated)")
                        .to_string(),
                )
            };
            RenderJsonFile {
                path: f.path.as_str(),
                entry_type: if is_symlink { "symlink" } else { "file" },
                mode: f.mode.as_str(),
                size: f.size,
                sha256: f.sha256.as_str(),
                symlink_target: f.symlink_target.as_deref(),
                encoding: f.encoding.as_deref(),
                content,
            }
        })
        .collect();

    let payload = RenderJson {
        snapshot_hash: header.snapshot_hash.as_str(),
        file_count: header.file_count,
        git_rev: header.git_rev.as_deref(),
        git_branch: header.git_branch.as_deref(),
        regular_file_count: regular_count,
        symlink_count,
        total_bytes,
        files: json_files,
    };

    let mut json = serde_json::to_string_pretty(&payload).expect("serialize render JSON");
    json.push('\n');
    json
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn count_file_types(files: &[SnapshotFile]) -> (usize, usize) {
    let symlinks = files.iter().filter(|f| f.symlink_target.is_some()).count();
    (files.len() - symlinks, symlinks)
}

/// Returns a Markdown fenced-code-block language hint for a given file path.
/// Returns an empty string for unrecognised extensions (produces a plain fence).
fn lang_hint(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "toml" => "toml",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "sh" | "bash" => "bash",
        "nix" => "nix",
        "md" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "sql" => "sql",
        "java" => "java",
        "rb" => "ruby",
        _ => "",
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn md_escape(s: &str) -> String {
    s.replace('|', "\\|").replace('`', "\\`")
}

/// Wraps a string in YAML double-quoted style, escaping `\` and `"`.
fn yaml_quote(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[derive(Debug, Serialize)]
struct RenderJson<'a> {
    snapshot_hash: &'a str,
    file_count: usize,
    git_rev: Option<&'a str>,
    git_branch: Option<&'a str>,
    regular_file_count: usize,
    symlink_count: usize,
    total_bytes: u64,
    files: Vec<RenderJsonFile<'a>>,
}

#[derive(Debug, Serialize)]
struct RenderJsonFile<'a> {
    path: &'a str,
    #[serde(rename = "type")]
    entry_type: &'a str,
    mode: &'a str,
    size: u64,
    sha256: &'a str,
    symlink_target: Option<&'a str>,
    /// Decoded text content for UTF-8 files; base64-encoded bytes for binary
    /// files; `null` for symlinks.
    content: Option<String>,
    /// Present and set to `"base64"` only for binary files.
    #[serde(skip_serializing_if = "Option::is_none")]
    encoding: Option<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::hash::{compute_snapshot_hash, sha256_hex};
    use crate::snapshot::serial::serialize_snapshot;
    use crate::snapshot::{SnapshotFile, SnapshotHeader};
    use std::fs;
    use tempfile::TempDir;

    fn text_file(path: &str, content: &str) -> SnapshotFile {
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

    fn symlink_file(path: &str, target: &str) -> SnapshotFile {
        SnapshotFile {
            path: path.to_string(),
            sha256: String::new(),
            mode: "120000".to_string(),
            size: 0,
            encoding: None,
            symlink_target: Some(target.to_string()),
            content: Vec::new(),
        }
    }

    fn write_snap(
        dir: &TempDir,
        files: &[SnapshotFile],
        git_rev: Option<&str>,
        git_branch: Option<&str>,
    ) -> std::path::PathBuf {
        let mut sorted = files.to_vec();
        sorted.sort_by(|a, b| a.path.cmp(&b.path));
        let snapshot_hash = compute_snapshot_hash(&sorted);
        let header = SnapshotHeader {
            snapshot_hash,
            file_count: sorted.len(),
            git_rev: git_rev.map(str::to_string),
            git_branch: git_branch.map(str::to_string),
            extra_headers: Vec::new(),
        };
        let text = serialize_snapshot(&sorted, &header);
        let path = dir.path().join("snap.gcl");
        fs::write(&path, text.as_bytes()).unwrap();
        path
    }

    #[test]
    fn render_markdown_contains_hash_and_file_count() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("src/main.rs", "fn main() {}")];
        let snap = write_snap(&dir, &files, None, None);

        let output = render_snapshot(&snap, RenderFormat::Markdown).unwrap();
        assert!(
            output.contains("# Snapshot Audit Report"),
            "markdown must start with h1"
        );
        assert!(
            output.contains("| File count | 1 |"),
            "must show file count"
        );
        assert!(output.contains("src/main.rs"), "must list file paths");
    }

    #[test]
    fn render_markdown_includes_git_metadata_when_present() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "a")];
        let snap = write_snap(&dir, &files, Some("cafebabe"), Some("main"));

        let output = render_snapshot(&snap, RenderFormat::Markdown).unwrap();
        assert!(
            output.contains("cafebabe"),
            "markdown must include git revision"
        );
        assert!(output.contains("main"), "markdown must include branch name");
    }

    #[test]
    fn render_markdown_has_yaml_front_matter() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "hello")];
        let snap = write_snap(&dir, &files, Some("cafebabe"), Some("main"));

        let output = render_snapshot(&snap, RenderFormat::Markdown).unwrap();

        assert!(
            output.starts_with("---\n"),
            "must open with YAML front matter"
        );
        assert!(
            output.contains("title: \"Snapshot Audit Report\""),
            "front matter must include title"
        );
        assert!(
            output.contains("file-count: 1"),
            "front matter must include file-count"
        );
        assert!(
            output.contains("git-rev: \"cafebabe\""),
            "front matter must include git-rev when present"
        );
        assert!(
            output.contains("git-branch: \"main\""),
            "front matter must include git-branch when present"
        );
        // Verify the closing delimiter and that the heading follows
        let front_matter_end = output
            .find("---\n\n")
            .expect("front matter must close with ---");
        let after = &output[front_matter_end + 5..];
        assert!(
            after.starts_with("# Snapshot Audit Report"),
            "heading must follow front matter"
        );
    }

    #[test]
    fn render_markdown_yaml_front_matter_omits_git_fields_when_absent() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "hello")];
        let snap = write_snap(&dir, &files, None, None);

        let output = render_snapshot(&snap, RenderFormat::Markdown).unwrap();
        assert!(
            !output.contains("git-rev:"),
            "git-rev must be absent when not set"
        );
        assert!(
            !output.contains("git-branch:"),
            "git-branch must be absent when not set"
        );
    }

    #[test]
    fn render_html_is_valid_html_structure() {
        let dir = TempDir::new().unwrap();
        // Use a path with a character that needs HTML escaping.
        let files = vec![text_file("src/main.rs", "fn main() {}")];
        let snap = write_snap(&dir, &files, None, None);

        let output = render_snapshot(&snap, RenderFormat::Html).unwrap();
        assert!(output.starts_with("<!DOCTYPE html>"), "must be proper HTML");
        assert!(output.ends_with("</html>\n"), "must end with </html>");
        assert!(output.contains("src/main.rs"), "must list file path");
        assert!(
            output.contains("<table>"),
            "must contain a table for file listing"
        );
    }

    #[test]
    fn render_json_is_parseable_and_contains_expected_fields() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "aaa"), text_file("b.txt", "bb")];
        let snap = write_snap(&dir, &files, Some("abc"), Some("dev"));

        let output = render_snapshot(&snap, RenderFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).expect("json must parse");
        assert_eq!(value["file_count"], serde_json::Value::from(2));
        assert_eq!(value["git_rev"], serde_json::Value::from("abc"));
        assert_eq!(value["git_branch"], serde_json::Value::from("dev"));
        assert!(value["total_bytes"].is_u64());
        let files = value["files"].as_array().expect("files must be an array");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["path"], serde_json::Value::from("a.txt"));
    }

    #[test]
    fn render_json_null_git_fields_when_absent() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("x.txt", "x")];
        let snap = write_snap(&dir, &files, None, None);

        let output = render_snapshot(&snap, RenderFormat::Json).unwrap();
        assert!(
            output.contains("\"git_rev\": null"),
            "git_rev must be null when absent"
        );
        assert!(
            output.contains("\"git_branch\": null"),
            "git_branch must be null when absent"
        );
    }

    #[test]
    fn render_outputs_are_deterministic_for_same_input() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "aaa"), symlink_file("link", "a.txt")];
        let snap = write_snap(&dir, &files, Some("abc123"), Some("main"));

        let md_a = render_snapshot(&snap, RenderFormat::Markdown).unwrap();
        let md_b = render_snapshot(&snap, RenderFormat::Markdown).unwrap();
        assert_eq!(md_a, md_b, "markdown output must be deterministic");

        let html_a = render_snapshot(&snap, RenderFormat::Html).unwrap();
        let html_b = render_snapshot(&snap, RenderFormat::Html).unwrap();
        assert_eq!(html_a, html_b, "html output must be deterministic");

        let json_a = render_snapshot(&snap, RenderFormat::Json).unwrap();
        let json_b = render_snapshot(&snap, RenderFormat::Json).unwrap();
        assert_eq!(json_a, json_b, "json output must be deterministic");
    }

    #[test]
    fn render_symlink_entries_are_consistent_across_formats() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "aaa"), symlink_file("link", "a.txt")];
        let snap = write_snap(&dir, &files, None, None);

        let markdown = render_snapshot(&snap, RenderFormat::Markdown).unwrap();
        assert!(
            markdown.contains("`link`")
                && markdown.contains("symlink")
                && markdown.contains("→ a.txt"),
            "markdown must identify symlink entries and targets"
        );

        let html = render_snapshot(&snap, RenderFormat::Html).unwrap();
        assert!(
            html.contains("<code>link</code>")
                && html.contains("symlink")
                && html.contains("→ a.txt"),
            "html must identify symlink entries and targets"
        );

        let json = render_snapshot(&snap, RenderFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).expect("json must parse");
        let files = value["files"].as_array().expect("files must be an array");
        let link_entry = files
            .iter()
            .find(|entry| entry["path"].as_str() == Some("link"))
            .expect("json must include link entry");
        assert_eq!(link_entry["type"], serde_json::Value::from("symlink"));
        assert_eq!(
            link_entry["symlink_target"],
            serde_json::Value::from("a.txt")
        );
    }

    #[test]
    fn render_markdown_escapes_symlink_target_pipe() {
        let dir = TempDir::new().unwrap();
        let files = vec![symlink_file("link", "../foo|bar")];
        let snap = write_snap(&dir, &files, None, None);

        let markdown = render_snapshot(&snap, RenderFormat::Markdown).unwrap();
        assert!(
            markdown.contains("→ ../foo\\|bar"),
            "markdown must escape pipe characters in symlink targets"
        );
    }

    #[test]
    fn render_markdown_escapes_symlink_target_backtick() {
        let dir = TempDir::new().unwrap();
        let files = vec![symlink_file("link", "`etc/passwd`")];
        let snap = write_snap(&dir, &files, None, None);

        let markdown = render_snapshot(&snap, RenderFormat::Markdown).unwrap();
        assert!(
            markdown.contains("→ \\`etc/passwd\\`"),
            "markdown must escape backticks in symlink targets"
        );
    }

    // ── Content rendering tests ───────────────────────────────────────────────

    #[test]
    fn render_markdown_includes_file_contents() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("src/main.rs", "fn main() {}")];
        let snap = write_snap(&dir, &files, None, None);

        let output = render_snapshot(&snap, RenderFormat::Markdown).unwrap();
        assert!(
            output.contains("## File Contents"),
            "markdown must have a File Contents section; got:\n{output}"
        );
        assert!(
            output.contains("fn main() {}"),
            "markdown must include the file's text content; got:\n{output}"
        );
    }

    #[test]
    fn render_markdown_multiline_content_uses_real_newlines() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "line1\nline2")];
        let snap = write_snap(&dir, &files, None, None);

        let output = render_snapshot(&snap, RenderFormat::Markdown).unwrap();
        // Content must contain actual newlines, not the escape sequence
        assert!(
            output.contains("line1\nline2"),
            "rendered content must use real newlines, not \\n escapes; got:\n{output}"
        );
        assert!(
            !output.contains("line1\\nline2"),
            "rendered content must not contain literal backslash-n; got:\n{output}"
        );
    }

    #[test]
    fn render_markdown_binary_file_shows_note() {
        let dir = TempDir::new().unwrap();
        let binary_bytes = vec![0xFF_u8, 0xFE, 0x00, 0x01];
        let sha = sha256_hex(&binary_bytes);
        let bin_file = SnapshotFile {
            path: "binary.bin".to_string(),
            sha256: sha,
            mode: "644".to_string(),
            size: binary_bytes.len() as u64,
            encoding: Some("base64".to_string()),
            symlink_target: None,
            content: binary_bytes,
        };
        let snap = write_snap(&dir, &[bin_file], None, None);

        let output = render_snapshot(&snap, RenderFormat::Markdown).unwrap();
        assert!(
            output.contains("[binary"),
            "markdown must note binary content rather than displaying raw bytes; got:\n{output}"
        );
    }

    #[test]
    fn render_html_includes_file_contents() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("x.txt", "hello world")];
        let snap = write_snap(&dir, &files, None, None);

        let output = render_snapshot(&snap, RenderFormat::Html).unwrap();
        assert!(
            output.contains("hello world"),
            "html must include file content; got:\n{output}"
        );
        assert!(
            output.contains("<pre>"),
            "html must wrap content in <pre> block; got:\n{output}"
        );
    }

    #[test]
    fn render_html_content_is_html_escaped() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.html", "<h1>Hello & World</h1>")];
        let snap = write_snap(&dir, &files, None, None);

        let output = render_snapshot(&snap, RenderFormat::Html).unwrap();
        assert!(
            output.contains("&lt;h1&gt;Hello &amp; World&lt;/h1&gt;"),
            "html must escape content characters; got:\n{output}"
        );
        assert!(
            !output.contains("<h1>Hello"),
            "html must not contain unescaped content tags; got:\n{output}"
        );
    }

    #[test]
    fn render_json_includes_content_field() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "hello")];
        let snap = write_snap(&dir, &files, None, None);

        let output = render_snapshot(&snap, RenderFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).expect("json must parse");
        let files = value["files"].as_array().expect("files must be array");
        assert_eq!(
            files[0]["content"],
            serde_json::Value::from("hello"),
            "json must include content field with the file text"
        );
    }

    #[test]
    fn render_markdown_symlink_omitted_from_contents_section() {
        let dir = TempDir::new().unwrap();
        let files = vec![
            text_file("a.txt", "real content"),
            symlink_file("link", "a.txt"),
        ];
        let snap = write_snap(&dir, &files, None, None);

        let output = render_snapshot(&snap, RenderFormat::Markdown).unwrap();
        // The symlink must appear in the Files table but not as a contents heading
        assert!(
            output.contains("`link`"),
            "symlink must appear in file table"
        );
        assert!(
            !output.contains("### `link`"),
            "symlink must not have a contents heading"
        );
        // The regular file must appear in both sections
        assert!(
            output.contains("### `a.txt`"),
            "regular file must have contents heading"
        );
        assert!(
            output.contains("real content"),
            "regular file content must be rendered"
        );
    }

    #[test]
    fn render_json_symlink_content_is_null() {
        let dir = TempDir::new().unwrap();
        let files = vec![
            text_file("a.txt", "has content"),
            symlink_file("link", "target.txt"),
        ];
        let snap = write_snap(&dir, &files, None, None);

        let output = render_snapshot(&snap, RenderFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&output).expect("json must parse");
        let files = value["files"].as_array().expect("files must be array");

        // Regular file must have non-null content
        let regular = files
            .iter()
            .find(|e| e["path"].as_str() == Some("a.txt"))
            .unwrap();
        assert_eq!(
            regular["content"],
            serde_json::Value::from("has content"),
            "regular file must have content field"
        );

        // Symlink must have null content (field present, value null)
        let symlink = files
            .iter()
            .find(|e| e["path"].as_str() == Some("link"))
            .unwrap();
        assert_eq!(
            symlink["content"],
            serde_json::Value::Null,
            "symlink content must be null"
        );
        // Confirm the field is explicitly serialized (not merely absent)
        assert!(
            output.contains("\"content\": null"),
            "json must explicitly emit content: null for symlinks; got:\n{output}"
        );
    }
}
