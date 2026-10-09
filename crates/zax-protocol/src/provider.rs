//! The provider half of the protocol, for providers written in Rust.
//!
//! A provider implements [`Provider`] and hands its stdin and stdout to
//! [`serve`]. Framing, dispatch, parameter decoding and the version check are
//! done here, once, so a provider is only its tracker's specifics.

use std::io::{self, BufRead, Write};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::codec::write_message;
use crate::rpc::{ErrorKind, Request, Response, RpcError};
use crate::tickets::{
    method, AssignedResult, InitializeParams, InitializeResult, KeyParams, TransitionParams,
    TransitionsResult, PROTOCOL_VERSION,
};

pub trait Provider {
    fn initialize(&mut self) -> Result<InitializeResult, RpcError>;
    fn assigned(&mut self) -> Result<AssignedResult, RpcError>;
    fn transitions(&mut self, key: &str) -> Result<TransitionsResult, RpcError>;
    fn transition(&mut self, key: &str, id: &str) -> Result<(), RpcError>;
}

pub fn serve<R: BufRead, W: Write, P: Provider>(
    mut input: R,
    mut output: W,
    provider: &mut P,
) -> io::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(e) => {
                // No id can be recovered from a line that is not a request.
                let error = RpcError::new(ErrorKind::InvalidRequest, format!("not a request: {e}"));
                write_message(&mut output, &Response::err(0, error))?;
                continue;
            }
        };
        let shutting_down = request.method == method::SHUTDOWN;
        let response = match dispatch(provider, &request) {
            Ok(result) => Response::ok(request.id, result),
            Err(error) => Response::err(request.id, error),
        };
        write_message(&mut output, &response)?;
        if shutting_down {
            return Ok(());
        }
    }
}

fn dispatch<P: Provider>(provider: &mut P, request: &Request) -> Result<Value, RpcError> {
    match request.method.as_str() {
        method::INITIALIZE => {
            let params: InitializeParams = params(&request.params)?;
            if params.protocol != PROTOCOL_VERSION {
                return Err(RpcError::new(
                    ErrorKind::InvalidRequest,
                    format!(
                        "zax speaks protocol {}, this provider speaks {PROTOCOL_VERSION}",
                        params.protocol
                    ),
                ));
            }
            to_value(provider.initialize()?)
        }
        method::ASSIGNED => to_value(provider.assigned()?),
        method::TRANSITIONS => {
            let params: KeyParams = params(&request.params)?;
            to_value(provider.transitions(&params.key)?)
        }
        method::TRANSITION => {
            let params: TransitionParams = params(&request.params)?;
            provider.transition(&params.key, &params.id)?;
            Ok(Value::Object(serde_json::Map::new()))
        }
        method::SHUTDOWN => Ok(Value::Object(serde_json::Map::new())),
        other => Err(RpcError::new(
            ErrorKind::Unsupported,
            format!("no method `{other}`"),
        )),
    }
}

fn params<T: DeserializeOwned>(value: &Value) -> Result<T, RpcError> {
    serde_json::from_value(value.clone())
        .map_err(|e| RpcError::new(ErrorKind::InvalidRequest, format!("bad params: {e}")))
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::new(ErrorKind::Unavailable, e.to_string()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::rpc::{ErrorKind, Response};
    use crate::tickets::{Capabilities, WireStatus, WireTicket};

    #[derive(Default)]
    struct Fake {
        transitioned: Vec<(String, String)>,
    }

    impl Provider for Fake {
        fn initialize(&mut self) -> Result<InitializeResult, RpcError> {
            Ok(InitializeResult {
                protocol: 1,
                name: "Fake".into(),
                capabilities: Capabilities { transitions: true },
                poll: None,
            })
        }
        fn assigned(&mut self) -> Result<AssignedResult, RpcError> {
            Ok(AssignedResult {
                tickets: vec![WireTicket {
                    key: "A-1".into(),
                    summary: "s".into(),
                    status: WireStatus {
                        category: "to_do".into(),
                        name: "To Do".into(),
                    },
                    priority: None,
                    kind: None,
                    url: "u".into(),
                }],
            })
        }
        fn transitions(&mut self, _key: &str) -> Result<TransitionsResult, RpcError> {
            Err(RpcError::new(ErrorKind::Unavailable, "nope"))
        }
        fn transition(&mut self, key: &str, id: &str) -> Result<(), RpcError> {
            self.transitioned.push((key.into(), id.into()));
            Ok(())
        }
    }

    fn run(input: &str, provider: &mut Fake) -> Vec<Response> {
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, provider).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn it_answers_each_request_by_id() {
        let mut p = Fake::default();
        let out = run(
            concat!(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":1}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","id":2,"method":"tickets/assigned","params":{}}"#,
                "\n",
            ),
            &mut p,
        );
        assert_eq!(out[0].id, 1);
        assert_eq!(out[0].result.as_ref().unwrap()["name"], "Fake");
        assert_eq!(out[1].result.as_ref().unwrap()["tickets"][0]["key"], "A-1");
    }

    #[test]
    fn a_different_protocol_is_refused_at_initialize() {
        let out = run(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":2}}"#,
            &mut Fake::default(),
        );
        let err = out[0].error.as_ref().unwrap();
        assert_eq!(err.kind(), Some(ErrorKind::InvalidRequest));
        assert!(err.message.contains("protocol 2"), "{}", err.message);
    }

    #[test]
    fn an_unknown_method_is_unsupported() {
        let out = run(
            r#"{"jsonrpc":"2.0","id":4,"method":"tickets/create","params":{}}"#,
            &mut Fake::default(),
        );
        assert_eq!(
            out[0].error.as_ref().unwrap().kind(),
            Some(ErrorKind::Unsupported)
        );
    }

    #[test]
    fn transition_params_reach_the_provider_and_errors_pass_through() {
        let mut p = Fake::default();
        let out = run(
            concat!(
                r#"{"jsonrpc":"2.0","id":1,"method":"tickets/transition","params":{"key":"A-1","id":"Done"}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","id":2,"method":"tickets/transitions","params":{"key":"A-1"}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","id":3,"method":"tickets/transitions","params":{}}"#,
                "\n",
            ),
            &mut p,
        );
        assert_eq!(p.transitioned, vec![("A-1".to_owned(), "Done".to_owned())]);
        assert_eq!(
            out[1].error.as_ref().unwrap().kind(),
            Some(ErrorKind::Unavailable)
        );
        assert_eq!(
            out[2].error.as_ref().unwrap().kind(),
            Some(ErrorKind::InvalidRequest)
        );
    }

    #[test]
    fn shutdown_is_answered_and_ends_the_loop() {
        let out = run(
            concat!(
                r#"{"jsonrpc":"2.0","id":9,"method":"shutdown","params":{}}"#,
                "\n",
                r#"{"jsonrpc":"2.0","id":10,"method":"tickets/assigned","params":{}}"#,
                "\n",
            ),
            &mut Fake::default(),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, 9);
    }

    #[test]
    fn a_line_that_is_not_a_request_gets_an_invalid_request_reply_and_the_loop_continues() {
        let out = run(
            concat!(
                "garbage\n",
                r#"{"jsonrpc":"2.0","id":2,"method":"tickets/assigned","params":{}}"#,
                "\n"
            ),
            &mut Fake::default(),
        );
        assert_eq!(
            out[0].error.as_ref().unwrap().kind(),
            Some(ErrorKind::InvalidRequest)
        );
        assert_eq!(out[1].id, 2);
    }
}
