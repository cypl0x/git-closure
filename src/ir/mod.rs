/// Semantic IR layer: the `Closure` type and its node types.
///
/// A [`Closure`] is the central semantic unit of `git-closure`: a
/// content-addressed, self-contained file tree. All major operations
/// (artifact backends, recipe evaluation, projections) work in terms of
/// `Closure` rather than any serialization format.
///
/// The `.gcl` snapshot format (`src/gcl/`) and the NAR artifact backend
/// (`src/nar.rs`) are both *representations* of a `Closure`; neither is the IR.
///
/// # Concrete vs. unrealized closures
///
/// A `Closure` is **concrete** when all nodes are [`ClosureNode::File`] or
/// [`ClosureNode::Symlink`]: its [`ClosureId`] can be computed immediately.
///
/// Future phases will add [`ClosureNode::Reference`] nodes that name external
/// sources to be fetched; such a closure is *unrealized* and must be resolved
/// via provider dispatch before its identity can be determined.  Phase 1 of the
/// migration introduces only concrete nodes.
pub mod identity;

use crate::error::GitClosureError;
use crate::gcl::{SnapshotFile, SnapshotHeader};

// ── Core IR types ─────────────────────────────────────────────────────────────

/// The central semantic unit: a content-addressed, self-contained file tree.
///
/// ## Provenance
///
/// `provenance` carries ordered key-value metadata that does **not** contribute
/// to the [`ClosureId`] computation. Well-known keys:
/// - `"git-rev"` — SHA-1 / SHA-256 commit captured at build time
/// - `"git-branch"` — short branch name captured at build time
///
/// Any other keys are preserved as-is (forward-compatibility metadata).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closure {
    /// Ordered list of file-tree nodes.  For a concrete closure all entries are
    /// [`ClosureNode::File`] or [`ClosureNode::Symlink`] in lexicographic path
    /// order.
    pub nodes: Vec<ClosureNode>,
    /// Ordered provenance key-value pairs.  Not part of the identity hash.
    pub provenance: Vec<(String, String)>,
}

/// A single node in a [`Closure`]'s file tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosureNode {
    File(FileNode),
    Symlink(SymlinkNode),
    // ReferenceNode variant reserved for a future phase.
}

/// A regular file node in a [`Closure`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileNode {
    /// Slash-delimited path relative to the closure root.
    pub path: String,
    /// Hex-encoded SHA-256 digest of `content`.
    pub sha256: String,
    /// Octal permission bits as a string (e.g. `"644"`, `"755"`).
    pub mode: String,
    /// Byte size of `content`.
    pub size: u64,
    /// Raw file content bytes.  Encoding details (e.g. base64 for binary
    /// content in `.gcl`) are a backend concern and are not stored here.
    pub content: Vec<u8>,
}

/// A symbolic link node in a [`Closure`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymlinkNode {
    /// Slash-delimited path relative to the closure root.
    pub path: String,
    /// Symlink target (may be relative or absolute; safety is enforced at the
    /// projection layer, not in the IR).
    pub target: String,
}

/// Content-addressed identity of a concrete [`Closure`].
///
/// Computed by [`identity::compute_closure_id`] over the closure's nodes in
/// lexicographic path order using the same algorithm as the v0.1
/// `snapshot-hash` field in `.gcl` files: SHA-256 over length-prefixed fields
/// with `u64` big-endian lengths.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClosureId(pub String);

