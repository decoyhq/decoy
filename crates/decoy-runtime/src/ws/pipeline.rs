//! Converts WebSocket frames to Case inputs and Behavior payloads to frames.

use super::Inbound;

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use rmpv::Value as Msgpack;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};

use crate::http::RuntimeConfig;
use crate::plugins::value::{to_json, to_msgpack};
use crate::plugins::{Call, CallConnection, CallError, Frame, Op, Plugin};
use crate::schema::Route;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Encoded {
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct WsPipeline {
    input: Vec<Step>,
    output: Vec<Step>,
}

#[derive(Debug, Clone)]
struct Step {
    name: String,
    runner: Runner,
}

#[derive(Debug, Clone)]
enum Runner {
    Worker(Arc<Plugin>),
    Command { argv: Vec<String>, dir: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    #[error("transformer `{name}`: {reason}")]
    Miss { name: String, reason: String },
    #[error("transformer `{name}` {op} failed: {error}")]
    Step {
        name: String,
        op: &'static str,
        error: CallError,
    },
}

impl PipelineError {
    /// Returns the stack trace reported by the worker.
    pub fn stack(&self) -> Option<&str> {
        match self {
            Self::Step {
                error: CallError::Failed { stack, .. },
                ..
            } => stack.as_deref(),
            _ => None,
        }
    }
}

impl WsPipeline {
    /// Looks up the Route’s Pipeline and validates its Transformer references in both directions.
    pub fn of(route: &Route, runtime: &RuntimeConfig) -> Result<Option<Self>, String> {
        let Some(name) = &route.pipeline_name else {
            return Ok(None);
        };
        Self::named(runtime, name)
            .map(Some)
            .map_err(|reason| format!("route `{}`: {reason}", route.id))
    }

    /// Resolves the input and output steps of a named Pipeline.
    pub(crate) fn named(runtime: &RuntimeConfig, name: &str) -> Result<Self, String> {
        let spec = runtime
            .pipelines
            .get(name)
            .ok_or_else(|| format!("unknown Pipeline `{name}`"))?;
        let resolve = |keys: &[String], op| {
            keys.iter()
                .map(|key| Step::resolve(runtime, key, op))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|reason| format!("Pipeline `{name}`: {reason}"))
        };
        Ok(Self {
            input: resolve(&spec.input, Op::Input)?,
            output: resolve(&spec.output, Op::Output)?,
        })
    }

    /// Whether the Pipeline transforms incoming payloads.
    pub fn has_input(&self) -> bool {
        !self.input.is_empty()
    }

    /// Whether the Pipeline transforms outgoing payloads.
    pub fn has_output(&self) -> bool {
        !self.output.is_empty()
    }

    /// Returns each worker used by the Pipeline once.
    pub fn plugins(&self) -> impl Iterator<Item = &Arc<Plugin>> {
        let mut seen = HashSet::new();
        self.input
            .iter()
            .chain(&self.output)
            .filter_map(|step| match &step.runner {
                Runner::Worker(worker) => Some(worker),
                Runner::Command { .. } => None,
            })
            .filter(move |worker| seen.insert(worker.key.as_str()))
    }

    /// Checks the literal payload if the first output step is a worker.
    pub async fn check(&self, payload: &Value) -> Result<(), PipelineError> {
        if let Some(
            step @ Step {
                runner: Runner::Worker(_),
                ..
            },
        ) = self.output.first()
        {
            step.run(Op::Check, to_msgpack(payload), None, None).await?;
        }
        Ok(())
    }

    /// Converts an incoming frame to JSON for the Message envelope.
    async fn input(
        &self,
        frame: Frame,
        payload: &[u8],
        connection: &CallConnection,
    ) -> Result<Value, PipelineError> {
        let mut value = Msgpack::Binary(payload.to_vec());
        for step in &self.input {
            value = step
                .run(Op::Input, value, Some(frame), Some(connection))
                .await?;
        }
        let step = self.input.last().expect("input Pipeline is nonempty");
        match (&step.runner, value) {
            (Runner::Command { .. }, Msgpack::Binary(bytes)) => serde_json::from_slice(&bytes)
                .map_err(|error| step.failed(Op::Input, error.to_string())),
            (_, value) => to_json(&value).map_err(|reason| step.failed(Op::Input, reason)),
        }
    }

    /// Converts a rendered payload to an outgoing frame.
    async fn output(
        &self,
        payload: &Value,
        connection: &CallConnection,
    ) -> Result<Encoded, PipelineError> {
        let mut value = to_msgpack(payload);
        for step in &self.output {
            value = step.run(Op::Output, value, None, Some(connection)).await?;
        }
        let step = self.output.last().expect("output Pipeline is nonempty");
        match value {
            Msgpack::String(text) => text
                .into_str()
                .map(Encoded::Text)
                .ok_or_else(|| step.failed(Op::Output, "returned non-UTF-8 text")),
            Msgpack::Binary(bytes) => Ok(Encoded::Binary(bytes)),
            other => Err(step.failed(Op::Output, format!("expected text or bytes, got {other}"))),
        }
    }
}

/// Turns frames into messages and back: as text, or through the Route's Pipeline.
pub(crate) struct Codec {
    pub(crate) pipeline: Option<WsPipeline>,
    pub(crate) call: CallConnection,
}

impl Codec {
    /// The inbound message a frame stands for; `None` after a plugin error (logged).
    pub(crate) async fn decode<'a>(&self, frame: Frame, payload: &'a [u8]) -> Option<Inbound<'a>> {
        let Some(pipeline) = self
            .pipeline
            .as_ref()
            .filter(|pipeline| pipeline.has_input())
        else {
            return Some(match frame {
                Frame::Binary => Inbound::Binary(payload),
                Frame::Text => {
                    Inbound::Text(std::str::from_utf8(payload).expect("text frames are UTF-8"))
                }
            });
        };
        match pipeline.input(frame, payload, &self.call).await {
            Ok(message) => Some(Inbound::Decoded(message)),
            Err(error @ PipelineError::Miss { .. }) => Some(Inbound::Missed {
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
    pub(crate) async fn encode(&self, message: &Value, origin: &str) -> Option<Encoded> {
        let Some(pipeline) = self
            .pipeline
            .as_ref()
            .filter(|pipeline| pipeline.has_output())
        else {
            return Some(Encoded::Text(crate::ws::encode_text(message)));
        };
        match pipeline.output(message, &self.call).await {
            Ok(encoded) => Some(encoded),
            Err(error) => {
                crate::events::error(self.report(&error, origin, &format!("message: {message}")));
                None
            }
        }
    }

    /// One error event with what's needed to fix it: where, what failed, the value, the stack.
    fn report(&self, error: &PipelineError, doing: &str, value: &str) -> String {
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
    pub(crate) fn closed(&self) {
        for plugin in self.pipeline.iter().flat_map(WsPipeline::plugins) {
            plugin.close(&self.call.id);
        }
    }
}

impl Step {
    /// Selects the worker or command for this direction.
    fn resolve(runtime: &RuntimeConfig, name: &str, op: Op) -> Result<Self, String> {
        let unsupported = || format!("Transformer `{name}` does not provide `{}`", op.name());
        let runner = if let Some(worker) = runtime.plugins.get(name) {
            if !worker.supports(op) {
                return Err(unsupported());
            }
            Runner::Worker(worker.clone())
        } else if let Some(commands) = runtime.commands.get(name) {
            let argv = match op {
                Op::Input => &commands.input,
                Op::Output => &commands.output,
                Op::Check => unreachable!("check is not a Pipeline direction"),
            }
            .as_ref()
            .ok_or_else(unsupported)?;
            if argv.first().is_none_or(String::is_empty) {
                return Err(format!(
                    "Transformer `{name}` `{}` needs an executable",
                    op.name()
                ));
            }
            Runner::Command {
                argv: argv.clone(),
                dir: commands.dir.clone(),
            }
        } else {
            return Err(format!("unknown Transformer `{name}`"));
        };
        Ok(Self {
            name: name.to_owned(),
            runner,
        })
    }

    /// Passes the value to the worker or command.
    async fn run(
        &self,
        op: Op,
        input: Msgpack,
        frame: Option<Frame>,
        connection: Option<&CallConnection>,
    ) -> Result<Msgpack, PipelineError> {
        match &self.runner {
            Runner::Worker(worker) => worker
                .call(Call {
                    op,
                    input,
                    frame,
                    connection,
                })
                .await
                .map_err(|error| match error {
                    CallError::Miss(reason) if op == Op::Input => PipelineError::Miss {
                        name: self.name.clone(),
                        reason,
                    },
                    error => PipelineError::Step {
                        name: worker.label(),
                        op: op.name(),
                        error,
                    },
                }),
            Runner::Command { argv, dir } => {
                let bytes = match input {
                    Msgpack::Binary(bytes) => bytes,
                    Msgpack::String(text) => text
                        .into_str()
                        .map(String::into_bytes)
                        .ok_or_else(|| self.failed(op, "expected UTF-8 text"))?,
                    value => to_json(&value)
                        .and_then(|json| {
                            serde_json::to_vec(&json).map_err(|error| error.to_string())
                        })
                        .map_err(|reason| self.failed(op, reason))?,
                };
                run_command(argv, dir, &bytes)
                    .await
                    .map(Msgpack::Binary)
                    .map_err(|reason| self.failed(op, reason))
            }
        }
    }

    /// Adds the Transformer name and direction to the error.
    fn failed(&self, op: Op, reason: impl Into<String>) -> PipelineError {
        PipelineError::Step {
            name: self.name.clone(),
            op: op.name(),
            error: CallError::Failed {
                message: reason.into(),
                stack: None,
            },
        }
    }
}

/// Runs a command, reading its output while writing the payload to stdin.
async fn run_command(
    argv: &[String],
    dir: &std::path::Path,
    bytes: &[u8],
) -> Result<Vec<u8>, String> {
    let mut child = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("could not start `{}`: {error}", argv[0]))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let write_input = async move {
        match stdin.write_all(bytes).await {
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            result => result,
        }
    };
    let collect = async {
        tokio::try_join!(
            write_input,
            limited_output(stdout, "stdout", 16 * 1024 * 1024),
            limited_output(stderr, "stderr", 64 * 1024),
            child.wait()
        )
    };
    let (_, stdout, stderr, status) =
        tokio::time::timeout(crate::config::DEFAULT_PLUGIN_TIMEOUT, collect)
            .await
            .map_err(|_| format!("`{}` timed out after 5s", argv[0]))?
            .map_err(|err| format!("`{}`: {err}", argv[0]))?;
    if !status.success() {
        return Err(format!(
            "`{}` {status}: {}",
            argv[0],
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    if !stderr.is_empty() {
        eprint!("{}: {}", argv[0], String::from_utf8_lossy(&stderr));
    }
    Ok(stdout)
}

async fn limited_output(
    pipe: impl AsyncRead + Unpin,
    name: &str,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.take(limit as u64 + 1).read_to_end(&mut bytes).await?;
    if bytes.len() > limit {
        return Err(std::io::Error::other(format!(
            "{name} exceeds {limit} bytes"
        )));
    }
    Ok(bytes)
}

/// `binary, 12 bytes: 0a 03 41 …` (the first 32), for error events.
pub fn preview(frame: Frame, payload: &[u8]) -> String {
    const SHOWN: usize = 32;
    let kind = match frame {
        Frame::Text => "text",
        Frame::Binary => "binary",
    };
    let hex: Vec<_> = payload
        .iter()
        .take(SHOWN)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let more = if payload.len() > SHOWN { " …" } else { "" };
    format!("{kind}, {} bytes: {}{more}", payload.len(), hex.join(" "))
}

/// Startup `check` of every outbound literal of the Catalog's WebSocket Routes with a Pipeline,
/// so a typo in a payload fails startup instead of a test. Host `protobuf.encode` calls only
/// validate while it runs.
pub async fn check_routes(catalog: &crate::engine::Catalog) -> Result<(), String> {
    let plugins = &catalog.runtime().plugins;
    plugins.set_checking(true);
    let checked = check_each(catalog).await;
    plugins.set_checking(false);
    checked
}

async fn check_each(catalog: &crate::engine::Catalog) -> Result<(), String> {
    for route in catalog.routes() {
        let Some(pipeline) = WsPipeline::of(route, catalog.runtime())? else {
            continue;
        };
        for (case_id, case) in &route.cases {
            for (behavior_id, behavior) in &case.behaviors {
                let crate::schema::Behavior::Response(response) = behavior else {
                    continue;
                };
                let single = response
                    .message
                    .iter()
                    .map(|message| ("`message`".to_owned(), message));
                let listed = response
                    .messages
                    .iter()
                    .flatten()
                    .enumerate()
                    .map(|(index, item)| (format!("`messages[{index}]`"), &item.payload));
                for (field, message) in single.chain(listed) {
                    pipeline.check(message).await.map_err(|error| {
                        format!(
                            "behavior `{}:{case_id}:{behavior_id}` {field}: {error}",
                            route.id
                        )
                    })?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_output_limit_accepts_exact_fit_and_rejects_overflow() {
        assert_eq!(
            limited_output(&b"abc"[..], "stdout", 3).await.unwrap(),
            b"abc"
        );
        assert!(
            limited_output(&b"abcd"[..], "stderr", 3)
                .await
                .unwrap_err()
                .to_string()
                .contains("stderr exceeds 3 bytes")
        );
    }
}
