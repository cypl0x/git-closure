/// Snapshot summary extraction for compact metadata reporting.
use std::fs;
use std::path::Path;

use crate::utils::io_error_with_path;

use super::serial::parse_snapshot;
use super::{Result, SnapshotSummary};

/// Computes aggregate snapshot metadata for human or machine-readable output.
pub fn summarize_snapshot(path: &Path) -> Result<SnapshotSummary> {
    let text = fs::read_to_string(path).map_err(|err| io_error_with_path(err, path))?;
    let (header, files) = parse_snapshot(&text)?;

    let symlink_count = files.iter().filter(|f| f.symlink_target.is_some()).count();
    let regular_count = files.len().saturating_sub(symlink_count);
    let total_bytes = files.iter().map(|f| f.size).sum::<u64>();

    let mut largest_files: Vec<(String, u64)> = files
        .iter()
        .filter(|f| f.symlink_target.is_none())
        .map(|f| (f.path.clone(), f.size))
        .collect();
    largest_files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    largest_files.truncate(5);

    Ok(SnapshotSummary {
        snapshot_hash: header.snapshot_hash,
        file_count: header.file_count,
        regular_count,
        symlink_count,
        total_bytes,
        git_rev: header.git_rev,
        git_branch: header.git_branch,
        largest_files,
    })
}

#[cfg(test)]
mod tests {
    use super::summarize_snapshot;
    use crate::snapshot::build::build_snapshot;
    use tempfile::TempDir;

    #[test]
    fn summarize_snapshot_largest_files_is_top_five_descending() {
        let source = TempDir::new().expect("create source tempdir");
        for i in 0..7u8 {
            let size = (i + 1) as usize;
            let name = format!("f{i}.txt");
            std::fs::write(source.path().join(name), vec![b'x'; size]).expect("write source file");
        }

        let snapshot = source.path().join("snapshot.gcl");
        build_snapshot(source.path(), &snapshot).expect("build snapshot");

        let summary = summarize_snapshot(&snapshot).expect("summarize snapshot");
        assert_eq!(summary.largest_files.len(), 5);
        assert_eq!(summary.largest_files[0], ("f6.txt".to_string(), 7));
        assert_eq!(summary.largest_files[4], ("f2.txt".to_string(), 3));
    }
}
