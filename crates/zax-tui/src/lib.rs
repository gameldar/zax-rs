//! Pane rendering. Depends on the domain, never on an adapter.

pub mod action;
pub mod branch_pane;
pub mod cache;
pub mod chrome;
pub mod config_modal;
pub mod exec_pane;
pub mod fmt;
pub mod my_prs_pane;
pub(crate) mod pr_fields;
pub mod prs_pane;
pub mod refresh;
pub mod tickets_pane;
pub mod todo_pane;

pub use action::PaneAction;
pub use branch_pane::BranchStatusPane;
pub use cache::Cached;
pub use config_modal::{ConfigAction, ConfigModal};
pub use exec_pane::ExecPane;
pub use my_prs_pane::MyPrsPane;
pub use prs_pane::PrsForReviewPane;
pub use tickets_pane::TicketsPane;
pub use todo_pane::TodoPane;
