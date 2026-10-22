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

use crate::connections::{Connections, Registration};
use crate::engine::Controller;
use crate::schema::WsClose;
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
    // Registered before the handshake task starts, so a drain that begins in between still
    // waits for this socket.
    let registration = connections.register(&session, &connection.route, crate::ws::OUTPUT);
    let events = crate::events::current();
    upgrade.on_upgrade(move |socket| async move {
        let open = run(socket, controller, session, connection, registration);
        match events {
            Some(events) => crate::events::logging_to(events, open).await,
            None => open.await,
        }
    })
}

/// What a reply hands the socket.
enum Outgoing {
    Text(String),
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

async fn run(
    socket: WebSocket,
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

    let mut step = answer(Inbound::Open).await.map_or(Step::Skip, Step::Reply);
    loop {
        let open = match step {
            Step::Reply(reply) => {
                // Frames due now go out from here, in arrival order; only the timed rest gets
                // its own task (Q16), so back-to-back messages are answered in order.
                let (now, later) = split(reply);
                let mut open = true;
                for frame in now {
                    open = open && send(&mut sink, &mut stream, frame).await;
                }
                if let Some(later) = later {
                    replies.spawn(schedule(later, outbox.clone()));
                }
                open
            }
            Step::Send(frame) => send(&mut sink, &mut stream, frame).await,
            Step::Skip => true,
            Step::End => false,
        };
        if !open {
            return;
        }
        step = tokio::select! {
            inbound = stream.next() => match inbound {
                Some(Ok(Message::Text(text))) => answer(Inbound::Text(text.as_str()))
                    .await
                    .map_or(Step::Skip, Step::Reply),
                Some(Ok(Message::Binary(bytes))) => answer(Inbound::Binary(&bytes))
                    .await
                    .map_or(Step::Skip, Step::Reply),
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
                Some(bytes) => Outgoing::Text(String::from_utf8_lossy(&bytes).into_owned()),
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

/// Writes one frame; `false` once the connection is over (a close sent, or the socket gone).
async fn send(sink: &mut Sink, stream: &mut Stream, frame: Outgoing) -> bool {
    match frame {
        Outgoing::Text(text) => sink.send(Message::Text(text.into())).await.is_ok(),
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
    let mut now: Vec<_> = reply
        .messages
        .into_iter()
        .map(|(_, text)| Outgoing::Text(text))
        .collect();
    if rest.is_empty() {
        now.extend(reply.close.map(Outgoing::Close));
        return (now, None);
    }
    let later = WsReply {
        delay: None,
        messages: rest,
        close: reply.close,
    };
    (now, Some(later))
}

/// Sends `reply`'s frames on their timers, then its close.
async fn schedule(reply: WsReply, outbox: mpsc::Sender<Outgoing>) {
    if let Some(delay) = reply.delay {
        tokio::time::sleep(delay).await;
    }
    for (delay, text) in reply.messages {
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if outbox.send(Outgoing::Text(text)).await.is_err() {
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
                Outgoing::Text(text) => text.as_str(),
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
            delay,
            messages: messages
                .into_iter()
                .map(|(delay, text)| (delay, text.to_owned()))
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
