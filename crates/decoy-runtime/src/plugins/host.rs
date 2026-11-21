//! One plugin process: the handshake, calls matched to their results by id, host calls, and its
//! stderr. `docs/design/plugin-protocol.md` is the contract.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rmpv::Value as Msgpack;
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};

use super::wire::{self, field, message};

/// The protocol version Decoy speaks.
pub const PROTOCOL: u64 = 1;

/// stderr lines kept for an exit report.
const STDERR_TAIL: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Input,
    Output,
    Check,
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
            Self::Check => "check",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    Text,
    Binary,
}

/// `ctx.connection` of a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallConnection {
    pub id: String,
    pub route: String,
    pub session: String,
}

/// One `call` to a step.
#[derive(Debug, Clone)]
pub struct Call<'a> {
    pub op: Op,
    pub input: Msgpack,
    pub frame: Option<Frame>,
    pub connection: Option<&'a CallConnection>,
}

/// Why a call returned no value. The text reaches error events and Control API bodies.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    /// The step called `miss(reason)`.
    #[error("{0}")]
    Miss(String),
    #[error("{message}")]
    Failed {
        message: String,
        /// The plugin's own stack trace, when it sent one.
        stack: Option<String>,
    },
    #[error("timeout after {}", humantime::format_duration(*.0))]
    Timeout(Duration),
    #[error("the plugin process is gone")]
    Gone,
    /// The call wouldn't fit one protocol message; the plugin never sees it.
    #[error("{0} bytes is over the {max} byte message limit", max = wire::MAX_MESSAGE)]
    TooLarge(usize),
}

/// What a plugin may ask Decoy for (`host` messages).
pub trait Host: Send + Sync + 'static {
    fn call(&self, op: &str, args: &[Msgpack]) -> Result<Msgpack, String>;
}

/// The task reading a plugin's stdout; it ends with why, if the plugin broke the protocol.
pub(super) type Reader = tokio::task::JoinHandle<Result<(), String>>;

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Msgpack, CallError>>>>;

/// A plugin that answered `ready`.
#[derive(Debug)]
pub struct Plugin {
    /// Имя Transformer в конфигурации.
    pub key: String,
    pub name: String,
    pub version: String,
    directions: Vec<Op>,
    timeout: Duration,
    outbox: mpsc::UnboundedSender<Msgpack>,
    pending: Arc<Pending>,
    next_id: AtomicU64,
}

impl Plugin {
    /// `env: typed-envelope@1.0.0`, as logs and errors name it.
    pub fn label(&self) -> String {
        format!("{}: {}@{}", self.key, self.name, self.version)
    }

    /// Проверяет направление, объявленное worker при запуске.
    pub fn supports(&self, op: Op) -> bool {
        self.directions.contains(&op)
    }

    pub async fn call(&self, call: Call<'_>) -> Result<Msgpack, CallError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, result) = oneshot::channel();
        self.pending
            .lock()
            .expect("plugin calls lock")
            .insert(id, reply);
        let mut fields = vec![
            ("id", Msgpack::from(id)),
            ("op", Msgpack::from(call.op.name())),
            ("input", call.input),
        ];
        if let Some(frame) = call.frame {
            let frame = match frame {
                Frame::Text => "text",
                Frame::Binary => "binary",
            };
            fields.push(("frame", Msgpack::from(frame)));
        }
        if let Some(connection) = call.connection {
            fields.push((
                "connection",
                Msgpack::Map(vec![
                    ("id".into(), connection.id.as_str().into()),
                    ("route".into(), connection.route.as_str().into()),
                    ("session".into(), connection.session.as_str().into()),
                ]),
            ));
        }
        let call = message("call", fields);
        // Over the limit, the plugin would end on a protocol error, and Decoy with it.
        let size = wire::encoded_len(&call);
        if size > wire::MAX_MESSAGE {
            self.forget(id);
            return Err(CallError::TooLarge(size));
        }
        if self.outbox.send(call).is_err() {
            self.forget(id);
            return Err(CallError::Gone);
        }
        match tokio::time::timeout(self.timeout, result).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(CallError::Gone),
            Err(_) => {
                // A late result finds no entry and is dropped.
                self.forget(id);
                Err(CallError::Timeout(self.timeout))
            }
        }
    }

    /// Lets the plugin drop what it keeps for `connection`.
    pub fn close(&self, connection: &str) {
        let _ = self
            .outbox
            .send(message("close", [("connection", connection.into())]));
    }

    pub(super) fn shutdown(&self) {
        let _ = self.outbox.send(message("shutdown", []));
    }

    fn forget(&self, id: u64) {
        self.pending.lock().expect("plugin calls lock").remove(&id);
    }
}

