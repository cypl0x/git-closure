/// Audit report rendering: Markdown, HTML, and JSON output from a snapshot.
use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::utils::io_error_with_path;

use super::serial::parse_snapshot;
use super::{ListEntry, Result, SnapshotHeader};

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
    let entries: Vec<ListEntry> = files
        .into_iter()
        .map(|f| ListEntry {
            is_symlink: f.symlink_target.is_some(),
            symlink_target: f.symlink_target,
            sha256: f.sha256,
            mode: f.mode,
            size: f.size,
            path: f.path,
        })
        .collect();

    match format {
        RenderFormat::Markdown => Ok(render_markdown(&header, &entries)),
        RenderFormat::Html => Ok(render_html(&header, &entries)),
        RenderFormat::Json => Ok(render_json(&header, &entries)),
    }
}

// ── Markdown renderer ─────────────────────────────────────────────────────────

fn render_markdown(header: &SnapshotHeader, entries: &[ListEntry]) -> String {
    let mut out = String::new();

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

    let (regular_count, symlink_count) = count_entry_types(entries);
    let total_bytes: u64 = entries.iter().map(|e| e.size).sum();
    out.push_str(&format!(
        "| Regular files | {regular_count} |\n| Symlinks | {symlink_count} |\n| Total bytes | {total_bytes} |\n"
    ));

    out.push_str("\n## Files\n\n");
    out.push_str("| Path | Type | Mode | Size | SHA-256 (prefix) |\n");
    out.push_str("|---|---|---|---|---|\n");

    for e in entries {
        let entry_type = if e.is_symlink { "symlink" } else { "file" };
        let sha256_display = if e.is_symlink {
            format!("→ {}", e.symlink_target.as_deref().unwrap_or(""))
        } else {
            format!("`{}`", &e.sha256[..16])
        };
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            md_escape(&e.path),
            entry_type,
            e.mode,
            e.size,
            sha256_display
        ));
    }

    out
}

// ── HTML renderer ─────────────────────────────────────────────────────────────

fn render_html(header: &SnapshotHeader, entries: &[ListEntry]) -> String {
    let mut out = String::new();

    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"UTF-8\">\n");
    out.push_str("<title>Snapshot Audit Report</title>\n");
    out.push_str("<style>body{font-family:monospace;max-width:1200px;margin:2em auto;padding:0 1em}table{border-collapse:collapse;width:100%}th,td{border:1px solid #ccc;padding:4px 8px;text-align:left}th{background:#f0f0f0}code{background:#f8f8f8;padding:2px 4px;border-radius:2px}</style>\n");
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
    let (regular_count, symlink_count) = count_entry_types(entries);
    let total_bytes: u64 = entries.iter().map(|e| e.size).sum();
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

    for e in entries {
        let entry_type = if e.is_symlink { "symlink" } else { "file" };
        let sha256_display = if e.is_symlink {
            format!(
                "→ {}",
                html_escape(e.symlink_target.as_deref().unwrap_or(""))
            )
        } else {
            format!("<code>{}</code>", &e.sha256[..16])
        };
        out.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            html_escape(&e.path),
            entry_type,
            e.mode,
            e.size,
            sha256_display
        ));
    }
    out.push_str("</tbody></table>\n</body>\n</html>\n");
    out
}

// ── JSON renderer ─────────────────────────────────────────────────────────────

fn render_json(header: &SnapshotHeader, entries: &[ListEntry]) -> String {
    let (regular_count, symlink_count) = count_entry_types(entries);
    let total_bytes: u64 = entries.iter().map(|e| e.size).sum();

    let files: Vec<RenderJsonFile<'_>> = entries
        .iter()
        .map(|entry| RenderJsonFile {
            path: entry.path.as_str(),
            entry_type: if entry.is_symlink { "symlink" } else { "file" },
            mode: entry.mode.as_str(),
            size: entry.size,
            sha256: entry.sha256.as_str(),
            symlink_target: entry.symlink_target.as_deref(),
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
        files,
    };

    let mut json = serde_json::to_string_pretty(&payload).expect("serialize render JSON");
    json.push('\n');
    json
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn count_entry_types(entries: &[ListEntry]) -> (usize, usize) {
    let symlinks = entries.iter().filter(|e| e.is_symlink).count();
    (entries.len() - symlinks, symlinks)
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
            .find(|entry| entry["path"] == serde_json::Value::from("link"))
            .expect("json must include link entry");
        assert_eq!(link_entry["type"], serde_json::Value::from("symlink"));
        assert_eq!(
            link_entry["symlink_target"],
            serde_json::Value::from("a.txt")
        );
    }
}
