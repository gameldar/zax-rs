//! JSON-RPC 2.0, as much of it as the provider hop uses.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const JSONRPC: &str = "2.0";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC.to_owned(),
            id,
            method: method.to_owned(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC.to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC.to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }

    /// A response with neither field is read as an empty success, which is
    /// what a method with nothing to return sends.
    pub fn into_result(self) -> Result<Value, RpcError> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.result.unwrap_or(Value::Null)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ErrorData>,
}

impl RpcError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            code: kind.code(),
            message: message.into(),
            data: Some(ErrorData { kind }),
        }
    }

    pub fn kind(&self) -> Option<ErrorKind> {
        self.data.as_ref().map(|d| d.kind)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorData {
    pub kind: ErrorKind,
}

/// Why a provider could not answer, in terms zax renders the same way it
/// renders a failing `gh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    NotAuthenticated,
    MissingTool,
    Unavailable,
    InvalidRequest,
    Unsupported,
}

impl ErrorKind {
    fn code(self) -> i64 {
        match self {
            Self::InvalidRequest => -32600,
            Self::Unsupported => -32601,
            Self::NotAuthenticated | Self::MissingTool | Self::Unavailable => -32000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// A line a provider wrote: an answer to a request, or a notification. A
/// response always has an `id` and a notification never does, which is what
/// tells the untagged variants apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FromProvider {
    Response(Response),
    Notification(Notification),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn a_request_is_jsonrpc_two() {
        let r = Request::new(7, "tickets/assigned", serde_json::json!({}));
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"jsonrpc":"2.0","id":7,"method":"tickets/assigned","params":{}}"#
        );
    }

    #[test]
    fn an_error_carries_its_kind_in_data() {
        let r = Response::err(
            3,
            RpcError::new(ErrorKind::NotAuthenticated, "run acli jira auth login"),
        );
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32000,"message":"run acli jira auth login","data":{"kind":"not_authenticated"}}}"#
        );
    }

    #[test]
    fn unsupported_and_invalid_use_the_jsonrpc_codes() {
        assert_eq!(RpcError::new(ErrorKind::Unsupported, "x").code, -32601);
        assert_eq!(RpcError::new(ErrorKind::InvalidRequest, "x").code, -32600);
    }

    #[test]
    fn provider_output_is_told_apart_by_its_id() {
        let response: FromProvider =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).unwrap();
        assert!(matches!(response, FromProvider::Response(_)));
        let note: FromProvider =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"tickets/changed","params":{}}"#)
                .unwrap();
        assert!(matches!(note, FromProvider::Notification(_)));
    }

    #[test]
    fn into_result_surfaces_the_error() {
        let err = Response::err(1, RpcError::new(ErrorKind::Unavailable, "down")).into_result();
        assert_eq!(err.unwrap_err().kind(), Some(ErrorKind::Unavailable));
    }
}
