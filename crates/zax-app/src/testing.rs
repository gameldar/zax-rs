//! In-memory ports. A fake records the calls it was asked to make, so a use
//! case test asserts on intent rather than on a subprocess.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use zax_domain::{
    BranchName, BranchStatus, Command, PaneSize, PrRef, PullRequest, RepoSlug, RootRepoName,
    SessionName,
};

use crate::ports::{
    Clock, FileSystem, Forge, GitRepo, Multiplexer, PortError, ProcessRunner, PullRequestHead,
    WorktreeEntry,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuxCall {
    CreateSession {
        name: String,
        cwd: PathBuf,
        argv: Vec<String>,
    },
    SplitPane {
        name: String,
        target_pane: u32,
        horizontal: bool,
        size: Option<u8>,
        cwd: PathBuf,
        argv: Vec<String>,
    },
    SelectPane {
        name: String,
        index: u32,
    },
    Attach {
        name: String,
    },
    KillSession {
        name: String,
    },
}

#[derive(Default)]
pub struct FakeMultiplexer {
    calls: RefCell<Vec<MuxCall>>,
    sessions: RefCell<HashSet<String>>,
    /// `session_exists` answers "no" whatever the session set says.
    blind: bool,
    fail_at: Option<usize>,
    failure: String,
}

impl FakeMultiplexer {
    pub fn with_sessions(names: impl IntoIterator<Item = SessionName>) -> Self {
        Self {
            sessions: RefCell::new(names.into_iter().map(|n| n.as_str().to_owned()).collect()),
            ..Self::default()
        }
    }

    /// Fail the call at `index`, counting every recorded call from zero, the
    /// same way [`FakeRunner::failing_at`] does. The failing call is still
    /// recorded, so a test can assert what was attempted before the failure.
    pub fn failing_at(index: usize, message: &str) -> Self {
        Self {
            fail_at: Some(index),
            failure: message.to_owned(),
            ..Self::default()
        }
    }

    /// Report every session as absent while still holding them. This is the
    /// race the real adapter cannot rule out: `has-session` answered before a
    /// concurrent `new-session` landed, or answered wrongly because tmux
    /// failed. The session set stays populated, so `create_session` still
    /// collides the way tmux does.
    pub fn pretending_no_session_exists(mut self) -> Self {
        self.blind = true;
        self
    }

    pub fn calls(&self) -> Vec<MuxCall> {
        self.calls.borrow().clone()
    }

    /// The sessions the fake is actually holding, regardless of what
    /// `session_exists` is willing to admit to.
    pub fn live_sessions(&self) -> Vec<String> {
        let mut names: Vec<_> = self.sessions.borrow().iter().cloned().collect();
        names.sort();
        names
    }

    /// Call after recording. Borrows and drops in one statement, so it cannot
    /// collide with the `borrow_mut` that recorded the call.
    fn outcome_of_call(&self, index: usize) -> Result<(), PortError> {
        if self.fail_at == Some(index) {
            return Err(PortError::CommandFailed {
                tool: "tmux",
                status: 1,
                stderr: self.failure.clone(),
            });
        }
        Ok(())
    }

    fn record(&self, call: MuxCall) -> Result<(), PortError> {
        let index = self.calls.borrow().len();
        self.calls.borrow_mut().push(call);
        self.outcome_of_call(index)
    }
}

impl Multiplexer for FakeMultiplexer {
    fn session_exists(&self, name: &SessionName) -> Result<bool, PortError> {
        if self.blind {
            return Ok(false);
        }
        Ok(self.sessions.borrow().contains(name.as_str()))
    }

    fn list_sessions(&self) -> Result<Vec<SessionName>, PortError> {
        let mut names: Vec<_> = self.sessions.borrow().iter().cloned().collect();
        names.sort();
        Ok(names
            .iter()
            .filter_map(|n| SessionName::parse(n).ok())
            .collect())
    }

    fn create_session(
        &self,
        name: &SessionName,
        cwd: &Path,
        command: &Command,
    ) -> Result<(), PortError> {
        let call = MuxCall::CreateSession {
            name: name.as_str().to_owned(),
            cwd: cwd.to_owned(),
            argv: command.argv().to_vec(),
        };
        // tmux refuses a duplicate session name rather than replacing the
        // session that is already there, and the difference matters: a caller
        // that cleans up after a failed create would be destroying somebody
        // else's live session. The attempt is still recorded.
        if self.sessions.borrow().contains(name.as_str()) {
            self.calls.borrow_mut().push(call);
            return Err(PortError::CommandFailed {
                tool: "tmux",
                status: 1,
                stderr: format!("duplicate session: {}", name.as_str()),
            });
        }
        self.sessions.borrow_mut().insert(name.as_str().to_owned());
        self.record(call)
    }

    fn split_pane(
        &self,
        name: &SessionName,
        target_pane: u32,
        horizontal: bool,
        size: Option<PaneSize>,
        cwd: &Path,
        command: &Command,
    ) -> Result<(), PortError> {
        self.record(MuxCall::SplitPane {
            name: name.as_str().to_owned(),
            target_pane,
            horizontal,
            size: size.map(PaneSize::get),
            cwd: cwd.to_owned(),
            argv: command.argv().to_vec(),
        })
    }

    fn select_pane(&self, name: &SessionName, index: u32) -> Result<(), PortError> {
        self.record(MuxCall::SelectPane {
            name: name.as_str().to_owned(),
            index,
        })
    }

    fn attach(&self, name: &SessionName) -> Result<(), PortError> {
        self.record(MuxCall::Attach {
            name: name.as_str().to_owned(),
        })
    }

    fn kill_session(&self, name: &SessionName) -> Result<(), PortError> {
        self.sessions.borrow_mut().remove(name.as_str());
        self.record(MuxCall::KillSession {
            name: name.as_str().to_owned(),
        })
    }

    fn probe(&self) -> Result<String, PortError> {
        Ok("fake tmux".to_owned())
    }
}

/// One recorded `add_worktree_*` call: path, branch, base (for a new branch),
/// and whether it created a new branch.
pub type AddedWorktreeCall = (PathBuf, String, Option<String>, bool);

pub struct FakeGitRepo {
    main_worktree: PathBuf,
    branch: Option<String>,
    existing_branches: RefCell<HashSet<String>>,
    pub worktrees: RefCell<Vec<WorktreeEntry>>,
    pub added: RefCell<Vec<AddedWorktreeCall>>,
    pub removed: RefCell<Vec<(PathBuf, bool)>>,
    pub fetched: RefCell<Vec<(String, String)>>,
}

impl FakeGitRepo {
    pub fn new(main_worktree: &str, branch: &str) -> Self {
        Self {
            main_worktree: PathBuf::from(main_worktree),
            branch: Some(branch.to_owned()),
            existing_branches: RefCell::new(HashSet::new()),
            worktrees: RefCell::new(Vec::new()),
            added: RefCell::new(Vec::new()),
            removed: RefCell::new(Vec::new()),
            fetched: RefCell::new(Vec::new()),
        }
    }

    pub fn detached(main_worktree: &str) -> Self {
        let mut repo = Self::new(main_worktree, "unused");
        repo.branch = None;
        repo
    }

    pub fn with_existing_branch(self, branch: &str) -> Self {
        self.existing_branches
            .borrow_mut()
            .insert(branch.to_owned());
        self
    }
}

impl GitRepo for FakeGitRepo {
    fn common_dir(&self) -> Result<PathBuf, PortError> {
        Ok(self.main_worktree.join(".git"))
    }

    fn main_worktree(&self) -> Result<PathBuf, PortError> {
        Ok(self.main_worktree.clone())
    }

    fn root_repo_name(&self) -> Result<RootRepoName, PortError> {
        RootRepoName::from_common_dir(&self.common_dir()?).map_err(|e| PortError::Unparseable {
            tool: "git",
            detail: e.to_string(),
        })
    }

    fn current_branch(&self) -> Result<Option<BranchName>, PortError> {
        match &self.branch {
            None => Ok(None),
            Some(raw) => {
                BranchName::parse_foreign(raw)
                    .map(Some)
                    .map_err(|e| PortError::Unparseable {
                        tool: "git",
                        detail: e.to_string(),
                    })
            }
        }
    }

    fn branch_exists(&self, branch: &BranchName) -> Result<bool, PortError> {
        Ok(self.existing_branches.borrow().contains(branch.as_str()))
    }

    fn add_worktree_new_branch(
        &self,
        path: &Path,
        branch: &BranchName,
        base: Option<&str>,
    ) -> Result<(), PortError> {
        self.added.borrow_mut().push((
            path.to_owned(),
            branch.as_str().to_owned(),
            base.map(str::to_owned),
            true,
        ));
        Ok(())
    }

    fn add_worktree_existing_branch(
        &self,
        path: &Path,
        branch: &BranchName,
    ) -> Result<(), PortError> {
        self.added
            .borrow_mut()
            .push((path.to_owned(), branch.as_str().to_owned(), None, false));
        Ok(())
    }

    fn list_worktrees(&self) -> Result<Vec<WorktreeEntry>, PortError> {
        Ok(self.worktrees.borrow().clone())
    }

    fn remove_worktree(&self, path: &Path, force: bool) -> Result<(), PortError> {
        self.removed.borrow_mut().push((path.to_owned(), force));
        Ok(())
    }

    fn fetch(&self, remote: &str, refspec: &str) -> Result<(), PortError> {
        self.fetched
            .borrow_mut()
            .push((remote.to_owned(), refspec.to_owned()));
        Ok(())
    }

    fn probe(&self) -> Result<String, PortError> {
        Ok("fake git".to_owned())
    }
}

#[derive(Default)]
pub struct FakeForge {
    pub review_requests: Vec<PullRequest>,
    pub branch_status: Option<BranchStatus>,
    pub head: Option<PullRequestHead>,
    pub current_repo: Option<RepoSlug>,
    pub fail_with: Option<PortError>,
}

impl Forge for FakeForge {
    fn review_requests(&self) -> Result<Vec<PullRequest>, PortError> {
        match &self.fail_with {
            Some(e) => Err(e.clone()),
            None => Ok(self.review_requests.clone()),
        }
    }

    fn branch_status(&self, _branch: &BranchName) -> Result<Option<BranchStatus>, PortError> {
        match &self.fail_with {
            Some(e) => Err(e.clone()),
            None => Ok(self.branch_status.clone()),
        }
    }

    fn current_repo(&self) -> Result<RepoSlug, PortError> {
        if let Some(e) = &self.fail_with {
            return Err(e.clone());
        }
        self.current_repo
            .clone()
            .ok_or_else(|| PortError::Unparseable {
                tool: "gh",
                detail: "no current repository fixture".to_owned(),
            })
    }

    fn pull_request_head(&self, pr: &PrRef) -> Result<PullRequestHead, PortError> {
        if let Some(e) = &self.fail_with {
            return Err(e.clone());
        }
        self.head.clone().ok_or_else(|| PortError::Unparseable {
            tool: "gh",
            detail: format!("no fixture for {}", pr.as_gh_arg()),
        })
    }

    fn auth_status(&self) -> Result<String, PortError> {
        match &self.fail_with {
            Some(e) => Err(e.clone()),
            None => Ok("fake gh auth".to_owned()),
        }
    }

    fn probe(&self) -> Result<String, PortError> {
        match &self.fail_with {
            Some(e) => Err(e.clone()),
            None => Ok("fake gh".to_owned()),
        }
    }
}

impl FakeForge {
    pub fn with_review_requests(prs: Vec<PullRequest>) -> Self {
        Self {
            review_requests: prs,
            ..Self::default()
        }
    }

    pub fn with_head(head: PullRequestHead) -> Self {
        Self {
            head: Some(head),
            ..Self::default()
        }
    }

    /// Which repository the forge thinks the current directory is in.
    pub fn in_repository(mut self, slug: &str) -> Self {
        self.current_repo = Some(
            RepoSlug::parse(slug).unwrap_or_else(|_| panic!("`{slug}` is not a repository")),
        );
        self
    }

    pub fn failing(error: PortError) -> Self {
        Self {
            fail_with: Some(error),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCall {
    pub cwd: PathBuf,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Default)]
pub struct FakeRunner {
    calls: RefCell<Vec<RunCall>>,
    fail_at: Option<usize>,
    failure: String,
}

impl FakeRunner {
    /// Fail on the call at `index`, counting from zero.
    pub fn failing_at(index: usize, message: &str) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            fail_at: Some(index),
            failure: message.to_owned(),
        }
    }

    pub fn calls(&self) -> Vec<RunCall> {
        self.calls.borrow().clone()
    }
}

impl ProcessRunner for FakeRunner {
    fn run(
        &self,
        cwd: &Path,
        command: &Command,
        env: &[(String, String)],
    ) -> Result<String, PortError> {
        let index = self.calls.borrow().len();
        self.calls.borrow_mut().push(RunCall {
            cwd: cwd.to_owned(),
            argv: command.argv().to_vec(),
            env: env.to_vec(),
        });
        if self.fail_at == Some(index) {
            return Err(PortError::CommandFailed {
                tool: "hook",
                status: 1,
                stderr: self.failure.clone(),
            });
        }
        Ok(String::new())
    }
}

#[derive(Default)]
pub struct FakeFs {
    files: RefCell<HashSet<PathBuf>>,
    dirs: RefCell<HashSet<PathBuf>>,
    copies: RefCell<Vec<(PathBuf, PathBuf)>>,
}

impl FakeFs {
    pub fn with_existing<'a>(paths: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            files: RefCell::new(paths.into_iter().map(PathBuf::from).collect()),
            dirs: RefCell::new(HashSet::new()),
            copies: RefCell::new(Vec::new()),
        }
    }

    pub fn copies(&self) -> Vec<(PathBuf, PathBuf)> {
        self.copies.borrow().clone()
    }

    pub fn created_dirs(&self) -> Vec<PathBuf> {
        self.dirs.borrow().iter().cloned().collect()
    }
}

