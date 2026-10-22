use serde_json::Value;

use super::{Trigger, WsConnection, WsReply, decode_text};
use crate::engine::{MissDiagnostic, SessionView};
use crate::events::{CapturedBody, CapturedResponse, Outcome};
use crate::schema::Transport;

/// What arrived on a connection.
#[derive(Debug, Clone, Copy)]
pub enum Inbound<'a> {
    /// The upgrade was accepted.
    Open,
    Text(&'a str),
    Binary(&'a [u8]),
}

/// Events show a WebSocket exchange with this method and status.
const METHOD: &str = "WS";
const SWITCHING_PROTOCOLS: u16 = 101;

impl SessionView {
    /// Answers the open or one inbound message on `connection`, and, if anyone subscribed, emits
    /// its [`RequestEvent`](crate::events::RequestEvent). The open emits one only when an
    /// `on: connect` Case answered it. An unmatched message, or a binary one (no Pipeline yet), is
    /// a miss: nothing to send, and a warning when nobody subscribed (Q4).
    pub fn execute_ws(&self, connection: &WsConnection, inbound: Inbound) -> Option<WsReply> {
        let capture = self.events.receiver_count() > 0;
        let started = capture.then(|| self.start(METHOD, &connection.url, &connection.headers));
        let (trigger, message, body) = match inbound {
            Inbound::Open => (Trigger::Connect, None, CapturedBody::raw(&[])),
            Inbound::Text(text) => {
                let message = decode_text(text);
                let body = if message.is_string() {
                    CapturedBody::raw(text.as_bytes())
                } else {
                    CapturedBody::decoded(&message)
                };
                (Trigger::Message, Some(message), body)
            }
            Inbound::Binary(bytes) => {
                let reason =
                    "binary frames need a pipeline, which WebSocket Routes do not take yet";
                let outcome = Outcome::Miss(MissDiagnostic {
                    collection: self.selection.collection.clone(),
                    reason: reason.to_owned(),
                    candidates: Vec::new(),
                });
                match started {
                    Some(started) => self.emit(
                        Transport::Ws,
                        started,
                        CapturedBody::raw(bytes),
                        outcome,
                        response(None),
                    ),
                    None => miss_warning(connection, reason),
                }
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

fn miss_warning(connection: &WsConnection, reason: &str) {
    crate::events::warn(format!(
        "ws `{}` {}: {reason}; nothing sent",
        connection.route, connection.url
    ));
}

/// The frames a reply sends, as a JSON list of their texts; its close as headers.
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
        .map(|(_, text)| Value::String(text.clone()))
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