impl ClosureId {
    /// Returns the underlying hex string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ClosureId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── Conversions ───────────────────────────────────────────────────────────────

/// Convert a parsed `.gcl` snapshot into the semantic IR.
///
/// Provenance is reconstructed from the well-known header fields (`git-rev`,
/// `git-branch`) and the forward-compatibility `extra_headers` vector.
/// The `snapshot-hash` and `file-count` fields are **not** carried into the
/// `Closure` — they are derivable from the nodes and therefore redundant in the
/// IR representation.
impl From<(SnapshotHeader, Vec<SnapshotFile>)> for Closure {
    fn from((header, files): (SnapshotHeader, Vec<SnapshotFile>)) -> Self {
        let nodes = files.into_iter().map(ClosureNode::from).collect();
        let mut provenance = Vec::new();
        if let Some(rev) = header.git_rev {
            provenance.push(("git-rev".to_string(), rev));
        }
        if let Some(branch) = header.git_branch {
            provenance.push(("git-branch".to_string(), branch));
        }
        provenance.extend(header.extra_headers);
        Closure { nodes, provenance }
    }
}

impl From<SnapshotFile> for ClosureNode {
    fn from(f: SnapshotFile) -> Self {
        if let Some(target) = f.symlink_target {
            ClosureNode::Symlink(SymlinkNode {
                path: f.path,
                target,
            })
        } else {
            ClosureNode::File(FileNode {
                path: f.path,
                sha256: f.sha256,
                mode: f.mode,
                size: f.size,
                content: f.content,
            })
        }
    }
}

/// Convert the semantic IR back to a `.gcl`-compatible parsed representation.
///
/// The `snapshot-hash` is recomputed from the resulting `SnapshotFile` list
/// so the round-trip produces a valid, verifiable `.gcl` header.
///
/// Returns [`GitClosureError::Parse`] if the closure contains node types that
/// cannot be represented in the `.gcl` v0.1 format (e.g. future
/// `ReferenceNode`s).
impl TryFrom<Closure> for (SnapshotHeader, Vec<SnapshotFile>) {
    type Error = GitClosureError;

