//! Pure domain types for zax. No I/O, no dependencies beyond `thiserror`.
//!
//! This is where parse-don't-validate lives: the project's branch and worktree
//! conventions are type constructors, not advisory documentation.

pub mod branch;
pub mod layout;
pub mod repo;
pub mod session;
pub mod ticket;

pub use branch::{BranchName, BranchNameError, BranchOrigin};
pub use layout::{
    format_duration, parse_duration, BuiltinPane, BuiltinPaneError, Command, CommandError,
    DurationError, LayoutError, LayoutSpec, PaneKind, PaneSize, PaneSizeError, PaneSpec,
};
pub use repo::{
    RootRepoName, RootRepoNameError, WorktreePath, WorktreePathError, WorktreeRoot,
    WorktreeRootError,
};
pub use session::{SessionName, SessionNameError};
pub use ticket::{TicketKey, TicketKeyError};
