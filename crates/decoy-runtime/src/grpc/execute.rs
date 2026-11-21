use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream::{self, BoxStream};
use prost_reflect::MethodDescriptor;
use serde_json::Value;
use tokio::sync::mpsc;

use super::codec::{decode, decode_named, encode_response_async, pipeline};
use super::plan::{prepare_unary, render, status_code, status_only};
use super::resolve::GrpcResolution;
use super::{GrpcRequest, GrpcResponse, INTERNAL, INVALID_ARGUMENT, OK, StreamPart, UNAVAILABLE};
use crate::collections::Activation;
use crate::connections::{Connections, Registration};
use crate::engine::{Catalog, MissDiagnostic, Rejection, reject};
use crate::events::{Match, Outcome};
use crate::http::RuntimeConfig;
use crate::pipeline::{Pipeline, PipelineError};
use crate::schema::{ResponseBehavior, Route};

/// A served call and what an event needs to explain it.
pub(crate) struct GrpcCall {
    /// For a stream, its end status if known at open, else an empty `OK`.
    pub response: GrpcResponse,
    /// A server-streaming answer, served by [`GrpcStream::serve`].
    pub stream: Option<GrpcStream>,
    /// Only when `capture` was asked for.
    pub outcome: Option<Outcome>,
    /// The request exposed to Cases, when decoded through protobuf or a named Pipeline and captured.
    pub request: Option<Value>,
    /// The response message before encoding; only when `capture` was asked for.
    pub message: Option<Value>,
}

