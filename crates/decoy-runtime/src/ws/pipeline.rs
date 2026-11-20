//! A WebSocket Route's plugin Pipeline: frames → JSON for the Cases, rendered messages → frames
//! (`docs/design/transports.md` §7). Every step is a plugin step; the socket calls these in its own
//! loop, so a connection's calls run one at a time, in order.

use std::sync::Arc;

use rmpv::Value as Msgpack;
use serde_json::Value;

use crate::plugins::value::{to_json, to_msgpack};
use crate::plugins::{Call, CallConnection, CallError, Frame, Op, Plugin, Plugins};
use crate::schema::{Route, WsTransformer};

/// A frame to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Encoded {
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct WsPipeline {
    steps: Vec<(Arc<Plugin>, String)>,
    transformer: Option<WsTransformer>,
}

/// Why a frame didn't decode or a message didn't encode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    #[error("transformer {direction}: {reason}")]
    Transformer {
        direction: &'static str,
        reason: String,
    },
    /// A step called `miss`: the frame matches nothing.
    #[error("plugin `{plugin}` step `{step}`: {reason}")]
    Miss {
        plugin: String,
        step: String,
        reason: String,
    },
    #[error("plugin `{plugin}` step `{step}` {op} failed: {error}")]
    Step {
        plugin: String,
        step: String,
        op: &'static str,
        error: CallError,
    },
}

impl PipelineError {
    /// The plugin's stack trace, when it sent one.
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
    /// The Route's Pipeline; `None` for a Route without one. Startup checked every step exists.
    pub fn of(route: &Route, plugins: &Plugins) -> Option<Self> {
        if let Some(transformer) = &route.transformer {
            return Some(Self {
                steps: vec![],
                transformer: Some(transformer.clone()),
            });
        }
        route.pipeline.as_ref()?;
        let steps = route
            .plugin_steps()
            .filter_map(|(key, step)| Some((plugins.get(key)?.clone(), step.to_owned())))
            .collect();
        Some(Self {
            steps,
            transformer: None,
        })
    }

    pub fn plugins(&self) -> impl Iterator<Item = &Arc<Plugin>> {
        self.steps.iter().map(|(plugin, _)| plugin)
    }

    /// Startup `check` of one outbound literal, as written (`${ }` templates included), by the
    /// step nearest the Cases.
    pub async fn check(&self, message: &Value) -> Result<(), PipelineError> {
        // Прототип не исполняет внешнюю команду при загрузке литералов.
        if self.transformer.is_some() {
            return Ok(());
        }
        let (plugin, step) = self.steps.last().expect("a pipeline has steps");
        call(plugin, step, Op::Check, to_msgpack(message), None, None).await?;
        Ok(())
    }

    /// Runs the steps top to bottom over a frame's payload; the last one's value must be JSON.
    pub async fn decode(
        &self,
        frame: Frame,
        payload: &[u8],
        connection: &CallConnection,
    ) -> Result<Value, PipelineError> {
        if let Some(transformer) = &self.transformer {
            let bytes = transform("inbound", &transformer.inbound, payload.to_vec()).await?;
            return serde_json::from_slice(&bytes).map_err(|error| PipelineError::Transformer {
                direction: "inbound",
                reason: error.to_string(),
            });
        }
        let mut value = Msgpack::Binary(payload.to_vec());
        for (plugin, step) in &self.steps {
            value = call(
                plugin,
                step,
                Op::Decode,
                value,
                Some(frame),
                Some(connection),
            )
            .await?;
        }
        let (plugin, step) = self.steps.last().expect("a pipeline has steps");
        to_json(&value).map_err(|reason| failed(plugin, step, Op::Decode, reason))
    }

    /// Runs the steps bottom to top over a message; the first step returns the frame. Without a
    /// `connection` (a Control API dry run) the steps get a fresh `ctx.state`.
    pub async fn encode(
        &self,
        message: &Value,
        connection: Option<&CallConnection>,
    ) -> Result<Encoded, PipelineError> {
        if let Some(transformer) = &self.transformer {
            let bytes = transform(
                "outbound",
                &transformer.outbound,
                message.to_string().into_bytes(),
            )
            .await?;
            return Ok(Encoded::Binary(bytes));
        }
        let mut value = to_msgpack(message);
        for (plugin, step) in self.steps.iter().rev() {
            value = call(plugin, step, Op::Encode, value, None, connection).await?;
        }
        let (plugin, step) = self.steps.first().expect("a pipeline has steps");
        match value {
            Msgpack::String(text) => match text.into_str() {
                Some(text) => Ok(Encoded::Text(text)),
                None => Err(failed(plugin, step, Op::Encode, "returned non-UTF-8 text")),
            },
            Msgpack::Binary(bytes) => Ok(Encoded::Binary(bytes)),
            other => Err(failed(
                plugin,
                step,
                Op::Encode,
                format!(
                    "the first step of a pipeline returns a string (text frame) or bytes (binary frame), got {other}"
                ),
            )),
        }
    }
}

async fn call(
    plugin: &Plugin,
    step: &str,
    op: Op,
    input: Msgpack,
    frame: Option<Frame>,
    connection: Option<&CallConnection>,
) -> Result<Msgpack, PipelineError> {
    let call = Call {
        op,
        step,
        input,
        frame,
        connection,
    };
    plugin.call(call).await.map_err(|error| match error {
        CallError::Miss(reason) if op == Op::Decode => PipelineError::Miss {
            plugin: plugin.label(),
            step: step.to_owned(),
            reason,
        },
        error => PipelineError::Step {
            plugin: plugin.label(),
            step: step.to_owned(),
            op: op.name(),
            error,
        },
    })
}

fn failed(plugin: &Plugin, step: &str, op: Op, reason: impl Into<String>) -> PipelineError {
    PipelineError::Step {
        plugin: plugin.label(),
        step: step.to_owned(),
        op: op.name(),
        error: CallError::Failed {
            message: reason.into(),
            stack: None,
        },
    }
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
    let checked = check_each(catalog, plugins).await;
    plugins.set_checking(false);
    checked
}

async fn check_each(catalog: &crate::engine::Catalog, plugins: &Plugins) -> Result<(), String> {
    for route in catalog.routes() {
        let Some(pipeline) = WsPipeline::of(route, plugins) else {
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
                    .map(|(index, item)| (format!("`messages[{index}]`"), &item.message));
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

/// Передаёт stdout каждой команды следующей в указанном порядке.
async fn transform(
    direction: &'static str,
    commands: &[Vec<String>],
    mut input: Vec<u8>,
) -> Result<Vec<u8>, PipelineError> {
    for argv in commands {
        let [program, args @ ..] = argv.as_slice() else {
            unreachable!("route validation requires a command");
        };
        input = run_command(program, args, input).await.map_err(|reason| {
            PipelineError::Transformer {
                direction,
                reason: format!("{program}: {reason}"),
            }
        })?;
    }
    Ok(input)
}

/// Запускает команду без shell и одновременно подаёт stdin и читает stdout.
async fn run_command(program: &str, args: &[String], input: Vec<u8>) -> Result<Vec<u8>, String> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let write = async move { stdin.write_all(&input).await };
    let (written, output) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(write, child.wait_with_output())
    })
    .await
    .map_err(|_| "command timed out".to_owned())?;
    let output = output.map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "{}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    written.map_err(|error| error.to_string())?;
    Ok(output.stdout)
}
