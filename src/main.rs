use std::path::{Path, PathBuf};
use std::{io, process};

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, shells};
use serde::Serialize;

use git_closure::{
    build_snapshot_from_source, compile_source, diff_snapshot_to_source, diff_snapshots,
    export_snapshot_as_nar, fmt_snapshot_with_options, list_snapshot, materialize_snapshot,
    providers::ProviderKind, recipe, render_snapshot, summarize_snapshot, verify_snapshot,
    BuildOptions, CompileFormat, DiffEntry, FmtOptions, GitClosureError, ListEntry, RenderFormat,
    SnapshotSummary,
};

#[derive(Parser, Debug)]
#[command(name = "git-closure")]
#[command(about = "Deterministic S-expression source snapshots")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[command(about = "Build a deterministic snapshot", visible_alias = "b")]
    Build {
        #[arg(help = "Source path or remote URL to snapshot")]
        source: String,
        #[arg(
            short,
            long,
            help = "Output file (default: <source-basename>[@ref].gcl; for '.' uses current directory basename)"
        )]
        output: Option<PathBuf>,
        #[arg(long)]
        include_untracked: bool,
        #[arg(long)]
        require_clean: bool,
        #[arg(long, value_enum, default_value_t = BuildProvider::Auto)]
        provider: BuildProvider,
    },
    #[command(about = "Materialize a snapshot to a directory", visible_alias = "m")]
    Materialize {
        #[arg(help = "Snapshot file to materialize")]
        snapshot: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    #[command(about = "Verify snapshot integrity", visible_alias = "v")]
    Verify {
        #[arg(help = "Snapshot file to verify")]
        snapshot: PathBuf,
        #[arg(short, long, help = "Suppress success output")]
        quiet: bool,
    },
    #[command(about = "List files in a snapshot", visible_alias = "l")]
    List {
        #[arg(help = "Snapshot file to list")]
        snapshot: PathBuf,
        #[arg(long, help = "Output JSON array")]
        json: bool,
        #[arg(long, help = "Show sha256, mode, size, and type for each entry")]
        long: bool,
    },
    #[command(
        about = "Compare two snapshots and show differences",
        visible_alias = "d"
    )]
    Diff {
        #[arg(help = "Left (old) snapshot")]
        left: PathBuf,
        #[arg(
            help = "Right (new) snapshot file, or a directory to compare against the live source tree"
        )]
        right: PathBuf,
        #[arg(long, help = "Output JSON", conflicts_with = "stat")]
        json: bool,
        #[arg(long, help = "Output summary counts only")]
        stat: bool,
    },
    #[command(about = "Canonically reformat a snapshot file", visible_alias = "f")]
    Fmt {
        #[arg(help = "Snapshot file to format")]
        snapshot: PathBuf,
        #[arg(
            long,
            help = "Check whether the snapshot is already canonical; exit non-zero if not"
        )]
        check: bool,
        #[arg(long, help = "Allow recomputing a mismatched snapshot-hash")]
        repair_hash: bool,
    },
    #[command(
        about = "Render a snapshot as a human-readable report (text, Markdown, HTML, or JSON)",
        visible_alias = "r"
    )]
    Render {
        #[arg(help = "Snapshot file to render")]
        snapshot: PathBuf,
        #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
        format: ReportFormat,
        #[arg(
            long,
            help = "Prepend YAML front matter for pandoc (only with --format markdown)"
        )]
        pandoc: bool,
        #[arg(short, long, help = "Write output to file instead of stdout")]
        output: Option<PathBuf>,
    },
    #[command(about = "Print compact snapshot metadata", visible_alias = "s")]
    Summary {
        #[arg(help = "Snapshot file to summarize")]
        snapshot: PathBuf,
        #[arg(long, help = "Output JSON")]
        json: bool,
    },
    #[command(about = "Generate shell completion scripts", visible_alias = "c")]
    Completion {
        #[arg(help = "Shell to generate completions for")]
        shell: CompletionShell,
    },
    #[command(
        about = "Compile a source into an artifact via the IR pipeline (gcl or nar)",
        visible_alias = "C",
        after_help = "PROVENANCE: compile does not read git metadata. \
                      For git-aware snapshots with git-rev/git-branch, use `build`."
    )]
    Compile {
        #[arg(help = "Source path or remote URL to compile")]
        source: String,
        #[arg(short, long, help = "Output file path")]
        output: PathBuf,
        #[arg(
            long,
            value_enum,
            default_value_t = CompileOutputFormat::Gcl,
            help = "Output format"
        )]
        format: CompileOutputFormat,
        #[arg(long, value_enum, default_value_t = BuildProvider::Auto)]
        provider: BuildProvider,
    },
    #[command(
        about = "Execute a recipe or manifest file",
        visible_alias = "R",
        after_help = "PROVENANCE: behavior depends on recipe mode. \
                      mode = \"compile\" (default) uses the provenance-light compile path — \
                      no git metadata is injected. \
                      mode = \"build\" uses the git-aware build path and records git metadata where available (gcl only). \
                      Manifests with multiple targets require --target <name> unless default_target is set. \
                      See `build` for the direct build command."
    )]
    Run {
        #[arg(help = "Recipe or manifest file (.toml)")]
        recipe: PathBuf,
        #[arg(
            long,
            value_name = "TARGET",
            help = "Target to execute (required when manifest has multiple targets with no default_target)"
        )]
        target: Option<String>,
    },
    #[command(
        about = "Export a snapshot to another archive format (currently: NAR)",
        visible_alias = "e",
        after_help = "METADATA LOSS: NAR does not store snapshot-hash, git-rev, git-branch, \
                      source-uri, or per-file sha256. Only file contents, symlink targets, and \
                      the executable flag are preserved. Use the .gcl format for provenance \
                      tracking."
    )]
    Export {
        #[arg(help = "Snapshot file to export")]
        snapshot: PathBuf,
        #[arg(short, long, help = "Output file path (required; NAR is binary)")]
        output: PathBuf,
        #[arg(
            long,
            value_enum,
            default_value_t = ExportFormat::Nar,
            help = "Export format (currently only 'nar' is supported)"
        )]
        format: ExportFormat,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum ReportFormat {
    Text,
    Markdown,
    Html,
    Json,
}

