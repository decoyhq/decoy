use serde_json::Value;

use super::{Trigger, WsConnection, WsReply, decode_text, encode_text};
use crate::engine::{MissDiagnostic, SessionView};
use crate::events::{CapturedBody, CapturedResponse, Outcome};
use crate::schema::Transport;

/// What arrived on a connection.
#[derive(Debug, Clone)]
pub enum Inbound<'a> {
    /// The upgrade was accepted.
    Open,
    Text(&'a str),
    /// A binary frame on a Route without a Pipeline: always a miss.
    Binary(&'a [u8]),
    /// A frame the Route's Pipeline decoded.
    Decoded(Value),
    /// A frame a Pipeline step called `miss` on.
    Missed {
        payload: &'a [u8],
        reason: String,
    },
}

/// Events show a WebSocket exchange with this method and status.
const METHOD: &str = "WS";
const SWITCHING_PROTOCOLS: u16 = 101;

impl SessionView {
    /// Answers the open or one inbound message on `connection`, and, if anyone subscribed, emits
    /// its [`RequestEvent`](crate::events::RequestEvent). The open emits one only when an
    /// `on: connect` Case answered it. An unmatched message, a binary one without a Pipeline, or one
    /// a step skipped is a miss: nothing to send, and a warning when nobody subscribed (Q4).
    pub fn execute_ws(&self, connection: &WsConnection, inbound: Inbound<'_>) -> Option<WsReply> {
        let capture = self.events.receiver_count() > 0;
        let started = capture.then(|| self.start(METHOD, &connection.url, &connection.headers));
        let (trigger, message, raw_text) = match inbound {
            Inbound::Open => (Trigger::Connect, None, None),
            Inbound::Text(text) => (Trigger::Message, Some(decode_text(text)), Some(text)),
            Inbound::Decoded(message) => (Trigger::Message, Some(message), None),
            Inbound::Binary(payload) => {
                let reason = "binary frames need a `pipeline` on the Route".to_owned();
                self.missed(connection, started, payload, reason);
                return None;
            }
            Inbound::Missed { payload, reason } => {
                self.missed(connection, started, payload, reason);
                return None;
            }
        };
        let envelope = connection.envelope(message.as_ref());
        let call = self.catalog.execute_ws(
            &self.selection.collection,
            &self.selection.route_overrides,
            connection,
            trigger,
            &envelope,
            capture,
        );
        let missed = matches!(call.reply, Ok(None));
        let reply = call.reply.unwrap_or_else(|reason| {
            crate::events::error(reason);
            None
        });
        match (started, call.outcome) {
            (Some(_), Some(Outcome::Miss(_))) if trigger == Trigger::Connect => {}
            (Some(started), Some(outcome)) => {
                let body = match (message.as_ref(), raw_text) {
                    (Some(Value::String(_)), Some(text)) => CapturedBody::raw(text.as_bytes()),
                    (Some(message), _) => CapturedBody::decoded(message),
                    (None, _) => CapturedBody::raw(&[]),
                };
                self.emit(
                    Transport::Ws,
                    started,
                    body,
                    outcome,
                    response(reply.as_ref()),
                );
            }
            (None, _) if missed && trigger == Trigger::Message => {
                miss_warning(connection, super::MESSAGE_MISS);
            }
            _ => {}
        }
        reply
    }
}

impl SessionView {
    fn missed(
        &self,
        connection: &WsConnection,
        started: Option<crate::engine::view::Started>,
        payload: &[u8],
        reason: String,
    ) {
        let Some(started) = started else {
            miss_warning(connection, &reason);
            return;
        };
        let outcome = Outcome::Miss(MissDiagnostic {
            collection: self.selection.collection.clone(),
            reason,
            candidates: Vec::new(),
        });
        self.emit(
            Transport::Ws,
            started,
            CapturedBody::raw(payload),
            outcome,
            response(None),
        );
    }
}

fn miss_warning(connection: &WsConnection, reason: &str) {
    crate::events::warn(format!(
        "ws `{}` {}: {reason}; nothing sent",
        connection.route, connection.url
    ));
}

/// The rendered reply before scheduling and output transforms, with its planned close as headers.
fn response(reply: Option<&WsReply>) -> CapturedResponse {
    let Some(reply) = reply else {
        return CapturedResponse {
            status: SWITCHING_PROTOCOLS,
            headers: Vec::new(),
            body: CapturedBody::raw(&[]),
        };
    };
    let frames: Vec<Value> = reply
        .messages
        .iter()
        .map(|(_, message)| Value::String(encode_text(message)))
        .collect();
    let headers = reply.close.iter().flat_map(|close| {
        [
            ("close-code".to_owned(), close.code.to_string()),
            ("close-reason".to_owned(), close.reason.clone()),
        ]
    });
    CapturedResponse {
        status: SWITCHING_PROTOCOLS,
        headers: headers.collect(),
        body: CapturedBody::decoded(&Value::Array(frames)),
    }
}
