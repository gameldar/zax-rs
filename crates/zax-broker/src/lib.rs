//! The broker: one per user, shared by every zax pane in every session.
//!
//! Panes subscribe to a source by key. The broker creates the source the
//! first time it is asked for, polls it while anyone is subscribed, caches
//! the last good value and fans every result out to every subscriber.

pub mod client;
pub mod hub;
pub mod paths;
pub mod provider_source;
pub mod server;
pub mod source;
pub mod todo_source;
pub mod topics;

pub use hub::{ClientId, Hub};
pub use paths::BrokerPaths;
pub use source::{Source, SourceFactory};
