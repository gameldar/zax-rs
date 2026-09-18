//! Pane rendering. Depends on the domain, never on an adapter.

pub mod action;
pub mod branch_pane;
pub mod cache;
pub mod chrome;
pub mod exec_pane;
pub mod fmt;
pub mod prs_pane;
pub mod refresh;

pub use action::PaneAction;
pub use branch_pane::BranchStatusPane;
pub use cache::Cached;
pub use exec_pane::ExecPane;
pub use prs_pane::PrsForReviewPane;