impl ReportFormat {
    fn into_render_format(self, pandoc: bool) -> RenderFormat {
        match self {
            ReportFormat::Text => RenderFormat::Text,
            ReportFormat::Markdown => RenderFormat::Markdown { pandoc },
            ReportFormat::Html => RenderFormat::Html,
            ReportFormat::Json => RenderFormat::Json,
        }
    }
}

#[derive(Clone, Debug, ValueEnum)]
enum CompletionShell {
    Bash,
    Zsh,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum BuildProvider {
    Auto,
    Local,
    GitClone,
    Nix,
    GithubApi,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExportFormat {
    Nar,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CompileOutputFormat {
    Gcl,
    Nar,
}

impl From<CompileOutputFormat> for CompileFormat {
    fn from(value: CompileOutputFormat) -> Self {
        match value {
            CompileOutputFormat::Gcl => CompileFormat::Gcl,
            CompileOutputFormat::Nar => CompileFormat::Nar,
        }
    }
}

impl From<BuildProvider> for ProviderKind {
    fn from(value: BuildProvider) -> Self {
        match value {
            BuildProvider::Auto => ProviderKind::Auto,
            BuildProvider::Local => ProviderKind::Local,
            BuildProvider::GitClone => ProviderKind::GitClone,
            BuildProvider::Nix => ProviderKind::Nix,
            BuildProvider::GithubApi => ProviderKind::GithubApi,
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        process::exit(4);
    }
}

fn run() -> Result<(), GitClosureError> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build {
            source,
            output,
            include_untracked,
            require_clean,
            provider,
        } => {
            let output = if let Some(path) = output {
                path
            } else {
                let derived = derive_output_path(&source);
                eprintln!("note: writing snapshot to {}", derived.display());
                derived
            };
            let options = BuildOptions {
                include_untracked,
                require_clean,
                source_annotation: None,
            };
            build_snapshot_from_source(&source, &output, &options, provider.into())?;
        }
        Commands::Materialize { snapshot, output } => {
            materialize_snapshot(&snapshot, &output)?;
        }
        Commands::Verify { snapshot, quiet } => {
            let report = verify_snapshot(&snapshot)?;
            if !quiet {
                println!("OK: verified {} file(s)", report.file_count);
            }
        }
        Commands::List {
            snapshot,
            json,
            long,
        } => {
            let entries = list_snapshot(&snapshot)?;
            print_list(&entries, json, long);
        }
        Commands::Diff {
            left,
            right,
            json,
            stat,
        } => {
            let result = if should_diff_against_source(&right) {
                diff_snapshot_to_source(&left, &right, &BuildOptions::default())?
            } else {
                diff_snapshots(&left, &right)?
            };
            if stat {
                print_diff_stat(&result.entries);
            } else {
                print_diff(&result.entries, json);
            }
            if !result.identical {
                process::exit(1);
            }
        }
        Commands::Fmt {
            snapshot,
            check,
            repair_hash,
        } => {
            let canonical = match fmt_snapshot_with_options(&snapshot, FmtOptions { repair_hash }) {
                Ok(canonical) => canonical,
                Err(GitClosureError::HashMismatch { .. }) if check => {
                    eprintln!(
                        "error: {} has an integrity mismatch between stored and recomputed snapshot-hash",
                        snapshot.display()
                    );
                    process::exit(4);
                }
                Err(
                    GitClosureError::Parse(_)
                    | GitClosureError::MissingHeader(_)
                    | GitClosureError::LegacyHeader,
                ) if check => {
                    eprintln!(
                        "error: {} is not parseable as a snapshot",
                        snapshot.display()
                    );
                    process::exit(4);
                }
                Err(err) => return Err(err),
            };
            if check {
                let current = std::fs::read_to_string(&snapshot).map_err(GitClosureError::from)?;
                if current != canonical {
                    eprintln!(
                        "error: {} is not in canonical format (run `git-closure fmt` to fix)",
                        snapshot.display()
                    );
                    process::exit(1);
                }
            } else {
                std::fs::write(&snapshot, canonical.as_bytes()).map_err(GitClosureError::from)?;
            }
        }
        Commands::Render {
            snapshot,
            format,
            pandoc,
            output,
        } => {
            let rendered = render_snapshot(&snapshot, format.into_render_format(pandoc))?;
            if let Some(path) = output {
                std::fs::write(path, rendered.as_bytes())?;
            } else {
                print!("{rendered}");
            }
        }
        Commands::Summary { snapshot, json } => {
            let summary = summarize_snapshot(&snapshot)?;
            if json {
                println!("{}", summary_json(&summary));
            } else {
                print_summary(&summary);
            }
        }
        Commands::Completion { shell } => {
            print_completion(shell);
        }
        Commands::Compile {
            source,
            output,
            format,
            provider,
        } => {
            compile_source(&source, &output, format.into(), provider.into())?;
        }
        Commands::Run {
            recipe: recipe_path,
            target,
        } => {
            let manifest = recipe::manifest_from_file(&recipe_path)?;
            let r = manifest.select(target.as_deref())?;
            recipe::execute(r)?;
        }
        Commands::Export {
            snapshot,
            output,
            format: _format, // NAR is the only format; field reserved for future extension
        } => {
            export_snapshot_as_nar(&snapshot, &output)?;
        }
    }

    Ok(())
}

fn should_diff_against_source(right: &Path) -> bool {
    right.is_dir()
}

// Local alias for utils::sha256_prefix; the library version cannot be
// accessed from the binary crate without a public re-export.
fn sha256_prefix(sha256: &str) -> &str {
    &sha256[..sha256.len().min(16)]
}

// ── Output helpers ────────────────────────────────────────────────────────────

fn print_diff(entries: &[DiffEntry], json: bool) {
    if json {
        println!("{}", diff_entries_json(entries));
    } else {
        for e in entries {
            match e {
                DiffEntry::Added { path } => println!("A\t{path}"),
                DiffEntry::Removed { path } => println!("D\t{path}"),
                DiffEntry::Modified {
                    path,
                    old_sha256,
                    new_sha256,
                } => {
                    println!("M\t{path}\t{old_sha256}\t->\t{new_sha256}")
                }
                DiffEntry::Renamed { old_path, new_path } => {
                    println!("R\t{old_path}\t->\t{new_path}")
                }
                DiffEntry::ModeChanged {
                    path,
                    old_mode,
                    new_mode,
                } => {
                    println!("T\t{path}\t{old_mode}->{new_mode}")
                }
                DiffEntry::SymlinkTargetChanged {
                    path,
                    old_target,
                    new_target,
                } => {
                    println!("S\t{path}\t{old_target}\t->\t{new_target}")
                }
                _ => {}
            }
        }
    }
}

fn print_diff_stat(entries: &[DiffEntry]) {
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut modified = 0usize;
    let mut mode_changed = 0usize;
    let mut symlink_changed = 0usize;
    let mut renamed = 0usize;

    for entry in entries {
        match entry {
            DiffEntry::Added { .. } => added += 1,
            DiffEntry::Removed { .. } => removed += 1,
            DiffEntry::Modified { .. } => modified += 1,
            DiffEntry::ModeChanged { .. } => mode_changed += 1,
            DiffEntry::SymlinkTargetChanged { .. } => symlink_changed += 1,
            DiffEntry::Renamed { .. } => renamed += 1,
            _ => {}
        }
    }

    let total = added + removed + modified + mode_changed + symlink_changed + renamed;
    println!("added:        {added}");
    println!("removed:      {removed}");
    println!("modified:     {modified}");
    println!("mode_changed: {mode_changed}");
    println!("symlink_changed: {symlink_changed}");
    println!("renamed:      {renamed}");
    println!("total:        {total}");
}

fn print_list(entries: &[ListEntry], json: bool, long: bool) {
    if json {
        println!("{}", list_entries_json(entries, long));
    } else if long {
        for e in entries {
            let entry_type = if e.is_symlink { "symlink" } else { "file" };
            let detail = if e.is_symlink {
                format!("-> {}", e.symlink_target.as_deref().unwrap_or(""))
            } else {
                format!("{}  {}  {}", e.mode, e.size, sha256_prefix(&e.sha256))
            };
            println!("{}\t{}\t{}", e.path, entry_type, detail);
        }
    } else {
        for e in entries {
            println!("{}", e.path);
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DiffJsonEntry {
    Added {
        path: String,
    },
    Removed {
        path: String,
    },
    Modified {
        path: String,
        old_sha256: String,
        new_sha256: String,
    },
    Renamed {
        old_path: String,
        new_path: String,
    },
    ModeChanged {
        path: String,
        old_mode: String,
        new_mode: String,
    },
    SymlinkTargetChanged {
        path: String,
        old_target: String,
        new_target: String,
    },
    Unknown {
        variant_name: String,
    },
}

fn diff_entries_json(entries: &[DiffEntry]) -> String {
    let payload: Vec<DiffJsonEntry> = entries
        .iter()
        .map(|entry| match entry {
            DiffEntry::Added { path } => DiffJsonEntry::Added { path: path.clone() },
            DiffEntry::Removed { path } => DiffJsonEntry::Removed { path: path.clone() },
            DiffEntry::Modified {
                path,
                old_sha256,
                new_sha256,
            } => DiffJsonEntry::Modified {
                path: path.clone(),
                old_sha256: old_sha256.clone(),
                new_sha256: new_sha256.clone(),
            },
            DiffEntry::Renamed { old_path, new_path } => DiffJsonEntry::Renamed {
                old_path: old_path.clone(),
                new_path: new_path.clone(),
            },
            DiffEntry::ModeChanged {
                path,
                old_mode,
                new_mode,
            } => DiffJsonEntry::ModeChanged {
                path: path.clone(),
                old_mode: old_mode.clone(),
                new_mode: new_mode.clone(),
            },
            DiffEntry::SymlinkTargetChanged {
                path,
                old_target,
                new_target,
            } => DiffJsonEntry::SymlinkTargetChanged {
                path: path.clone(),
                old_target: old_target.clone(),
                new_target: new_target.clone(),
            },
            _ => DiffJsonEntry::Unknown {
                variant_name: entry.stable_variant_name().to_string(),
            },
        })
        .collect();
    serde_json::to_string_pretty(&payload).expect("serialize diff JSON")
}

#[derive(Debug, Serialize)]
struct ListJsonEntry {
    path: String,
    r#type: &'static str,
    size: u64,
    mode: String,
    sha256: String,
    symlink_target: Option<String>,
}

fn list_entries_json(entries: &[ListEntry], long: bool) -> String {
    if long {
        let payload: Vec<ListJsonEntry> = entries
            .iter()
            .map(|entry| ListJsonEntry {
                path: entry.path.clone(),
                r#type: if entry.is_symlink { "symlink" } else { "file" },
                size: entry.size,
                mode: entry.mode.clone(),
                sha256: entry.sha256.clone(),
                symlink_target: entry.symlink_target.clone(),
            })
            .collect();
        serde_json::to_string_pretty(&payload).expect("serialize list JSON")
    } else {
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        serde_json::to_string_pretty(&paths).expect("serialize list JSON paths")
    }
}

fn print_summary(summary: &SnapshotSummary) {
    println!("snapshot_hash: {}", summary.snapshot_hash);
    println!("file_count: {}", summary.file_count);
    println!("regular_count: {}", summary.regular_count);
    println!("symlink_count: {}", summary.symlink_count);
    println!("total_bytes: {}", summary.total_bytes);
    println!(
        "git_rev: {}",
        summary.git_rev.as_deref().unwrap_or("(none)")
    );
    println!(
        "git_branch: {}",
        summary.git_branch.as_deref().unwrap_or("(none)")
    );
    if summary.largest_files.is_empty() {
        println!("largest_files: (none)");
        return;
    }
    println!("largest_files:");
    for (path, size) in &summary.largest_files {
        println!("  - {path}\t{size}");
    }
}

fn summary_json(summary: &SnapshotSummary) -> String {
    serde_json::to_string_pretty(summary).expect("serialize summary JSON")
}

// ── Output filename derivation (T-31) ─────────────────────────────────────────

/// Derives a default output path for the `build` command when `--output` is
/// not supplied.
///
/// Rules (first match wins):
/// 1. `gh:owner/repo[@ref]` or `gl:owner/repo[@ref]` → `<repo>[@ref].gcl`
/// 2. Local filesystem path → `<basename>.gcl` (skipping `.` / `..`)
/// 3. Fallback → `snapshot.gcl`
pub(crate) fn derive_output_path(source: &str) -> PathBuf {
    // Handle gh:/gl: shorthand — preserve optional @ref in output name.
    if let Some(rest) = source
        .strip_prefix("gh:")
        .or_else(|| source.strip_prefix("gl:"))
    {
        let (repo, reference) = rest
            .rsplit_once('@')
            .map(|(r, rf)| (r, Some(rf)))
            .unwrap_or((rest, None));
        let name = repo.rsplit_once('/').map(|(_, n)| n).unwrap_or(repo);
        if !name.is_empty() {
            let output = if let Some(reference) = reference {
                format!("{name}@{reference}.gcl")
            } else {
                format!("{name}.gcl")
            };
            return PathBuf::from(output);
        }
    }

    if source == "." {
        if let Ok(cwd) = std::env::current_dir() {
            if let Some(name) = cwd.file_name().and_then(|n| n.to_str()) {
                if !name.is_empty() && name != "." && name != ".." {
                    return PathBuf::from(format!("{name}.gcl"));
                }
            }
        }
    }

    // Local path: use the last non-trivial component.
    let path = Path::new(source);
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if !name.is_empty() && name != "." && name != ".." {
            return PathBuf::from(format!("{name}.gcl"));
        }
    }

    PathBuf::from("snapshot.gcl")
}

fn print_completion(shell: CompletionShell) {
    let mut cmd = Cli::command();
    let bin_name = cmd.get_name().to_string();
    match shell {
        CompletionShell::Bash => {
            generate(shells::Bash, &mut cmd, bin_name, &mut io::stdout());
        }
        CompletionShell::Zsh => {
            generate(shells::Zsh, &mut cmd, bin_name, &mut io::stdout());
        }
    }
    process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::{derive_output_path, diff_entries_json, list_entries_json, summary_json};
    use git_closure::{DiffEntry, ListEntry, SnapshotSummary};
    use serde_json::Value;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static CWD_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn derive_output_path_gh_shorthand() {
        assert_eq!(
            derive_output_path("gh:owner/repo"),
            PathBuf::from("repo.gcl")
        );
        assert_eq!(
            derive_output_path("gh:owner/repo@main"),
            PathBuf::from("repo@main.gcl")
        );
    }

    #[test]
    fn derive_output_path_gl_shorthand() {
        assert_eq!(
            derive_output_path("gl:group/project@v1.2"),
            PathBuf::from("project@v1.2.gcl")
        );
    }

    #[test]
    fn derive_output_path_local_path() {
        assert_eq!(
            derive_output_path("/home/user/myrepo"),
            PathBuf::from("myrepo.gcl")
        );
        assert_eq!(
            derive_output_path("relative/myrepo"),
            PathBuf::from("myrepo.gcl")
        );
    }

    #[test]
    fn derive_output_path_fallback() {
        // A bare URL without a recognizable last segment.
        assert_eq!(derive_output_path(""), PathBuf::from("snapshot.gcl"));
    }

    #[test]
    fn derive_output_path_dot_uses_current_directory_basename() {
        let _guard = CWD_LOCK.lock().expect("lock current dir");
        let original = std::env::current_dir().expect("capture current dir");
        let temp = tempfile::TempDir::new().expect("create tempdir");
        let project = temp.path().join("myproj");
        std::fs::create_dir_all(&project).expect("create cwd fixture dir");

        std::env::set_current_dir(&project).expect("switch cwd");
        let derived = derive_output_path(".");
        std::env::set_current_dir(original).expect("restore cwd");

        assert_eq!(derived, PathBuf::from("myproj.gcl"));
    }

    #[test]
    fn diff_entries_json_round_trips_with_serde_json() {
        let entries = vec![
            DiffEntry::Modified {
                path: "a.txt".to_string(),
                old_sha256: "oldhash".to_string(),
                new_sha256: "newhash".to_string(),
            },
            DiffEntry::ModeChanged {
                path: "script.sh".to_string(),
                old_mode: "644".to_string(),
                new_mode: "755".to_string(),
            },
        ];

        let json = diff_entries_json(&entries);
        let value: Value = serde_json::from_str(&json).expect("diff JSON must parse");
        let arr = value.as_array().expect("diff JSON must be an array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], Value::String("modified".to_string()));
        assert_eq!(arr[0]["path"], Value::String("a.txt".to_string()));
        assert_eq!(arr[1]["type"], Value::String("mode_changed".to_string()));
    }

    #[test]
    fn diff_entries_json_includes_symlink_target_changed() {
        let entries = vec![DiffEntry::SymlinkTargetChanged {
            path: "link".to_string(),
            old_target: "a".to_string(),
            new_target: "b".to_string(),
        }];

        let json = diff_entries_json(&entries);
        let value: Value = serde_json::from_str(&json).expect("diff JSON must parse");
        let arr = value.as_array().expect("diff JSON must be an array");
        assert_eq!(
            arr[0]["type"],
            Value::String("symlink_target_changed".to_string())
        );
        assert_eq!(arr[0]["old_target"], Value::String("a".to_string()));
        assert_eq!(arr[0]["new_target"], Value::String("b".to_string()));
    }

    #[test]
    fn diff_entries_json_all_known_variants_produce_typed_output() {
        // Update this table when DiffEntry gains new variants.
        let cases = vec![
            (
                DiffEntry::Added {
                    path: "added.txt".to_string(),
                },
                "added",
            ),
            (
                DiffEntry::Removed {
                    path: "removed.txt".to_string(),
                },
                "removed",
            ),
            (
                DiffEntry::Modified {
                    path: "modified.txt".to_string(),
                    old_sha256: "oldhash".to_string(),
                    new_sha256: "newhash".to_string(),
                },
                "modified",
            ),
            (
                DiffEntry::Renamed {
                    old_path: "old/name.txt".to_string(),
                    new_path: "new/name.txt".to_string(),
                },
                "renamed",
            ),
            (
                DiffEntry::ModeChanged {
                    path: "script.sh".to_string(),
                    old_mode: "644".to_string(),
                    new_mode: "755".to_string(),
                },
                "mode_changed",
            ),
            (
                DiffEntry::SymlinkTargetChanged {
                    path: "link".to_string(),
                    old_target: "a".to_string(),
                    new_target: "b".to_string(),
                },
                "symlink_target_changed",
            ),
        ];

        let entries: Vec<DiffEntry> = cases.iter().map(|(entry, _)| entry.clone()).collect();
        let json = diff_entries_json(&entries);
        let value: Value = serde_json::from_str(&json).expect("diff JSON must parse");
        let arr = value.as_array().expect("diff JSON must be an array");

        assert_eq!(
            arr.len(),
            cases.len(),
            "output should preserve one JSON entry per input variant"
        );

        for (idx, (entry, expected_type)) in cases.iter().enumerate() {
            assert_eq!(
                entry.stable_variant_name(),
                *expected_type,
                "DiffEntry::stable_variant_name must align with expected type strings"
            );
            assert_eq!(
                arr[idx].get("type"),
                Some(&Value::String((*expected_type).to_string())),
                "entry {idx} should use expected type string"
            );
            assert_ne!(
                arr[idx].get("type"),
                Some(&Value::String("unknown".to_string())),
                "known variants must not serialize as unknown"
            );
        }
    }

    #[test]
    fn diff_json_unknown_entry_uses_variant_name_field() {
        let payload = vec![super::DiffJsonEntry::Unknown {
            variant_name: "future_variant".to_string(),
        }];

        let value = serde_json::to_value(&payload).expect("serialize unknown diff JSON payload");
        let arr = value.as_array().expect("unknown payload must be an array");
        let obj = arr[0]
            .as_object()
            .expect("unknown payload entry must be an object");

        assert_eq!(obj.get("type"), Some(&Value::String("unknown".to_string())));
        assert_eq!(
            obj.get("variant_name"),
            Some(&Value::String("future_variant".to_string())),
            "unknown payload should expose stable variant_name field"
        );
        assert!(
            !obj.contains_key("description"),
            "unknown payload must not expose debug-oriented description field"
        );
    }

    #[test]
    fn list_entries_json_round_trips_with_serde_json() {
        let entries = vec![
            ListEntry {
                path: "a.txt".to_string(),
                is_symlink: false,
                symlink_target: None,
                sha256: "abc".to_string(),
                mode: "644".to_string(),
                size: 3,
            },
            ListEntry {
                path: "link".to_string(),
                is_symlink: true,
                symlink_target: Some("a.txt".to_string()),
                sha256: String::new(),
                mode: "120000".to_string(),
                size: 0,
            },
        ];

        let short_json = list_entries_json(&entries, false);
        let short_value: Value =
            serde_json::from_str(&short_json).expect("short list JSON must parse");
        assert_eq!(short_value[0], Value::String("a.txt".to_string()));

        let long_json = list_entries_json(&entries, true);
        let long_value: Value =
            serde_json::from_str(&long_json).expect("long list JSON must parse");
        assert_eq!(long_value[1]["type"], Value::String("symlink".to_string()));
        assert_eq!(
            long_value[1]["symlink_target"],
            Value::String("a.txt".to_string())
        );
    }

    #[test]
    fn should_diff_against_source_true_for_directory() {
        let dir = TempDir::new().expect("create temp dir");
        assert!(super::should_diff_against_source(dir.path()));
    }

    #[test]
    fn should_diff_against_source_false_for_snapshot_file() {
        let dir = TempDir::new().expect("create temp dir");
        let file = dir.path().join("snap.gcl");
        std::fs::write(&file, "").expect("create file");
        assert!(!super::should_diff_against_source(&file));
    }

    #[test]
    fn summary_json_round_trips_with_serde_json() {
        let summary = SnapshotSummary {
            snapshot_hash: "abc123".to_string(),
            file_count: 3,
            regular_count: 2,
            symlink_count: 1,
            total_bytes: 11,
            git_rev: Some("deadbeef".to_string()),
            git_branch: Some("main".to_string()),
            largest_files: vec![("a.txt".to_string(), 6), ("b.txt".to_string(), 5)],
        };
        let json = summary_json(&summary);
        let value: Value = serde_json::from_str(&json).expect("summary JSON must parse");
        assert_eq!(value["file_count"], Value::from(3));
        assert_eq!(value["largest_files"][0][0], Value::from("a.txt"));
    }
}