/// The last stderr lines of a plugin, and its label once known, shared with the stderr reader.
#[derive(Debug, Default)]
pub(super) struct Stderr {
    tail: Mutex<VecDeque<String>>,
    label: OnceLock<String>,
}

impl Stderr {
    /// `\n  stderr:\n    <line>…`, or nothing when the plugin wrote none.
    pub(super) fn tail(&self) -> String {
        let tail = self.tail.lock().expect("plugin stderr lock");
        if tail.is_empty() {
            return String::new();
        }
        let lines: Vec<_> = tail.iter().map(|line| format!("\n    {line}")).collect();
        format!("\n  stderr:{}", lines.concat())
    }
}

/// Forwards each stderr line as `[<label>] <line>`, keeping the last few for exit reports.
pub(super) async fn forward_stderr(
    key: String,
    stderr: impl AsyncRead + Unpin,
    shared: Arc<Stderr>,
    events: crate::events::Sender,
) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let label = shared.label.get().map_or(key.as_str(), String::as_str);
        let shown = format!("[{label}] {line}");
        crate::events::logging_to(events.clone(), async { crate::events::info(shown) }).await;
        let mut tail = shared.tail.lock().expect("plugin stderr lock");
        if tail.len() == STDERR_TAIL {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

/// Speaks the protocol over `stdout`/`stdin`: sends `hello` and resolves once the plugin answers
/// `ready`. The returned future ends when the plugin's stdout does, with why if it broke the
/// protocol.
pub(super) fn connect(
    key: &str,
    timeout: Duration,
    stdout: impl AsyncRead + Unpin + Send + 'static,
    mut stdin: impl AsyncWrite + Unpin + Send + 'static,
    host: Arc<dyn Host>,
    stderr: Arc<Stderr>,
) -> (impl Future<Output = Result<Arc<Plugin>, String>>, Reader) {
    let (outbox, mut outgoing) = mpsc::unbounded_channel::<Msgpack>();
    tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            if wire::write(&mut stdin, &message).await.is_err() {
                return;
            }
        }
    });
    let pending: Arc<Pending> = Arc::default();
    let (ready, readied) = oneshot::channel();
    let reader = tokio::spawn(read_loop(
        stdout,
        outbox.clone(),
        pending.clone(),
        host,
        ready,
    ));
    let key = key.to_owned();
    let decoy = env!("CARGO_PKG_VERSION");
    let _ = outbox.send(message(
        "hello",
        [("protocol", PROTOCOL.into()), ("decoy", decoy.into())],
    ));
    let handshake = async move {
        let ready = readied
            .await
            .map_err(|_| "it closed stdout before answering `hello`".to_owned())??;
        let plugin = Plugin {
            key,
            name: ready.name,
            version: ready.version,
            directions: ready.directions,
            timeout,
            outbox,
            pending,
            next_id: AtomicU64::new(1),
        };
        let _ = stderr.label.set(plugin.label());
        Ok(Arc::new(plugin))
    };
    (handshake, reader)
}

struct Ready {
    name: String,
    version: String,
    directions: Vec<Op>,
}

async fn read_loop(
    mut stdout: impl AsyncRead + Unpin,
    outbox: mpsc::UnboundedSender<Msgpack>,
    pending: Arc<Pending>,
    host: Arc<dyn Host>,
    ready: oneshot::Sender<Result<Ready, String>>,
) -> Result<(), String> {
    let mut ready = Some(ready);
    let result = loop {
        let message = match wire::read(&mut stdout).await {
            Ok(Some(message)) => message,
            Ok(None) => break Ok(()),
            Err(reason) => break Err(reason),
        };
        match field(&message, "kind").as_str() {
            Some("ready") => {
                if let Some(ready) = ready.take() {
                    let _ = ready.send(parse_ready(&message));
                }
            }
            Some("result") => {
                let Some(id) = field(&message, "id").as_u64() else {
                    break Err("`result` without an `id`".to_owned());
                };
                let reply = pending.lock().expect("plugin calls lock").remove(&id);
                if let Some(reply) = reply {
                    let _ = reply.send(parse_result(&message));
                }
            }
            Some("host") => {
                let id = field(&message, "id").clone();
                let op = field(&message, "op").as_str().unwrap_or_default();
                let args = field(&message, "args")
                    .as_array()
                    .map_or(&[][..], Vec::as_slice);
                let answer = match host.call(op, args) {
                    Ok(value) => message_with(id, [("ok", true.into()), ("value", value)]),
                    Err(error) => message_with(id, [("ok", false.into()), ("error", error.into())]),
                };
                let _ = outbox.send(answer);
            }
            Some(other) => break Err(format!("unknown message kind `{other}`")),
            None => unreachable!("wire::read checks `kind`"),
        }
    };
    // Calls still waiting will never get a result.
    pending.lock().expect("plugin calls lock").clear();
    if let Some(ready) = ready {
        let reason = match &result {
            Ok(()) => "it closed stdout before answering `hello`".to_owned(),
            Err(reason) => reason.clone(),
        };
        let _ = ready.send(Err(reason));
    }
    result
}

