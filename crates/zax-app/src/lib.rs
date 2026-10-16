//! Use cases expressed over ports. Nothing here names tmux, git or GitHub.

pub mod create_worktree;
pub mod ports;
pub mod provision;
pub mod queries;
pub mod review_worktree;
pub mod start_session;
pub mod testing;

pub use create_worktree::{
    CreateWorktree, CreateWorktreeError, CreateWorktreeOutcome, CreateWorktreeRequest,
};
pub use ports::{
    Clock, Editor, FileSystem, Forge, GitRepo, Multiplexer, PortError, ProcessRunner,
    PullRequestHead, SshAgent, WorktreeEntry, WorktreeOpener, WorktreeScanner,
};
pub use provision::{ProvisionError, ProvisionReport, Provisioning};
pub use queries::{DoctorCheck, RemoveWorktreeError};
pub use review_worktree::{
    CreateReviewWorktree, CreateReviewWorktreeError, CreateReviewWorktreeOutcome,
    CreateReviewWorktreeRequest, ForcedBranchUpdate,
};
pub use start_session::{
    pane_command, StartSession, StartSessionError, StartSessionOutcome, StartSessionRequest,
};
