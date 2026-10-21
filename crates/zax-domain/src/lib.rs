//! Pure domain types for zax. No I/O, no dependencies beyond `thiserror`.
//!
//! This is where parse-don't-validate lives: the project's branch and worktree
//! conventions are type constructors, not advisory documentation.

pub mod branch;
pub mod date;
pub mod forge;
pub mod layout;
pub mod projects;
pub mod prs_filter;
pub mod remote;
pub mod repo;
pub mod session;
pub mod settings;
pub mod ticket;
pub mod todo;
pub mod todo_view;
pub mod work;

pub use branch::{BranchName, BranchNameError, BranchOrigin};
pub use date::{Date, DateError, Weekday};
pub use forge::{
    Attention, BranchView, CheckState, Mergeable, MyPullRequest, PrRef, PrRefError, PullRequest,
    PullRequestStatus, RepoSlug, RepoSlugError, ReviewState,
};
pub use layout::{
    format_duration, parse_duration, BuiltinPane, BuiltinPaneError, Command, CommandError,
    DurationError, LayoutSpec, PaneKind, PaneSize, PaneSizeError, PaneSpec, RootPane,
};
pub use projects::{
    accept as accept_checkout, verify as verify_checkout, CheckoutProbe, MainCheckout, NoCheckout,
    ProjectPaths, ProjectPathsError, Projects, ProjectsRoot, ProjectsRootError, Rejection,
    RepoChoices, RepoTarget, RepoTargetError,
};
pub use prs_filter::{PrFilter, RepoPattern, RepoPatternError};
pub use remote::Transport;
pub use repo::{
    RootRepoName, RootRepoNameError, WorktreePath, WorktreePathError, WorktreeRoot,
    WorktreeRootError,
};
pub use session::{
    choose_directory_session, choose_session, worktree_sessions, LiveSession, SessionChoice,
    SessionName, SessionNameError, SessionSlot,
};
pub use settings::{
    ConfigView, LayerInfo, LayerSlot, Origin, PaneEdit, PaneField, PaneKindTag, PanesView, Pending,
    Saved, SettingKey, SettingRow, SettingShape, SettingValue,
};
pub use ticket::{
    branch_slug, StatusCategory, Ticket, TicketKey, TicketKeyError, TicketStatus, Transition,
};
pub use todo::{
    Due, LineRef, TodoDocument, TodoEdit, TodoError, TodoFile, TodoFileError, TodoItem, TodoList,
    COMPLETION, DUE,
};
pub use todo_view::{
    due_state, is_late, parse_due_spec, parse_prompt, prompt_text, todo_rows, DueError, DueState,
    PromptEntry, PromptError,
};
pub use work::{join, TicketRank, TicketRow, TicketWork, WorkIndex, Worktree};
