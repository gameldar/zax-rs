//! Use cases expressed over ports. Nothing here names tmux, git or GitHub.

pub mod ports;
pub mod testing;

pub use ports::{
    Clock, FileSystem, Forge, GitRepo, Multiplexer, PortError, ProcessRunner, PullRequestHead,
    WorktreeEntry,
};
