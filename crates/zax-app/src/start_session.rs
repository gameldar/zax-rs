//! Zed's `agent.terminal_init_command`. Resolve context from the cwd, pick one
//! of the worktree's sessions, attach if it is already there, otherwise build
//! it detached and then attach.

use std::path::{Path, PathBuf};

use thiserror::Error;
use zax_domain::{
    choose_session, format_duration, Command, CommandError, LayoutSpec, PaneKind, SessionChoice,
    SessionName, SessionSlot,
};

use crate::ports::{GitRepo, Multiplexer, PortError};

pub struct StartSession<'a, M, G> {
    mux: &'a M,
    git: &'a G,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartSessionRequest {
    /// Where the session's panes should start. Normally the worktree we are in.
    pub cwd: PathBuf,
    /// The absolute path of the running `zax`, so built-in panes re-invoke this
    /// build rather than whatever is first on PATH inside tmux.
    pub zax_bin: PathBuf,
    pub layout: LayoutSpec,
    /// `$TMUX` was set.
    pub inside_multiplexer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartSessionOutcome {
    Attached(SessionName),
    Created(SessionName),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StartSessionError {
    #[error(
        "already inside a terminal multiplexer ($TMUX is set); zax will not nest \
         one inside another"
    )]
    InsideMultiplexer,
    #[error("HEAD is detached; check out a branch before starting a zax session")]
    DetachedHead,
    #[error(
        "all {} zax sessions for this worktree are attached; close one, or free \
         one with `zax session kill <name>`",
        SessionSlot::MAX
    )]
    AllSlotsInUse,
    #[error("{0}")]
    Domain(String),
    #[error(transparent)]
    Port(#[from] PortError),
}

impl From<CommandError> for StartSessionError {
    fn from(e: CommandError) -> Self {
        Self::Domain(e.to_string())
    }
}

impl<'a, M: Multiplexer, G: GitRepo> StartSession<'a, M, G> {
    pub fn new(mux: &'a M, git: &'a G) -> Self {
        Self { mux, git }
    }

    pub fn run(&self, req: StartSessionRequest) -> Result<StartSessionOutcome, StartSessionError> {
        if req.inside_multiplexer {
            return Err(StartSessionError::InsideMultiplexer);
        }

        let repo = self.git.root_repo_name()?;
        let branch = self
            .git
            .current_branch()?
            .ok_or(StartSessionError::DetachedHead)?;
        // A detached session is an agent someone left and wants back. An
        // attached one belongs to another agent thread in this worktree, which
        // wants its own agent rather than a mirror of that one.
        let name = match choose_session(&repo, &branch, &self.mux.list_sessions()?) {
            SessionChoice::Reattach(name) => {
                self.mux.attach(&name)?;
                return Ok(StartSessionOutcome::Attached(name));
            }
            SessionChoice::Create(name) => name,
            SessionChoice::Full => return Err(StartSessionError::AllSlotsInUse),
        };

        // The creation is deliberately outside the teardown below. The choice
        // above is not a lock: two `zax session start` for one worktree, which
        // Zed produces by opening two agent terminals, can both pick the same
        // free slot and the loser's `create_session` then collides with the
        // winner's live agent. Killing the name on that failure would
        // destroy a running session and its scrollback. Nothing is torn down
        // unless this call is the one that created it.
        self.mux
            .create_session(&name, &req.cwd, req.layout.agent())?;

        // A session that is only half built must not survive. It would be
        // listed, detached, on the next start, so a later invocation would
        // take the reattach branch above and silently hand the user a session
        // missing its panes, with no error and no way to finish it.
        if let Err(error) = self.build_panes(&name, &req) {
            // Best effort, and its own failure is deliberately discarded: the
            // session was created moments ago in this same call and holds
            // nothing the user can have touched, so reporting a teardown
            // problem would only bury the error that actually matters.
            let _ = self.mux.kill_session(&name);
            return Err(error);
        }

        // Past this point the layout is complete and the session is usable, so
        // a failure is reported without tearing it down. Attaching is the step
        // most likely to fail for reasons that say nothing about the session -
        // no controlling terminal, most often - and the user can still reach it
        // with a later `zax session start`.
        self.mux.select_pane(&name, 0)?;
        self.mux.attach(&name)?;
        Ok(StartSessionOutcome::Created(name))
    }

