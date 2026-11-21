//! The in-process event broadcast: one [`RequestEvent`] per
//! finished HTTP request or unary gRPC call, per WebSocket message and answered open, a
//! [`SessionEvent`] per Selection change, and a [`LogEvent`] per runtime warning or error (on
//! stderr instead while nobody listens). Sending
//! never blocks; a subscriber that falls more than [`CAPACITY`] events behind gets
//! `RecvError::Lagged` and loses the oldest ones. With no subscriber nothing is captured.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::collections::Activation;
use crate::engine::{Candidate, MissDiagnostic, SelectionSnapshot};
use crate::schema::Transport;

/// Events a subscriber can fall behind by before it starts losing them.
pub const CAPACITY: usize = 1024;
/// Each captured body keeps at most this many bytes; [`CapturedBody::size`] has the full size.
pub const BODY_CAP: usize = 64 * 1024;

pub type Sender = tokio::sync::broadcast::Sender<Event>;

#[derive(Debug, Clone)]
pub enum Event {
    Request(Arc<RequestEvent>),
    Session(SessionEvent),
    Log(LogEvent),
}

/// One finished HTTP request or unary gRPC call.
#[derive(Debug, Clone)]
pub struct RequestEvent {
    /// Increases by one per event, process-wide.
    pub id: u64,
    pub at: SystemTime,
    pub transport: Transport,
    pub session: String,
    /// The HTTP method; `POST` for gRPC.
    pub method: String,
    /// Path and query; `/package.Service/Method` for gRPC.
    pub url: String,
    /// Lowercased; gRPC metadata for a gRPC call.
    pub headers: BTreeMap<String, String>,
    pub body: CapturedBody,
    pub outcome: Outcome,
    pub response: CapturedResponse,
    pub duration: Duration,
}

/// What the engine did with a request, and why.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Hit(Match),
    Passthrough {
        matched: Match,
        /// `None` when no upstream target was resolved.
        upstream_url: Option<String>,
        /// `None` when forwarding failed.
        upstream_status: Option<u16>,
    },
    Miss(MissDiagnostic),
}

/// The winning active route entry and the explanation of how it won.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub activation: Activation,
    /// Candidates rejected before the winner, in evaluation order.
    pub skipped: Vec<Candidate>,
    /// The winning Case's `match`; every condition in it passed.
    pub conditions: Value,
    /// Empty for gRPC.
    pub path_params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedResponse {
    /// The HTTP status, or the gRPC status code for a gRPC call.
    pub status: u16,
    /// In order, repeats kept; gRPC trailers (with `grpc-message`) for a gRPC call.
    pub headers: Vec<(String, String)>,
    pub body: CapturedBody,
}

/// A body in its most-decoded form, cut to [`BODY_CAP`] bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedBody {
    /// JSON text when `decoded`, otherwise the raw body.
    pub bytes: Vec<u8>,
    /// The Pipeline decoded the body (protobuf → JSON).
    pub decoded: bool,
    /// Size of the full body (the full JSON text when `decoded`) before the cap.
    pub size: usize,
}

impl CapturedBody {
    pub fn raw(body: &[u8]) -> Self {
        Self {
            bytes: body[..body.len().min(BODY_CAP)].to_vec(),
            decoded: false,
            size: body.len(),
        }
    }

    pub fn decoded(value: &Value) -> Self {
        let text = serde_json::to_vec(value).expect("JSON value serializes");
        Self {
            decoded: true,
            ..Self::raw(&text)
        }
    }
}

/// Captures a streamed body as it passes, cut to [`BODY_CAP`], and hands it over when dropped:
/// when the stream ends or the client hangs up.
pub(crate) struct StreamCapture {
    bytes: Vec<u8>,
    size: usize,
    done: Option<Box<dyn FnOnce(CapturedBody) + Send>>,
}

impl StreamCapture {
    pub(crate) fn new(done: impl FnOnce(CapturedBody) + Send + 'static) -> Self {
        Self {
            bytes: Vec::new(),
            size: 0,
            done: Some(Box::new(done)),
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        let room = BODY_CAP - self.bytes.len();
        self.bytes
            .extend_from_slice(&chunk[..chunk.len().min(room)]);
        self.size += chunk.len();
    }
}

impl Drop for StreamCapture {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            done(CapturedBody {
                bytes: std::mem::take(&mut self.bytes),
                decoded: false,
                size: self.size,
            });
        }
    }
}

/// A Selection created, changed, or ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEvent {
    pub session: String,
    pub change: SessionChange,
    pub source: SessionSource,
    /// The Selection after the change; the defaults once ended.
    pub selection: SelectionSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionChange {
    /// The Session's first mutation created its Selection; the verb's own event follows.
    Created,
    UseCollection,
    UseRoute,
    Reset,
    Ended,
    /// A config reload changed the Selection: a new default Collection, or a fallback or dropped
    /// override because the old one is gone.
    Reload,
}

/// Who changed the Selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionSource {
    /// A direct `Controller` call in this process, e.g. the TUI.
    #[default]
    InProcess,
    ControlApi,
}

/// A runtime log line: a warning or error (a predicate that failed to evaluate), or a line a plugin
/// wrote to its stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEvent {
    pub level: Level,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Plain output, e.g. a plugin's stderr.
    Info,
    Warn,
    Error,
}

tokio::task_local! {
    /// The broadcast of the server whose request (or accept loop) is running.
    static SINK: Sender;
}

/// Runs `future` with [`warn`] and [`error`] going to `events`; the sites that log (matching,
/// forwarding, accept loops) don't hold a `Controller`.
pub(crate) async fn logging_to<F: Future>(events: Sender, future: F) -> F::Output {
    SINK.scope(events, future).await
}

/// The events of the server whose scope this runs in, for work it defers past that scope (a
/// response body hyper polls later); hand it to [`logging_to_sync`] there.
pub(crate) fn current() -> Option<Sender> {
    SINK.try_with(Sender::clone).ok()
}

pub(crate) fn logging_to_sync<R>(events: Option<Sender>, f: impl FnOnce() -> R) -> R {
    match events {
        Some(events) => SINK.sync_scope(events, f),
        None => f(),
    }
}

pub(crate) fn info(message: impl Into<String>) {
    log(Level::Info, message.into());
}

pub(crate) fn warn(message: impl Into<String>) {
    log(Level::Warn, message.into());
}

pub(crate) fn error(message: impl Into<String>) {
    log(Level::Error, message.into());
}

/// An [`Event::Log`] while someone subscribes; otherwise stderr, as headless `decoy serve` prints it.
fn log(level: Level, message: String) {
    let event = Event::Log(LogEvent { level, message });
    // `send` fails (and hands the event back) when nobody subscribes.
    let unsent = match SINK.try_with(Sender::clone) {
        Ok(events) => events.send(event).err().map(|unsent| unsent.0),
        Err(_) => Some(event),
    };
    if let Some(Event::Log(LogEvent { message, .. })) = unsent {
        eprintln!("decoy serve: {message}");
    }
}

pub(crate) fn channel() -> Sender {
    tokio::sync::broadcast::channel(CAPACITY).0
}

pub(crate) fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bodies_keep_the_first_64kb_and_the_full_size() {
        let body = CapturedBody::raw(&vec![b'a'; BODY_CAP + 10]);
        assert_eq!((body.bytes.len(), body.size), (BODY_CAP, BODY_CAP + 10));

        let body = CapturedBody::decoded(&serde_json::json!({ "id": "a-1" }));
        assert_eq!(body.bytes, br#"{"id":"a-1"}"#);
        assert_eq!((body.decoded, body.size), (true, 12));
    }
}