    fn try_from(closure: Closure) -> Result<Self, Self::Error> {
        let mut files = Vec::with_capacity(closure.nodes.len());
        for node in closure.nodes {
            match node {
                ClosureNode::File(f) => {
                    let encoding = if std::str::from_utf8(&f.content).is_ok() {
                        None
                    } else {
                        Some("base64".to_string())
                    };
                    files.push(SnapshotFile {
                        path: f.path,
                        sha256: f.sha256,
                        mode: f.mode,
                        size: f.size,
                        encoding,
                        symlink_target: None,
                        content: f.content,
                    });
                }
                ClosureNode::Symlink(s) => {
                    files.push(SnapshotFile {
                        path: s.path,
                        sha256: String::new(),
                        mode: "120000".to_string(),
                        size: 0,
                        encoding: None,
                        symlink_target: Some(s.target),
                        content: Vec::new(),
                    });
                }
            }
        }

        let mut git_rev = None;
        let mut git_branch = None;
        let mut extra_headers = Vec::new();
        for (key, value) in closure.provenance {
            match key.as_str() {
                "git-rev" => git_rev = Some(value),
                "git-branch" => git_branch = Some(value),
                _ => extra_headers.push((key, value)),
            }
        }

        let snapshot_hash = crate::gcl::hash::compute_snapshot_hash(&files);

        let header = SnapshotHeader {
            snapshot_hash,
            file_count: files.len(),
            git_rev,
            git_branch,
            extra_headers,
        };

        Ok((header, files))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcl::hash::compute_snapshot_hash;

    fn make_file_node(path: &str, content: &[u8], mode: &str) -> ClosureNode {
        let sha256 = crate::gcl::hash::sha256_hex(content);
        ClosureNode::File(FileNode {
            path: path.to_string(),
            sha256,
            mode: mode.to_string(),
            size: content.len() as u64,
            content: content.to_vec(),
        })
    }

    fn make_symlink_node(path: &str, target: &str) -> ClosureNode {
        ClosureNode::Symlink(SymlinkNode {
            path: path.to_string(),
            target: target.to_string(),
        })
    }

    fn make_snapshot_file(path: &str, content: &[u8], mode: &str) -> SnapshotFile {
        let sha256 = crate::gcl::hash::sha256_hex(content);
        SnapshotFile {
            path: path.to_string(),
            sha256,
            mode: mode.to_string(),
            size: content.len() as u64,
            encoding: if std::str::from_utf8(content).is_ok() {
                None
            } else {
                Some("base64".to_string())
            },
            symlink_target: None,
            content: content.to_vec(),
        }
    }

    // ── ClosureId ─────────────────────────────────────────────────────────────

    #[test]
    fn closure_id_display_is_the_hex_string() {
        let id = ClosureId("abc123".to_string());
        assert_eq!(id.to_string(), "abc123");
        assert_eq!(id.as_str(), "abc123");
    }

    // ── compute_closure_id matches compute_snapshot_hash ─────────────────────

    /// The ClosureId algorithm must produce the same value as the v0.1
    /// snapshot-hash algorithm for equivalent inputs.  This is the
    /// compatibility invariant between the IR and the .gcl format.
    #[test]
    fn closure_id_matches_snapshot_hash_for_regular_file() {
        let content = b"hello world";
        let sha256 = crate::gcl::hash::sha256_hex(content);

        let snap_file = SnapshotFile {
            path: "a.txt".to_string(),
            sha256: sha256.clone(),
            mode: "644".to_string(),
            size: content.len() as u64,
            encoding: None,
            symlink_target: None,
            content: content.to_vec(),
        };
        let closure_node = ClosureNode::File(FileNode {
            path: "a.txt".to_string(),
            sha256: sha256.clone(),
            mode: "644".to_string(),
            size: content.len() as u64,
            content: content.to_vec(),
        });

        let snap_hash = compute_snapshot_hash(&[snap_file]);
        let closure_id = identity::compute_closure_id(&[closure_node]);

        assert_eq!(
            snap_hash,
            closure_id.as_str(),
            "ClosureId must equal snapshot-hash for equivalent regular file input"
        );
    }

    #[test]
    fn closure_id_matches_snapshot_hash_for_symlink() {
        let snap_file = SnapshotFile {
            path: "link".to_string(),
            sha256: String::new(),
            mode: "120000".to_string(),
            size: 0,
            encoding: None,
            symlink_target: Some("target/path".to_string()),
            content: Vec::new(),
        };
        let closure_node = ClosureNode::Symlink(SymlinkNode {
            path: "link".to_string(),
            target: "target/path".to_string(),
        });

        let snap_hash = compute_snapshot_hash(&[snap_file]);
        let closure_id = identity::compute_closure_id(&[closure_node]);

        assert_eq!(
            snap_hash,
            closure_id.as_str(),
            "ClosureId must equal snapshot-hash for equivalent symlink input"
        );
    }

    #[test]
    fn closure_id_matches_snapshot_hash_for_mixed_tree() {
        let content_a = b"file a";
        let sha256_a = crate::gcl::hash::sha256_hex(content_a);
        let content_b = b"file b";
        let sha256_b = crate::gcl::hash::sha256_hex(content_b);

        let snap_files = vec![
            SnapshotFile {
                path: "a.txt".to_string(),
                sha256: sha256_a.clone(),
                mode: "644".to_string(),
                size: content_a.len() as u64,
                encoding: None,
                symlink_target: None,
                content: content_a.to_vec(),
            },
            SnapshotFile {
                path: "b/link".to_string(),
                sha256: String::new(),
                mode: "120000".to_string(),
                size: 0,
                encoding: None,
                symlink_target: Some("../a.txt".to_string()),
                content: Vec::new(),
            },
            SnapshotFile {
                path: "c.txt".to_string(),
                sha256: sha256_b.clone(),
                mode: "755".to_string(),
                size: content_b.len() as u64,
                encoding: None,
                symlink_target: None,
                content: content_b.to_vec(),
            },
        ];
        let closure_nodes = vec![
            ClosureNode::File(FileNode {
                path: "a.txt".to_string(),
                sha256: sha256_a,
                mode: "644".to_string(),
                size: content_a.len() as u64,
                content: content_a.to_vec(),
            }),
            ClosureNode::Symlink(SymlinkNode {
                path: "b/link".to_string(),
                target: "../a.txt".to_string(),
            }),
            ClosureNode::File(FileNode {
                path: "c.txt".to_string(),
                sha256: sha256_b,
                mode: "755".to_string(),
                size: content_b.len() as u64,
                content: content_b.to_vec(),
            }),
        ];

        let snap_hash = compute_snapshot_hash(&snap_files);
        let closure_id = identity::compute_closure_id(&closure_nodes);

        assert_eq!(
            snap_hash,
            closure_id.as_str(),
            "ClosureId must equal snapshot-hash for mixed tree"
        );
    }

    #[test]
    fn closure_id_empty_nodes_is_stable() {
        let id1 = identity::compute_closure_id(&[]);
        let id2 = identity::compute_closure_id(&[]);
        assert_eq!(id1, id2);
    }

    // ── From<(SnapshotHeader, Vec<SnapshotFile>)> for Closure ─────────────────

    #[test]
    fn closure_from_snapshot_parts_regular_file() {
        let content = b"hello";
        let node = make_snapshot_file("hello.txt", content, "644");
        let header = SnapshotHeader {
            snapshot_hash: compute_snapshot_hash(&[node.clone()]),
            file_count: 1,
            git_rev: None,
            git_branch: None,
            extra_headers: vec![],
        };

        let closure = Closure::from((header, vec![node]));
        assert_eq!(closure.nodes.len(), 1);
        assert!(matches!(&closure.nodes[0], ClosureNode::File(f) if f.path == "hello.txt"));
        assert!(closure.provenance.is_empty());
    }

    #[test]
    fn closure_from_snapshot_parts_symlink() {
        let sf = SnapshotFile {
            path: "link".to_string(),
            sha256: String::new(),
            mode: "120000".to_string(),
            size: 0,
            encoding: None,
            symlink_target: Some("target".to_string()),
            content: Vec::new(),
        };
        let header = SnapshotHeader {
            snapshot_hash: compute_snapshot_hash(&[sf.clone()]),
            file_count: 1,
            git_rev: None,
            git_branch: None,
            extra_headers: vec![],
        };

        let closure = Closure::from((header, vec![sf]));
        assert!(
            matches!(&closure.nodes[0], ClosureNode::Symlink(s) if s.path == "link" && s.target == "target")
        );
    }

    #[test]
    fn closure_from_snapshot_parts_preserves_provenance() {
        let header = SnapshotHeader {
            snapshot_hash: String::new(),
            file_count: 0,
            git_rev: Some("abc123".to_string()),
            git_branch: Some("main".to_string()),
            extra_headers: vec![("source-uri".to_string(), "gh:owner/repo".to_string())],
        };

        let closure = Closure::from((header, vec![]));
        assert_eq!(
            closure.provenance,
            vec![
                ("git-rev".to_string(), "abc123".to_string()),
                ("git-branch".to_string(), "main".to_string()),
                ("source-uri".to_string(), "gh:owner/repo".to_string()),
            ]
        );
    }

    #[test]
    fn closure_from_snapshot_parts_skips_none_provenance_fields() {
        let header = SnapshotHeader {
            snapshot_hash: String::new(),
            file_count: 0,
            git_rev: None,
            git_branch: None,
            extra_headers: vec![],
        };

        let closure = Closure::from((header, vec![]));
        assert!(closure.provenance.is_empty());
    }

    // ── TryFrom<Closure> for (SnapshotHeader, Vec<SnapshotFile>) ──────────────

    #[test]
    fn try_from_closure_round_trips_file_node() {
        let content = b"round trip content";
        let sha256 = crate::gcl::hash::sha256_hex(content);
        let node = make_file_node("rt.txt", content, "644");
        let closure = Closure {
            nodes: vec![node],
            provenance: vec![],
        };

        let (header, files) = <(SnapshotHeader, Vec<SnapshotFile>)>::try_from(closure).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "rt.txt");
        assert_eq!(files[0].sha256, sha256);
        assert_eq!(files[0].mode, "644");
        assert_eq!(files[0].content, content);
        assert_eq!(files[0].symlink_target, None);
        assert_eq!(header.file_count, 1);
        // snapshot_hash must match what compute_snapshot_hash would give
        assert_eq!(header.snapshot_hash, compute_snapshot_hash(&files));
    }

