//! The socket half of WebSocket Routes: accept the upgrade, then run each inbound message (and
//! the open) through a fresh [`SessionView`](crate::engine::SessionView), so Selection changes
//! apply to the next message (§10). Each reply runs on its own timer and interleaves with the
//! others (Q16); Control API `send` pushes arrive through [`Connections`].

use std::sync::Arc;

use axum::body::Body;
use axum::extract::FromRequestParts as _;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{Request, Response};
use axum::response::IntoResponse as _;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinSet;

use serde_json::Value;

use crate::connections::{Connections, Registration};
use crate::engine::Controller;
use crate::plugins::{CallConnection, Frame};
use crate::schema::WsClose;
use crate::ws::pipeline::{Encoded, WsPipeline, preview};
use crate::ws::{Inbound, WsConnection, WsReply};

/// Frames a connection's replies may have waiting for the socket.
const OUTBOX: usize = 64;

/// How long a server-side close waits for the client's close reply.
const CLOSE_REPLY: std::time::Duration = std::time::Duration::from_secs(1);

/// Close code for a server going away (RFC 6455 §7.4.1).
const GOING_AWAY: u16 = 1001;

/// Whether the request asks to upgrade to a WebSocket.
pub(super) fn is_upgrade<B>(request: &Request<B>) -> bool {
    request.method() == axum::http::Method::GET
        && request
            .headers()
            .get(axum::http::header::UPGRADE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

pub(super) async fn accept(
    controller: Arc<RwLock<Controller>>,
    connections: Connections,
    session: String,
    connection: WsConnection,
    request: Request<Body>,
) -> Response<Body> {
    let (mut parts, _) = request.into_parts();
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    // Fixed for the connection's life, like its Route; a reload applies to new sockets.
    let pipeline = {
        let catalog = controller.read().await.catalog();
        catalog
            .route(&connection.route)
            .and_then(|route| WsPipeline::of(route, &catalog.runtime().plugins))
    };
    let output = match pipeline {
        Some(_) => crate::ws::OUTPUT_PIPELINE,
        None => crate::ws::OUTPUT,
    };
    // Registered before the handshake task starts, so a drain that begins in between still
    // waits for this socket.
    let registration = connections.register(&session, &connection.route, output);
    let events = crate::events::current();
    upgrade.on_upgrade(move |socket| async move {
        let codec = Codec {
            pipeline,
            call: CallConnection {
                id: next_connection_id(),
                route: connection.route.clone(),
                session: session.clone(),
            },
        };
        let open = run(socket, codec, controller, session, connection, registration);
        match events {
            Some(events) => crate::events::logging_to(events, open).await,
            None => open.await,
        }
    })
}

/// `c-1`, `c-2`, …: a connection's id in plugin calls and error events.
fn next_connection_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    format!(
        "c-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// What a reply hands the socket.
enum Outgoing {
    /// A message, encoded as it goes out; `origin` names where it came from in error events.
    Message {
        message: Value,
        origin: Arc<str>,
    },
    Close(WsClose),
}

/// What one turn of the loop does next.
enum Step {
    Reply(WsReply),
    Send(Outgoing),
    Skip,
    End,
}

type Sink = SplitSink<WebSocket, Message>;
type Stream = SplitStream<WebSocket>;

/// Turns frames into messages and back: as text, or through the Route's Pipeline.
struct Codec {
    pipeline: Option<WsPipeline>,
    call: CallConnection,
}

impl Codec {
    /// The inbound message a frame stands for; `None` after a plugin error (logged).
    async fn decode<'a>(&self, frame: Frame, payload: &'a [u8]) -> Option<Inbound<'a>> {
        let Some(pipeline) = &self.pipeline else {
            return Some(match frame {
                Frame::Binary => Inbound::Binary(payload),
                Frame::Text => {
                    Inbound::Text(std::str::from_utf8(payload).expect("text frames are UTF-8"))
                }
            });
        };
        match pipeline.decode(frame, payload, &self.call).await {
            Ok(message) => Some(Inbound::Decoded(message)),
            Err(error @ crate::ws::pipeline::PipelineError::Miss { .. }) => Some(Inbound::Missed {
                payload,
                reason: error.to_string(),
            }),
            Err(error) => {
                crate::events::error(self.report(
                    &error,
                    "decoding a frame",
                    &format!("frame: {}", preview(frame, payload)),
                ));
                None
            }
        }
    }

    /// The frame for `message`; `None` after a plugin error (logged).
    async fn encode(&self, message: &Value, origin: &str) -> Option<Encoded> {
        let Some(pipeline) = &self.pipeline else {
            return Some(Encoded::Text(crate::ws::encode_text(message)));
        };
        match pipeline.encode(message, Some(&self.call)).await {
            Ok(encoded) => Some(encoded),
            Err(error) => {
                crate::events::error(self.report(&error, origin, &format!("message: {message}")));
                None
            }
        }
    }

    /// One error event with what's needed to fix it: where, what failed, the value, the stack.
    fn report(
        &self,
        error: &crate::ws::pipeline::PipelineError,
        doing: &str,
        value: &str,
    ) -> String {
        let CallConnection { id, route, session } = &self.call;
        let stack = error.stack().map_or(String::new(), |stack| {
            let lines: Vec<_> = stack.lines().map(|line| format!("\n    {line}")).collect();
            format!("\n  stack:{}", lines.concat())
        });
        format!(
            "ws `{route}` connection {id} (session {session}), {doing}: {error}\n  {value}{stack}"
        )
    }

    /// Lets the plugins drop what they kept for this connection.
    fn closed(&self) {
        for plugin in self.pipeline.iter().flat_map(WsPipeline::plugins) {
            plugin.close(&self.call.id);
        }
    }
}

async fn run(
    socket: WebSocket,
    codec: Codec,
    controller: Arc<RwLock<Controller>>,
    session: String,
    connection: WsConnection,
    registration: (Registration, mpsc::Receiver<Vec<u8>>),
) {
    serve_socket(
        socket,
        &codec,
        controller,
        session,
        connection,
        registration,
    )
    .await;
    codec.closed();
}

async fn serve_socket(
    socket: WebSocket,
    codec: &Codec,
    controller: Arc<RwLock<Controller>>,
    session: String,
    connection: WsConnection,
    (_registration, mut pushed): (Registration, mpsc::Receiver<Vec<u8>>),
) {
    let (mut sink, mut stream) = socket.split();
    let (outbox, mut outgoing) = mpsc::channel(OUTBOX);
    // Dropped when the connection ends, which cancels every pending reply.
    let mut replies = JoinSet::new();
    let answer = async |inbound: Inbound<'_>| {
        let view = controller.read().await.session_view(&session);
        view.execute_ws(&connection, inbound)
    };
    // Frames are decoded and messages encoded here, one at a time, so a connection's plugin
    // calls run in order (`docs/design/plugin-protocol.md`).
    let receive = async |frame: Frame, payload: &[u8]| match codec.decode(frame, payload).await {
        Some(inbound) => answer(inbound).await.map_or(Step::Skip, Step::Reply),
        None => Step::Skip,
    };

    let mut step = answer(Inbound::Open).await.map_or(Step::Skip, Step::Reply);
    loop {
        let open = match step {
            Step::Reply(reply) => {
                // Frames due now go out from here, in arrival order; only the timed rest gets
                // its own task (Q16), so back-to-back messages are answered in order.
                let (now, later) = split(reply);
                let mut open = true;
                for frame in now {
                    open = open && send(&mut sink, &mut stream, codec, frame).await;
                }
                if let Some(later) = later {
                    replies.spawn(schedule(later, outbox.clone()));
                }
                open
            }
            Step::Send(frame) => send(&mut sink, &mut stream, codec, frame).await,
            Step::Skip => true,
            Step::End => false,
        };
        if !open {
            return;
        }
        step = tokio::select! {
            inbound = stream.next() => match inbound {
                Some(Ok(Message::Text(text))) => receive(Frame::Text, text.as_bytes()).await,
                Some(Ok(Message::Binary(bytes))) => receive(Frame::Binary, &bytes).await,
                // The client closed; answer its close. Pending replies are dropped.
                Some(Ok(Message::Close(_))) => {
                    let _ = sink.close().await;
                    Step::End
                }
                Some(Err(_)) | None => Step::End,
                // Ping/pong are answered by the socket and never reach Cases.
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => Step::Skip,
            },
            Some(frame) = outgoing.recv() => Step::Send(frame),
            message = pushed.recv() => Step::Send(match message {
                Some(bytes) => Outgoing::Message {
                    message: pushed_message(codec, &bytes),
                    origin: Arc::from("control send"),
                },
                // Connections closed on shutdown.
                None => Outgoing::Close(WsClose {
                    code: GOING_AWAY,
                    reason: "decoy is shutting down".to_owned(),
                }),
            }),
            // Reap finished replies so the set doesn't grow on a long-lived socket.
            Some(_) = replies.join_next() => Step::Skip,
        };
    }
}

