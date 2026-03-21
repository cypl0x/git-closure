/// Audit report rendering: text, Markdown, HTML, and JSON output from a snapshot.
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
    /// Plain-text terminal output (default).
    Text,
    /// Markdown report. When `pandoc` is true, a YAML front-matter block is
    /// prepended so that `git-closure render | pandoc -o report.pdf` works.
    Markdown { pandoc: bool },
    /// Standalone HTML page.
    Html,
    /// Pretty-printed JSON.
    Json,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Renders a snapshot file as an audit report in the given format.
pub fn render_snapshot(snapshot: &Path, format: RenderFormat) -> Result<String> {
    let text = fs::read_to_string(snapshot).map_err(|err| io_error_with_path(err, snapshot))?;
    let (header, files) = parse_snapshot(&text)?;
    Ok(match format {
        RenderFormat::Text => TextRenderer.render_document(&header, &files),
        RenderFormat::Markdown { pandoc } => {
            MarkdownRenderer { pandoc }.render_document(&header, &files)
        }
        RenderFormat::Html => HtmlRenderer.render_document(&header, &files),
        RenderFormat::Json => render_json(&header, &files),
    })
}

// ── Renderer trait ────────────────────────────────────────────────────────────

/// Structural skeleton shared by all non-JSON renderers.
///
/// Each implementation provides `render_header`, `render_file`, and
/// `render_symlink`.  The default `render_document` assembles them in `.gcl`
/// order: optional prologue → header → entries → optional epilogue.
trait Renderer {
    /// Optional document prologue emitted before the header (e.g. HTML `<head>`).
    fn prologue(&self) -> String {
        String::new()
    }
    /// Format the snapshot-level metadata section.
    fn render_header(&self, header: &SnapshotHeader) -> String;
    /// Format one regular or binary file entry (metadata + content).
    fn render_file(&self, file: &SnapshotFile) -> String;
    /// Format one symlink entry (metadata only, no content).
    fn render_symlink(&self, file: &SnapshotFile) -> String;
    /// Optional document epilogue emitted after all entries (e.g. HTML `</body>`).
    fn epilogue(&self) -> String {
        String::new()
    }

    /// Assemble the full document in `.gcl` order.
    fn render_document(&self, header: &SnapshotHeader, files: &[SnapshotFile]) -> String {
        let mut out = self.prologue();
        out.push_str(&self.render_header(header));
        for f in files {
            if f.symlink_target.is_some() {
                out.push_str(&self.render_symlink(f));
            } else {
                out.push_str(&self.render_file(f));
            }
        }
        out.push_str(&self.epilogue());
        out
    }
}

// ── Text renderer ─────────────────────────────────────────────────────────────

const TEXT_SEP: &str = "────────────────────────────────────────────────\n";

struct TextRenderer;

impl Renderer for TextRenderer {
    fn render_header(&self, header: &SnapshotHeader) -> String {
        let mut out = String::new();
        out.push_str(&format!("snapshot-hash: {}\n", header.snapshot_hash));
        out.push_str(&format!("file-count:    {}\n", header.file_count));
        if let Some(rev) = &header.git_rev {
            out.push_str(&format!("git-rev:       {rev}\n"));
        }
        if let Some(branch) = &header.git_branch {
            out.push_str(&format!("git-branch:    {branch}\n"));
        }
        out.push('\n');
        out
    }

