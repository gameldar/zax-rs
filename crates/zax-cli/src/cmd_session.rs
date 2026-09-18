//! `zax session start | list | kill`.

use anyhow::{bail, Result};
use zax_app::{Multiplexer, StartSession, StartSessionOutcome, StartSessionRequest};
use zax_domain::{BranchName, SessionName};

use crate::cli::SessionCommand;
use crate::guard::inside_multiplexer_now;
use crate::wiring;

pub fn run(command: SessionCommand) -> Result<()> {
    match command {
        SessionCommand::Start => start(),
        SessionCommand::List => list(),
        SessionCommand::Kill { name } => kill(name),
    }
}

fn start() -> Result<()> {
    let ctx = wiring::resolve()?;
    let tmux = wiring::tmux(&ctx.config)?;

    let outcome = StartSession::new(&tmux, &ctx.git).run(StartSessionRequest {
        cwd: ctx.cwd.clone(),
        zax_bin: wiring::zax_bin()?,
        layout: ctx.config.layout.clone(),
        inside_multiplexer: inside_multiplexer_now(),
    })?;

    // Reached only when the handover did not replace this process.
    match outcome {
        StartSessionOutcome::Attached(name) => println!("attached to {name}"),
        StartSessionOutcome::Created(name) => println!("created {name}"),
    }
    Ok(())
}

fn list() -> Result<()> {
    let ctx = wiring::resolve()?;
    let tmux = wiring::tmux(&ctx.config)?;
    let sessions = tmux.list_sessions()?;
    if sessions.is_empty() {
        println!("no zax sessions");
    }
    for name in sessions {
        println!("{name}");
    }
    Ok(())
}

fn kill(name: Option<String>) -> Result<()> {
    let ctx = wiring::resolve()?;
    let tmux = wiring::tmux(&ctx.config)?;

    let session = match name {
        Some(raw) => SessionName::parse(&raw)?,
        None => {
            use zax_app::GitRepo;
            let branch: BranchName = ctx.git.current_branch()?.ok_or_else(|| {
                anyhow::anyhow!("HEAD is detached, so there is no session name to derive")
            })?;
            SessionName::derive(&ctx.repo, &branch)
        }
    };

    if !tmux.session_exists(&session)? {
        bail!("no such session: {session}");
    }
    tmux.kill_session(&session)?;
    println!("killed {session}");
    Ok(())
}
