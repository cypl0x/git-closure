//! GCL-to-NAR export bridge.
//!
//! Converts a parsed `.gcl` snapshot into a [`NarNode`] tree and writes it as
//! a binary NAR archive.
//!
//! # Metadata loss
//!
//! The following `.gcl` fields have **no NAR equivalent** and are silently
//! dropped during export:
//!
//! - `snapshot-hash`, `file-count`, `git-rev`, `git-branch`, and any
//!   `source-uri` / `source-provider` extra headers — NAR is a pure
//!   filesystem tree archive with no provenance metadata.
//! - Per-file `sha256` — not stored in NAR; recomputable from the content.
//! - Per-file `size` — implicit in the NAR content length field.
//! - Full Unix mode string — only the executable/non-executable distinction is
//!   preserved (matching Nix's own semantics).
//! - The `encoding` hint (`"base64"`) — base64 is decoded transparently by the
//!   snapshot parser before this function receives the file records.
//!
//! This loss is by design: use the `.gcl` format for auditability and
//! provenance tracking.

use std::fs;
use std::io;
use std::path::Path;

use crate::backends::ArtifactBackend;
use crate::error::GitClosureError;
use crate::snapshot::serial::parse_snapshot;

#[cfg(test)]
use crate::nar::{write_nar, NarNode};
#[cfg(test)]
use crate::snapshot::SnapshotFile;
#[cfg(test)]
use std::collections::BTreeMap;

type Result<T> = std::result::Result<T, GitClosureError>;

/// Export a `.gcl` snapshot file as a binary NAR archive.
///
/// Reads and parses `snapshot_path`, converts the file list to a NAR tree,
/// and writes the NAR byte stream to `output_path`.
///
/// # Errors
///
/// Returns an error if `snapshot_path` cannot be read, the snapshot is
/// malformed, or `output_path` cannot be created or written.
///
/// # Metadata loss
///
/// See the [module-level documentation](self) for the list of `.gcl` fields
/// that are dropped during NAR export.
pub fn export_snapshot_as_nar(snapshot_path: &Path, output_path: &Path) -> Result<()> {
    let text = fs::read_to_string(snapshot_path).map_err(|e| {
        GitClosureError::Io(io::Error::new(
            e.kind(),
            format!("{}: {e}", snapshot_path.display()),
        ))
    })?;

    let (header, files) = parse_snapshot(&text)?;
    let closure = crate::ir::Closure::from((header, files));
    crate::backends::nar::NarBackend.write(&closure, output_path)
}

/// Convert a flat list of [`SnapshotFile`] entries into a [`NarNode`] tree.
#[cfg(test)]
///
/// Each entry's slash-delimited `path` is split into components and inserted
/// recursively into a [`BTreeMap`]-based tree.  [`BTreeMap`] guarantees
/// strictly ascending lexicographic iteration order, satisfying the NAR wire
/// format requirement automatically.
///
/// The input list must be in lexicographic path order (guaranteed by the
/// `.gcl` snapshot format invariant).
///
/// # Errors
///
/// Returns a [`GitClosureError::Parse`] if a path component is used as both a
/// file leaf and a directory prefix (which would indicate a malformed
/// snapshot).
pub(crate) fn build_nar_tree(files: Vec<SnapshotFile>) -> Result<NarNode> {
    let mut root: BTreeMap<String, NarNode> = BTreeMap::new();
    for file in files {
        // Clone path so that `components` (which borrows from the path string)
        // does not conflict with moving `file` into `insert_into_tree`.
        let path = file.path.clone();
        let components: Vec<&str> = path.split('/').collect();
        insert_into_tree(&mut root, &components, file)?;
    }
    Ok(NarNode::Directory(root))
}

/// Recursively insert a [`SnapshotFile`] into `tree` following `components`.
#[cfg(test)]
fn insert_into_tree(
    tree: &mut BTreeMap<String, NarNode>,
    components: &[&str],
    file: SnapshotFile,
) -> Result<()> {
    debug_assert!(
        !components.is_empty(),
        "insert_into_tree called with empty path slice"
    );

    let first = components[0];
    let rest = &components[1..];

    if rest.is_empty() {
        // Leaf: insert the file or symlink directly.
        if tree.contains_key(first) {
            return Err(GitClosureError::Parse(format!(
                "NAR tree conflict: '{}' is already a directory but is used as a file name",
                file.path
            )));
        }
        let leaf = if let Some(target) = file.symlink_target {
            NarNode::Symlink { target }
        } else {
            NarNode::File {
                executable: is_executable(&file.mode),
                content: file.content,
            }
        };
        tree.insert(first.to_string(), leaf);
    } else {
        // Intermediate directory component.
        let entry = tree
            .entry(first.to_string())
            .or_insert_with(|| NarNode::Directory(BTreeMap::new()));
        match entry {
            NarNode::Directory(ref mut inner) => {
                insert_into_tree(inner, rest, file)?;
            }
            _ => {
                return Err(GitClosureError::Parse(format!(
                    "NAR tree conflict: path component '{}' in '{}' was previously inserted as a file",
                    first, file.path
                )));
            }
        }
    }
    Ok(())
}