    fn render_file(&self, file: &SnapshotFile) -> String {
        let mut out = String::new();
        out.push_str(TEXT_SEP);
        out.push_str(&format!("path:   {}\n", file.path));
        out.push_str(&format!("mode:   {}\n", file.mode));
        out.push_str(&format!("size:   {} bytes\n", file.size));
        out.push_str(&format!("sha256: {}\n", sha256_prefix(&file.sha256)));
        if file.encoding.as_deref() == Some("base64") {
            out.push_str("encoding: base64\n");
            out.push('\n');
            out.push_str(&format!("[binary content, {} bytes]\n", file.size));
        } else {
            let text = std::str::from_utf8(&file.content)
                .expect("non-base64 content must be valid UTF-8 (invariant violated)");
            out.push('\n');
            out.push_str(text);
            if !text.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }

    fn render_symlink(&self, file: &SnapshotFile) -> String {
        let target = file.symlink_target.as_deref().unwrap_or("");
        let mut out = String::new();
        out.push_str(TEXT_SEP);
        out.push_str(&format!("path:   {}\n", file.path));
        out.push_str(&format!("type:   symlink \u{2192} {target}\n"));
        out.push_str(&format!("mode:   {}\n", file.mode));
        out.push('\n');
        out
    }
}

// ── Markdown renderer ─────────────────────────────────────────────────────────

struct MarkdownRenderer {
    pandoc: bool,
}

impl Renderer for MarkdownRenderer {
    fn render_header(&self, header: &SnapshotHeader) -> String {
        let mut out = String::new();
        if self.pandoc {
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
        }
        out.push_str("# Snapshot Audit Report\n\n");
        out.push_str(&format!("snapshot-hash: `{}`\n", header.snapshot_hash));
        out.push_str(&format!("file-count: {}\n", header.file_count));
        if let Some(rev) = &header.git_rev {
            out.push_str(&format!("git-rev: `{rev}`\n"));
        }
        if let Some(branch) = &header.git_branch {
            out.push_str(&format!("git-branch: `{branch}`\n"));
        }
        out.push('\n');
        out
    }

    fn render_file(&self, file: &SnapshotFile) -> String {
        let mut out = String::new();
        out.push_str("---\n\n");
        out.push_str(&format!("## `{}`\n\n", md_escape(&file.path)));
        let sha = sha256_prefix(&file.sha256);
        if file.encoding.as_deref() == Some("base64") {
            out.push_str(&format!(
                "mode: `{}` \u{00b7} size: {} bytes \u{00b7} sha256: `{}` \u{00b7} encoding: base64\n\n",
                file.mode, file.size, sha
            ));
            out.push_str(&format!("[binary content, {} bytes]\n", file.size));
        } else {
            out.push_str(&format!(
                "mode: `{}` \u{00b7} size: {} bytes \u{00b7} sha256: `{}`\n\n",
                file.mode, file.size, sha
            ));
            let text = std::str::from_utf8(&file.content)
                .expect("non-base64 content must be valid UTF-8 (invariant violated)");
            let lang = lang_hint(&file.path);
            out.push_str(&format!("```{lang}\n{text}\n```\n"));
        }
        out
    }

    fn render_symlink(&self, file: &SnapshotFile) -> String {
        let target = file.symlink_target.as_deref().unwrap_or("");
        let mut out = String::new();
        out.push_str("---\n\n");
        out.push_str(&format!(
            "## `{}` \u{2192} `{}`\n\n",
            md_escape(&file.path),
            md_escape(target)
        ));
        out.push_str(&format!("symlink \u{00b7} mode: `{}`\n", file.mode));
        out
    }
}

// ── HTML renderer ─────────────────────────────────────────────────────────────

struct HtmlRenderer;

impl Renderer for HtmlRenderer {
    fn prologue(&self) -> String {
        concat!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n",
            "<meta charset=\"UTF-8\">\n",
            "<title>Snapshot Audit Report</title>\n",
            "<style>",
            "body{font-family:monospace;max-width:1200px;margin:2em auto;padding:0 1em}",
            "h2{border-top:1px solid #ccc;padding-top:0.5em}",
            "code{background:#f8f8f8;padding:2px 4px;border-radius:2px}",
            "pre{background:#f8f8f8;padding:1em;overflow-x:auto;white-space:pre-wrap}",
            "</style>\n",
            "</head>\n<body>\n",
        )
        .to_string()
    }