/// A Control API `send`: the message as JSON for a Pipeline, its text frame otherwise.
fn pushed_message(codec: &Codec, bytes: &[u8]) -> Value {
    match codec.pipeline {
        Some(_) => serde_json::from_slice(bytes).expect("send pushes JSON to pipeline connections"),
        None => Value::String(String::from_utf8_lossy(bytes).into_owned()),
    }
}

/// Writes one frame; `false` once the connection is over (a close sent, or the socket gone). A
/// message that fails to encode is skipped (the error is logged).
async fn send(sink: &mut Sink, stream: &mut Stream, codec: &Codec, frame: Outgoing) -> bool {
    match frame {
        Outgoing::Message { message, origin } => {
            let frame = match codec.encode(&message, &origin).await {
                Some(Encoded::Text(text)) => Message::Text(text.into()),
                Some(Encoded::Binary(bytes)) => Message::Binary(bytes.into()),
                None => return true,
            };
            sink.send(frame).await.is_ok()
        }
        Outgoing::Close(close) => {
            let frame = CloseFrame {
                code: close.code,
                reason: close.reason.into(),
            };
            if sink.send(Message::Close(Some(frame))).await.is_ok() {
                // Read until the client's close reply, so the socket isn't dropped with unread
                // frames (the kernel would reset it and the close code be lost).
                let reply = async { while let Some(Ok(_)) = stream.next().await {} };
                let _ = tokio::time::timeout(CLOSE_REPLY, reply).await;
            }
            false
        }
    }
}

