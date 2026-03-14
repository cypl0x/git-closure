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