    #[test]
    fn try_from_closure_round_trips_symlink_node() {
        let node = make_symlink_node("link", "target/path");
        let closure = Closure {
            nodes: vec![node],
            provenance: vec![],
        };

        let (_header, files) = <(SnapshotHeader, Vec<SnapshotFile>)>::try_from(closure).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "link");
        assert_eq!(files[0].symlink_target, Some("target/path".to_string()));
        assert_eq!(files[0].mode, "120000");
        assert_eq!(files[0].sha256, "");
        assert_eq!(files[0].size, 0);
    }

    #[test]
    fn try_from_closure_restores_provenance_into_header_fields() {
        let closure = Closure {
            nodes: vec![],
            provenance: vec![
                ("git-rev".to_string(), "deadbeef".to_string()),
                ("git-branch".to_string(), "feat/x".to_string()),
                ("source-uri".to_string(), "gh:foo/bar".to_string()),
            ],
        };

        let (header, _files) = <(SnapshotHeader, Vec<SnapshotFile>)>::try_from(closure).unwrap();

        assert_eq!(header.git_rev, Some("deadbeef".to_string()));
        assert_eq!(header.git_branch, Some("feat/x".to_string()));
        assert_eq!(
            header.extra_headers,
            vec![("source-uri".to_string(), "gh:foo/bar".to_string())]
        );
    }

