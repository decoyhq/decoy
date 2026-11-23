//! Serving WebSocket Routes: pick the Route for an upgrade, match Cases over each inbound message
//! (or the open, for an `on: connect` Case), and render the Behavior into timed text frames. The
//! socket itself is the server's job. See `apps/docs/content/docs/websocket/index.md`.

pub mod execute;
pub mod pipeline;
pub mod resolve;
pub mod schema;
mod view;

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};

use crate::schema::WsClose;

pub use resolve::WsResolution;
pub use view::Inbound;

/// Payload type for Control API pushes. Each connection receives JSON.
pub const OUTPUT: &str = "ws:json";

/// Why a message got no reply.
pub(crate) const MESSAGE_MISS: &str = "no active route case matched message";

/// What runs a Case: the socket opening, or one inbound message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Connect,
    Message,
}

/// An accepted upgrade: the Route it belongs to and the envelope fields every message shares.
#[derive(Debug, Clone)]
pub struct WsConnection {
    pub route: String,
    /// Path and query, as events show it.
    pub url: String,
    /// Lowercased.
    pub headers: BTreeMap<String, String>,
    /// `{ path, pathParams, query, headers, cookies }` of the upgrade request.
    pub envelope: Value,
}

impl WsConnection {
    /// The envelope Cases and templates see: the upgrade's, plus `message` unless on connect.
    pub fn envelope(&self, message: Option<&Value>) -> Value {
        let mut envelope = self.envelope.clone();
        if let Some(message) = message {
            envelope["message"] = message.clone();
        }
        envelope
    }
}

/// A text frame as Cases see it: JSON when it parses, the text otherwise.
pub fn decode_text(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()))
}

/// A message as a text frame: a string as-is, anything else as JSON.
pub fn encode_text(message: &Value) -> String {
    match message {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A rendered Behavior: wait `delay`, send each message `delay` after the one before, then
/// `close` if set. Messages are encoded as they go out: as text without a Pipeline, by its steps
/// with one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsReply {
    /// The Behavior's address, for error events.
    pub behavior: String,
    pub delay: Option<Duration>,
    pub messages: Vec<(Option<Duration>, Value)>,
    pub close: Option<WsClose>,
}

pub(crate) fn upgrade_envelope(
    path: &str,
    path_params: &BTreeMap<String, String>,
    request: &crate::request::RequestEnvelope,
) -> Value {
    json!({
        "path": path,
        "pathParams": path_params,
        "query": request.query_json(),
        "headers": request.headers(),
        "cookies": request.cookies,
    })
}
