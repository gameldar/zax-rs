//! Pane rendering. Depends on the domain, never on an adapter.

pub mod action;
pub mod cache;
pub mod chrome;
pub mod exec_pane;
pub mod fmt;
pub mod refresh;

pub use action::PaneAction;
pub use cache::Cached;
pub use exec_pane::ExecPane;
