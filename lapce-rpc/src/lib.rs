#![allow(clippy::manual_clamp)]

pub mod buffer;
pub mod core;
pub mod counter;
pub mod file;
pub mod file_line;
mod parse;
pub mod plugin;
pub mod proxy;
pub mod stdio;
pub mod style;

pub use parse::{Call, RequestId, RpcObject};
use serde::{Deserialize, Serialize};
pub use stdio::stdio_transport;

/// Generic RPC message envelope that can represent any of the four message types
/// in Lapce's protocol. Parameterized by the request, notification, and response
/// types so the same infrastructure works for both directions of communication:
/// - UI->Proxy: RpcMessage<ProxyRequest, ProxyNotification, ProxyResponse>
/// - Proxy->UI: RpcMessage<CoreRequest, CoreNotification, CoreResponse>
#[derive(Debug)]
pub enum RpcMessage<Req, Notif, Resp> {
    Request(RequestId, Req),
    Response(RequestId, Resp),
    Notification(Notif),
    Error(RequestId, RpcError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}
