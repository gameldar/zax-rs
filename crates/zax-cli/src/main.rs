//! zax: the Zed agent terminal, with context around it.

mod cli;
mod cmd_doctor;
mod cmd_pane;
mod cmd_session;
mod cmd_worktree;
mod guard;
mod wiring;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Commands};

fn main() {
    if let Err(error) = run() {
        // `{error:#}` prints the whole anyhow chain on one line.
        eprintln!("zax: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Commands::Session { command } => cmd_session::run(command),
        Commands::Worktree { command } => cmd_worktree::run(command),
        Commands::Pane { command } => cmd_pane::run(command),
        Commands::Doctor => cmd_doctor::run(),
    }
}
