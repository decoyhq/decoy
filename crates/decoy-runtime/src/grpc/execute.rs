use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream::{self, BoxStream};
use prost_reflect::MethodDescriptor;
use serde_json::Value;
use tokio::sync::mpsc;

use super::codec::{decode, encode, pipeline};
use super::plan::{render, respond, status_code, status_only};
use super::resolve::GrpcResolution;
use super::{GrpcRequest, GrpcResponse, INTERNAL, OK, StreamPart, UNAVAILABLE};
use crate::collections::Activation;
use crate::connections::{Connections, Registration};
use crate::engine::{Catalog, MissDiagnostic};
use crate::events::{Match, Outcome};
use crate::schema::{ResponseBehavior, Route};

/// A served call and what an event needs to explain it.
pub(crate) struct GrpcCall {
    /// For a stream, its end status if known at open, else an empty `OK`.
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
                let failed = |reason| behavior_failed(&activation.address(), reason);
                let (response, stream) = if method.is_server_streaming() {
                    match respond_stream(&activation, route, &method, response, &envelope) {
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
        route: activation.route.clone(),
        output: method.output().full_name().to_owned(),
    })
}

/// `INTERNAL` naming the Behavior that could not render or encode.
fn behavior_failed(address: &str, reason: impl std::fmt::Display) -> GrpcResponse {
    GrpcResponse::error(INTERNAL, format!("behavior `{address}` failed: {reason}"))
}
