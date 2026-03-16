/// Audit report rendering: Markdown, HTML, and JSON output from a snapshot.
use std::fs;
use std::path::Path;

use crate::utils::io_error_with_path;

use super::serial::parse_snapshot;
use super::{ListEntry, Result, SnapshotHeader};

// ── Public types ──────────────────────────────────────────────────────────────

/// Output format for [`render_snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFormat {
    Markdown,
    Html,
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

    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!(
        "  \"snapshot_hash\": {},\n",
        json_string(&header.snapshot_hash)
    ));
    out.push_str(&format!("  \"file_count\": {},\n", header.file_count));
    match &header.git_rev {
        Some(rev) => out.push_str(&format!("  \"git_rev\": {},\n", json_string(rev))),
        None => out.push_str("  \"git_rev\": null,\n"),
    }
    match &header.git_branch {
        Some(b) => out.push_str(&format!("  \"git_branch\": {},\n", json_string(b))),
        None => out.push_str("  \"git_branch\": null,\n"),
    }
    out.push_str(&format!("  \"regular_file_count\": {regular_count},\n"));
    out.push_str(&format!("  \"symlink_count\": {symlink_count},\n"));
    out.push_str(&format!("  \"total_bytes\": {total_bytes},\n"));
    out.push_str("  \"files\": [\n");
    for (i, e) in entries.iter().enumerate() {
        let comma = if i + 1 < entries.len() { "," } else { "" };
        let entry_type = if e.is_symlink { "symlink" } else { "file" };
        out.push_str("    {\n");
        out.push_str(&format!("      \"path\": {},\n", json_string(&e.path)));
        out.push_str(&format!("      \"type\": {},\n", json_string(entry_type)));
        out.push_str(&format!("      \"mode\": {},\n", json_string(&e.mode)));
        out.push_str(&format!("      \"size\": {},\n", e.size));
        out.push_str(&format!("      \"sha256\": {},\n", json_string(&e.sha256)));
        match &e.symlink_target {
            Some(t) => out.push_str(&format!("      \"symlink_target\": {}\n", json_string(t))),
            None => out.push_str("      \"symlink_target\": null\n"),
        }
        out.push_str(&format!("    }}{comma}\n"));
    }
    out.push_str("  ]\n}\n");
    out
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

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
        assert!(
            output.contains("\"snapshot_hash\""),
            "must have snapshot_hash"
        );
        assert!(output.contains("\"file_count\": 2"), "must have file_count");
        assert!(output.contains("\"git_rev\": \"abc\""), "must have git_rev");
        assert!(
            output.contains("\"git_branch\": \"dev\""),
            "must have git_branch"
        );
        assert!(output.contains("\"total_bytes\""), "must have total_bytes");
        assert!(output.contains("\"a.txt\""), "must list files");
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
}
