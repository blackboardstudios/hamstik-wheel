// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

mod config;
mod git;
mod hamstik;
mod loop_engine;
mod pi;
mod state;
mod validation;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{config::Config, git::GitRepo, loop_engine::LoopEngine};

#[derive(Debug, Parser)]
#[command(name = "hamstik-wheel")]
#[command(about = "Autonomously work through your Hamstik backlog, one item at a time")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create .hamstik-wheel.toml in the current Git repository.
    Init,
    /// Verify Git, Pi, Hamstik CLI, context, configuration, and repository readiness.
    Doctor,
    /// Process exactly one Work Item, resuming an active item first when needed.
    Once,
    /// Process Work Items sequentially until the configured/requested limit or no work remains.
    Run {
        /// Maximum number of Work Items to complete in this invocation.
        #[arg(long)]
        max_items: Option<usize>,
    },
    /// Resume an interrupted active Work Item.
    Resume,
    /// Show persisted orchestration state.
    Status,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init => {
            let repo = GitRepo::discover()?;
            let path = Config::init(repo.root())?;
            println!("Created {}", path.display());
            println!("Edit it, then run `hamstik-wheel doctor`.");
            Ok(())
        }
        Commands::Doctor => LoopEngine::load()?.doctor(),
        Commands::Once => LoopEngine::load()?.once(),
        Commands::Run { max_items } => LoopEngine::load()?.run(max_items),
        Commands::Resume => LoopEngine::load()?.resume(),
        Commands::Status => LoopEngine::load()?.status(),
    }
    .context("Hamstik Wheel command failed")
}
