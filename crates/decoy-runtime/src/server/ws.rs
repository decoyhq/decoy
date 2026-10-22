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
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinSet;

use crate::connections::Connections;
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
    let events = crate::events::current();
    upgrade.on_upgrade(move |socket| async move {
        let open = run(socket, controller, connections, session, connection);
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

async fn run(
    socket: WebSocket,
    controller: Arc<RwLock<Controller>>,
    connections: Connections,
    session: String,
    connection: WsConnection,
) {
    let (mut sink, mut stream) = socket.split();
    let (_registration, mut pushed) =
        connections.register(&session, &connection.route, crate::ws::OUTPUT);
    let (outbox, mut outgoing) = mpsc::channel(OUTBOX);
    // Dropped when the connection ends, which cancels every pending reply.
    let mut replies = JoinSet::new();
    let answer = async |inbound: Inbound<'_>| {
        let view = controller.read().await.session_view(&session);
        view.execute_ws(&connection, inbound)
    };

    if let Some(reply) = answer(Inbound::Open).await {
        replies.spawn(schedule(reply, outbox.clone()));
    }
    loop {
        let frame = tokio::select! {
            inbound = stream.next() => {
                let reply = match inbound {
                    Some(Ok(Message::Text(text))) => answer(Inbound::Text(text.as_str())).await,
                    Some(Ok(Message::Binary(bytes))) => answer(Inbound::Binary(&bytes)).await,
                    // The client closed or went away; pending replies are dropped.
                    Some(Ok(Message::Close(_))) => {
                        let _ = sink.close().await;
                        return;
                    }
                    Some(Err(_)) | None => return,
                    // Ping/pong are answered by the socket and never reach Cases.
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => None,
                };
                if let Some(reply) = reply {
                    replies.spawn(schedule(reply, outbox.clone()));
                }
                continue;
            }
            Some(outgoing) = outgoing.recv() => outgoing,
            message = pushed.recv() => match message {
                Some(bytes) => Outgoing::Text(String::from_utf8_lossy(&bytes).into_owned()),
                // Connections closed on shutdown.
                None => Outgoing::Close(WsClose {
                    code: GOING_AWAY,
                    reason: "decoy is shutting down".to_owned(),
                }),
            },
            // Reap finished replies so the set doesn't grow on a long-lived socket.
            Some(_) = replies.join_next() => continue,
        };
        let sent = match frame {
            Outgoing::Text(text) => sink.send(Message::Text(text.into())).await,
            Outgoing::Close(close) => {
                let frame = CloseFrame {
                    code: close.code,
                    reason: close.reason.into(),
                };
                if sink.send(Message::Close(Some(frame))).await.is_ok() {
                    // Read until the client's close reply, so the socket isn't dropped with
                    // unread frames (the kernel would reset it and the close code be lost).
                    let reply = async { while let Some(Ok(_)) = stream.next().await {} };
                    let _ = tokio::time::timeout(CLOSE_REPLY, reply).await;
                }
                return;
            }
        };
        if sent.is_err() {
            return;
        }
    }
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