impl FileSystem for FakeFs {
    fn exists(&self, path: &Path) -> bool {
        self.files.borrow().contains(path) || self.dirs.borrow().contains(path)
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), PortError> {
        self.dirs.borrow_mut().insert(path.to_owned());
        Ok(())
    }

    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), PortError> {
        if !self.files.borrow().contains(from) {
            return Err(PortError::Io {
                operation: "copy",
                path: from.to_owned(),
                detail: "no such file".to_owned(),
            });
        }
        self.files.borrow_mut().insert(to.to_owned());
        self.copies
            .borrow_mut()
            .push((from.to_owned(), to.to_owned()));
        Ok(())
    }
}

pub struct FixedClock(SystemTime);

impl FixedClock {
    pub fn at_unix_seconds(secs: u64) -> Self {
        Self(UNIX_EPOCH + Duration::from_secs(secs))
    }
}

impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        self.0
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::{Path, PathBuf};

    use zax_domain::{BranchName, Command, RootRepoName, SessionName};

    use super::*;

    #[test]
    fn the_multiplexer_fake_records_the_calls_it_was_asked_to_make() {
        let mux = FakeMultiplexer::default();
        let repo = RootRepoName::parse("acme").unwrap();
        let branch = BranchName::parse_local("topic").unwrap();
        let name = SessionName::derive(&repo, &branch);
        let cmd = Command::new(vec!["claude".to_owned()]).unwrap();

        assert!(!mux.session_exists(&name).unwrap());
        mux.create_session(&name, Path::new("/w"), &cmd).unwrap();
        assert!(mux.session_exists(&name).unwrap());
        mux.attach(&name).unwrap();

        assert_eq!(
            mux.calls(),
            vec![
                MuxCall::CreateSession {
                    name: name.as_str().to_owned(),
                    cwd: PathBuf::from("/w"),
                    argv: vec!["claude".to_owned()],
                },
                MuxCall::Attach {
                    name: name.as_str().to_owned()
                },
            ]
        );
    }

    #[test]
    fn the_multiplexer_fake_can_be_told_a_session_already_exists() {
        let repo = RootRepoName::parse("acme").unwrap();
        let branch = BranchName::parse_local("topic").unwrap();
        let name = SessionName::derive(&repo, &branch);
        let mux = FakeMultiplexer::with_sessions([name.clone()]);
        assert!(mux.session_exists(&name).unwrap());
    }

    #[test]
    fn the_git_fake_serves_a_configured_repo_and_branch() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        assert_eq!(git.root_repo_name().unwrap().as_str(), "acme");
        assert_eq!(
            git.current_branch().unwrap().unwrap().as_str(),
            "OMEGA-1963-stamp"
        );
        assert_eq!(
            git.main_worktree().unwrap(),
            PathBuf::from("/Users/me/src/acme")
        );
    }

    #[test]
    fn the_git_fake_can_report_a_detached_head() {
        let git = FakeGitRepo::detached("/Users/me/src/acme");
        assert_eq!(git.current_branch().unwrap(), None);
    }

    #[test]
    fn the_runner_fake_records_cwd_argv_and_env() {
        let runner = FakeRunner::default();
        let cmd = Command::shell("direnv allow").unwrap();
        runner
            .run(
                Path::new("/w"),
                &cmd,
                &[("ZAX_BRANCH".to_owned(), "topic".to_owned())],
            )
            .unwrap();
        assert_eq!(
            runner.calls(),
            vec![RunCall {
                cwd: PathBuf::from("/w"),
                argv: vec!["sh".to_owned(), "-c".to_owned(), "direnv allow".to_owned()],
                env: vec![("ZAX_BRANCH".to_owned(), "topic".to_owned())],
            }]
        );
    }

    #[test]
    fn the_runner_fake_can_be_told_to_fail_at_a_given_call() {
        let runner = FakeRunner::failing_at(1, "hook blew up");
        let cmd = Command::shell("true").unwrap();
        assert!(runner.run(Path::new("/w"), &cmd, &[]).is_ok());
        assert!(runner.run(Path::new("/w"), &cmd, &[]).is_err());
    }

    #[test]
    fn the_filesystem_fake_tracks_existence_and_copies() {
        let fs = FakeFs::with_existing(["/Users/me/src/acme/.env"]);
        assert!(fs.exists(Path::new("/Users/me/src/acme/.env")));
        assert!(!fs.exists(Path::new("/Users/me/src/acme/.envrc")));
        fs.copy_file(
            Path::new("/Users/me/src/acme/.env"),
            Path::new("/w/.env"),
        )
        .unwrap();
        assert!(fs.exists(Path::new("/w/.env")));
        assert_eq!(fs.copies().len(), 1);
    }

    #[test]
    fn the_clock_fake_does_not_move() {
        let clock = FixedClock::at_unix_seconds(1_700_000_000);
        assert_eq!(clock.now(), clock.now());
    }
}