fn message_with(id: Msgpack, fields: [(&'static str, Msgpack); 2]) -> Msgpack {
    message("hostResult", std::iter::once(("id", id)).chain(fields))
}

fn parse_ready(message: &Msgpack) -> Result<Ready, String> {
    let protocol = field(message, "protocol");
    if protocol.as_u64() != Some(PROTOCOL) {
        return Err(format!(
            "it speaks plugin protocol {protocol}; this Decoy speaks {PROTOCOL}"
        ));
    }
    let text = |name: &str| {
        field(message, name)
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("`ready` has no string `{name}`"))
    };
    let directions = field(message, "directions")
        .as_array()
        .and_then(|values| {
            values
                .iter()
                .map(|value| match value.as_str() {
                    Some("input") => Some(Op::Input),
                    Some("output") => Some(Op::Output),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
        })
        .ok_or("`ready` has no valid list of directions")?;
    Ok(Ready {
        name: text("name")?,
        version: text("version")?,
        directions,
    })
}

fn parse_result(message: &Msgpack) -> Result<Msgpack, CallError> {
    if field(message, "ok").as_bool() == Some(true) {
        return Ok(field(message, "value").clone());
    }
    if let Some(reason) = field(message, "miss").as_str() {
        return Err(CallError::Miss(reason.to_owned()));
    }
    Err(CallError::Failed {
        message: field(message, "error")
            .as_str()
            .unwrap_or("the step failed without a message")
            .to_owned(),
        stack: field(message, "stack").as_str().map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use tokio::io::{DuplexStream, duplex};

    use super::*;

    struct Echo;

    impl Host for Echo {
        fn call(&self, op: &str, args: &[Msgpack]) -> Result<Msgpack, String> {
            match op {
                "echo" => Ok(args.first().cloned().unwrap_or(Msgpack::Nil)),
                _ => Err(format!("unknown host call {op}")),
            }
        }
    }

    /// The plugin's ends: what Decoy wrote, and where to write back.
    struct Fake {
        from_decoy: DuplexStream,
        to_decoy: DuplexStream,
    }

    impl Fake {
        async fn next(&mut self) -> Msgpack {
            wire::read(&mut self.from_decoy).await.unwrap().unwrap()
        }
        async fn send(&mut self, kind: &str, fields: Vec<(&'static str, Msgpack)>) {
            wire::write(&mut self.to_decoy, &message(kind, fields))
                .await
                .unwrap();
        }
    }

    fn start(
        timeout: Duration,
    ) -> (
        impl Future<Output = Result<Arc<Plugin>, String>>,
        Reader,
        Fake,
    ) {
        let (stdout, to_decoy) = duplex(1 << 16);
        let (stdin, from_decoy) = duplex(1 << 16);
        let (handshake, reader) = connect(
            "env",
            timeout,
            stdout,
            stdin,
            Arc::new(Echo),
            Arc::default(),
        );
        (
            handshake,
            reader,
            Fake {
                from_decoy,
                to_decoy,
            },
        )
    }

    fn ready(protocol: u64) -> Vec<(&'static str, Msgpack)> {
        vec![
            ("protocol", protocol.into()),
            ("name", "envelope".into()),
            ("version", "1.0.0".into()),
            ("directions", Msgpack::Array(vec!["input".into()])),
        ]
    }

    #[tokio::test]
    async fn the_handshake_reads_the_workers_name_and_directions() {
        let (handshake, _reader, mut fake) = start(Duration::from_secs(1));
        let hello = fake.next().await;
        assert_eq!(field(&hello, "kind").as_str(), Some("hello"));
        assert_eq!(field(&hello, "protocol").as_u64(), Some(PROTOCOL));
        fake.send("ready", ready(PROTOCOL)).await;
        let plugin = handshake.await.unwrap();
        assert_eq!(plugin.label(), "env: envelope@1.0.0");
        assert!(plugin.supports(Op::Input) && !plugin.supports(Op::Output));
    }

    #[tokio::test]
    async fn another_protocol_or_an_early_close_fails_the_handshake() {
        let (handshake, _reader, mut fake) = start(Duration::from_secs(1));
        fake.next().await;
        fake.send("ready", ready(2)).await;
        assert_eq!(
            handshake.await.unwrap_err(),
            "it speaks plugin protocol 2; this Decoy speaks 1"
        );

        let (handshake, _reader, fake) = start(Duration::from_secs(1));
        drop(fake);
        assert!(handshake.await.unwrap_err().contains("closed stdout"));
    }

    #[tokio::test]
    async fn a_call_over_the_message_limit_fails_without_reaching_the_plugin() {
        let (handshake, _reader, mut fake) = start(Duration::from_millis(200));
        fake.next().await;
        fake.send("ready", ready(PROTOCOL)).await;
        let plugin = handshake.await.unwrap();
        let call = |input: Msgpack| Call {
            op: Op::Input,
            input,
            frame: Some(Frame::Binary),
            connection: None,
        };

        let huge = Msgpack::Binary(vec![0; wire::MAX_MESSAGE]);
        let error = plugin.call(call(huge)).await.unwrap_err();
        assert!(matches!(error, CallError::TooLarge(size) if size > wire::MAX_MESSAGE));

        // The plugin's next message is the small call: the huge one was never written.
        let (result, ()) = tokio::join!(plugin.call(call("small".into())), async {
            let next = fake.next().await;
            assert_eq!(field(&next, "input").as_str(), Some("small"));
            let id = field(&next, "id").clone();
            fake.send(
                "result",
                vec![("id", id), ("ok", true.into()), ("value", "ok".into())],
            )
            .await;
        });
        assert_eq!(result, Ok("ok".into()));
    }

    #[tokio::test]
    async fn calls_get_their_own_results_misses_errors_and_timeouts() {
        let (handshake, _reader, mut fake) = start(Duration::from_millis(200));
        fake.next().await;
        fake.send("ready", ready(PROTOCOL)).await;
        let plugin = handshake.await.unwrap();
        let connection = CallConnection {
            id: "c-1".into(),
            route: "live".into(),
            session: "s-1".into(),
        };
        let call = |input: &str| Call {
            op: Op::Input,
            input: input.into(),
            frame: Some(Frame::Binary),
            connection: Some(&connection),
        };

        let (first, second, third, fourth, ()) = tokio::join!(
            plugin.call(call("a")),
            plugin.call(call("b")),
            plugin.call(call("c")),
            plugin.call(call("d")),
            async {
                let calls = [
                    fake.next().await,
                    fake.next().await,
                    fake.next().await,
                    fake.next().await,
                ];
                let id = |index: usize| field(&calls[index], "id").clone();
                assert_eq!(field(&calls[0], "frame").as_str(), Some("binary"));
                assert_eq!(
                    field(field(&calls[0], "connection"), "session").as_str(),
                    Some("s-1")
                );
                // Answered out of order; the fourth never.
                fake.send(
                    "result",
                    vec![("id", id(1)), ("ok", true.into()), ("value", "B".into())],
                )
                .await;
                fake.send(
                    "result",
                    vec![("id", id(0)), ("ok", false.into()), ("miss", "nope".into())],
                )
                .await;
                fake.send(
                    "result",
                    vec![
                        ("id", id(2)),
                        ("ok", false.into()),
                        ("error", "bad".into()),
                        ("stack", "at x".into()),
                    ],
                )
                .await;
            }
        );
        assert_eq!(first, Err(CallError::Miss("nope".into())));
        assert_eq!(second, Ok("B".into()));
        assert_eq!(
            third,
            Err(CallError::Failed {
                message: "bad".into(),
                stack: Some("at x".into())
            })
        );
        assert_eq!(fourth, Err(CallError::Timeout(Duration::from_millis(200))));
        assert_eq!(fourth.unwrap_err().to_string(), "timeout after 200ms");
    }

    #[tokio::test]
    async fn host_calls_are_answered_and_a_closed_stdout_ends_pending_calls() {
        let (handshake, reader, mut fake) = start(Duration::from_secs(5));
        fake.next().await;
        fake.send("ready", ready(PROTOCOL)).await;
        let plugin = handshake.await.unwrap();

        fake.send(
            "host",
            vec![
                ("id", 7.into()),
                ("op", "echo".into()),
                ("args", Msgpack::Array(vec!["hi".into()])),
            ],
        )
        .await;
        let answer = fake.next().await;
        assert_eq!(field(&answer, "kind").as_str(), Some("hostResult"));
        assert_eq!(field(&answer, "id").as_u64(), Some(7));
        assert_eq!(field(&answer, "value").as_str(), Some("hi"));

        let call = plugin.call(Call {
            op: Op::Output,
            input: Msgpack::Nil,
            frame: None,
            connection: None,
        });
        let (result, ()) = tokio::join!(call, async {
            fake.next().await;
            drop(fake);
        });
        assert_eq!(result, Err(CallError::Gone));
        assert_eq!(reader.await.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn a_broken_message_ends_the_reader_with_why() {
        let (_handshake, reader, mut fake) = start(Duration::from_secs(1));
        fake.next().await;
        fake.send("gossip", Vec::new()).await;
        assert_eq!(
            reader.await.unwrap(),
            Err("unknown message kind `gossip`".to_owned())
        );
    }
}
