//! NAR artifact backend for git-closure.
//!
//! [`NarBackend`] implements [`super::ArtifactBackend`] and is the first
//! concrete artifact backend in the new `Closure → backend` pipeline.
//!
//! # Architecture note
//!
//! The low-level NAR wire-format writer lives in [`crate::nar`] and is
//! unchanged.  This module is the adapter layer: it converts a [`Closure`]
//! into a [`crate::nar::NarNode`] tree and then delegates to
//! [`crate::nar::write_nar`] for serialization.  The physical consolidation
//! of the low-level codec into this module is deferred to a later cleanup
//! phase.
//!
//! # Metadata loss
//!
//! NAR is a pure filesystem tree archive.  The following [`Closure`]
//! provenance fields have no NAR equivalent and are silently dropped:
//! - `provenance` key-value pairs
//! - Per-file `sha256` and `size` — recomputable from content
//! - Full Unix mode string — only the executable/non-executable bit is
//!   preserved (matching Nix semantics)

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufWriter, Write as _};
use std::path::Path;

use crate::error::GitClosureError;
use crate::ir::{Closure, ClosureNode};
use crate::nar::{write_nar, NarNode};

use super::{ArtifactBackend, Result};

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

/// NAR artifact backend.
///
/// A zero-size struct — all state lives in the [`Closure`] passed to
/// [`ArtifactBackend::write`].
pub struct NarBackend;

impl ArtifactBackend for NarBackend {
    fn name(&self) -> &'static str {
        "nar"
    }

    fn extension(&self) -> &'static str {
        "nar"
    }

    fn write(&self, closure: &Closure, output: &Path) -> Result<()> {
        let tree = closure_to_nar_tree(closure)?;

        let output_file = fs::File::create(output).map_err(|e| {
            GitClosureError::Io(io::Error::new(
                e.kind(),
                format!("{}: {e}", output.display()),
            ))
        })?;
        let mut writer = BufWriter::new(output_file);

        write_nar(&mut writer, &tree).map_err(|e| {
            GitClosureError::Io(io::Error::new(
                e.kind(),
                format!("writing NAR to {}: {e}", output.display()),
            ))
        })?;

        writer.flush().map_err(|e| {
            GitClosureError::Io(io::Error::new(
                e.kind(),
                format!("flushing NAR output to {}: {e}", output.display()),
            ))
        })?;

        Ok(())
    }
}

/// Convert a [`Closure`] into a [`NarNode`] tree suitable for [`write_nar`].
///
/// Nodes must already be in **lexicographic path order** — guaranteed by
/// [`crate::ir::Closure`]'s invariant and the [`From`] conversion from
/// `(SnapshotHeader, Vec<SnapshotFile>)`.
///
/// # Errors
///
/// Returns [`GitClosureError::Parse`] if a path component is used as both a
/// file leaf and a directory prefix (indicating a malformed closure).
fn closure_to_nar_tree(closure: &Closure) -> Result<NarNode> {
    let mut root: BTreeMap<String, NarNode> = BTreeMap::new();
    for node in &closure.nodes {
        let (path, leaf) = match node {
            ClosureNode::File(f) => {
                let leaf = NarNode::File {
                    executable: is_executable(&f.mode),
                    content: f.content.clone(),
                };
                (f.path.clone(), leaf)
            }
            ClosureNode::Symlink(s) => {
                let leaf = NarNode::Symlink {
                    target: s.target.clone(),
                };
                (s.path.clone(), leaf)
            }
        };
        let components: Vec<&str> = path.split('/').collect();
        insert_into_nar_tree(&mut root, &components, leaf, &path)?;
    }
    Ok(NarNode::Directory(root))
}

