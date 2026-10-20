use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream::{self, BoxStream};
use prost_reflect::{MessageDescriptor, MethodDescriptor};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::codec::{decode, encode, pipeline};
use super::resolve::GrpcResolution;
use super::schema::STATUS_CODES;
use super::{GrpcRequest, GrpcResponse, INTERNAL, OK, StreamPart};
use crate::collections::Activation;
use crate::connections::{Connections, Registration};
use crate::engine::{Catalog, MissDiagnostic};
use crate::events::{Match, Outcome};
use crate::expression::to_rcvar;
use crate::schema::{PipelineStep, ResponseBehavior, Route};
use crate::template::render_grpc_response;

/// A served call and what an event needs to explain it.
pub(crate) struct GrpcCall {
    /// For a stream, an empty `OK`: what the call opened with.
    pub response: GrpcResponse,
    /// A server-streaming answer, served by [`GrpcStream::serve`].
    pub stream: Option<GrpcStream>,
    /// Only when `capture` was asked for.
    pub outcome: Option<Outcome>,
    /// The Pipeline-decoded request message, when a Route decoded protobuf and `capture` was
    /// asked for.
    pub request: Option<Value>,
    /// The response message before encoding; only when `capture` was asked for.
    pub message: Option<Value>,
}

impl Catalog {
    pub(crate) async fn execute_grpc(
        &self,
        collection_id: &str,
        overrides: &[Activation],
        request: GrpcRequest,
        capture: bool,
    ) -> GrpcCall {
        match self.resolve_grpc_with(collection_id, overrides, &request, capture) {
            GrpcResolution::Matched {
                activation,
                skipped,
                conditions,
                route,
                method,
                response,
                mut envelope,
            } => {
                if let Some(delay) = response.delay {
                    tokio::time::sleep(delay).await;
                }
                let failed = |reason| {
                    GrpcResponse::error(
                        INTERNAL,
                        format!("behavior `{}` failed: {reason}", activation.address()),
                    )
                };
                let (response, stream) = if method.is_server_streaming() {
                    match respond_stream(&activation, route, &method, response, &envelope) {
                        Ok(stream) => (GrpcResponse::error(OK, ""), Some(stream)),
                        Err(reason) => (failed(reason), None),
                    }
                } else {
                    let response = respond(route, &method, response, &envelope);
                    (response.unwrap_or_else(failed), None)
                };
                // A raw Pipeline leaves base64 in `message`; that is not a decoded body.
                let decoded = capture && route.decodes_protobuf();
                let message = (decoded && !response.body.is_empty())
                    .then(|| decode(&response.body, pipeline(route), &method.output()).ok())
                    .flatten();
                GrpcCall {
                    response,
                    stream,
                    outcome: capture.then(|| {
                        Outcome::Hit(Match {
                            activation,
                            skipped,
                            conditions: conditions.clone(),
                            path_params: BTreeMap::new(),
                        })
                    }),
                    request: decoded.then(|| envelope["message"].take()),
                    message,
                }
            }
            GrpcResolution::Miss(miss) => GrpcCall {
                response: GrpcResponse::miss(miss.reason.clone()),
                stream: None,
                outcome: capture.then_some(Outcome::Miss(miss)),
                request: None,
                message: None,
            },
            GrpcResolution::Failed {
                response,
                candidates,
            } => GrpcCall {
                outcome: capture.then(|| {
                    Outcome::Miss(MissDiagnostic {
                        collection: collection_id.to_owned(),
                        reason: response.status_message.clone(),
                        candidates,
                    })
                }),
                response,
                stream: None,
                request: None,
                message: None,
            },
        }
    }
}

/// A server-streaming answer, rendered and encoded when the call came in.
pub(crate) struct GrpcStream {
    messages: Vec<(Option<Duration>, Vec<u8>)>,
    /// `None`: after `messages` the call stays open for Control API `send`.
    end: Option<GrpcResponse>,
    steps: Vec<PipelineStep>,
    output: MessageDescriptor,
    /// The Route id, which `send` addresses.
    route: String,
    /// `route:case:behavior`, for error text.
    address: String,
}

