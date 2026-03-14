<<<<<<< HEAD
use std::path::PathBuf;
use std::{io, process};

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, shells};

use git_closure::{
    build_snapshot_with_options, materialize_snapshot, verify_snapshot, BuildOptions,
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
        #[arg(help = "Source directory to snapshot")]
        source: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        include_untracked: bool,
        #[arg(long)]
        require_clean: bool,
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build {
            source,
            output,
            include_untracked,
            require_clean,
        } => {
            let options = BuildOptions {
                include_untracked,
                require_clean,
            };
            build_snapshot_with_options(&source, &output, &options)?;
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
<<<<<<< HEAD
||||||| parent of 8191579 (feat: add deterministic build and materialize commands)
=======
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
>>>>>>> 8191579 (feat: add deterministic build and materialize commands)
||||||| parent of 8d0b8d4 (feat(cli): add verify quiet mode and shell completions)
=======

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
>>>>>>> 8d0b8d4 (feat(cli): add verify quiet mode and shell completions)
