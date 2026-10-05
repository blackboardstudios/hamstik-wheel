// Copyright 2026 Blackboard Studios LLC
// SPDX-License-Identifier: Apache-2.0

mod activity;
mod config;
mod git;
mod hamstik;
mod logging;
mod loop_engine;
mod pi;
mod process;
mod progress;
mod security;
mod state;
mod validation;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{
    config::Config,
    git::GitRepo,
    logging::{Logger, TimestampMode},
    loop_engine::LoopEngine,
};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "hamstik-wheel")]
#[command(about = "Autonomously work through your Hamstik backlog, one item at a time")]
#[command(version)]
struct Cli {
    /// Annotate every Wheel progress line with a timestamp.
    #[arg(long, value_enum, default_value_t = TimestampsArg::Local, overrides_with = "no_timestamps")]
    timestamps: TimestampsArg,
    /// Disable output timestamps (shorthand for --timestamps none).
    #[arg(long, conflicts_with = "timestamps")]
    no_timestamps: bool,
    /// Append a mirror of Wheel output (timestamps included) to this file.
    #[arg(long, value_name = "PATH")]
    log_file: Option<PathBuf>,
    /// Stream full validation subprocess output instead of only concise progress.
    #[arg(long)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum TimestampsArg {
    /// Local time, the default.
    Local,
    /// UTC time.
    Utc,
    /// No timestamps.
    None,
}

impl From<TimestampsArg> for TimestampMode {
    fn from(value: TimestampsArg) -> Self {
        match value {
            TimestampsArg::Local => TimestampMode::Local,
            TimestampsArg::Utc => TimestampMode::Utc,
            TimestampsArg::None => TimestampMode::None,
        }
    }
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
        /// Maximum number of Work Items to attempt (completed plus skipped).
        #[arg(long)]
        max_items: Option<usize>,
    },
    /// Resume an interrupted active Work Item.
    Resume,
    /// Acknowledge repaired failures and re-arm unattended Work Item selection.
    ResetFailures,
    /// Show persisted orchestration state.
    Status,
    /// Summarize the latest recorded progress for each Work Item from local logs.
    ProgressReport,
}

fn main() {
    if let Err(error) = run() {
        let redactor = security::Redactor::new(&std::env::current_dir().unwrap_or_default());
        eprintln!("error: {}", redactor.text(&format!("{error:#}")));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let timestamps = if cli.no_timestamps {
        TimestampMode::None
    } else {
        cli.timestamps.into()
    };
    let logger = Logger::new(timestamps, cli.log_file.as_deref(), cli.verbose)
        .context("failed to initialize logging")?;
    let result = match cli.command {
        Commands::Init => {
            let repo = GitRepo::discover()?;
            let path = Config::init(repo.root())?;
            logger.info(&format!("Created {}", path.display()));
            logger.info("Edit it, then run `hamstik-wheel doctor`.");
            Ok(())
        }
        Commands::Doctor => LoopEngine::load_with_logger(&logger)?.doctor(),
        Commands::Once => LoopEngine::load_with_logger(&logger)?.once(),
        Commands::Run { max_items } => LoopEngine::load_with_logger(&logger)?.run(max_items),
        Commands::Resume => LoopEngine::load_with_logger(&logger)?.resume(),
        Commands::ResetFailures => LoopEngine::load_with_logger(&logger)?.reset_failures(),
        Commands::Status => LoopEngine::load_with_logger(&logger)?.status(),
        Commands::ProgressReport => progress::report(&GitRepo::discover()?, &logger),
    }
    .context("Hamstik Wheel command failed");
    if let Err(error) = &result {
        // Include terminal failures in --log-file, not only terminal stderr.
        logger.append_raw(&format!("error: {error:#}"));
    }
    result
}
