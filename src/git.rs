/// Git repository discovery and index enumeration.
///
/// This module wraps the git CLI to collect the set of files that belong to a
/// snapshot.  All git interaction goes through `run_command_output` from the
/// providers layer — the binary is assumed to be on `PATH`.
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::GitClosureError;
use crate::providers::run_command_output;
use crate::utils::truncate_stderr;

type Result<T> = std::result::Result<T, GitClosureError>;

// ── Repository context ────────────────────────────────────────────────────────

/// Resolved git repository context for a given source directory.
///
/// `workdir` is the canonical repository root (`git rev-parse --show-toplevel`).
/// `source_prefix` is the path of the snapshot source relative to `workdir`
/// (empty for a whole-repo snapshot, non-empty for a sub-tree snapshot).
pub(crate) struct GitRepoContext {
    pub(crate) workdir: PathBuf,
    pub(crate) source_prefix: PathBuf,
}

impl GitRepoContext {
    /// Attempts to discover a git repository containing `source`.
    /// Returns `None` if `source` is not inside a git working tree.
    pub(crate) fn discover(source: &Path) -> Result<Option<Self>> {
        let output = run_command_output("git", &["rev-parse", "--show-toplevel"], Some(source))?;
        if !output.status.success() {
            return Ok(None);
        }

        let workdir = String::from_utf8(output.stdout)
            .map_err(|err| {
                GitClosureError::Parse(format!("git returned non-UTF-8 repository root: {err}"))
            })?
            .trim()
            .to_string();
        let workdir = fs::canonicalize(PathBuf::from(workdir))?;
        let source = fs::canonicalize(source)?;

        if !source.starts_with(&workdir) {
            return Ok(None);
        }

        let source_prefix = source
            .strip_prefix(&workdir)
            .map_err(|err| {
                GitClosureError::Parse(format!(
                    "failed to determine source prefix under git workdir: {} ({err})",
                    source.display(),
                ))
            })?
            .to_path_buf();

        Ok(Some(Self {
            workdir,
            source_prefix,
        }))
    }
}

// ── File enumeration ──────────────────────────────────────────────────────────

/// Returns the git-tracked paths from the index (repo-relative).
pub(crate) fn tracked_paths_from_index(context: &GitRepoContext) -> Result<Vec<PathBuf>> {
    git_ls_files(context, false)
}

/// Returns untracked (but not ignored) paths from `git status` (repo-relative).
pub(crate) fn untracked_paths_from_status(context: &GitRepoContext) -> Result<Vec<PathBuf>> {
    git_ls_files(context, true)
}

/// Core enumeration: wraps `git ls-files -z [--cached] [--others]`.
pub(crate) fn git_ls_files(
    context: &GitRepoContext,
    include_untracked: bool,
) -> Result<Vec<PathBuf>> {
    let mut args = vec!["ls-files", "-z", "--cached"];
    if include_untracked {
        args.extend(["--others", "--exclude-standard"]);
    }

    let output = run_command_output("git", &args, Some(&context.workdir))?;

    if !output.status.success() {
        return Err(GitClosureError::CommandExitFailure {
            command: "git",
            status: output.status.to_string(),
            stderr: truncate_stderr(&output.stderr),
        });
    }

    let mut paths = Vec::new();
    for chunk in output.stdout.split(|b| *b == 0u8) {
        if chunk.is_empty() {
            continue;
        }
        let path = std::str::from_utf8(chunk).map_err(|err| {
            GitClosureError::Parse(format!("git ls-files produced non-UTF-8 path: {err}"))
        })?;
        paths.push(PathBuf::from(path));
    }

    Ok(paths)
}

// ── Cleanliness check ─────────────────────────────────────────────────────────

/// Verifies that the source tree has no staged, unstaged, or untracked changes.
/// Returns `Err` with a descriptive message if the tree is dirty.
pub(crate) fn ensure_git_source_is_clean(context: &GitRepoContext) -> Result<()> {
    let output = run_command_output(
        "git",
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        Some(&context.workdir),
    )?;

    if !output.status.success() {
        return Err(GitClosureError::CommandExitFailure {
            command: "git",
            status: output.status.to_string(),
            stderr: truncate_stderr(&output.stderr),
        });
    }

    evaluate_git_status_porcelain(&output.stdout, &context.source_prefix)
}

