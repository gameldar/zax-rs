//! Pane to broker. Tagged by `type`, one JSON object per line.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which shared source a message is about: a topic, plus the resolved spec
/// of the source behind it (a provider's argv, the worktree root). Two panes
/// whose config resolves alike share one source.
///
/// `source` is canonical JSON text: `serde_json`'s maps are sorted, so the
/// same spec always produces the same key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceKey {
    pub topic: String,
    pub source: String,
}

impl SourceKey {
    pub fn new<S: Serialize>(topic: &str, source: &S) -> Self {
        let source = serde_json::to_value(source)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "null".to_owned());
        Self {
            topic: topic.to_owned(),
            source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// `identity` is the client binary's modification time, in seconds.
    Hello {
        identity: u64,
    },
    Subscribe {
        key: SourceKey,
    },
    Refresh {
        key: SourceKey,
    },
    Call {
        id: u64,
        key: SourceKey,
        method: String,
        params: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrokerMessage {
    Welcome,
    /// This broker is older than the client; it exits after sending this.
    RestartRequired,
    Snapshot(Snapshot),
    Reply {
        id: u64,
        result: Option<Value>,
        error: Option<String>,
    },
}

/// A source's cache: the last good value, when it was fetched, and the last
/// error. A value and an error together mean "stale".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub key: SourceKey,
    pub value: Option<Value>,
    /// Seconds since the epoch.
    pub fetched_at: Option<u64>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn a_source_key_is_canonical_whatever_the_field_order() {
        #[derive(serde::Serialize)]
        struct A {
            b: u8,
            a: u8,
        }
        let key = SourceKey::new("work", &A { b: 2, a: 1 });
        assert_eq!(key.source, r#"{"a":1,"b":2}"#);
    }

    #[test]
    fn client_messages_are_tagged_by_type() {
        let m = ClientMessage::Hello { identity: 5 };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"type":"hello","identity":5}"#
        );
    }

    #[test]
    fn a_snapshot_round_trips() {
        let m = BrokerMessage::Snapshot(Snapshot {
            key: SourceKey {
                topic: "tickets".into(),
                source: "{}".into(),
            },
            value: Some(serde_json::json!([1])),
            fetched_at: Some(10),
            error: None,
        });
        let text = serde_json::to_string(&m).unwrap();
        assert!(text.starts_with(r#"{"type":"snapshot""#), "got {text}");
        assert_eq!(serde_json::from_str::<BrokerMessage>(&text).unwrap(), m);
    }

    fn key() -> SourceKey {
        SourceKey {
            topic: "t".into(),
            source: "{}".into(),
        }
    }

    #[test]
    fn every_client_message_on_the_wire() {
        let k = r#"{"topic":"t","source":"{}"}"#;
        let cases = [
            (
                ClientMessage::Subscribe { key: key() },
                format!(r#"{{"type":"subscribe","key":{k}}}"#),
            ),
            (
                ClientMessage::Refresh { key: key() },
                format!(r#"{{"type":"refresh","key":{k}}}"#),
            ),
            (
                ClientMessage::Call {
                    id: 2,
                    key: key(),
                    method: "m".into(),
                    params: serde_json::json!({"a": 1}),
                },
                format!(r#"{{"type":"call","id":2,"key":{k},"method":"m","params":{{"a":1}}}}"#),
            ),
        ];
        for (message, text) in cases {
            assert_eq!(serde_json::to_string(&message).unwrap(), text);
        }
    }

    #[test]
    fn every_broker_message_on_the_wire() {
        let k = r#"{"topic":"t","source":"{}"}"#;
        let cases = [
            (BrokerMessage::Welcome, r#"{"type":"welcome"}"#.to_owned()),
            (
                BrokerMessage::RestartRequired,
                r#"{"type":"restart_required"}"#.to_owned(),
            ),
            (
                BrokerMessage::Reply {
                    id: 3,
                    result: Some(serde_json::json!(true)),
                    error: None,
                },
                r#"{"type":"reply","id":3,"result":true,"error":null}"#.to_owned(),
            ),
            (
                BrokerMessage::Snapshot(Snapshot {
                    key: key(),
                    value: Some(serde_json::json!([1])),
                    fetched_at: Some(10),
                    error: Some("e".into()),
                }),
                format!(
                    r#"{{"type":"snapshot","key":{k},"value":[1],"fetched_at":10,"error":"e"}}"#
                ),
            ),
        ];
        for (message, text) in cases {
            assert_eq!(serde_json::to_string(&message).unwrap(), text);
        }
    }
}
