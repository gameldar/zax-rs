//! The provider zax-jira serves when its config cannot be read.
//!
//! Exiting would show the pane only that the provider died. Answering every
//! request with the config's own error puts the file and what is wrong
//! with it on the pane's error line instead.

use zax_protocol::provider::Provider;
use zax_protocol::{AssignedResult, ErrorKind, InitializeResult, RpcError, TransitionsResult};

pub struct Misconfigured {
    message: String,
}

impl Misconfigured {
    pub fn new(message: String) -> Self {
        Self { message }
    }

    fn refuse<T>(&self) -> Result<T, RpcError> {
        Err(RpcError::new(
            ErrorKind::InvalidRequest,
            self.message.clone(),
        ))
    }
}

impl Provider for Misconfigured {
    fn initialize(&mut self) -> Result<InitializeResult, RpcError> {
        self.refuse()
    }
    fn assigned(&mut self) -> Result<AssignedResult, RpcError> {
        self.refuse()
    }
    fn transitions(&mut self, _key: &str) -> Result<TransitionsResult, RpcError> {
        self.refuse()
    }
    fn transition(&mut self, _key: &str, _id: &str) -> Result<(), RpcError> {
        self.refuse()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use zax_protocol::provider::serve;
    use zax_protocol::{ErrorKind, Response};

    use super::*;

    const MESSAGE: &str = "/home/u/.config/zax/providers/jira.toml: line 2: invalid string";

    fn answers(input: &str) -> Vec<Response> {
        let mut out = Vec::new();
        serve(
            input.as_bytes(),
            &mut out,
            &mut Misconfigured::new(MESSAGE.into()),
        )
        .unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn every_method_is_refused_with_the_config_error() {
        let replies = answers(concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":1}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tickets/assigned","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"tickets/transitions","params":{"key":"A-1"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":4,"method":"tickets/transition","params":{"key":"A-1","id":"Done"}}"#,
            "\n",
        ));
        assert_eq!(replies.len(), 4);
        for reply in replies {
            let error = reply.error.expect("an error answer");
            assert_eq!(error.kind(), Some(ErrorKind::InvalidRequest));
            assert_eq!(error.message, MESSAGE);
        }
    }

    #[test]
    fn shutdown_is_still_honoured() {
        let replies = answers(concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"shutdown","params":{}}"#,
            "\n",
        ));
        assert!(replies[0].error.is_none());
    }
}
