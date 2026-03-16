use std::path::{Path, PathBuf};
use std::{io, process};

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, shells};

use git_closure::{
    build_snapshot_from_source, fmt_snapshot, list_snapshot, materialize_snapshot,
    providers::ProviderKind, verify_snapshot, BuildOptions, GitClosureError, ListEntry,
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
            help = "Output file (default: <source-basename>.gcl or snapshot.gcl)"
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
    #[command(about = "Canonically reformat a snapshot file", visible_alias = "f")]
    Fmt {
        #[arg(help = "Snapshot file to format")]
        snapshot: PathBuf,
        #[arg(
            long,
            help = "Check whether the snapshot is already canonical; exit non-zero if not"
        )]
        check: bool,
    },
    #[command(about = "Generate shell completion scripts", visible_alias = "c")]
    Completion {
        #[arg(help = "Shell to generate completions for")]
        shell: CompletionShell,
    },
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
        process::exit(1);
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
            let output = output.unwrap_or_else(|| derive_output_path(&source));
            let options = BuildOptions {
                include_untracked,
                require_clean,
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
        Commands::Fmt { snapshot, check } => {
            let canonical = fmt_snapshot(&snapshot)?;
            if check {
                let current = std::fs::read_to_string(&snapshot).map_err(GitClosureError::from)?;
                if current != canonical {
                    eprintln!(
                        "error: {} is not in canonical format (run `git-closure fmt` to fix)",
                        snapshot.display()
                    );
                    process::exit(2);
                }
            } else {
                std::fs::write(&snapshot, canonical.as_bytes()).map_err(GitClosureError::from)?;
            }
        }
        Commands::Completion { shell } => {
            print_completion(shell);
        }
    }

    Ok(())
}

// ── Output helpers ────────────────────────────────────────────────────────────

fn print_list(entries: &[ListEntry], json: bool, long: bool) {
    if json {
        if long {
            println!("[");
            for (i, e) in entries.iter().enumerate() {
                let comma = if i + 1 < entries.len() { "," } else { "" };
                let entry_type = if e.is_symlink { "symlink" } else { "file" };
                println!(
                    "  {{\"path\":{},\"type\":{},\"size\":{},\"mode\":{},\"sha256\":{},\"symlink_target\":{}}}{}",
                    json_string(&e.path),
                    json_string(entry_type),
                    e.size,
                    json_string(&e.mode),
                    json_string(&e.sha256),
                    match &e.symlink_target {
                        Some(t) => json_string(t),
                        None => "null".to_string(),
                    },
                    comma
                );
            }
            println!("]");
        } else {
            println!("[");
            for (i, e) in entries.iter().enumerate() {
                let comma = if i + 1 < entries.len() { "," } else { "" };
                println!("  {}{}", json_string(&e.path), comma);
            }
            println!("]");
        }
    } else if long {
        for e in entries {
            let entry_type = if e.is_symlink { "symlink" } else { "file" };
            let detail = if e.is_symlink {
                format!("-> {}", e.symlink_target.as_deref().unwrap_or(""))
            } else {
                format!("{}  {}  {}", e.mode, e.size, &e.sha256[..16])
            };
            println!("{}\t{}\t{}", e.path, entry_type, detail);
        }
    } else {
        for e in entries {
            println!("{}", e.path);
        }
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── Output filename derivation (T-31) ─────────────────────────────────────────

/// Derives a default output path for the `build` command when `--output` is
/// not supplied.
///
/// Rules (first match wins):
/// 1. `gh:owner/repo[@ref]` or `gl:owner/repo[@ref]` → `<repo>.gcl`
/// 2. Local filesystem path → `<basename>.gcl` (skipping `.` / `..`)
/// 3. Fallback → `snapshot.gcl`
pub(crate) fn derive_output_path(source: &str) -> PathBuf {
    // Handle gh:/gl: shorthand — strip prefix, strip optional @ref, take last segment.
    if let Some(rest) = source
        .strip_prefix("gh:")
        .or_else(|| source.strip_prefix("gl:"))
    {
        let repo = rest.rsplit_once('@').map(|(r, _)| r).unwrap_or(rest);
        let name = repo.rsplit_once('/').map(|(_, n)| n).unwrap_or(repo);
        if !name.is_empty() {
            return PathBuf::from(format!("{name}.gcl"));
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
    use super::derive_output_path;
    use std::path::PathBuf;

    #[test]
    fn derive_output_path_gh_shorthand() {
        assert_eq!(
            derive_output_path("gh:owner/repo"),
            PathBuf::from("repo.gcl")
        );
        assert_eq!(
            derive_output_path("gh:owner/repo@main"),
            PathBuf::from("repo.gcl")
        );
    }

    #[test]
    fn derive_output_path_gl_shorthand() {
        assert_eq!(
            derive_output_path("gl:group/project@v1.2"),
            PathBuf::from("project.gcl")
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
}