/// Return `true` if the octal mode string indicates an executable file.
///
/// Any execute bit (owner, group, or other) causes the file to be serialized
/// with `TOK_EXE`.  This matches Nix's own semantics: only the presence or
/// absence of any execute permission is preserved; specific bit patterns are
/// not representable in NAR.
///
/// Symlinks (mode `"120000"`) must be identified and handled before calling
/// this function; they never reach this predicate.
pub(crate) fn is_executable(mode: &str) -> bool {
    // git uses both short ("644", "755") and long ("100644", "100755") forms.
    u32::from_str_radix(mode, 8)
        .map(|m| (m & 0o111) != 0)
        .unwrap_or(false)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file(path: &str, content: &[u8], mode: &str) -> SnapshotFile {
        SnapshotFile {
            path: path.to_string(),
            sha256: "a".repeat(64),
            mode: mode.to_string(),
            size: content.len() as u64,
            encoding: None,
            symlink_target: None,
            content: content.to_vec(),
        }
    }

    fn make_symlink(path: &str, target: &str) -> SnapshotFile {
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

    // ── is_executable ─────────────────────────────────────────────────────────

    #[test]
    fn is_executable_mode_644_is_false() {
        assert!(!is_executable("100644"), "100644 must not be executable");
        assert!(!is_executable("644"), "644 must not be executable");
    }

    #[test]
    fn is_executable_mode_755_is_true() {
        assert!(is_executable("100755"), "100755 must be executable");
        assert!(is_executable("755"), "755 must be executable");
        assert!(is_executable("0755"), "0755 must be executable");
    }

    #[test]
    fn is_executable_mode_111_only_execute_bits() {
        assert!(is_executable("111"), "pure execute bits must be executable");
    }

    #[test]
    fn is_executable_invalid_mode_returns_false_not_panic() {
        assert!(
            !is_executable("invalid"),
            "unparsable mode must return false, not panic"
        );
        assert!(
            !is_executable(""),
            "empty mode must return false, not panic"
        );
    }

    // ── build_nar_tree ────────────────────────────────────────────────────────

    #[test]
    fn build_tree_single_file_produces_directory_root_with_file_leaf() {
        let files = vec![make_file("hello.txt", b"hello", "644")];
        let node = build_nar_tree(files).expect("build_nar_tree must succeed");
        match node {
            NarNode::Directory(ref entries) => {
                assert_eq!(entries.len(), 1, "root must contain exactly one entry");
                assert!(
                    entries.contains_key("hello.txt"),
                    "root must contain 'hello.txt'"
                );
                match &entries["hello.txt"] {
                    NarNode::File { executable, .. } => {
                        assert!(!executable, "mode 644 must not be executable");
                    }
                    other => panic!("expected File node, got {other:?}"),
                }
            }
            other => panic!("expected Directory root, got {other:?}"),
        }
    }

    #[test]
    fn build_tree_nested_path_creates_intermediate_directories() {
        let files = vec![make_file("a/b/c.txt", b"c", "644")];
        let node = build_nar_tree(files).expect("build_nar_tree must succeed");
        match node {
            NarNode::Directory(ref root) => match root.get("a").expect("root must contain 'a'") {
                NarNode::Directory(ref a) => match a.get("b").expect("'a' must contain 'b'") {
                    NarNode::Directory(ref b) => {
                        assert!(b.contains_key("c.txt"), "'b' must contain 'c.txt'");
                    }
                    other => panic!("expected Directory for 'b', got {other:?}"),
                },
                other => panic!("expected Directory for 'a', got {other:?}"),
            },
            other => panic!("expected Directory root, got {other:?}"),
        }
    }

    #[test]
    fn build_tree_symlink_produces_symlink_leaf() {
        let files = vec![make_symlink("link", "target.txt")];
        let node = build_nar_tree(files).expect("build_nar_tree must succeed");
        match node {
            NarNode::Directory(ref entries) => {
                match entries.get("link").expect("root must contain 'link'") {
                    NarNode::Symlink { target } => {
                        assert_eq!(target, "target.txt", "symlink target must be preserved");
                    }
                    other => panic!("expected Symlink, got {other:?}"),
                }
            }
            other => panic!("expected Directory root, got {other:?}"),
        }
    }

    #[test]
    fn build_tree_executable_file_sets_executable_flag() {
        let files = vec![make_file("run.sh", b"#!/bin/sh", "755")];
        let node = build_nar_tree(files).expect("build_nar_tree must succeed");
        match node {
            NarNode::Directory(ref entries) => {
                match entries.get("run.sh").expect("root must contain 'run.sh'") {
                    NarNode::File { executable, .. } => {
                        assert!(*executable, "mode 755 must be executable");
                    }
                    other => panic!("expected File, got {other:?}"),
                }
            }
            other => panic!("expected Directory root, got {other:?}"),
        }
    }

    #[test]
    fn build_tree_file_and_directory_conflict_is_error() {
        // "a" used as a file leaf AND "a/b" tries to use "a" as a directory.
        let files = vec![
            make_file("a", b"file-a", "644"),
            make_file("a/b", b"file-b", "644"),
        ];
        let result = build_nar_tree(files);
        assert!(
            result.is_err(),
            "using the same path component as both file and directory must return an error"
        );
    }

    #[test]
    fn build_tree_multiple_files_constructs_correct_structure() {
        // This mirrors the tests/fixtures/simple.gcl content structure.
        let files = vec![
            make_file("alpha.txt", b"alpha\n", "644"),
            make_file("bin/data.bin", b"\x00\xff", "644"),
            make_symlink("link", "alpha.txt"),
            make_file("nested/beta.txt", b"beta\n", "644"),
            make_file("scripts/run.sh", b"#!/bin/sh\nprintf \"ok\\n\"\n", "755"),
        ];
        let node = build_nar_tree(files).expect("build_nar_tree must succeed");
        match node {
            NarNode::Directory(ref entries) => {
                assert_eq!(entries.len(), 5, "root must contain 5 top-level entries");
                assert!(entries.contains_key("alpha.txt"));
                assert!(entries.contains_key("bin"));
                assert!(entries.contains_key("link"));
                assert!(entries.contains_key("nested"));
                assert!(entries.contains_key("scripts"));
                // Verify executable flag on scripts/run.sh
                match &entries["scripts"] {
                    NarNode::Directory(ref scripts) => match scripts.get("run.sh") {
                        Some(NarNode::File { executable, .. }) => {
                            assert!(*executable, "scripts/run.sh (mode 755) must be executable");
                        }
                        _ => panic!("expected File for scripts/run.sh"),
                    },
                    _ => panic!("expected Directory for scripts"),
                }
            }
            other => panic!("expected Directory root, got {other:?}"),
        }
    }

    // ── Integration: build_nar_tree + write_nar ───────────────────────────────

    #[test]
    fn full_pipeline_produces_nar_starting_with_magic() {
        let files = vec![make_file("hello.txt", b"hello\n", "644")];
        let root = build_nar_tree(files).expect("build_nar_tree");

        let mut buf = Vec::new();
        write_nar(&mut buf, &root).expect("write_nar");

        assert!(buf.len() >= 56, "NAR output must be at least 56 bytes");
        assert!(
            buf.starts_with(b"\x0d\x00\x00\x00\x00\x00\x00\x00nix-archive-1"),
            "NAR must begin with the nix-archive-1 magic string"
        );
    }

    #[test]
    fn full_pipeline_output_is_deterministic() {
        let make_files = || {
            vec![
                make_file("a.txt", b"aaa", "644"),
                make_file("b/c.txt", b"bbb", "755"),
                make_symlink("d", "a.txt"),
            ]
        };
        let root1 = build_nar_tree(make_files()).expect("build_nar_tree 1");
        let root2 = build_nar_tree(make_files()).expect("build_nar_tree 2");

        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();
        write_nar(&mut buf1, &root1).expect("write_nar 1");
        write_nar(&mut buf2, &root2).expect("write_nar 2");

        assert_eq!(
            buf1, buf2,
            "NAR export must be deterministic for identical inputs"
        );
    }
}
