//! zax: the Zed agent terminal, with context around it.

mod cli;
mod cmd_broker;
mod cmd_config;
mod cmd_doctor;
mod cmd_pane;
mod cmd_session;
mod cmd_tickets;
mod cmd_todo;
mod cmd_worktree;
mod guard;
mod marked;
mod screen;
mod wiring;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Commands};

fn main() {
    if let Err(error) = run() {
        // `{error:#}` prints the whole anyhow chain on one line.
        marked::fail(&format!("{error:#}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Commands::Session { command } => cmd_session::run(command),
        Commands::Worktree { command } => cmd_worktree::run(command),
        Commands::Pane { command } => cmd_pane::run(command),
        Commands::Config { command } => cmd_config::run(command),
        Commands::Doctor => cmd_doctor::run(),
        Commands::Broker => cmd_broker::run(),
    }
}