    fn render_header(&self, header: &SnapshotHeader) -> String {
        let mut out = String::new();
        out.push_str("<h1>Snapshot Audit Report</h1>\n<p>\n");
        out.push_str(&format!(
            "snapshot-hash: <code>{}</code><br>\n",
            html_escape(&header.snapshot_hash)
        ));
        out.push_str(&format!("file-count: {}<br>\n", header.file_count));
        if let Some(rev) = &header.git_rev {
            out.push_str(&format!("git-rev: <code>{}</code><br>\n", html_escape(rev)));
        }
        if let Some(branch) = &header.git_branch {
            out.push_str(&format!(
                "git-branch: <code>{}</code><br>\n",
                html_escape(branch)
            ));
        }
        out.push_str("</p>\n");
        out
    }

    fn render_file(&self, file: &SnapshotFile) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "<section>\n<h2><code>{}</code></h2>\n",
            html_escape(&file.path)
        ));
        let sha = sha256_prefix(&file.sha256);
        if file.encoding.as_deref() == Some("base64") {
            out.push_str(&format!(
                "<p>mode: <code>{}</code> \u{00b7} size: {} bytes \u{00b7} sha256: <code>{}</code> \u{00b7} encoding: base64</p>\n",
                file.mode, file.size, sha
            ));
            out.push_str(&format!(
                "<pre>[binary content, {} bytes]</pre>\n",
                file.size
            ));
        } else {
            out.push_str(&format!(
                "<p>mode: <code>{}</code> \u{00b7} size: {} bytes \u{00b7} sha256: <code>{}</code></p>\n",
                file.mode, file.size, sha
            ));
            let text = std::str::from_utf8(&file.content)
                .expect("non-base64 content must be valid UTF-8 (invariant violated)");
            out.push_str(&format!("<pre><code>{}</code></pre>\n", html_escape(text)));
        }
        out.push_str("</section>\n");
        out
    }

    fn render_symlink(&self, file: &SnapshotFile) -> String {
        let target = file.symlink_target.as_deref().unwrap_or("");
        let mut out = String::new();
        out.push_str(&format!(
            "<section>\n<h2><code>{}</code> \u{2192} <code>{}</code></h2>\n",
            html_escape(&file.path),
            html_escape(target)
        ));
        out.push_str(&format!(
            "<p>symlink \u{00b7} mode: <code>{}</code></p>\n",
            file.mode
        ));
        out.push_str("</section>\n");
        out
    }

    fn epilogue(&self) -> String {
        "</body>\n</html>\n".to_string()
    }
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

    fn binary_file(path: &str, bytes: Vec<u8>) -> SnapshotFile {
        let sha = sha256_hex(&bytes);
        let size = bytes.len() as u64;
        SnapshotFile {
            path: path.to_string(),
            sha256: sha,
            mode: "644".to_string(),
            size,
            encoding: Some("base64".to_string()),
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

    // ── Text renderer tests ───────────────────────────────────────────────────

    #[test]
    fn render_text_contains_snapshot_metadata() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "hello")];
        let snap = write_snap(&dir, &files, None, None);

        let out = render_snapshot(&snap, RenderFormat::Text).unwrap();
        assert!(
            out.contains("snapshot-hash:"),
            "text must include snapshot-hash key"
        );
        assert!(out.contains("file-count:"), "text must include file-count");
        assert!(
            !out.contains("git-rev:"),
            "git-rev must be absent when not set"
        );
    }

    #[test]
    fn render_text_includes_git_metadata_when_present() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "x")];
        let snap = write_snap(&dir, &files, Some("cafebabe"), Some("main"));

        let out = render_snapshot(&snap, RenderFormat::Text).unwrap();
        assert!(out.contains("git-rev:       cafebabe"), "must show git-rev");
        assert!(out.contains("git-branch:    main"), "must show git-branch");
    }

    #[test]
    fn render_text_has_separator_before_each_entry() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "a"), text_file("b.txt", "b")];
        let snap = write_snap(&dir, &files, None, None);

        let out = render_snapshot(&snap, RenderFormat::Text).unwrap();
        assert!(
            out.contains('─'),
            "text must include box-drawing separator characters"
        );
        // Two files → two separators
        assert_eq!(
            out.matches(TEXT_SEP).count(),
            2,
            "must have one separator per entry"
        );
    }

    #[test]
    fn render_text_includes_file_path_mode_size_sha256() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("src/main.rs", "fn main() {}")];
        let snap = write_snap(&dir, &files, None, None);

        let out = render_snapshot(&snap, RenderFormat::Text).unwrap();
        assert!(out.contains("path:   src/main.rs"), "must show path");
        assert!(out.contains("mode:   644"), "must show mode");
        assert!(out.contains("size:   12 bytes"), "must show size");
        assert!(out.contains("sha256:"), "must show sha256 prefix");
    }

    #[test]
    fn render_text_includes_file_content() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "hello world")];
        let snap = write_snap(&dir, &files, None, None);

        let out = render_snapshot(&snap, RenderFormat::Text).unwrap();
        assert!(
            out.contains("hello world"),
            "text must include file content; got:\n{out}"
        );
    }

    #[test]
    fn render_text_multiline_content_uses_real_newlines() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "line1\nline2")];
        let snap = write_snap(&dir, &files, None, None);

        let out = render_snapshot(&snap, RenderFormat::Text).unwrap();
        assert!(
            out.contains("line1\nline2"),
            "must render real newlines, not \\n escapes"
        );
        assert!(
            !out.contains("line1\\nline2"),
            "must not contain literal backslash-n"
        );
    }

    #[test]
    fn render_text_binary_shows_note() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(
            &dir,
            &[binary_file("data.bin", vec![0xFF, 0x00])],
            None,
            None,
        );

        let out = render_snapshot(&snap, RenderFormat::Text).unwrap();
        assert!(
            out.contains("[binary content, 2 bytes]"),
            "must note binary content; got:\n{out}"
        );
        assert!(
            out.contains("encoding: base64"),
            "must show encoding field for binary; got:\n{out}"
        );
    }

    #[test]
    fn render_text_symlink_shows_target_inline() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(&dir, &[symlink_file("link", "a.txt")], None, None);

        let out = render_snapshot(&snap, RenderFormat::Text).unwrap();
        assert!(
            out.contains("type:   symlink → a.txt"),
            "must show symlink target inline; got:\n{out}"
        );
        assert!(out.contains("path:   link"), "must show symlink path");
        assert!(out.contains("mode:   120000"), "must show symlink mode");
    }

    #[test]
    fn render_text_symlink_has_no_content_block() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "real"), symlink_file("link", "a.txt")];
        let snap = write_snap(&dir, &files, None, None);

        let out = render_snapshot(&snap, RenderFormat::Text).unwrap();
        // The regular file content appears; the symlink does not repeat target as content
        assert!(out.contains("real"), "regular file content must appear");
        // Count separators: one per entry (2 entries = 2 separators)
        assert_eq!(out.matches(TEXT_SEP).count(), 2);
    }

    // ── Markdown renderer tests ───────────────────────────────────────────────

    #[test]
    fn render_markdown_has_h1_and_metadata() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("src/main.rs", "fn main() {}")];
        let snap = write_snap(&dir, &files, None, None);

        let out = render_snapshot(&snap, RenderFormat::Markdown { pandoc: false }).unwrap();
        assert!(
            out.starts_with("# Snapshot Audit Report\n"),
            "markdown without --pandoc must start with h1; got:\n{out}"
        );
        assert!(out.contains("snapshot-hash:"), "must show snapshot-hash");
        assert!(out.contains("file-count: 1"), "must show file-count");
    }

    #[test]
    fn render_markdown_no_pandoc_no_front_matter() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "x")];
        let snap = write_snap(&dir, &files, None, None);

        let out = render_snapshot(&snap, RenderFormat::Markdown { pandoc: false }).unwrap();
        assert!(
            !out.starts_with("---\n"),
            "without --pandoc, must not have YAML front matter"
        );
        assert!(
            !out.contains("title:"),
            "without --pandoc, must not have title field"
        );
    }

    #[test]
    fn render_markdown_pandoc_prepends_yaml_front_matter() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "hello")];
        let snap = write_snap(&dir, &files, Some("cafebabe"), Some("main"));

        let out = render_snapshot(&snap, RenderFormat::Markdown { pandoc: true }).unwrap();
        assert!(
            out.starts_with("---\n"),
            "with --pandoc, must start with YAML front matter"
        );
        assert!(
            out.contains("title: \"Snapshot Audit Report\""),
            "front matter must include title"
        );
        assert!(
            out.contains("git-rev: \"cafebabe\""),
            "front matter must include git-rev"
        );
        assert!(
            out.contains("git-branch: \"main\""),
            "front matter must include git-branch"
        );
        // After front matter, h1 follows
        let fm_end = out
            .find("---\n\n")
            .expect("front matter must close with ---");
        assert!(
            out[fm_end + 5..].starts_with("# Snapshot Audit Report"),
            "h1 must follow front matter"
        );
    }

    #[test]
    fn render_markdown_pandoc_omits_git_fields_when_absent() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(&dir, &[text_file("a.txt", "x")], None, None);

        let out = render_snapshot(&snap, RenderFormat::Markdown { pandoc: true }).unwrap();
        assert!(out.starts_with("---\n"), "front matter present");
        assert!(!out.contains("git-rev:"), "git-rev absent when not set");
        assert!(
            !out.contains("git-branch:"),
            "git-branch absent when not set"
        );
    }

    #[test]
    fn render_markdown_file_uses_h2_heading() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(
            &dir,
            &[text_file("src/main.rs", "fn main() {}")],
            None,
            None,
        );

        let out = render_snapshot(&snap, RenderFormat::Markdown { pandoc: false }).unwrap();
        assert!(
            out.contains("## `src/main.rs`"),
            "file entry must use h2 heading with backtick path; got:\n{out}"
        );
    }

    #[test]
    fn render_markdown_includes_file_content() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(&dir, &[text_file("a.rs", "fn main() {}")], None, None);

        let out = render_snapshot(&snap, RenderFormat::Markdown { pandoc: false }).unwrap();
        assert!(
            out.contains("fn main() {}"),
            "must include file content; got:\n{out}"
        );
        assert!(
            out.contains("```rust"),
            "must use rust lang hint for .rs files; got:\n{out}"
        );
    }

    #[test]
    fn render_markdown_multiline_content_uses_real_newlines() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(&dir, &[text_file("a.txt", "line1\nline2")], None, None);

        let out = render_snapshot(&snap, RenderFormat::Markdown { pandoc: false }).unwrap();
        assert!(
            out.contains("line1\nline2"),
            "must render real newlines; got:\n{out}"
        );
        assert!(!out.contains("line1\\nline2"), "must not escape newlines");
    }

    #[test]
    fn render_markdown_binary_file_shows_note() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(
            &dir,
            &[binary_file("data.bin", vec![0xFF, 0x00])],
            None,
            None,
        );

        let out = render_snapshot(&snap, RenderFormat::Markdown { pandoc: false }).unwrap();
        assert!(
            out.contains("[binary content, 2 bytes]"),
            "must note binary content; got:\n{out}"
        );
    }

    #[test]
    fn render_markdown_symlink_uses_arrow_heading() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(&dir, &[symlink_file("link", "a.txt")], None, None);

        let out = render_snapshot(&snap, RenderFormat::Markdown { pandoc: false }).unwrap();
        assert!(
            out.contains("## `link` → `a.txt`"),
            "symlink must use arrow heading; got:\n{out}"
        );
        assert!(
            out.contains("symlink"),
            "symlink section must say symlink; got:\n{out}"
        );
    }

    #[test]
    fn render_markdown_symlink_has_no_content_fenced_block() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "real"), symlink_file("link", "a.txt")];
        let snap = write_snap(&dir, &files, None, None);

        let out = render_snapshot(&snap, RenderFormat::Markdown { pandoc: false }).unwrap();
        // The symlink section must not be followed by a fenced code block.
        // Find the symlink heading and verify no fence appears before the next section.
        let symlink_pos = out
            .find("## `link`")
            .expect("symlink heading must be present");
        let after_symlink = &out[symlink_pos..];
        // There must be no opening fence in the symlink section
        assert!(
            !after_symlink.contains("```"),
            "symlink section must not have a fenced code block; got:\n{out}"
        );
        // The regular file must have a code block
        assert!(out.contains("real"), "regular file content must appear");
    }

    #[test]
    fn render_markdown_includes_git_metadata_when_present() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(
            &dir,
            &[text_file("a.txt", "a")],
            Some("cafebabe"),
            Some("main"),
        );

        let out = render_snapshot(&snap, RenderFormat::Markdown { pandoc: false }).unwrap();
        assert!(out.contains("cafebabe"), "must include git revision");
        assert!(out.contains("main"), "must include branch name");
    }

    #[test]
    fn render_markdown_no_inventory_table() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(&dir, &[text_file("a.txt", "x")], None, None);

        let out = render_snapshot(&snap, RenderFormat::Markdown { pandoc: false }).unwrap();
        assert!(
            !out.contains("| Path |"),
            "new markdown must not have inventory table; got:\n{out}"
        );
        assert!(
            !out.contains("|---|"),
            "new markdown must not have table separators; got:\n{out}"
        );
    }

    // ── HTML renderer tests ───────────────────────────────────────────────────

    #[test]
    fn render_html_is_valid_html_structure() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(
            &dir,
            &[text_file("src/main.rs", "fn main() {}")],
            None,
            None,
        );

        let out = render_snapshot(&snap, RenderFormat::Html).unwrap();
        assert!(out.starts_with("<!DOCTYPE html>"), "must be proper HTML");
        assert!(out.ends_with("</html>\n"), "must end with </html>");
        assert!(out.contains("src/main.rs"), "must list file path");
        assert!(
            out.contains("<section>"),
            "must use <section> per file; got:\n{out}"
        );
    }

    #[test]
    fn render_html_file_uses_h2_heading() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(&dir, &[text_file("a.txt", "hello")], None, None);

        let out = render_snapshot(&snap, RenderFormat::Html).unwrap();
        assert!(
            out.contains("<h2><code>a.txt</code></h2>"),
            "file must use h2 with code path; got:\n{out}"
        );
    }

    #[test]
    fn render_html_includes_file_content() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(&dir, &[text_file("x.txt", "hello world")], None, None);

        let out = render_snapshot(&snap, RenderFormat::Html).unwrap();
        assert!(
            out.contains("hello world"),
            "html must include file content; got:\n{out}"
        );
        assert!(out.contains("<pre>"), "must wrap content in <pre>");
    }

    #[test]
    fn render_html_content_is_html_escaped() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(
            &dir,
            &[text_file("a.html", "<h1>Hello & World</h1>")],
            None,
            None,
        );

        let out = render_snapshot(&snap, RenderFormat::Html).unwrap();
        assert!(
            out.contains("&lt;h1&gt;Hello &amp; World&lt;/h1&gt;"),
            "must escape content characters; got:\n{out}"
        );
    }

    #[test]
    fn render_html_symlink_uses_arrow_heading() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(&dir, &[symlink_file("link", "a.txt")], None, None);

        let out = render_snapshot(&snap, RenderFormat::Html).unwrap();
        assert!(
            out.contains("<code>link</code>") && out.contains("<code>a.txt</code>"),
            "html symlink must show both path and target; got:\n{out}"
        );
        assert!(
            out.contains("symlink"),
            "html symlink section must say symlink; got:\n{out}"
        );
    }

    #[test]
    fn render_html_no_inventory_table() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(&dir, &[text_file("a.txt", "x")], None, None);

        let out = render_snapshot(&snap, RenderFormat::Html).unwrap();
        assert!(
            !out.contains("<table>"),
            "new html must not use inventory tables; got:\n{out}"
        );
    }

    // ── JSON renderer tests ───────────────────────────────────────────────────

    #[test]
    fn render_json_is_parseable_and_contains_expected_fields() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "aaa"), text_file("b.txt", "bb")];
        let snap = write_snap(&dir, &files, Some("abc"), Some("dev"));

        let out = render_snapshot(&snap, RenderFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).expect("json must parse");
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
        let snap = write_snap(&dir, &[text_file("x.txt", "x")], None, None);

        let out = render_snapshot(&snap, RenderFormat::Json).unwrap();
        assert!(out.contains("\"git_rev\": null"));
        assert!(out.contains("\"git_branch\": null"));
    }

    #[test]
    fn render_json_includes_content_field() {
        let dir = TempDir::new().unwrap();
        let snap = write_snap(&dir, &[text_file("a.txt", "hello")], None, None);

        let out = render_snapshot(&snap, RenderFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).expect("json must parse");
        let files = value["files"].as_array().expect("files must be array");
        assert_eq!(files[0]["content"], serde_json::Value::from("hello"));
    }

    #[test]
    fn render_json_symlink_content_is_null() {
        let dir = TempDir::new().unwrap();
        let files = vec![
            text_file("a.txt", "has content"),
            symlink_file("link", "target.txt"),
        ];
        let snap = write_snap(&dir, &files, None, None);

        let out = render_snapshot(&snap, RenderFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).expect("json must parse");
        let files = value["files"].as_array().expect("files must be array");

        let regular = files
            .iter()
            .find(|e| e["path"].as_str() == Some("a.txt"))
            .unwrap();
        assert_eq!(regular["content"], serde_json::Value::from("has content"));

        let symlink = files
            .iter()
            .find(|e| e["path"].as_str() == Some("link"))
            .unwrap();
        assert_eq!(symlink["content"], serde_json::Value::Null);
        assert!(
            out.contains("\"content\": null"),
            "must explicitly emit content: null for symlinks"
        );
    }

    // ── Cross-format consistency tests ────────────────────────────────────────

    #[test]
    fn render_outputs_are_deterministic_for_same_input() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "aaa"), symlink_file("link", "a.txt")];
        let snap = write_snap(&dir, &files, Some("abc123"), Some("main"));

        let md_a = render_snapshot(&snap, RenderFormat::Markdown { pandoc: false }).unwrap();
        let md_b = render_snapshot(&snap, RenderFormat::Markdown { pandoc: false }).unwrap();
        assert_eq!(md_a, md_b, "markdown output must be deterministic");

        let txt_a = render_snapshot(&snap, RenderFormat::Text).unwrap();
        let txt_b = render_snapshot(&snap, RenderFormat::Text).unwrap();
        assert_eq!(txt_a, txt_b, "text output must be deterministic");

        let html_a = render_snapshot(&snap, RenderFormat::Html).unwrap();
        let html_b = render_snapshot(&snap, RenderFormat::Html).unwrap();
        assert_eq!(html_a, html_b, "html output must be deterministic");

        let json_a = render_snapshot(&snap, RenderFormat::Json).unwrap();
        let json_b = render_snapshot(&snap, RenderFormat::Json).unwrap();
        assert_eq!(json_a, json_b, "json output must be deterministic");
    }
}
