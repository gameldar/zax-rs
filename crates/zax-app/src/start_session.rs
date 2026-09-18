//! Zed's `agent.terminal_init_command`. Resolve context from the cwd, derive a
//! session name, attach if it is already there, otherwise build it detached and
//! then attach.

use std::path::{Path, PathBuf};

use thiserror::Error;
use zax_domain::{format_duration, Command, CommandError, LayoutSpec, PaneKind, SessionName};

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
        let name = SessionName::derive(&repo, &branch);

        if self.mux.session_exists(&name)? {
            self.mux.attach(&name)?;
            return Ok(StartSessionOutcome::Attached(name));
        }

        self.mux
            .create_session(&name, &req.cwd, req.layout.agent())?;

        for (index, pane) in req.layout.panes().iter().enumerate() {
            let command = pane_command(&req.zax_bin, &pane.kind)?;
            // Pane 0 is the agent. The first context pane splits it
            // horizontally; every later one stacks below its predecessor.
            let target_pane = u32::try_from(index).unwrap_or(u32::MAX);
            self.mux.split_pane(
                &name,
                target_pane,
                index == 0,
                pane.size,
                &req.cwd,
                &command,
            )?;
        }

        self.mux.select_pane(&name, 0)?;
        self.mux.attach(&name)?;
        Ok(StartSessionOutcome::Created(name))
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
    fn builds_the_session_detached_then_attaches() {
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        let cwd = PathBuf::from("/Users/me/src/worktrees/acme/OMEGA-1963-stamp");
        // Derived, never hand-written. This test is about the call sequence, not
        // about how a name is built, and `SessionName::derive` appends a
        // fingerprint that a literal here would silently fall out of step with.
        let name = zax_domain::SessionName::derive(
            &git.root_repo_name().unwrap(),
            &git.current_branch().unwrap().unwrap(),
        )
        .as_str()
        .to_owned();

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
        let bare =
            LayoutSpec::new(Command::new(vec!["claude".to_owned()]).unwrap(), vec![]).unwrap();

        StartSession::new(&mux, &git)
            .run(StartSessionRequest {
                layout: bare,
                ..request()
            })
            .unwrap();

        let kinds: Vec<_> = mux
            .calls()
            .into_iter()
            .map(|c| matches!(c, MuxCall::SplitPane { .. }))
            .collect();
        assert_eq!(kinds, vec![false, false, false]);
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
