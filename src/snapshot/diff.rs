/// Snapshot diffing: compare two `.gcl` files and report changes.
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::utils::io_error_with_path;

use super::build::collect_files;
use super::serial::parse_snapshot;
use super::{BuildOptions, Result, SnapshotFile};

// ── Public types ──────────────────────────────────────────────────────────────

/// A single change entry produced by [`diff_snapshots`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DiffEntry {
    /// File exists in the right snapshot but not the left.
    Added { path: String },
    /// File exists in the left snapshot but not the right.
    Removed { path: String },
    /// File exists in both snapshots but its content identity changed.
    Modified {
        path: String,
        old_sha256: String,
        new_sha256: String,
    },
    /// File was removed from `old_path` and appeared at `new_path` with the
    /// same SHA-256.  This is a heuristic: a true rename has identical content.
    Renamed { old_path: String, new_path: String },
    /// File content is unchanged but Unix mode changed.
    ModeChanged {
        path: String,
        old_mode: String,
        new_mode: String,
    },
    /// Symlink exists in both snapshots but points to a different target.
    SymlinkTargetChanged {
        path: String,
        old_target: String,
        new_target: String,
    },
}

/// Result of comparing two snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffResult {
    /// Ordered list of changes.  Entries are sorted: renames first (by new
    /// path), then removed, added, modified — each group in path order.
    pub entries: Vec<DiffEntry>,
    /// Convenience flag: `true` when `entries` is empty.
    pub identical: bool,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Compares two `.gcl` snapshot files and returns the set of differences.
///
/// The returned [`DiffResult`] is in a deterministic, sorted order suitable
/// for human display and golden-file tests.
///
/// # Rename detection
///
/// A file is reported as `Renamed` when a path disappears from the left
/// snapshot and a path with the **same SHA-256** appears in the right
/// snapshot.  When there are multiple candidates (duplicate content), the
/// lexicographically smallest new path is chosen.  This is O(n log n) via a
/// reverse-index over the right snapshot's sha256 values.
///
/// Symlinks are compared by target string, not sha256 (which is empty for
/// symlinks).  Two symlinks with the same target pointing to the same path
/// are considered identical; different targets are `SymlinkTargetChanged`.
pub fn diff_snapshots(left: &Path, right: &Path) -> Result<DiffResult> {
    let left_text = fs::read_to_string(left).map_err(|err| io_error_with_path(err, left))?;
    let right_text = fs::read_to_string(right).map_err(|err| io_error_with_path(err, right))?;

    let (_, left_files) = parse_snapshot(&left_text)?;
    let (_, right_files) = parse_snapshot(&right_text)?;

    Ok(compute_diff(&left_files, &right_files))
}

/// Compares a snapshot file against a live source directory.
///
/// This parses the left `.gcl` snapshot and collects the right-hand entries
/// directly from `source` using the same build-time file selection rules.
pub fn diff_snapshot_to_source(
    snapshot: &Path,
    source: &Path,
    options: &BuildOptions,
) -> Result<DiffResult> {
    let snapshot_text =
        fs::read_to_string(snapshot).map_err(|err| io_error_with_path(err, snapshot))?;
    let (_header, left_files) = parse_snapshot(&snapshot_text)?;

    let source = fs::canonicalize(source).map_err(|err| io_error_with_path(err, source))?;
    if !source.is_dir() {
        return Err(crate::error::GitClosureError::Parse(format!(
            "source is not a directory: {}",
            source.display()
        )));
    }

    let mut right_files = collect_files(&source, options)?;
    right_files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(compute_diff(&left_files, &right_files))
}

// ── Core algorithm ────────────────────────────────────────────────────────────

