//! Pure domain types for zax. No I/O, no dependencies beyond `thiserror`.
//!
//! This is where parse-don't-validate lives: the project's branch and worktree
//! conventions are type constructors, not advisory documentation.

pub mod ticket;

pub use ticket::{TicketKey, TicketKeyError};