/// Parses the NUL-delimited `git status --porcelain=v1 -z` output and
/// returns an error if any entry falls within `source_prefix`.
pub(crate) fn evaluate_git_status_porcelain(stdout: &[u8], source_prefix: &Path) -> Result<()> {
    let mut chunks = stdout.split(|b| *b == 0u8);
    while let Some(entry) = chunks.next() {
        if entry.is_empty() {
            continue;
        }

        let (xy, path) = parse_porcelain_entry(entry)?;

        let repo_relative = Path::new(path);
        if is_within_prefix(repo_relative, source_prefix) {
            return Err(GitClosureError::Parse(format!(
                "source tree is dirty at {path} (use --include-untracked or clean working tree)"
            )));
        }

        if matches!(xy[0], b'R' | b'C') || matches!(xy[1], b'R' | b'C') {
            let source_path_bytes = chunks.next().ok_or_else(|| {
                GitClosureError::Parse(
                    "git status rename/copy entry missing source path chunk".to_string(),
                )
            })?;
            if source_path_bytes.is_empty() {
                return Err(GitClosureError::Parse(
                    "git status rename/copy source path is empty".to_string(),
                ));
            }
            let source_path = std::str::from_utf8(source_path_bytes).map_err(|err| {
                GitClosureError::Parse(format!("git status produced non-UTF-8 path: {err}"))
            })?;
            if is_within_prefix(Path::new(source_path), source_prefix) {
                return Err(GitClosureError::Parse(format!(
                    "source tree is dirty at {source_path} (use --include-untracked or clean working tree)"
                )));
            }
        }
    }

    Ok(())
}

/// Parses a single entry from `git status --porcelain=v1 -z` output.
/// Returns the XY status bytes and the NUL-terminated path string.
pub(crate) fn parse_porcelain_entry(entry: &[u8]) -> Result<([u8; 2], &str)> {
    if entry.len() < 4 || entry[2] != b' ' {
        return Err(GitClosureError::Parse(format!(
            "git status produced malformed porcelain entry: {entry:?}"
        )));
    }

    let xy = [entry[0], entry[1]];
    let path = std::str::from_utf8(&entry[3..]).map_err(|err| {
        GitClosureError::Parse(format!("git status produced non-UTF-8 path: {err}"))
    })?;
    Ok((xy, path))
}

// ── Path utilities ────────────────────────────────────────────────────────────

