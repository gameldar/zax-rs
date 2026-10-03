//! Pure domain types for zax. No I/O, no dependencies beyond `thiserror`.
//!
//! This is where parse-don't-validate lives: the project's branch and worktree
//! conventions are type constructors, not advisory documentation.

pub mod branch;
pub mod forge;
pub mod layout;
pub mod prs_filter;
pub mod repo;
pub mod session;
pub mod settings;
pub mod ticket;

pub use branch::{BranchName, BranchNameError, BranchOrigin};
pub use forge::{
    Attention, BranchView, CheckState, Mergeable, MyPullRequest, PrRef, PrRefError, PullRequest,
    PullRequestStatus, RepoSlug, RepoSlugError, ReviewState,
};
pub use layout::{
    format_duration, parse_duration, BuiltinPane, BuiltinPaneError, Command, CommandError,
    DurationError, LayoutError, LayoutSpec, PaneKind, PaneSize, PaneSizeError, PaneSpec,
};
pub use prs_filter::{PrFilter, RepoPattern, RepoPatternError};
pub use repo::{
    RootRepoName, RootRepoNameError, WorktreePath, WorktreePathError, WorktreeRoot,
    WorktreeRootError,
};
pub use session::{
    choose_session, worktree_sessions, LiveSession, SessionChoice, SessionName, SessionNameError,
    SessionSlot,
};
pub use settings::{
    ConfigView, LayerInfo, LayerSlot, Origin, PaneEdit, PaneField, PaneKindTag, PanesView, Pending,
    Saved, SettingKey, SettingRow, SettingShape, SettingValue,
};
pub use ticket::{TicketKey, TicketKeyError};
