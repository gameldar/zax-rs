//! The wire protocol. Two hops, both newline-delimited JSON:
//!
//! - broker to provider: JSON-RPC 2.0 over the provider's stdin and stdout.
//!   This is the hop a third party implements, so it is the documented one.
//! - pane to broker: messages tagged by `type`, over a unix socket.
//!
//! No zax types appear here, so a provider needs nothing else from zax.

pub mod broker;
pub mod codec;
pub mod provider;
pub mod rpc;
pub mod tickets;

pub use broker::{BrokerMessage, ClientMessage, Snapshot, SourceKey};
pub use codec::{read_message, write_message};
pub use rpc::{
    ErrorData, ErrorKind, FromProvider, Notification, Request, Response, RpcError, JSONRPC,
};
pub use tickets::*;