/// Returns `true` if `path` is at or below `prefix`.
/// An empty prefix matches everything (whole-repo snapshot).
pub(crate) fn is_within_prefix(path: &Path, prefix: &Path) -> bool {
    if prefix.as_os_str().is_empty() {
        return true;
    }
    path.starts_with(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    #[test]
    fn is_within_prefix_empty_prefix_matches_everything() {
        assert!(is_within_prefix(Path::new("a/b"), Path::new("")));
        assert!(is_within_prefix(Path::new(""), Path::new("")));
    }

    #[test]
    fn is_within_prefix_matches_sub_paths() {
        assert!(is_within_prefix(Path::new("src/lib.rs"), Path::new("src")));
        assert!(!is_within_prefix(Path::new("tests/foo"), Path::new("src")));
    }

    #[test]
    fn parse_porcelain_entry_happy_path() {
        let entry = b"M  src/lib.rs";
        let (xy, path) = parse_porcelain_entry(entry).expect("parse entry");
        assert_eq!(xy, [b'M', b' ']);
        assert_eq!(path, "src/lib.rs");
    }

    #[test]
    fn parse_porcelain_entry_rejects_short_entries() {
        let err = parse_porcelain_entry(b"M ").expect_err("short entry should fail");
        assert!(matches!(err, GitClosureError::Parse(_)));
    }

    #[test]
    fn evaluate_git_status_porcelain_empty_output_is_clean() {
        evaluate_git_status_porcelain(b"", Path::new("")).expect("empty output means clean tree");
    }

    #[test]
    fn evaluate_git_status_porcelain_dirty_file_outside_prefix_passes() {
        let stdout = b"M  outside/dirty.txt\0";
        evaluate_git_status_porcelain(stdout, Path::new("src"))
            .expect("dirty file outside prefix should not fail");
    }

    #[test]
    fn evaluate_git_status_porcelain_dirty_file_inside_prefix_fails() {
        let stdout = b"M  src/dirty.txt\0";
        let err = evaluate_git_status_porcelain(stdout, Path::new("src"))
            .expect_err("dirty file inside prefix must fail");
        assert!(matches!(err, GitClosureError::Parse(_)));
    }

    #[test]
    fn git_ls_files_non_repo_returns_command_exit_failure() {
        let temp = tempfile::TempDir::new().expect("create tempdir");
        let context = GitRepoContext {
            workdir: temp.path().to_path_buf(),
            source_prefix: PathBuf::new(),
        };
        let err = git_ls_files(&context, false).expect_err("non-repo should fail");
        assert!(
            matches!(
                err,
                GitClosureError::CommandExitFailure { command: "git", .. }
            ),
            "expected CommandExitFailure, got {err:?}"
        );
    }

    #[test]
    fn ensure_git_source_is_clean_non_repo_error_includes_stderr_context() {
        let temp = tempfile::TempDir::new().expect("create tempdir");
        let context = GitRepoContext {
            workdir: temp.path().to_path_buf(),
            source_prefix: PathBuf::new(),
        };

        let err = ensure_git_source_is_clean(&context).expect_err("non-repo should fail");
        match err {
            GitClosureError::CommandExitFailure { stderr, .. } => {
                let display = stderr.to_lowercase();
                assert!(
                    display.contains("not a git repository")
                        || display.contains("work tree")
                        || display.contains("git"),
                    "stderr payload should contain actionable git context, got: {stderr:?}"
                );
            }
            other => panic!("expected CommandExitFailure, got {other:?}"),
        }
    }

    #[test]
    fn git_ls_files_non_repo_error_includes_stderr_context() {
        let temp = tempfile::TempDir::new().expect("create tempdir");
        let context = GitRepoContext {
            workdir: temp.path().to_path_buf(),
            source_prefix: PathBuf::new(),
        };

        let err = git_ls_files(&context, false).expect_err("non-repo should fail");
        match err {
            GitClosureError::CommandExitFailure { stderr, .. } => {
                let display = stderr.to_lowercase();
                assert!(
                    display.contains("not a git repository")
                        || display.contains("work tree")
                        || display.contains("git"),
                    "stderr payload should contain actionable git context, got: {stderr:?}"
                );
            }
            other => panic!("expected CommandExitFailure, got {other:?}"),
        }
    }

    #[test]
    fn discover_returns_none_outside_git_repo() {
        let temp = tempfile::TempDir::new().expect("create tempdir");
        let discovered = GitRepoContext::discover(temp.path()).expect("discover should not error");
        assert!(discovered.is_none());
    }

    #[test]
    fn discover_returns_context_inside_git_repo() {
        let repo = tempfile::TempDir::new().expect("create tempdir");
        init_git_repo(repo.path());
        fs::create_dir_all(repo.path().join("src")).expect("create src directory");

        let source = fs::canonicalize(repo.path().join("src")).expect("canonicalize source path");
        let discovered = GitRepoContext::discover(&source)
            .expect("discover should not error")
            .expect("repo context should be found");

        let expected_workdir = fs::canonicalize(repo.path()).expect("canonicalize repo path");
        assert_eq!(discovered.workdir, expected_workdir);
        assert_eq!(discovered.source_prefix, PathBuf::from("src"));
    }

    #[cfg(unix)]
    #[test]
    fn discover_returns_context_for_symlinked_repo_path() {
        let repo = tempfile::TempDir::new().expect("create tempdir");
        let alias_root = tempfile::TempDir::new().expect("create alias tempdir");
        init_git_repo(repo.path());
        fs::create_dir_all(repo.path().join("src")).expect("create src directory");

        let alias = alias_root.path().join("repo-link");
        std::os::unix::fs::symlink(repo.path(), &alias).expect("create repo symlink");
        let source = alias.join("src");

        let discovered = GitRepoContext::discover(&source)
            .expect("discover should not error")
            .expect("repo context should be found through symlink alias");

        let expected_workdir = fs::canonicalize(repo.path()).expect("canonicalize repo path");
        assert_eq!(discovered.workdir, expected_workdir);
        assert_eq!(discovered.source_prefix, PathBuf::from("src"));
    }

    #[test]
    fn tracked_paths_from_index_returns_committed_files() {
        let repo = tempfile::TempDir::new().expect("create tempdir");
        init_git_repo(repo.path());
        fs::write(repo.path().join("tracked.txt"), b"tracked\n").expect("write tracked");
        run_git(repo.path(), &["add", "tracked.txt"]);
        run_git(repo.path(), &["commit", "-m", "initial"]);

        let source = fs::canonicalize(repo.path()).expect("canonicalize repo path");
        let context = GitRepoContext::discover(&source)
            .expect("discover should not error")
            .expect("repo context should exist");
        let tracked = tracked_paths_from_index(&context).expect("list tracked paths");
        assert!(tracked.iter().any(|p| p == &PathBuf::from("tracked.txt")));
    }

    #[test]
    fn untracked_paths_from_status_excludes_gitignored() {
        let repo = tempfile::TempDir::new().expect("create tempdir");
        init_git_repo(repo.path());
        fs::write(repo.path().join("tracked.txt"), b"tracked\n").expect("write tracked");
        fs::write(repo.path().join(".gitignore"), b"ignored.txt\n").expect("write gitignore");
        run_git(repo.path(), &["add", "tracked.txt", ".gitignore"]);
        run_git(repo.path(), &["commit", "-m", "initial"]);

        fs::write(repo.path().join("new.txt"), b"new\n").expect("write new file");
        fs::write(repo.path().join("ignored.txt"), b"ignored\n").expect("write ignored file");

        let source = fs::canonicalize(repo.path()).expect("canonicalize repo path");
        let context = GitRepoContext::discover(&source)
            .expect("discover should not error")
            .expect("repo context should exist");
        let untracked = untracked_paths_from_status(&context).expect("list untracked paths");

        assert!(untracked.iter().any(|p| p == &PathBuf::from("new.txt")));
        assert!(!untracked.iter().any(|p| p == &PathBuf::from("ignored.txt")));
    }

    fn init_git_repo(path: &std::path::Path) {
        run_git(path, &["init"]);
        run_git(path, &["config", "user.name", "git-closure-test"]);
        run_git(
            path,
            &["config", "user.email", "git-closure-test@example.com"],
        );
    }

    fn run_git(path: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("run git command");
        assert!(status.success(), "git command failed: git {:?}", args);
    }
}