    /// Everything around the agent pane, in a session that already exists.
    fn build_panes(
        &self,
        name: &SessionName,
        req: &StartSessionRequest,
    ) -> Result<(), StartSessionError> {
        for (index, pane) in req.layout.panes().iter().enumerate() {
            let command = pane_command(&req.zax_bin, &pane.kind)?;
            // Pane 0 is the agent. The first context pane splits it
            // horizontally; every later one stacks below its predecessor.
            let target_pane = u32::try_from(index).unwrap_or(u32::MAX);
            self.mux
                .split_pane(name, target_pane, index == 0, pane.size, &req.cwd, &command)?;
        }

        Ok(())
    }
}

/// The command that actually runs in a context pane.
pub fn pane_command(zax_bin: &Path, kind: &PaneKind) -> Result<Command, CommandError> {
    let zax = zax_bin.to_string_lossy().into_owned();
    match kind {
        PaneKind::Builtin(builtin) => {
            Command::new(vec![zax, "pane".to_owned(), builtin.as_str().to_owned()])
        }
        PaneKind::Exec {
            title,
            every,
            command,
        } => {
            let mut argv = vec![
                zax,
                "pane".to_owned(),
                "exec".to_owned(),
                "--title".to_owned(),
                title.clone(),
                "--every".to_owned(),
                format_duration(*every),
                "--".to_owned(),
            ];
            argv.extend(command.argv().iter().cloned());
            Command::new(argv)
        }
        PaneKind::Raw { command } => Ok(command.clone()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::PathBuf;
    use std::time::Duration;

    use zax_domain::{BuiltinPane, Command, LayoutSpec, PaneKind, PaneSize, PaneSpec};

    use super::*;
    use crate::testing::{FakeGitRepo, FakeMultiplexer, MuxCall};

    fn layout() -> LayoutSpec {
        LayoutSpec::new(
            Command::new(vec!["claude".to_owned()]).unwrap(),
            vec![
                PaneSpec {
                    kind: PaneKind::Builtin(BuiltinPane::PrsForReview),
                    size: Some(PaneSize::percent(30).unwrap()),
                },
                PaneSpec {
                    kind: PaneKind::Exec {
                        title: "Deploys".to_owned(),
                        every: Duration::from_secs(60),
                        command: Command::new(vec![
                            "kubectl".to_owned(),
                            "get".to_owned(),
                            "pods".to_owned(),
                        ])
                        .unwrap(),
                    },
                    size: Some(PaneSize::percent(25).unwrap()),
                },
                PaneSpec {
                    kind: PaneKind::Raw {
                        command: Command::new(vec!["lazygit".to_owned()]).unwrap(),
                    },
                    size: None,
                },
            ],
        )
        .unwrap()
    }

    /// The session name these fakes derive. Never spelled out: this file tests
    /// the call sequence, not how a name is built.
    fn session_name(git: &FakeGitRepo) -> String {
        zax_domain::SessionName::derive(
            &git.root_repo_name().unwrap(),
            &git.current_branch().unwrap().unwrap(),
        )
        .as_str()
        .to_owned()
    }

    fn request() -> StartSessionRequest {
        StartSessionRequest {
            cwd: PathBuf::from("/Users/me/src/worktrees/acme/OMEGA-1963-stamp"),
            zax_bin: PathBuf::from("/nix/store/abc/bin/zax"),
            layout: layout(),
            inside_multiplexer: false,
        }
    }

    #[test]
    fn refuses_to_nest_inside_an_existing_multiplexer() {
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        let req = StartSessionRequest {
            inside_multiplexer: true,
            ..request()
        };

        assert_eq!(
            StartSession::new(&mux, &git).run(req),
            Err(StartSessionError::InsideMultiplexer)
        );
        assert!(mux.calls().is_empty(), "nothing should have been attempted");
    }

    #[test]
    fn refuses_a_detached_head() {
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::detached("/Users/me/src/acme");
        assert_eq!(
            StartSession::new(&mux, &git).run(request()),
            Err(StartSessionError::DetachedHead)
        );
    }

    #[test]
    fn attaches_to_an_existing_session_without_rebuilding_it() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        let name = zax_domain::SessionName::derive(
            &git.root_repo_name().unwrap(),
            &git.current_branch().unwrap().unwrap(),
        );
        let mux = FakeMultiplexer::with_sessions([name.clone()]);

        let outcome = StartSession::new(&mux, &git).run(request()).unwrap();

        assert_eq!(outcome, StartSessionOutcome::Attached(name.clone()));
        assert_eq!(
            mux.calls(),
            vec![MuxCall::Attach {
                name: name.as_str().to_owned()
            }]
        );
    }

    #[test]
    fn a_second_agent_thread_in_the_worktree_gets_its_own_session() {
        // Attaching to a session somebody is already looking at would mirror
        // their agent rather than start a new one.
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        let repo = git.root_repo_name().unwrap();
        let branch = git.current_branch().unwrap().unwrap();
        let first = zax_domain::SessionName::derive(&repo, &branch);
        let second = zax_domain::SessionName::derive_slot(
            &repo,
            &branch,
            zax_domain::SessionSlot::new(2).unwrap(),
        );
        let mux = FakeMultiplexer::with_attached_sessions([first]);

        let outcome = StartSession::new(&mux, &git).run(request()).unwrap();

        assert_eq!(outcome, StartSessionOutcome::Created(second.clone()));
        assert!(
            matches!(
                mux.calls().first(),
                Some(MuxCall::CreateSession { name, .. }) if name == second.as_str()
            ),
            "got: {:?}",
            mux.calls()
        );
        assert_eq!(
            mux.calls().last(),
            Some(&MuxCall::Attach {
                name: second.as_str().to_owned()
            })
        );
    }

    #[test]
    fn a_worktree_with_every_slot_in_use_is_refused() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        let repo = git.root_repo_name().unwrap();
        let branch = git.current_branch().unwrap().unwrap();
        let mux = FakeMultiplexer::with_attached_sessions(
            zax_domain::SessionSlot::all()
                .map(|slot| zax_domain::SessionName::derive_slot(&repo, &branch, slot)),
        );

        assert_eq!(
            StartSession::new(&mux, &git).run(request()),
            Err(StartSessionError::AllSlotsInUse)
        );
        assert!(mux.calls().is_empty(), "nothing should have been attempted");
    }

    #[test]
    fn builds_the_session_detached_then_attaches() {
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        let cwd = PathBuf::from("/Users/me/src/worktrees/acme/OMEGA-1963-stamp");
        let name = session_name(&git);

        let outcome = StartSession::new(&mux, &git).run(request()).unwrap();
        assert!(matches!(outcome, StartSessionOutcome::Created(_)));

        assert_eq!(
            mux.calls(),
            vec![
                MuxCall::CreateSession {
                    name: name.clone(),
                    cwd: cwd.clone(),
                    argv: vec!["claude".to_owned()],
                },
                // The agent keeps the left side: the first split is horizontal.
                MuxCall::SplitPane {
                    name: name.clone(),
                    target_pane: 0,
                    horizontal: true,
                    size: Some(30),
                    cwd: cwd.clone(),
                    argv: vec![
                        "/nix/store/abc/bin/zax".to_owned(),
                        "pane".to_owned(),
                        "prs-for-review".to_owned(),
                    ],
                },
                // Later panes stack down the right.
                MuxCall::SplitPane {
                    name: name.clone(),
                    target_pane: 1,
                    horizontal: false,
                    size: Some(25),
                    cwd: cwd.clone(),
                    argv: vec![
                        "/nix/store/abc/bin/zax".to_owned(),
                        "pane".to_owned(),
                        "exec".to_owned(),
                        "--title".to_owned(),
                        "Deploys".to_owned(),
                        "--every".to_owned(),
                        "1m".to_owned(),
                        "--".to_owned(),
                        "kubectl".to_owned(),
                        "get".to_owned(),
                        "pods".to_owned(),
                    ],
                },
                MuxCall::SplitPane {
                    name: name.clone(),
                    target_pane: 2,
                    horizontal: false,
                    size: None,
                    cwd: cwd.clone(),
                    argv: vec!["lazygit".to_owned()],
                },
                // Focus lands on the agent, not on the last pane created.
                MuxCall::SelectPane {
                    name: name.clone(),
                    index: 0
                },
                MuxCall::Attach { name },
            ]
        );
    }

    #[test]
    fn a_layout_with_no_context_panes_still_creates_and_attaches() {
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        let cwd = PathBuf::from("/Users/me/src/worktrees/acme/OMEGA-1963-stamp");
        let bare =
            LayoutSpec::new(Command::new(vec!["claude".to_owned()]).unwrap(), vec![]).unwrap();
        let name = session_name(&git);

        let outcome = StartSession::new(&mux, &git)
            .run(StartSessionRequest {
                layout: bare,
                ..request()
            })
            .unwrap();

        // Assert the whole sequence, not just the absence of splits: reducing
        // it to "did any split happen" cannot tell a wrong select index, a
        // swapped select and attach, or a wrong cwd from a correct run.
        assert_eq!(
            outcome,
            StartSessionOutcome::Created(zax_domain::SessionName::parse(&name).unwrap())
        );
        assert_eq!(
            mux.calls(),
            vec![
                MuxCall::CreateSession {
                    name: name.clone(),
                    cwd,
                    argv: vec!["claude".to_owned()],
                },
                MuxCall::SelectPane {
                    name: name.clone(),
                    index: 0
                },
                MuxCall::Attach { name },
            ]
        );
    }

    #[test]
    fn a_session_that_cannot_be_finished_is_torn_down() {
        // Call 0 creates the session, call 1 is the first split. Fail the
        // second split, leaving a session with one context pane of three.
        let mux = FakeMultiplexer::failing_at(2, "no space for a new pane");
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        let name = session_name(&git);

        let error = StartSession::new(&mux, &git).run(request()).unwrap_err();
        assert!(
            matches!(error, StartSessionError::Port(_)),
            "got: {error:?}"
        );

        assert!(
            matches!(mux.calls().last(), Some(MuxCall::KillSession { .. })),
            "the half-built session must be torn down: {:?}",
            mux.calls()
        );
        assert!(
            !mux.session_exists(&zax_domain::SessionName::parse(&name).unwrap())
                .unwrap(),
            "a surviving half-built session would be attached to forever after"
        );
    }

    #[test]
    fn a_session_this_call_did_not_create_is_never_torn_down() {
        // The race Zed can produce: two `zax session start` for one worktree.
        // Both ask whether the session exists before either has created it, so
        // both are told no, and the loser's `create_session` collides with the
        // winner's live agent session. Tearing down after that failure destroys
        // a running agent and its scrollback.
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        let name = session_name(&git);
        let live = zax_domain::SessionName::parse(&name).unwrap();
        let mux = FakeMultiplexer::with_sessions([live.clone()]).pretending_no_session_exists();

        let error = StartSession::new(&mux, &git).run(request()).unwrap_err();
        assert!(
            error.to_string().contains("duplicate session"),
            "the collision itself is what the user must be told: {error:?}"
        );

        assert!(
            !mux.calls()
                .iter()
                .any(|c| matches!(c, MuxCall::KillSession { .. })),
            "nothing was created here, so nothing may be destroyed: {:?}",
            mux.calls()
        );
        assert_eq!(
            mux.live_sessions(),
            vec![name],
            "the session that was already running must survive"
        );
    }

    #[test]
    fn a_complete_session_survives_a_failure_to_attach() {
        // Six calls: create, three splits, select, attach. Fail the attach.
        let mux = FakeMultiplexer::failing_at(5, "open terminal failed: not a terminal");
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        let name = session_name(&git);

        let error = StartSession::new(&mux, &git).run(request()).unwrap_err();
        assert!(
            matches!(error, StartSessionError::Port(_)),
            "got: {error:?}"
        );

        assert!(
            !mux.calls()
                .iter()
                .any(|c| matches!(c, MuxCall::KillSession { .. })),
            "a fully built session must not be destroyed over a failed attach"
        );
        assert!(
            mux.session_exists(&zax_domain::SessionName::parse(&name).unwrap())
                .unwrap(),
            "the session is complete and the user can still reach it"
        );
    }

    #[test]
    fn a_builtin_pane_becomes_a_zax_pane_subcommand() {
        let cmd = pane_command(
            std::path::Path::new("/bin/zax"),
            &PaneKind::Builtin(BuiltinPane::BranchStatus),
        )
        .unwrap();
        assert_eq!(cmd.argv(), ["/bin/zax", "pane", "branch-status"]);
    }
}
