//! The ticket provider's methods and their payloads.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const ASSIGNED: &str = "tickets/assigned";
    pub const TRANSITIONS: &str = "tickets/transitions";
    pub const TRANSITION: &str = "tickets/transition";
    /// Provider to broker, optional: "poll me now".
    pub const CHANGED: &str = "tickets/changed";
    pub const SHUTDOWN: &str = "shutdown";
}

/// The tracker-neutral status categories, as written on the wire.
pub mod category {
    pub const TO_DO: &str = "to_do";
    pub const IN_PROGRESS: &str = "in_progress";
    pub const DONE: &str = "done";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeParams {
    pub protocol: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeResult {
    pub protocol: u32,
    /// What the pane calls the tracker, such as "Jira".
    pub name: String,
    pub capabilities: Capabilities,
    /// How often the broker should poll, as `30s`, `5m`, `2h`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub transitions: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssignedResult {
    pub tickets: Vec<WireTicket>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireTicket {
    pub key: String,
    pub summary: String,
    pub status: WireStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub url: String,
}

/// `category` is a string rather than an enum so that one ticket with a
/// category zax does not know drops that ticket, not the whole list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireStatus {
    pub category: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyParams {
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitionsResult {
    pub transitions: Vec<WireTransition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireTransition {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransitionParams {
    pub key: String,
    pub id: String,
}

/// What the broker publishes on the `tickets` topic: the provider's name and
/// capabilities travel with the tickets, so a pane needs nothing else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketsTopic {
    pub name: String,
    pub transitions: bool,
    pub tickets: Vec<WireTicket>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn a_ticket_on_the_wire() {
        let json = r#"{"key":"OMEGA-1","summary":"s","status":{"category":"in_progress","name":"In Review"},"priority":"High","url":"https://x/browse/OMEGA-1"}"#;
        let t: WireTicket = serde_json::from_str(json).unwrap();
        assert_eq!(t.status.category, category::IN_PROGRESS);
        assert_eq!(t.kind, None);
        assert_eq!(serde_json::to_string(&t).unwrap(), json);
    }

    #[test]
    fn initialize_result_poll_is_optional() {
        let r: InitializeResult = serde_json::from_str(
            r#"{"protocol":1,"name":"Jira","capabilities":{"transitions":true}}"#,
        )
        .unwrap();
        assert_eq!(r.poll, None);
        assert!(r.capabilities.transitions);
    }
}
