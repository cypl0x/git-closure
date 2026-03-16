/// Snapshot-hash and per-file SHA-256 utilities.
///
/// The snapshot-hash is a *structural* hash: it covers the ordered set of
/// (path, mode, sha256) tuples for regular files and (path, target) for
/// symlinks.  It does **not** cover the raw byte payload — that is addressed
/// separately by the per-file `:sha256` field.
///
/// Hash construction (v0.1 format):
///
/// ```text
/// For each file in lexicographic path order:
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
use sha2::{Digest, Sha256};

use super::SnapshotFile;

/// Computes the canonical snapshot-hash over all file metadata.
/// Files must already be in lexicographic path order.
pub(crate) fn compute_snapshot_hash(files: &[SnapshotFile]) -> String {
    let mut hasher = Sha256::new();
    for file in files {
        if let Some(target) = &file.symlink_target {
            hash_length_prefixed(&mut hasher, b"symlink");
            hash_length_prefixed(&mut hasher, file.path.as_bytes());
            hash_length_prefixed(&mut hasher, target.as_bytes());
        } else {
            hash_length_prefixed(&mut hasher, b"regular");
            hash_length_prefixed(&mut hasher, file.path.as_bytes());
            hash_length_prefixed(&mut hasher, file.mode.as_bytes());
            hash_length_prefixed(&mut hasher, file.sha256.as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

/// Computes a hex-encoded SHA-256 digest of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_empty_is_known_constant() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn compute_snapshot_hash_empty_file_list_is_stable() {
        // The hash of an empty file list must be deterministic across runs.
        let h1 = compute_snapshot_hash(&[]);
        let h2 = compute_snapshot_hash(&[]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn compute_snapshot_hash_order_matters() {
        let a = SnapshotFile {
            path: "a.txt".into(),
            sha256: "aaa".into(),
            mode: "644".into(),
            size: 0,
            encoding: None,
            symlink_target: None,
            content: vec![],
        };
        let b = SnapshotFile {
            path: "b.txt".into(),
            sha256: "bbb".into(),
            mode: "644".into(),
            size: 0,
            encoding: None,
            symlink_target: None,
            content: vec![],
        };
        let h_ab = compute_snapshot_hash(&[a.clone(), b.clone()]);
        let h_ba = compute_snapshot_hash(&[b, a]);
        assert_ne!(h_ab, h_ba, "hash must be order-sensitive");
    }

    #[test]
    fn compute_snapshot_hash_symlink_distinct_from_regular() {
        let regular = SnapshotFile {
            path: "link".into(),
            sha256: "abc".into(),
            mode: "644".into(),
            size: 0,
            encoding: None,
            symlink_target: None,
            content: vec![],
        };
        let symlink = SnapshotFile {
            path: "link".into(),
            sha256: String::new(),
            mode: "120000".into(),
            size: 0,
            encoding: None,
            symlink_target: Some("target".into()),
            content: vec![],
        };
        assert_ne!(
            compute_snapshot_hash(&[regular]),
            compute_snapshot_hash(&[symlink]),
            "symlink and regular file with same path must produce different hashes"
        );
    }
}
