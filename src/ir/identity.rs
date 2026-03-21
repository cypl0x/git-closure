/// Closure identity computation.
///
/// [`compute_closure_id`] produces the [`super::ClosureId`] for a concrete
/// [`super::Closure`].  The algorithm is identical to the v0.1 `.gcl`
/// `snapshot-hash` field defined in `SPEC.md §6` and implemented in
/// `src/gcl/hash.rs`: SHA-256 over length-prefixed fields using `u64`
/// big-endian lengths.
///
/// This intentional alignment means that:
/// - converting a `.gcl` snapshot to a `Closure` and computing its `ClosureId`
///   yields the same value as the header's `snapshot-hash` field, and
/// - the golden fixture tests that lock `snapshot-hash` transitively lock the
///   `ClosureId` algorithm.
use sha2::{Digest, Sha256};

use super::{ClosureId, ClosureNode};

/// Computes the canonical [`ClosureId`] for a slice of concrete closure nodes.
///
/// Nodes must already be in **lexicographic path order**.  The computation
/// covers only the structural metadata (path, mode, SHA-256 for files; path,
/// target for symlinks) — not the raw byte payload, which is addressed
/// separately by the per-file `sha256` field.
///
/// # Hash construction
///
/// ```text
/// For each node in lexicographic path order:
///   [entry_type_len: u64 be] [entry_type: UTF-8]   ("regular" or "symlink")
///   [path_len: u64 be]       [path: UTF-8]
///
/// For "regular":
///   [mode_len: u64 be]    [mode: UTF-8]
///   [sha256_len: u64 be]  [sha256_hex: UTF-8]
///
/// For "symlink":
///   [target_len: u64 be]  [target: UTF-8]
/// ```
pub fn compute_closure_id(nodes: &[ClosureNode]) -> ClosureId {
    let mut hasher = Sha256::new();
    for node in nodes {
        match node {
            ClosureNode::Symlink(s) => {
                hash_length_prefixed(&mut hasher, b"symlink");
                hash_length_prefixed(&mut hasher, s.path.as_bytes());
                hash_length_prefixed(&mut hasher, s.target.as_bytes());
            }
            ClosureNode::File(f) => {
                hash_length_prefixed(&mut hasher, b"regular");
                hash_length_prefixed(&mut hasher, f.path.as_bytes());
                hash_length_prefixed(&mut hasher, f.mode.as_bytes());
                hash_length_prefixed(&mut hasher, f.sha256.as_bytes());
            }
        }
    }
    ClosureId(format!("{:x}", hasher.finalize()))
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{FileNode, SymlinkNode};

    #[test]
    fn empty_node_list_is_deterministic() {
        let id1 = compute_closure_id(&[]);
        let id2 = compute_closure_id(&[]);
        assert_eq!(id1, id2);
    }

    #[test]
    fn order_sensitivity() {
        let a = ClosureNode::File(FileNode {
            path: "a.txt".to_string(),
            sha256: "aaa".to_string(),
            mode: "644".to_string(),
            size: 0,
            content: vec![],
        });
        let b = ClosureNode::File(FileNode {
            path: "b.txt".to_string(),
            sha256: "bbb".to_string(),
            mode: "644".to_string(),
            size: 0,
            content: vec![],
        });
        let id_ab = compute_closure_id(&[a.clone(), b.clone()]);
        let id_ba = compute_closure_id(&[b, a]);
        assert_ne!(id_ab, id_ba, "ClosureId must be order-sensitive");
    }

    #[test]
    fn symlink_distinct_from_regular_same_path() {
        let regular = ClosureNode::File(FileNode {
            path: "x".to_string(),
            sha256: "abc".to_string(),
            mode: "644".to_string(),
            size: 0,
            content: vec![],
        });
        let symlink = ClosureNode::Symlink(SymlinkNode {
            path: "x".to_string(),
            target: "abc".to_string(),
        });
        assert_ne!(
            compute_closure_id(&[regular]),
            compute_closure_id(&[symlink]),
            "symlink and regular file with same path must produce different ClosureIds"
        );
    }

    #[test]
    fn payload_bytes_not_in_identity() {
        // The identity covers the sha256 field, not the raw content bytes.
        let a = ClosureNode::File(FileNode {
            path: "f.txt".to_string(),
            sha256: "same_sha".to_string(),
            mode: "644".to_string(),
            size: 3,
            content: b"one".to_vec(),
        });
        let b = ClosureNode::File(FileNode {
            path: "f.txt".to_string(),
            sha256: "same_sha".to_string(),
            mode: "644".to_string(),
            size: 3,
            content: b"two".to_vec(),
        });
        assert_eq!(
            compute_closure_id(&[a]),
            compute_closure_id(&[b]),
            "ClosureId must not depend on raw content when sha256 is identical"
        );
    }
}
