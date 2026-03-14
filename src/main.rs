use std::path::PathBuf;
use std::{io, process};

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, shells};

use git_closure::{
    build_snapshot_from_source, materialize_snapshot, providers::ProviderKind, verify_snapshot,
    BuildOptions,
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
        #[arg(short, long)]
        output: PathBuf,
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build {
            source,
            output,
            include_untracked,
            require_clean,
            provider,
        } => {
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
        Commands::Completion { shell } => {
            print_completion(shell);
        }
    }

    Ok(())
}
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use git_closure::{build_snapshot, materialize_snapshot, verify_snapshot};

#[derive(Parser, Debug)]
#[command(name = "git-closure")]
#[command(about = "Deterministic S-expression source snapshots")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Build {
        source: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    Materialize {
        snapshot: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
    Verify {
        snapshot: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build { source, output } => {
            build_snapshot(&source, &output)?;
        }
        Commands::Materialize { snapshot, output } => {
            materialize_snapshot(&snapshot, &output)?;
        }
        Commands::Verify { snapshot } => {
            verify_snapshot(&snapshot)?;
        }
    }

    Ok(())
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