impl GrpcStream {
    /// Each message after its delay, then the end status; or, with none, every `send` to this
    /// Route in `session` until the body is dropped (the client went away).
    pub(crate) fn serve(
        self,
        connections: &Connections,
        session: &str,
    ) -> BoxStream<'static, StreamPart> {
        let scripted = stream::iter(self.messages).then(|(delay, message)| async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            StreamPart::Message(message)
        });
        let rest = match self.end {
            Some(end) => stream::iter([StreamPart::End(end)]).boxed(),
            None => {
                let (registration, receiver) = connections.register(session, &self.route);
                let open = Open {
                    _registration: registration,
                    receiver,
                    steps: self.steps,
                    output: self.output,
                    address: self.address,
                };
                stream::unfold(Some(open), |open| async move {
                    let mut open = open?;
                    let message = open.receiver.recv().await?;
                    Some(match encode(message, &open.steps, &open.output) {
                        Ok(bytes) => (StreamPart::Message(bytes), Some(open)),
                        Err(error) => {
                            let reason = format!("behavior `{}` failed: {error}", open.address);
                            (StreamPart::End(GrpcResponse::error(INTERNAL, reason)), None)
                        }
                    })
                })
                .boxed()
            }
        };
        scripted.chain(rest).boxed()
    }
}

/// An open stream's link to Control API `send`; dropping it unregisters the call.
struct Open {
    _registration: Registration,
    receiver: mpsc::UnboundedReceiver<Value>,
    steps: Vec<PipelineStep>,
    output: MessageDescriptor,
    address: String,
}

fn render(response: &ResponseBehavior, envelope: &Value) -> Result<ResponseBehavior, String> {
    let jmes = OnceCell::new();
    render_grpc_response(response, &|| {
        jmes.get_or_init(|| to_rcvar(envelope)).clone()
    })
    .map_err(|error| error.to_string())
}

fn respond_stream(
    activation: &Activation,
    route: &Route,
    method: &MethodDescriptor,
    response: &ResponseBehavior,
    envelope: &Value,
) -> Result<GrpcStream, String> {
    let rendered = render(response, envelope)?;
    let steps = pipeline(route);
    let messages = rendered
        .stream
        .unwrap_or_default()
        .into_iter()
        .map(|item| {
            let message = encode(item.message, steps, &method.output());
            Ok((item.delay, message.map_err(|error| error.to_string())?))
        })
        .collect::<Result<_, String>>()?;
    let end = match &rendered.status {
        None => None,
        Some(status) => Some(status_only(
            status_code(status)?,
            rendered.message,
            rendered.metadata,
        )?),
    };
    Ok(GrpcStream {
        messages,
        end,
        steps: steps.to_vec(),
        output: method.output(),
        route: activation.route.clone(),
        address: activation.address(),
    })
}

fn respond(
    route: &Route,
    method: &MethodDescriptor,
    response: &ResponseBehavior,
    envelope: &Value,
) -> Result<GrpcResponse, String> {
    let rendered = render(response, envelope)?;
    let status = match &rendered.status {
        None => OK,
        Some(status) => status_code(status)?,
    };
    if status != OK {
        return status_only(status, rendered.message, rendered.metadata);
    }
    let message = rendered.message.unwrap_or_else(|| json!({}));
    Ok(GrpcResponse {
        body: encode(message, pipeline(route), &method.output())
            .map_err(|error| error.to_string())?,
        status: OK,
        status_message: String::new(),
        metadata: rendered.metadata,
    })
}

/// A bodyless answer: `status`, with `message` as its text.
fn status_only(
    status: u8,
    message: Option<Value>,
    metadata: BTreeMap<String, String>,
) -> Result<GrpcResponse, String> {
    let message = match message {
        None => String::new(),
        Some(Value::String(text)) => text,
        Some(other) => return Err(format!("status message must be a string, got {other}")),
    };
    Ok(GrpcResponse {
        metadata,
        ..GrpcResponse::error(status, message)
    })
}

/// A status name such as `NOT_FOUND`, or its number.
fn status_code(status: &Value) -> Result<u8, String> {
    let code = match status {
        Value::String(name) => STATUS_CODES.iter().position(|code| code == name),
        Value::Number(number) => number
            .as_u64()
            .and_then(|code| usize::try_from(code).ok())
            .filter(|code| *code < STATUS_CODES.len()),
        _ => None,
    };
    code.and_then(|code| u8::try_from(code).ok())
        .ok_or_else(|| format!("status rendered {status}, not a gRPC status name"))
}
