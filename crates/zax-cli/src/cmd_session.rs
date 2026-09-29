//! `zax session start | list | kill`.

use anyhow::{bail, Result};
use zax_app::{Multiplexer, StartSession, StartSessionOutcome, StartSessionRequest};
use zax_domain::{worktree_sessions, BranchName, SessionName};

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
    for session in sessions {
        if session.attached {
            println!("{} (attached)", session.name);
        } else {
            println!("{}", session.name);
        }
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
            only_session(worktree_sessions(
                &ctx.repo,
                &branch,
                &tmux.list_sessions()?,
            ))?
        }
    };

    if !tmux.session_exists(&session)? {
        bail!("no such session: {session}");
    }
    tmux.kill_session(&session)?;
    println!("killed {session}");
    Ok(())
}

/// `zax session kill` with no name. A worktree running several agents has no
/// single session to default to, and guessing would end the wrong agent.
fn only_session(mut sessions: Vec<SessionName>) -> Result<SessionName> {
    match sessions.len() {
        0 => bail!("this worktree has no zax session"),
        1 => Ok(sessions.remove(0)),
        _ => {
            let names: Vec<_> = sessions.iter().map(SessionName::as_str).collect();
            bail!(
                "this worktree has {} zax sessions; name the one to kill:\n  {}",
                names.len(),
                names.join("\n  ")
            )
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn name(raw: &str) -> SessionName {
        SessionName::parse(raw).unwrap()
    }

    #[test]
    fn a_worktree_with_one_session_defaults_to_it() {
        assert_eq!(only_session(vec![name("zax-a")]).unwrap(), name("zax-a"));
    }

    #[test]
    fn a_worktree_with_no_session_has_nothing_to_kill() {
        let err = only_session(vec![]).unwrap_err().to_string();
        assert!(err.contains("no zax session"), "got: {err}");
    }

    #[test]
    fn a_worktree_with_several_sessions_makes_the_user_choose() {
        let err = only_session(vec![name("zax-a"), name("zax-a-2")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("2 zax sessions"), "got: {err}");
        assert!(
            err.contains("zax-a\n") && err.contains("zax-a-2"),
            "got: {err}"
        );
    }
}