/// Recursively insert a [`NarNode`] leaf into `tree` following `components`.
fn insert_into_nar_tree(
    tree: &mut BTreeMap<String, NarNode>,
    components: &[&str],
    leaf: NarNode,
    full_path: &str,
) -> Result<()> {
    debug_assert!(
        !components.is_empty(),
        "insert_into_nar_tree called with empty path slice"
    );

    let first = components[0];
    let rest = &components[1..];

    if rest.is_empty() {
        if tree.contains_key(first) {
            return Err(GitClosureError::Parse(format!(
                "NAR tree conflict: '{}' is already a directory but is used as a file name",
                full_path
            )));
        }
        tree.insert(first.to_string(), leaf);
    } else {
        let entry = tree
            .entry(first.to_string())
            .or_insert_with(|| NarNode::Directory(BTreeMap::new()));
        match entry {
            NarNode::Directory(ref mut inner) => {
                insert_into_nar_tree(inner, rest, leaf, full_path)?;
            }
            _ => {
                return Err(GitClosureError::Parse(format!(
                    "NAR tree conflict: path component '{}' in '{}' was previously inserted as a file",
                    first, full_path
                )));
            }
        }
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Closure, FileNode, SymlinkNode};
    use crate::nar::TOK_NAR;

    fn one_file_closure(path: &str, content: &[u8], mode: &str) -> Closure {
        Closure {
            nodes: vec![ClosureNode::File(FileNode {
                path: path.to_string(),
                sha256: "a".repeat(64),
                mode: mode.to_string(),
                size: content.len() as u64,
                content: content.to_vec(),
            })],
            provenance: vec![],
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

    // ── NarBackend::write ──────────────────────────────────────────────────────

    #[test]
    fn nar_backend_write_produces_valid_nar_magic() {
        let closure = one_file_closure("hello.txt", b"hello\n", "644");
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        NarBackend
            .write(&closure, tmp.path())
            .expect("NarBackend::write must succeed");
        let bytes = std::fs::read(tmp.path()).expect("read output");
        assert!(
            bytes.starts_with(&TOK_NAR),
            "NarBackend output must start with NAR magic (TOK_NAR)"
        );
    }

    #[test]
    fn nar_backend_write_matches_current_export_path() {
        // Verify byte-for-byte compatibility between the new NarBackend path
        // and the legacy build_nar_tree + write_nar path.
        use crate::nar::write_nar;
        use crate::snapshot::export::build_nar_tree;
        use crate::snapshot::{SnapshotFile, SnapshotHeader};

        let files = vec![
            SnapshotFile {
                path: "alpha.txt".to_string(),
                sha256: "a".repeat(64),
                mode: "644".to_string(),
                size: 6,
                encoding: None,
                symlink_target: None,
                content: b"alpha\n".to_vec(),
            },
            SnapshotFile {
                path: "link".to_string(),
                sha256: String::new(),
                mode: "120000".to_string(),
                size: 0,
                encoding: None,
                symlink_target: Some("alpha.txt".to_string()),
                content: vec![],
            },
        ];

        // Legacy path.
        let legacy_tree = build_nar_tree(files.clone()).expect("build_nar_tree");
        let mut legacy_bytes: Vec<u8> = Vec::new();
        write_nar(&mut legacy_bytes, &legacy_tree).expect("write_nar");

        // New path: (SnapshotHeader, Vec<SnapshotFile>) → Closure → NarBackend.
        let closure = Closure::from((SnapshotHeader::default(), files));
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        NarBackend
            .write(&closure, tmp.path())
            .expect("NarBackend::write");
        let new_bytes = std::fs::read(tmp.path()).expect("read output");

        assert_eq!(
            legacy_bytes, new_bytes,
            "NarBackend must produce byte-identical output to legacy build_nar_tree path"
        );
    }

    // ── closure_to_nar_tree ────────────────────────────────────────────────────

    #[test]
    fn closure_to_nar_tree_file_executable_flag() {
        let closure = one_file_closure("run.sh", b"#!/bin/sh\n", "755");
        let tree = closure_to_nar_tree(&closure).expect("closure_to_nar_tree");
        match tree {
            NarNode::Directory(ref entries) => {
                match entries.get("run.sh").expect("run.sh must be in tree") {
                    NarNode::File { executable, .. } => {
                        assert!(*executable, "mode 755 must produce executable=true");
                    }
                    other => panic!("expected File node, got {other:?}"),
                }
            }
            other => panic!("expected Directory root, got {other:?}"),
        }
    }

    #[test]
    fn closure_to_nar_tree_symlink() {
        let closure = Closure {
            nodes: vec![ClosureNode::Symlink(SymlinkNode {
                path: "link".to_string(),
                target: "target.txt".to_string(),
            })],
            provenance: vec![],
        };
        let tree = closure_to_nar_tree(&closure).expect("closure_to_nar_tree");
        match tree {
            NarNode::Directory(ref entries) => {
                match entries.get("link").expect("link must be in tree") {
                    NarNode::Symlink { target } => {
                        assert_eq!(target, "target.txt", "symlink target must be preserved");
                    }
                    other => panic!("expected Symlink node, got {other:?}"),
                }
            }
            other => panic!("expected Directory root, got {other:?}"),
        }
    }

    #[test]
    fn closure_to_nar_tree_non_executable_file() {
        let closure = one_file_closure("data.txt", b"data", "644");
        let tree = closure_to_nar_tree(&closure).expect("closure_to_nar_tree");
        match tree {
            NarNode::Directory(ref entries) => {
                match entries.get("data.txt").expect("data.txt must be in tree") {
                    NarNode::File { executable, .. } => {
                        assert!(!executable, "mode 644 must produce executable=false");
                    }
                    other => panic!("expected File node, got {other:?}"),
                }
            }
            other => panic!("expected Directory root, got {other:?}"),
        }
    }

    #[test]
    fn closure_to_nar_tree_nested_path() {
        let closure = Closure {
            nodes: vec![ClosureNode::File(FileNode {
                path: "a/b/c.txt".to_string(),
                sha256: "a".repeat(64),
                mode: "644".to_string(),
                size: 1,
                content: b"c".to_vec(),
            })],
            provenance: vec![],
        };
        let tree = closure_to_nar_tree(&closure).expect("closure_to_nar_tree");
        match tree {
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
}