/// Splits `reply` into the frames due at once (leading ones without a delay, then its close if
/// nothing is left to wait for) and the timed rest.
fn split(mut reply: WsReply) -> (Vec<Outgoing>, Option<WsReply>) {
    if reply.delay.is_some() {
        return (Vec::new(), Some(reply));
    }
    let due = reply
        .messages
        .iter()
        .position(|(delay, _)| delay.is_some())
        .unwrap_or(reply.messages.len());
    let rest = reply.messages.split_off(due);
    let origin = origin(&reply);
    let mut now: Vec<_> = reply
        .messages
        .into_iter()
        .map(|(_, message)| Outgoing::Message {
            message,
            origin: origin.clone(),
        })
        .collect();
    if rest.is_empty() {
        now.extend(reply.close.map(Outgoing::Close));
        return (now, None);
    }
    let later = WsReply {
        behavior: reply.behavior,
        delay: None,
        messages: rest,
        close: reply.close,
    };
    (now, Some(later))
}

fn origin(reply: &WsReply) -> Arc<str> {
    Arc::from(format!("behavior `{}`", reply.behavior))
}

/// Sends `reply`'s frames on their timers, then its close.
async fn schedule(reply: WsReply, outbox: mpsc::Sender<Outgoing>) {
    if let Some(delay) = reply.delay {
        tokio::time::sleep(delay).await;
    }
    let origin = origin(&reply);
    for (delay, message) in reply.messages {
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let message = Outgoing::Message {
            message,
            origin: origin.clone(),
        };
        if outbox.send(message).await.is_err() {
            return;
        }
    }
    if let Some(close) = reply.close {
        let _ = outbox.send(Outgoing::Close(close)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn texts(frames: &[Outgoing]) -> Vec<&str> {
        frames
            .iter()
            .map(|frame| match frame {
                Outgoing::Message { message, .. } => message.as_str().unwrap(),
                Outgoing::Close(_) => "<close>",
            })
            .collect()
    }

    #[test]
    fn frames_without_a_delay_go_out_at_once_and_the_rest_waits() {
        let close = Some(WsClose {
            code: 4001,
            reason: String::new(),
        });
        let second = Some(Duration::from_millis(5));
        let reply = |delay, messages: Vec<(Option<Duration>, &str)>| WsReply {
            behavior: "live:any:b".to_owned(),
            delay,
            messages: messages
                .into_iter()
                .map(|(delay, text)| (delay, Value::from(text)))
                .collect(),
            close: close.clone(),
        };

        let (now, later) = split(reply(None, vec![(None, "a"), (None, "b")]));
        assert_eq!(texts(&now), ["a", "b", "<close>"]);
        assert!(later.is_none());

        let (now, later) = split(reply(None, vec![(None, "a"), (second, "b"), (None, "c")]));
        assert_eq!(texts(&now), ["a"]);
        let later = later.unwrap();
        assert_eq!(later.messages.len(), 2);
        assert_eq!(later.close, close);

        let (now, later) = split(reply(second, vec![(None, "a")]));
        assert!(now.is_empty());
        assert_eq!(later.unwrap().delay, second);
    }
}