    #[test]
    fn try_from_closure_binary_content_gets_base64_encoding() {
        let content = vec![0u8, 1, 2, 255, 254]; // non-UTF-8
        let sha256 = crate::gcl::hash::sha256_hex(&content);
        let node = ClosureNode::File(FileNode {
            path: "bin".to_string(),
            sha256,
            mode: "644".to_string(),
            size: content.len() as u64,
            content: content.clone(),
        });
        let closure = Closure {
            nodes: vec![node],
            provenance: vec![],
        };

        let (_header, files) = <(SnapshotHeader, Vec<SnapshotFile>)>::try_from(closure).unwrap();

        assert_eq!(files[0].encoding, Some("base64".to_string()));
    }

    #[test]
    fn try_from_closure_utf8_content_has_no_encoding() {
        let content = b"plain text";
        let node = make_file_node("plain.txt", content, "644");
        let closure = Closure {
            nodes: vec![node],
            provenance: vec![],
        };

        let (_header, files) = <(SnapshotHeader, Vec<SnapshotFile>)>::try_from(closure).unwrap();

        assert_eq!(files[0].encoding, None);
    }

    // ── Full round-trip: snapshot → Closure → snapshot ───────────────────────

    #[test]
    fn full_round_trip_snapshot_to_closure_and_back() {
        let content_a = b"content of a";
        let sha256_a = crate::gcl::hash::sha256_hex(content_a);
        let content_b = b"content of b \xff\xfe"; // binary

        let files_original = vec![
            SnapshotFile {
                path: "a.txt".to_string(),
                sha256: sha256_a.clone(),
                mode: "644".to_string(),
                size: content_a.len() as u64,
                encoding: None,
                symlink_target: None,
                content: content_a.to_vec(),
            },
            make_snapshot_file("b.bin", content_b, "600"),
            SnapshotFile {
                path: "link".to_string(),
                sha256: String::new(),
                mode: "120000".to_string(),
                size: 0,
                encoding: None,
                symlink_target: Some("a.txt".to_string()),
                content: Vec::new(),
            },
        ];

        let header_original = SnapshotHeader {
            snapshot_hash: compute_snapshot_hash(&files_original),
            file_count: files_original.len(),
            git_rev: Some("cafebabe".to_string()),
            git_branch: Some("main".to_string()),
            extra_headers: vec![("source-uri".to_string(), "local".to_string())],
        };

        // Forward: snapshot → Closure
        let closure = Closure::from((header_original.clone(), files_original.clone()));

        // Backward: Closure → snapshot
        let (header_rt, files_rt) =
            <(SnapshotHeader, Vec<SnapshotFile>)>::try_from(closure).unwrap();

        // Header fields must be equal (snapshot_hash is recomputed, so compare separately)
        assert_eq!(header_rt.git_rev, header_original.git_rev);
        assert_eq!(header_rt.git_branch, header_original.git_branch);
        assert_eq!(header_rt.extra_headers, header_original.extra_headers);
        assert_eq!(header_rt.file_count, header_original.file_count);
        assert_eq!(header_rt.snapshot_hash, header_original.snapshot_hash);

        // Files must round-trip exactly
        assert_eq!(files_rt.len(), files_original.len());
        for (orig, rt) in files_original.iter().zip(files_rt.iter()) {
            assert_eq!(orig.path, rt.path);
            assert_eq!(orig.sha256, rt.sha256);
            assert_eq!(orig.mode, rt.mode);
            assert_eq!(orig.size, rt.size);
            assert_eq!(orig.content, rt.content);
            assert_eq!(orig.symlink_target, rt.symlink_target);
        }
    }
}
