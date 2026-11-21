//! The socket half of WebSocket Routes: accept the upgrade, then run each inbound message (and
//! the open) through a fresh [`SessionView`](crate::engine::SessionView), so Selection changes
//! apply to the next message (§10). Each reply runs on its own timer and interleaves with the
//! others (Q16); Control API `send` pushes arrive through [`Connections`].

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::FromRequestParts as _;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{Request, Response};
use axum::response::IntoResponse as _;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio::task::JoinSet;

use serde_json::Value;

use crate::connections::{Connections, Registration};
use crate::engine::Controller;
use crate::plugins::{CallConnection, Frame};
use crate::schema::WsClose;
use crate::ws::pipeline::{Codec, Encoded, WsPipeline};
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
    catalog: &crate::engine::Catalog,
    request: Request<Body>,
) -> Response<Body> {
    let (mut parts, _) = request.into_parts();
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    // Fixed for the connection's life, like its Route; a reload applies to new sockets.
    let pipeline = {
        catalog.route(&connection.route).and_then(|route| {
            WsPipeline::of(route, catalog.runtime()).expect("Route Pipeline is validated at load")
        })
    };
    // Registered before the handshake task starts, so a drain that begins in between still
    // waits for this socket.
    let registration = connections.register_shared(&session, &connection.route, crate::ws::OUTPUT);
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
        sent: Option<oneshot::Sender<()>>,
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

async fn run(
    socket: WebSocket,
    codec: Codec,
    controller: Arc<RwLock<Controller>>,
    session: String,
    connection: WsConnection,
    registration: (Registration, mpsc::Receiver<bytes::Bytes>),
) {
    let (mut sink, mut stream) = socket.split();
    let (registration, pushed) = registration;
    tokio::select! {
        biased;
        () = registration.shutdown() => {
            close_socket(&mut sink, &mut stream, WsClose {
                code: GOING_AWAY,
                reason: "decoy is shutting down".into(),
            }).await;
        }
        () = serve_socket(&mut sink, &mut stream, &codec, controller, session, connection, pushed) => {}
    }
    codec.closed();
}

async fn serve_socket(
    sink: &mut Sink,
    stream: &mut Stream,
    codec: &Codec,
    controller: Arc<RwLock<Controller>>,
    session: String,
    connection: WsConnection,
    mut pushed: mpsc::Receiver<bytes::Bytes>,
) {
    let mut queued = BufferedFrames::default();
    let (outbox, mut outgoing) = mpsc::channel(OUTBOX);
    // Dropped when the connection ends, which cancels every pending reply.
    let mut replies = JoinSet::new();
    let answer = async |inbound: Inbound<'_>| {
        let view = controller.read().await.session_view(&session);
        view.execute_ws(&connection, inbound)
    };
    // Frames are decoded and messages encoded here, one at a time, so a connection's plugin
    // calls run in order (`docs/reference/plugin-protocol.md`).
    let receive =
        async |frame: Frame, payload: &[u8], stream: &mut Stream, queued: &mut BufferedFrames| {
            match while_connected(stream, queued, codec.decode(frame, payload)).await {
                Some(Some(inbound)) => answer(inbound).await.map_or(Step::Skip, Step::Reply),
                Some(None) => Step::Skip,
                None => Step::End,
            }
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
                    open = open && send(sink, stream, &mut queued, codec, frame).await;
                }
                if let Some(later) = later {
                    replies.spawn(schedule(later, outbox.clone()));
                }
                open
            }
            Step::Send(frame) => send(sink, stream, &mut queued, codec, frame).await,
            Step::Skip => true,
            Step::End => {
                let _ = sink.close().await;
                false
            }
        };
        if !open {
            return;
        }
        step = tokio::select! {
            inbound = next_frame(stream, &mut queued) => match inbound {
                Some(Ok(Message::Text(text))) => receive(Frame::Text, text.as_bytes(), stream, &mut queued).await,
                Some(Ok(Message::Binary(bytes))) => receive(Frame::Binary, &bytes, stream, &mut queued).await,
                // The client closed; answer its close. Pending replies are dropped.
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => Step::End,
                // Ping/pong are answered by the socket and never reach Cases.
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => Step::Skip,
            },
            Some(frame) = outgoing.recv() => Step::Send(frame),
            Some(bytes) = pushed.recv() => Step::Send(Outgoing::Message {
                message: serde_json::from_slice(&bytes).expect("send queues JSON"),
                origin: Arc::from("control send"),
                sent: None,
            }),
            // Reap finished replies so the set doesn't grow on a long-lived socket.
            Some(_) = replies.join_next() => Step::Skip,
        };
    }
}

// Limit read-ahead while a connection is waiting on a transform. The frame
// limit also bounds the cost of buffering empty messages.
const MAX_BUFFERED_FRAMES: usize = 64;
const MAX_BUFFERED_BYTES: usize = 16 * 1024 * 1024;

// Charge at least one KiB per frame to account for small and empty messages.
static READ_AHEAD: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(64 * 1024);