fn compute_diff(left: &[SnapshotFile], right: &[SnapshotFile]) -> DiffResult {
    // Key for content-equality: sha256 for regular files, target for symlinks.
    fn content_key(f: &SnapshotFile) -> String {
        if let Some(target) = &f.symlink_target {
            format!("symlink:{target}")
        } else {
            f.sha256.clone()
        }
    }

    let left_map: HashMap<&str, &SnapshotFile> =
        left.iter().map(|f| (f.path.as_str(), f)).collect();
    let right_map: HashMap<&str, &SnapshotFile> =
        right.iter().map(|f| (f.path.as_str(), f)).collect();

    // Build a reverse index: content_key → sorted list of new paths (right-only).
    // We build this incrementally after we know which right paths are "added".
    let mut candidates_removed: Vec<&SnapshotFile> = Vec::new();
    let mut candidates_added: Vec<&SnapshotFile> = Vec::new();
    let mut mode_changed: Vec<DiffEntry> = Vec::new();
    let mut modified: Vec<DiffEntry> = Vec::new();
    let mut forced_added_paths: HashSet<&str> = HashSet::new();

    for lf in left {
        match right_map.get(lf.path.as_str()) {
            None => candidates_removed.push(lf),
            Some(&rf) => {
                let left_is_symlink = lf.symlink_target.is_some();
                let right_is_symlink = rf.symlink_target.is_some();

                // Explicit design decision: regular<->symlink transitions are
                // represented as Removed + Added (type replacement), not
                // Modified/ModeChanged.
                if left_is_symlink != right_is_symlink {
                    candidates_removed.push(lf);
                    forced_added_paths.insert(rf.path.as_str());
                    continue;
                }

                if content_key(lf) != content_key(rf) {
                    if let (Some(old_target), Some(new_target)) =
                        (&lf.symlink_target, &rf.symlink_target)
                    {
                        modified.push(DiffEntry::SymlinkTargetChanged {
                            path: lf.path.clone(),
                            old_target: old_target.clone(),
                            new_target: new_target.clone(),
                        });
                    } else {
                        modified.push(DiffEntry::Modified {
                            path: lf.path.clone(),
                            old_sha256: lf.sha256.clone(),
                            new_sha256: rf.sha256.clone(),
                        });
                    }
                } else if lf.mode != rf.mode {
                    mode_changed.push(DiffEntry::ModeChanged {
                        path: lf.path.clone(),
                        old_mode: lf.mode.clone(),
                        new_mode: rf.mode.clone(),
                    });
                }
                // identical — skip
            }
        }
    }

    for rf in right {
        if !left_map.contains_key(rf.path.as_str()) || forced_added_paths.contains(rf.path.as_str())
        {
            candidates_added.push(rf);
        }
    }

    // Build reverse index for rename detection.
    let mut added_by_key: HashMap<String, Vec<&str>> = HashMap::new();
    for rf in &candidates_added {
        added_by_key
            .entry(content_key(rf))
            .or_default()
            .push(&rf.path);
    }
    // Sort each bucket so we pick the lexicographically smallest new path.
    for v in added_by_key.values_mut() {
        v.sort_unstable();
    }

    let mut renames: Vec<DiffEntry> = Vec::new();
    let mut renamed_old_paths: std::collections::HashSet<String> = Default::default();
    let mut renamed_new_paths: std::collections::HashSet<String> = Default::default();

    // Match removals to additions by content key.  Each addition can only be
    // consumed once, so we track which new paths have already been claimed.
    let mut consumed: std::collections::HashSet<String> = Default::default();

    // Sort candidates_removed by path for determinism.
    let mut candidates_removed_sorted = candidates_removed.to_vec();
    candidates_removed_sorted.sort_by(|a, b| a.path.cmp(&b.path));

    for lf in &candidates_removed_sorted {
        let key = content_key(lf);
        if let Some(new_paths) = added_by_key.get(&key) {
            if let Some(&new_path) = new_paths.iter().find(|&&p| !consumed.contains(p)) {
                consumed.insert(new_path.to_string());
                renamed_old_paths.insert(lf.path.clone());
                renamed_new_paths.insert(new_path.to_string());
                renames.push(DiffEntry::Renamed {
                    old_path: lf.path.clone(),
                    new_path: new_path.to_string(),
                });
            }
        }
    }

    renames.sort_by(|a, b| {
        let ap = if let DiffEntry::Renamed { new_path, .. } = a {
            new_path
        } else {
            unreachable!()
        };
        let bp = if let DiffEntry::Renamed { new_path, .. } = b {
            new_path
        } else {
            unreachable!()
        };
        ap.cmp(bp)
    });

    let mut removed: Vec<DiffEntry> = candidates_removed_sorted
        .iter()
        .filter(|f| !renamed_old_paths.contains(&f.path))
        .map(|f| DiffEntry::Removed {
            path: f.path.clone(),
        })
        .collect();
    removed.sort_by(|a, b| {
        let ap = if let DiffEntry::Removed { path } = a {
            path
        } else {
            unreachable!()
        };
        let bp = if let DiffEntry::Removed { path } = b {
            path
        } else {
            unreachable!()
        };
        ap.cmp(bp)
    });

    let mut added: Vec<DiffEntry> = candidates_added
        .iter()
        .filter(|f| !renamed_new_paths.contains(&f.path))
        .map(|f| DiffEntry::Added {
            path: f.path.clone(),
        })
        .collect();
    added.sort_by(|a, b| {
        let ap = if let DiffEntry::Added { path } = a {
            path
        } else {
            unreachable!()
        };
        let bp = if let DiffEntry::Added { path } = b {
            path
        } else {
            unreachable!()
        };
        ap.cmp(bp)
    });

    modified.sort_by(|a, b| {
        let ap = match a {
            DiffEntry::Modified { path, .. } => path,
            DiffEntry::SymlinkTargetChanged { path, .. } => path,
            _ => unreachable!(),
        };
        let bp = match b {
            DiffEntry::Modified { path, .. } => path,
            DiffEntry::SymlinkTargetChanged { path, .. } => path,
            _ => unreachable!(),
        };
        ap.cmp(bp)
    });
    mode_changed.sort_by(|a, b| {
        let ap = if let DiffEntry::ModeChanged { path, .. } = a {
            path
        } else {
            unreachable!()
        };
        let bp = if let DiffEntry::ModeChanged { path, .. } = b {
            path
        } else {
            unreachable!()
        };
        ap.cmp(bp)
    });
    let mut entries = Vec::new();
    entries.extend(renames);
    entries.extend(removed);
    entries.extend(added);
    entries.extend(mode_changed);
    entries.extend(modified);

    let identical = entries.is_empty();
    DiffResult { entries, identical }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::hash::{compute_snapshot_hash, sha256_hex};
    use crate::snapshot::serial::serialize_snapshot;
    use crate::snapshot::SnapshotFile;
    use std::fs;
    use tempfile::TempDir;

    fn text_file(path: &str, content: &str) -> SnapshotFile {
        text_file_mode(path, content, "644")
    }

    fn text_file_mode(path: &str, content: &str, mode: &str) -> SnapshotFile {
        let bytes = content.as_bytes().to_vec();
        SnapshotFile {
            path: path.to_string(),
            sha256: sha256_hex(&bytes),
            mode: mode.to_string(),
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

    fn write_snap(dir: &TempDir, name: &str, files: &[SnapshotFile]) -> std::path::PathBuf {
        use crate::snapshot::SnapshotHeader;
        let mut sorted = files.to_vec();
        sorted.sort_by(|a, b| a.path.cmp(&b.path));
        let snapshot_hash = compute_snapshot_hash(&sorted);
        let header = SnapshotHeader {
            snapshot_hash,
            file_count: sorted.len(),
            git_rev: None,
            git_branch: None,
            extra_headers: Vec::new(),
        };
        let text = serialize_snapshot(&sorted, &header);
        let path = dir.path().join(name);
        fs::write(&path, text.as_bytes()).unwrap();
        path
    }

    #[test]
    fn diff_identical_snapshots_is_empty() {
        let dir = TempDir::new().unwrap();
        let files = vec![text_file("a.txt", "hello"), text_file("b.txt", "world")];
        let left = write_snap(&dir, "left.gcl", &files);
        let right = write_snap(&dir, "right.gcl", &files);
        let result = diff_snapshots(&left, &right).unwrap();
        assert!(result.identical);
        assert!(result.entries.is_empty());
    }

    #[test]
    fn diff_detects_added_file() {
        let dir = TempDir::new().unwrap();
        let left_files = vec![text_file("a.txt", "a")];
        let right_files = vec![text_file("a.txt", "a"), text_file("b.txt", "b")];
        let left = write_snap(&dir, "left.gcl", &left_files);
        let right = write_snap(&dir, "right.gcl", &right_files);
        let result = diff_snapshots(&left, &right).unwrap();
        assert!(!result.identical);
        assert!(result.entries.contains(&DiffEntry::Added {
            path: "b.txt".to_string()
        }));
    }

    #[test]
    fn diff_detects_removed_file() {
        let dir = TempDir::new().unwrap();
        let left_files = vec![text_file("a.txt", "a"), text_file("b.txt", "b")];
        let right_files = vec![text_file("a.txt", "a")];
        let left = write_snap(&dir, "left.gcl", &left_files);
        let right = write_snap(&dir, "right.gcl", &right_files);
        let result = diff_snapshots(&left, &right).unwrap();
        assert!(result.entries.contains(&DiffEntry::Removed {
            path: "b.txt".to_string()
        }));
    }

    #[test]
    fn diff_detects_modified_file() {
        let dir = TempDir::new().unwrap();
        let left_files = vec![text_file("a.txt", "old content")];
        let right_files = vec![text_file("a.txt", "new content")];
        let left = write_snap(&dir, "left.gcl", &left_files);
        let right = write_snap(&dir, "right.gcl", &right_files);
        let result = diff_snapshots(&left, &right).unwrap();
        assert!(result.entries.iter().any(|entry| {
            matches!(
                entry,
                DiffEntry::Modified {
                    path,
                    old_sha256,
                    new_sha256
                } if path == "a.txt" && old_sha256 != new_sha256
            )
        }));
    }

    #[test]
    fn diff_detects_rename() {
        let dir = TempDir::new().unwrap();
        // Same content, different path: rename.
        let left_files = vec![text_file("old/name.txt", "content")];
        let right_files = vec![text_file("new/name.txt", "content")];
        let left = write_snap(&dir, "left.gcl", &left_files);
        let right = write_snap(&dir, "right.gcl", &right_files);
        let result = diff_snapshots(&left, &right).unwrap();
        assert!(
            result.entries.contains(&DiffEntry::Renamed {
                old_path: "old/name.txt".to_string(),
                new_path: "new/name.txt".to_string(),
            }),
            "expected Renamed, got {:?}",
            result.entries
        );
        // Must NOT also appear as Added/Removed.
        assert!(!result.entries.contains(&DiffEntry::Added {
            path: "new/name.txt".to_string()
        }));
        assert!(!result.entries.contains(&DiffEntry::Removed {
            path: "old/name.txt".to_string()
        }));
    }

    #[test]
    fn diff_symlink_target_change_uses_dedicated_variant() {
        let dir = TempDir::new().unwrap();
        let left_files = vec![symlink_file("link", "target_a.txt")];
        let right_files = vec![symlink_file("link", "target_b.txt")];
        let left = write_snap(&dir, "left.gcl", &left_files);
        let right = write_snap(&dir, "right.gcl", &right_files);
        let result = diff_snapshots(&left, &right).unwrap();
        assert!(result.entries.contains(&DiffEntry::SymlinkTargetChanged {
            path: "link".to_string(),
            old_target: "target_a.txt".to_string(),
            new_target: "target_b.txt".to_string(),
        }));
        assert!(
            !result
                .entries
                .iter()
                .any(|entry| matches!(entry, DiffEntry::Modified { path, .. } if path == "link")),
            "symlink-vs-symlink changes must not emit Modified"
        );
    }

    #[test]
    fn diff_output_ordering_renames_first() {
        let dir = TempDir::new().unwrap();
        // One rename, one addition, one removal.
        let left_files = vec![
            text_file("old.txt", "renamed content"),
            text_file("removed.txt", "gone"),
        ];
        let right_files = vec![
            text_file("new.txt", "renamed content"),
            text_file("added.txt", "new"),
        ];
        let left = write_snap(&dir, "left.gcl", &left_files);
        let right = write_snap(&dir, "right.gcl", &right_files);
        let result = diff_snapshots(&left, &right).unwrap();
        assert_eq!(result.entries.len(), 3);
        assert!(
            matches!(result.entries[0], DiffEntry::Renamed { .. }),
            "first entry must be Renamed, got {:?}",
            result.entries[0]
        );
    }

    #[test]
    fn diff_detects_mode_change_without_modified() {
        let dir = TempDir::new().unwrap();
        let left_files = vec![text_file_mode("bin/tool.sh", "echo hi\n", "644")];
        let right_files = vec![text_file_mode("bin/tool.sh", "echo hi\n", "755")];
        let left = write_snap(&dir, "left.gcl", &left_files);
        let right = write_snap(&dir, "right.gcl", &right_files);

        let result = diff_snapshots(&left, &right).unwrap();
        assert!(
            result.entries.contains(&DiffEntry::ModeChanged {
                path: "bin/tool.sh".to_string(),
                old_mode: "644".to_string(),
                new_mode: "755".to_string(),
            }),
            "expected ModeChanged entry, got {:?}",
            result.entries
        );
        assert!(
            !result.entries.iter().any(
                |entry| matches!(entry, DiffEntry::Modified { path, .. } if path == "bin/tool.sh")
            ),
            "mode-only change must not be reported as Modified"
        );
    }

    #[test]
    fn diff_rename_with_mode_change_stays_single_rename() {
        let dir = TempDir::new().unwrap();
        let left_files = vec![text_file_mode("old.sh", "echo hi\n", "644")];
        let right_files = vec![text_file_mode("new.sh", "echo hi\n", "755")];
        let left = write_snap(&dir, "left.gcl", &left_files);
        let right = write_snap(&dir, "right.gcl", &right_files);

        let result = diff_snapshots(&left, &right).unwrap();
        assert!(
            result.entries.contains(&DiffEntry::Renamed {
                old_path: "old.sh".to_string(),
                new_path: "new.sh".to_string(),
            }),
            "rename+mode-change should still report a rename"
        );
        assert!(
            !result
                .entries
                .iter()
                .any(|entry| matches!(entry, DiffEntry::ModeChanged { .. })),
            "rename+mode-change should not emit an extra ModeChanged entry"
        );
    }

    #[test]
    fn diff_regular_to_symlink_is_reported_as_removed_plus_added() {
        let dir = TempDir::new().unwrap();
        let left_files = vec![text_file("path", "payload")];
        let right_files = vec![symlink_file("path", "target.txt")];
        let left = write_snap(&dir, "left.gcl", &left_files);
        let right = write_snap(&dir, "right.gcl", &right_files);

        let result = diff_snapshots(&left, &right).unwrap();
        assert!(
            result.entries.contains(&DiffEntry::Removed {
                path: "path".to_string()
            }),
            "type change should include Removed"
        );
        assert!(
            result.entries.contains(&DiffEntry::Added {
                path: "path".to_string()
            }),
            "type change should include Added"
        );
    }

    #[test]
    fn diff_snapshot_to_source_identical_tree_is_identical() {
        let source = TempDir::new().unwrap();
        let snapshots = TempDir::new().unwrap();
        fs::write(source.path().join("a.txt"), b"alpha\n").unwrap();

        let snapshot = snapshots.path().join("snap.gcl");
        crate::snapshot::build::build_snapshot(source.path(), &snapshot).unwrap();

        let result = diff_snapshot_to_source(
            &snapshot,
            source.path(),
            &crate::snapshot::BuildOptions::default(),
        )
        .unwrap();
        assert!(result.identical);
        assert!(result.entries.is_empty());
    }

    #[test]
    fn diff_snapshot_to_source_detects_modified_file() {
        let source = TempDir::new().unwrap();
        let snapshots = TempDir::new().unwrap();
        fs::write(source.path().join("a.txt"), b"alpha\n").unwrap();

        let snapshot = snapshots.path().join("snap.gcl");
        crate::snapshot::build::build_snapshot(source.path(), &snapshot).unwrap();

        fs::write(source.path().join("a.txt"), b"beta\n").unwrap();

        let result = diff_snapshot_to_source(
            &snapshot,
            source.path(),
            &crate::snapshot::BuildOptions::default(),
        )
        .unwrap();
        assert!(result
            .entries
            .iter()
            .any(|entry| matches!(entry, DiffEntry::Modified { path, .. } if path == "a.txt")));
    }

    #[test]
    fn diff_snapshot_to_source_detects_added_file() {
        let source = TempDir::new().unwrap();
        let snapshots = TempDir::new().unwrap();
        fs::write(source.path().join("a.txt"), b"alpha\n").unwrap();

        let snapshot = snapshots.path().join("snap.gcl");
        crate::snapshot::build::build_snapshot(source.path(), &snapshot).unwrap();

        fs::write(source.path().join("b.txt"), b"new\n").unwrap();

        let result = diff_snapshot_to_source(
            &snapshot,
            source.path(),
            &crate::snapshot::BuildOptions::default(),
        )
        .unwrap();
        assert!(result.entries.contains(&DiffEntry::Added {
            path: "b.txt".to_string(),
        }));
    }
}
