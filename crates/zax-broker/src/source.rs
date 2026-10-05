//! What the broker shares. A source is polled on its own worker thread, so
//! one slow source never holds up another.

use std::time::Duration;

use serde_json::Value;
use zax_protocol::SourceKey;

pub trait Source: Send {
    /// The current value. An error is rendered on every subscriber's error
    /// line, so it is a sentence, not a code.
    fn fetch(&mut self) -> Result<Value, String>;
    /// A write the source supports, such as a ticket transition.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String>;
    fn interval(&self) -> Duration;
}

/// Builds the source a key names. The broker is generic; what a `tickets`
/// or `work` key means is the composition root's business.
pub trait SourceFactory: Send + Sync {
    fn create(&self, key: &SourceKey) -> Result<Box<dyn Source>, String>;
}