#[derive(Default)]
struct BufferedFrames {
    frames: VecDeque<(Message, tokio::sync::SemaphorePermit<'static>)>,
    bytes: usize,
}

fn frame_size(frame: &Message) -> usize {
    match frame {
        Message::Text(text) => text.len(),
        Message::Binary(bytes) => bytes.len(),
        _ => unreachable!("only data frames are buffered"),
    }
}

impl BufferedFrames {
    fn push(&mut self, frame: Message) -> bool {
        if self.frames.len() == MAX_BUFFERED_FRAMES
            || frame_size(&frame) > MAX_BUFFERED_BYTES - self.bytes
        {
            return false;
        }
        let size = frame_size(&frame);
        let kib = size.div_ceil(1024).max(1) as u32;
        let Ok(permit) = READ_AHEAD.try_acquire_many(kib) else {
            return false;
        };
        self.bytes += size;
        self.frames.push_back((frame, permit));
        true
    }

    fn pop_front(&mut self) -> Option<Message> {
        let (frame, _permit) = self.frames.pop_front()?;
        self.bytes -= frame_size(&frame);
        Some(frame)
    }
}

/// Returns the oldest buffered frame or reads the next one from the socket.
async fn next_frame(
    stream: &mut Stream,
    queued: &mut BufferedFrames,
) -> Option<Result<Message, axum::Error>> {
    if let Some(message) = queued.pop_front() {
        return Some(Ok(message));
    }
    stream.next().await
}

/// Cancels the transform if the socket closes and buffers incoming data frames in order.
async fn while_connected<T>(
    stream: &mut Stream,
    queued: &mut BufferedFrames,
    transform: impl Future<Output = T>,
) -> Option<T> {
    tokio::pin!(transform);
    loop {
        tokio::select! {
            biased;
            result = &mut transform => return Some(result),
            message = stream.next() => match message {
                Some(Ok(message @ (Message::Text(_) | Message::Binary(_)))) => {
                    if !queued.push(message) { return None; }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return None,
            },
        }
    }
}

/// Sends a frame and signals when the Behavior can start its next delay.
async fn send(
    sink: &mut Sink,
    stream: &mut Stream,
    queued: &mut BufferedFrames,
    codec: &Codec,
    frame: Outgoing,
) -> bool {
    match frame {
        Outgoing::Message {
            message,
            origin,
            sent,
        } => {
            let output = while_connected(stream, queued, codec.encode(&message, &origin)).await;
            let open = match output {
                Some(Some(Encoded::Text(text))) => {
                    sink.send(Message::Text(text.into())).await.is_ok()
                }
                Some(Some(Encoded::Binary(bytes))) => {
                    sink.send(Message::Binary(bytes.into())).await.is_ok()
                }
                Some(None) => true,
                None => {
                    let _ = sink.close().await;
                    false
                }
            };
            if open && let Some(sent) = sent {
                let _ = sent.send(());
            }
            open
        }
        Outgoing::Close(close) => {
            close_socket(sink, stream, close).await;
            false
        }
    }
}

async fn close_socket(sink: &mut Sink, stream: &mut Stream, close: WsClose) {
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
            sent: None,
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
        let (sent, delivered) = oneshot::channel();
        let message = Outgoing::Message {
            message,
            origin: origin.clone(),
            sent: Some(sent),
        };
        if outbox.send(message).await.is_err() || delivered.await.is_err() {
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
    fn transform_read_ahead_limits_bytes_and_empty_frames() {
        let mut queued = BufferedFrames::default();
        let bytes = vec![0; MAX_BUFFERED_BYTES];
        assert!(queued.push(Message::Binary(bytes.into())));
        assert!(!queued.push(Message::Text("x".into())));
        queued.pop_front().unwrap();
        assert!(queued.push(Message::Text("x".into())));
        for _ in 1..MAX_BUFFERED_FRAMES {
            assert!(queued.push(Message::Binary(Default::default())));
        }
        assert!(!queued.push(Message::Binary(Default::default())));
        drop(queued);

        let frame = Message::Binary(vec![0; MAX_BUFFERED_BYTES].into());
        let mut sockets: [_; 4] = std::array::from_fn(|_| BufferedFrames::default());
        for socket in &mut sockets {
            assert!(socket.push(frame.clone()));
        }
        let mut next = BufferedFrames::default();
        assert!(!next.push(Message::Binary(Default::default())));
        sockets[0].pop_front().unwrap();
        assert!(next.push(frame));
    }

    #[tokio::test]
    async fn each_message_waits_for_the_previous_frame_to_be_sent() {
        let reply = WsReply {
            behavior: "live:any:b".to_owned(),
            delay: None,
            messages: vec![
                (None, Value::from("first")),
                (Some(Duration::from_millis(20)), Value::from("second")),
            ],
            close: None,
        };
        let (outbox, mut outgoing) = mpsc::channel(2);
        let running = tokio::spawn(schedule(reply, outbox));

        let Some(Outgoing::Message {
            sent: Some(sent), ..
        }) = outgoing.recv().await
        else {
            panic!("expected scheduled message");
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(50), outgoing.recv())
                .await
                .is_err()
        );

        let start = tokio::time::Instant::now();
        sent.send(()).unwrap();
        let Some(Outgoing::Message {
            message,
            sent: Some(sent),
            ..
        }) = outgoing.recv().await
        else {
            panic!("expected next scheduled message");
        };
        assert_eq!(message, Value::from("second"));
        assert!(start.elapsed() >= Duration::from_millis(20));
        sent.send(()).unwrap();
        running.await.unwrap();
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
