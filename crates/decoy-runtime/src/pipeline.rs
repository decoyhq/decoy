//! Ordered Transformer and built-in steps shared by Route transports.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use rmpv::Value as Msgpack;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};

use crate::http::RuntimeConfig;
use crate::plugins::value::{to_json, to_msgpack};
use crate::plugins::{Call, CallConnection, CallError, CallRequest, Frame, Op, Plugin};
use crate::schema::{PipelineStep, Route, Transport};

#[derive(Debug, Clone)]
pub struct Pipeline {
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
    Builtin(PipelineStep),
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

impl Pipeline {
    /// Looks up the Route’s Pipeline and validates its Transformer references in both directions.
    pub fn of(route: &Route, runtime: &RuntimeConfig) -> Result<Option<Self>, String> {
        let Some(name) = &route.pipeline_name else {
            return Ok(None);
        };
        let pipeline = Self::named(runtime, name)
            .map_err(|reason| format!("route `{}`: {reason}", route.id))?;
        if route.transport != Transport::Grpc
            && let Some(step) = pipeline
                .input
                .iter()
                .chain(&pipeline.output)
                .find(|step| matches!(step.runner, Runner::Builtin(_)))
        {
            return Err(format!(
                "route `{}`: Pipeline `{name}`: built-in step `{}` requires gRPC",
                route.id, step.name
            ));
        }
        Ok(Some(pipeline))
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
                Runner::Command { .. } | Runner::Builtin(_) => None,
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
            step.run(
                Op::Check,
                to_msgpack(payload),
                None,
                None,
                None,
                Self::unsupported_builtin,
            )
            .await?;
        }
        Ok(())
    }

    /// Run HTTP steps in their authored order, preserving each Transformer's result type.
    pub(crate) async fn run_http(
        &self,
        op: Op,
        value: Msgpack,
        request: CallRequest<'_>,
    ) -> Result<Msgpack, PipelineError> {
        self.transform(
            op,
            value,
            None,
            None,
            Some(request),
            Self::unsupported_builtin,
        )
        .await
    }

    pub(crate) async fn run_grpc(
        &self,
        op: Op,
        value: Msgpack,
        builtin: impl FnMut(PipelineStep, Msgpack) -> Result<Msgpack, String>,
    ) -> Result<Msgpack, PipelineError> {
        self.transform(op, value, None, None, None, builtin).await
    }

    async fn transform(
        &self,
        op: Op,
        mut value: Msgpack,
        frame: Option<Frame>,
        connection: Option<&CallConnection>,
        request: Option<CallRequest<'_>>,
        mut builtin: impl FnMut(PipelineStep, Msgpack) -> Result<Msgpack, String>,
    ) -> Result<Msgpack, PipelineError> {
        for step in self.steps(op) {
            value = step
                .run(op, value, frame, connection, request, &mut builtin)
                .await?;
        }
        Ok(value)
    }

    fn unsupported_builtin(step: PipelineStep, _: Msgpack) -> Result<Msgpack, String> {
        Err(format!("built-in step `{step}` requires gRPC"))
    }

    fn steps(&self, op: Op) -> &[Step] {
        match op {
            Op::Input => &self.input,
            Op::Output => &self.output,
            Op::Check => unreachable!("check is not a Pipeline direction"),
        }
    }

    pub(crate) fn input_ends_in_command(&self) -> bool {
        self.input
            .last()
            .is_some_and(|step| matches!(step.runner, Runner::Command { .. }))
    }

    pub(crate) async fn run_ws(
        &self,
        op: Op,
        value: Msgpack,
        frame: Option<Frame>,
        connection: &CallConnection,
    ) -> Result<Msgpack, PipelineError> {
        self.transform(
            op,
            value,
            frame,
            Some(connection),
            None,
            Self::unsupported_builtin,
        )
        .await
    }

    /// Attribute an invalid final value to the Transformer that returned it.
    pub(crate) fn invalid_result(&self, op: Op, reason: impl Into<String>) -> PipelineError {
        self.steps(op)
            .last()
            .expect("nonempty direction")
            .failed(op, reason)
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
            match name {
                "grpc-frame" => Runner::Builtin(PipelineStep::GrpcFrame),
                "protobuf" => Runner::Builtin(PipelineStep::Protobuf),
                _ => return Err(format!("unknown Transformer `{name}`")),
            }
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
        request: Option<CallRequest<'_>>,
        mut builtin: impl FnMut(PipelineStep, Msgpack) -> Result<Msgpack, String>,
    ) -> Result<Msgpack, PipelineError> {
        match &self.runner {
            Runner::Builtin(kind) => {
                builtin(*kind, input).map_err(|reason| self.failed(op, reason))
            }
            Runner::Worker(worker) => worker
                .call(Call {
                    op,
                    input,
                    frame,
                    connection,
                    request,
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

const COMMAND_LIMIT: usize = 8;
static COMMAND_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(COMMAND_LIMIT);

/// Runs a command, reading its output while writing the payload to stdin.
async fn run_command(
    argv: &[String],
    dir: &std::path::Path,
    bytes: &[u8],
) -> Result<Vec<u8>, String> {
    let _slot = COMMAND_SLOTS
        .try_acquire()
        .map_err(|_| format!("command Transformer limit reached ({COMMAND_LIMIT} running)"))?;
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

/// Startup `check` of authored response payloads on Routes with a named Pipeline,
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
        let Some(pipeline) = Pipeline::of(route, catalog.runtime())? else {
            continue;
        };
        for (case_id, case) in &route.cases {
            for (behavior_id, behavior) in &case.behaviors {
                let crate::schema::Behavior::Response(response) = behavior else {
                    continue;
                };
                // A gRPC error's message is a trailer, so it never goes through output.
                let single = match route.transport {
                    Transport::Http => response.body.as_ref().map(|value| ("`body`", value)),
                    Transport::Grpc
                        if response
                            .status
                            .as_ref()
                            .is_some_and(|status| status != "OK") =>
                    {
                        None
                    }
                    _ => response.message.as_ref().map(|value| ("`message`", value)),
                };
                let listed = response
                    .messages
                    .iter()
                    .flatten()
                    .enumerate()
                    .map(|(index, item)| (format!("`messages[{index}]`"), &item.payload));
                let streamed = response
                    .stream
                    .iter()
                    .flatten()
                    .enumerate()
                    .map(|(index, item)| (format!("`stream[{index}].payload`"), &item.message));
                let single = single.map(|(field, value)| (field.to_owned(), value));
                for (field, message) in single.into_iter().chain(listed).chain(streamed) {
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
    async fn saturated_commands_fail_before_spawn() {
        let _busy = COMMAND_SLOTS
            .try_acquire_many(COMMAND_LIMIT as u32)
            .unwrap();
        let error = run_command(
            &["missing-executable".into()],
            std::path::Path::new("."),
            &[],
        )
        .await
        .unwrap_err();
        assert!(error.contains("command Transformer limit"));
    }

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
