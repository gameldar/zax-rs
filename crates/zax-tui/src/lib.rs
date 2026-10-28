//! Pane rendering. Depends on the domain, never on an adapter.

pub mod action;
pub mod branch_pane;
pub mod cache;
pub mod chrome;
pub mod config_modal;
pub mod exec_pane;
pub mod fmt;
pub mod keys;
pub mod my_prs_pane;
pub(crate) mod pr_fields;
pub mod prs_pane;
pub mod refresh;
pub mod repo_picker;
pub mod tickets_pane;
pub mod todo_pane;
pub mod worktrees_modal;

pub use action::{PaneAction, ReviewMode};
pub use branch_pane::BranchStatusPane;
pub use cache::Cached;
pub use config_modal::{ConfigAction, ConfigModal};
pub use exec_pane::ExecPane;
pub use keys::KeyInput;
pub use my_prs_pane::MyPrsPane;
pub use prs_pane::PrsForReviewPane;
pub use repo_picker::{RepoPicker, RepoPickerAction};
pub use tickets_pane::TicketsPane;
pub use todo_pane::TodoPane;
pub use worktrees_modal::{WorktreesAction, WorktreesModal};
