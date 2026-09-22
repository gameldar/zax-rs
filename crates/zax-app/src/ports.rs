//! The boundary. Every outside world a use case can touch, as a trait.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use thiserror::Error;
use zax_domain::{
    BranchName, BranchStatus, Command, PaneSize, PrFilter, PrRef, PullRequest, RepoSlug,
    RootRepoName, SessionName,
};

/// Every way the outside world can fail, in terms a pane can render.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PortError {
    #[error("{tool} is not installed or not on PATH")]
    MissingTool { tool: &'static str },
    #[error("not authenticated with {tool}: {detail}")]
    NotAuthenticated { tool: &'static str, detail: String },
    #[error("{tool} exited with status {status}: {stderr}")]
    CommandFailed {
        tool: &'static str,
        status: i32,
        stderr: String,
    },
    /// A tool command failed and the adapter has more to say than the bare
    /// stderr does: which operation was running, and the environment it ran
    /// in. Adapters prefer this to `CommandFailed` wherever the stderr alone
    /// would leave the reader reconstructing the context by hand.
    ///
    /// `context` is already-rendered lines. The adapter owns the structure
    /// because only the adapter knows which facts about its own tool matter;
    /// the port carries the message.
    #[error("{tool} {operation} failed with status {status}: {stderr}\n{context}")]
    CommandFailedInContext {
        tool: &'static str,
        operation: &'static str,
        status: i32,
        stderr: String,
        context: String,
    },
    #[error("could not understand {tool} output: {detail}")]
    Unparseable { tool: &'static str, detail: String },
    #[error("{operation} failed on {path}: {detail}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        detail: String,
    },
}

/// The terminal multiplexer that hosts a session.
pub trait Multiplexer {
    fn session_exists(&self, name: &SessionName) -> Result<bool, PortError>;
    fn list_sessions(&self) -> Result<Vec<SessionName>, PortError>;
    fn create_session(
        &self,
        name: &SessionName,
        cwd: &Path,
        command: &Command,
    ) -> Result<(), PortError>;
    /// Split `target_pane` and run `command` in the new pane. The first split of
    /// a session is horizontal (the agent keeps the left side); every later
    /// split is vertical, stacking context panes down the right.
    fn split_pane(
        &self,
        name: &SessionName,
        target_pane: u32,
        horizontal: bool,
        size: Option<PaneSize>,
        cwd: &Path,
        command: &Command,
    ) -> Result<(), PortError>;
    fn select_pane(&self, name: &SessionName, index: u32) -> Result<(), PortError>;
    /// Hand the terminal over. On success the real implementation replaces the
    /// current process, so this does not return.
    fn attach(&self, name: &SessionName) -> Result<(), PortError>;
    fn kill_session(&self, name: &SessionName) -> Result<(), PortError>;
    /// For `zax doctor`: is the multiplexer binary usable at all?
    fn probe(&self) -> Result<String, PortError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<BranchName>,
    pub is_main: bool,
    pub is_detached: bool,
}

pub trait GitRepo {
    /// `git rev-parse --path-format=absolute --git-common-dir`.
    fn common_dir(&self) -> Result<PathBuf, PortError>;
    /// The main working tree's directory, which is the parent of the common dir.
    fn main_worktree(&self) -> Result<PathBuf, PortError>;
    fn root_repo_name(&self) -> Result<RootRepoName, PortError>;
    /// `None` when HEAD is detached.
    fn current_branch(&self) -> Result<Option<BranchName>, PortError>;
    fn branch_exists(&self, branch: &BranchName) -> Result<bool, PortError>;
    /// `git worktree add -b <branch> <path> [<base>]`.
    fn add_worktree_new_branch(
        &self,
        path: &Path,
        branch: &BranchName,
        base: Option<&str>,
    ) -> Result<(), PortError>;
    /// `git worktree add <path> <branch>` for a branch that already exists.
    fn add_worktree_existing_branch(
        &self,
        path: &Path,
        branch: &BranchName,
    ) -> Result<(), PortError>;
    fn list_worktrees(&self) -> Result<Vec<WorktreeEntry>, PortError>;
    fn remove_worktree(&self, path: &Path, force: bool) -> Result<(), PortError>;
    fn fetch(&self, remote: &str, refspec: &str) -> Result<(), PortError>;
    /// The commit a revision names, as a full object name.
    fn resolve_commit(&self, rev: &str) -> Result<String, PortError>;
    /// Is `ancestor` reachable from `descendant`? A branch update is a
    /// fast-forward exactly when its current tip is.
    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, PortError>;
    fn probe(&self) -> Result<String, PortError>;
}

/// The head of a pull request, enough to build a worktree from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestHead {
    pub number: u32,
    pub head_ref: String,
    /// `None` when the forge no longer has the head repository, which is what
    /// GitHub reports once a fork has been deleted. `refs/pull/<n>/head`
    /// outlives the fork, so this is not on its own a reason to give up.
    pub head_repo: Option<RepoSlug>,
    pub is_cross_repository: bool,
}

pub trait Forge {
    /// The filter is passed so a forge can narrow its own search; it is a hint,
    /// not the authority. Whatever comes back is filtered again in the
    /// application layer, which is where the rules actually live.
    fn review_requests(&self, filter: &PrFilter) -> Result<Vec<PullRequest>, PortError>;
    /// The repository the current directory belongs to, as the forge resolves
    /// it. This is the repository a bare pull request number is read against
    /// and the one `origin` points at, so it is what a pull request from
    /// somewhere else has to be compared with.
    fn current_repo(&self) -> Result<RepoSlug, PortError>;
    /// `None` when the branch has no open pull request.
    fn branch_status(&self, branch: &BranchName) -> Result<Option<BranchStatus>, PortError>;
    fn pull_request_head(&self, pr: &PrRef) -> Result<PullRequestHead, PortError>;
    fn auth_status(&self) -> Result<String, PortError>;
    fn probe(&self) -> Result<String, PortError>;
}

pub trait ProcessRunner {
    /// Run to completion in `cwd` with `env` added to the inherited environment.
    /// Returns the command's stdout.
    fn run(
        &self,
        cwd: &Path,
        command: &Command,
        env: &[(String, String)],
    ) -> Result<String, PortError>;
}

pub trait FileSystem {
    fn exists(&self, path: &Path) -> bool;
    fn create_dir_all(&self, path: &Path) -> Result<(), PortError>;
    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), PortError>;
}

pub trait Clock {
    fn now(&self) -> SystemTime;
}