impl Catalog {
    /// Resolve a call with all candidate diagnostics, including named input Transformers.
    pub async fn resolve_grpc_async(
        &self,
        collection_id: &str,
        overrides: &[Activation],
        request: &GrpcRequest,
    ) -> GrpcResolution<'_> {
        self.select_grpc(collection_id, overrides, request, true, true)
            .await
    }

    pub(super) async fn select_grpc(
        &self,
        collection_id: &str,
        overrides: &[Activation],
        request: &GrpcRequest,
        explain: bool,
        run_transformers: bool,
    ) -> GrpcResolution<'_> {
        let mut activations = match self.activations(collection_id, overrides) {
            Ok(activations) => activations.rev(),
            Err(error) => {
                return GrpcResolution::Miss(MissDiagnostic {
                    collection: collection_id.to_owned(),
                    reason: error.to_string(),
                    candidates: vec![],
                });
            }
        };
        let mut transformed = HashMap::new();
        let mut candidates = explain.then(Vec::new);
        let mut decode_failure = None;
        let mut resume = None;
        loop {
            let remaining = resume.take().into_iter().chain(&mut activations);
            match self.grpc_candidates(
                remaining,
                request,
                &transformed,
                &mut candidates,
                &mut decode_failure,
            ) {
                Ok(Some(hit)) => return hit,
                Ok(None) => break,
                Err(pending) if !run_transformers => {
                    return GrpcResolution::Failed {
                        response: GrpcResponse::error(
                            INVALID_ARGUMENT,
                            format!(
                                "route `{}`: use resolve_grpc_async for named Pipelines",
                                pending.route.id
                            ),
                        ),
                        candidates: candidates.unwrap_or_default(),
                    };
                }
                Err(pending) => {
                    let failed = |reason: String, mut candidates: Option<Vec<_>>| {
                        reject(&mut candidates, pending.activation, || Rejection::Decode {
                            error: reason.clone(),
                        });
                        GrpcResolution::Failed {
                            response: GrpcResponse::error(
                                INVALID_ARGUMENT,
                                format!(
                                    "route `{}` could not decode the request: {reason}",
                                    pending.route.id
                                ),
                            ),
                            candidates: candidates.unwrap_or_default(),
                        }
                    };
                    let pipeline = match Pipeline::of(pending.route, self.runtime()) {
                        Ok(Some(pipeline)) => pipeline,
                        Ok(None) => unreachable!("only a named Pipeline needs preparation"),
                        Err(error) => return failed(error, candidates),
                    };
                    let result =
                        match decode_named(&request.body, &pipeline, &pending.method.input()).await
                        {
                            Ok(value) => Ok(value),
                            Err(error @ PipelineError::Miss { .. }) => Err(error.to_string()),
                            Err(error) => return failed(error.to_string(), candidates),
                        };
                    transformed.insert(pending.route.id.as_str(), result);
                    resume = Some(pending.activation);
                }
            }
        }
        let candidates = candidates.unwrap_or_default();
        match decode_failure {
            Some(reason) => GrpcResolution::Failed {
                response: GrpcResponse::error(INVALID_ARGUMENT, reason),
                candidates,
            },
            None => GrpcResolution::Miss(MissDiagnostic {
                collection: collection_id.to_owned(),
                reason: "no active route case matched request".to_owned(),
                candidates,
            }),
        }
    }

    pub(crate) async fn execute_grpc(
        &self,
        collection_id: &str,
        overrides: &[Activation],
        request: GrpcRequest,
        capture: bool,
    ) -> GrpcCall {
        let resolution = self
            .select_grpc(collection_id, overrides, &request, capture, true)
            .await;
        match resolution {
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
                let failed = |reason| behavior_failed(&activation.address(), reason);
                let mut logical_reply = None;
                let (response, stream) = if method.is_server_streaming() {
                    match respond_stream(
                        &activation,
                        route,
                        self.runtime(),
                        &method,
                        response,
                        &envelope,
                    )
                    .await
                    {
                        // The event records the end status when it is known at open.
                        Ok(stream) => (
                            stream
                                .end
                                .clone()
                                .unwrap_or_else(|| GrpcResponse::error(OK, "")),
                            Some(stream),
                        ),
                        Err(reason) => (failed(reason), None),
                    }
                } else if let Some(reply) = self.grpc_replies.get(&activation) {
                    (reply.clone(), None)
                } else {
                    let reply = async {
                        let (mut reply, message) = prepare_unary(response, &envelope)?;
                        if let Some(message) = message {
                            let captured =
                                (capture && route.pipeline_name.is_some()).then(|| message.clone());
                            reply.body = encode_response_async(
                                route,
                                self.runtime(),
                                message,
                                &method.output(),
                            )
                            .await
                            .map_err(|error| error.to_string())?;
                            logical_reply = captured;
                        }
                        Ok(reply)
                    }
                    .await;
                    (reply.unwrap_or_else(failed), None)
                };
                // A raw Pipeline leaves base64 in `message`; that is not a decoded body.
                let decoded = capture && route.decodes_protobuf();
                let message = logical_reply.or_else(|| {
                    (decoded && !response.body.is_empty())
                        .then(|| decode(&response.body, pipeline(route), &method.output()).ok())
                        .flatten()
                });
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
                    request: (decoded || capture && route.pipeline_name.is_some())
                        .then(|| envelope["message"].take()),
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
    /// The Route id, which `send` addresses.
    route: String,
    /// Full name of the output type, so `send` skips this stream once a reload changes it.
    output: String,
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
                let (registration, receiver) =
                    connections.register_shared(session, &self.route, &self.output);
                let open = Open {
                    _registration: registration,
                    receiver,
                };
                stream::unfold(Some(open), |open| async move {
                    let mut open = open?;
                    // `None`: Connections closed on shutdown.
                    let Some(message) = open.receiver.recv().await else {
                        let end = GrpcResponse::error(UNAVAILABLE, "decoy is shutting down");
                        return Some((StreamPart::End(end), None));
                    };
                    Some((StreamPart::Message(message.into()), Some(open)))
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
    receiver: mpsc::Receiver<bytes::Bytes>,
}

async fn respond_stream(
    activation: &Activation,
    route: &Route,
    runtime: &RuntimeConfig,
    method: &MethodDescriptor,
    response: &ResponseBehavior,
    envelope: &Value,
) -> Result<GrpcStream, String> {
    let rendered = render(response, envelope)?;
    let mut messages = Vec::new();
    for item in rendered.stream.unwrap_or_default() {
        let message = encode_response_async(route, runtime, item.message, &method.output())
            .await
            .map_err(|error| error.to_string())?;
        messages.push((item.delay, message));
    }
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
        route: activation.route.clone(),
        output: method.output().full_name().to_owned(),
    })
}

/// `INTERNAL` naming the Behavior that could not render or encode.
fn behavior_failed(address: &str, reason: impl std::fmt::Display) -> GrpcResponse {
    GrpcResponse::error(INTERNAL, format!("behavior `{address}` failed: {reason}"))
}
