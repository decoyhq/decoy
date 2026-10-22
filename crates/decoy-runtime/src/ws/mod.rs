//! Serving WebSocket Routes: pick the Route for an upgrade, match Cases over each inbound message
//! (or the open, for an `on: connect` Case), and render the Behavior into timed text frames. The
//! socket itself is the server's job. See `docs/design/transports.md` §6.

pub mod execute;
pub mod resolve;
pub mod schema;
mod view;

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};

use crate::schema::WsClose;

pub use resolve::WsResolution;
pub use view::Inbound;

/// What a `ws` connection registers with in [`crate::connections::Connections`]: its messages
/// are text, whatever the Route.
pub const OUTPUT: &str = "ws:text";

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

/// A rendered Behavior: wait `delay`, send each text frame `delay` after the one before, then
/// `close` if set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsReply {
    pub delay: Option<Duration>,
    pub messages: Vec<(Option<Duration>, String)>,
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
