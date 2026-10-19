use std::cell::OnceCell;
use std::collections::BTreeMap;

use prost_reflect::MethodDescriptor;
use serde_json::{Value, json};

use super::codec::{decode, encode, pipeline};
use super::resolve::GrpcResolution;
use super::schema::STATUS_CODES;
use super::{GrpcRequest, GrpcResponse, INTERNAL, OK};
use crate::collections::Activation;
use crate::engine::{Catalog, MissDiagnostic};
use crate::events::{Match, Outcome};
use crate::expression::to_rcvar;
use crate::schema::{ResponseBehavior, Route};
use crate::template::render_grpc_response;

/// A served unary call and what an event needs to explain it.
pub(crate) struct GrpcCall {
    pub response: GrpcResponse,
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
                let response =
                    respond(route, &method, response, &envelope).unwrap_or_else(|reason| {
                        GrpcResponse::error(
                            INTERNAL,
                            format!("behavior `{}` failed: {reason}", activation.address()),
                        )
                    });
                // A raw Pipeline leaves base64 in `message`; that is not a decoded body.
                let decoded = capture && route.decodes_protobuf();
                let message = (decoded && !response.body.is_empty())
                    .then(|| decode(&response.body, pipeline(route), &method.output()).ok())
                    .flatten();
                GrpcCall {
                    response,
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
                request: None,
                message: None,
            },
        }
    }
}

fn respond(
    route: &Route,
    method: &MethodDescriptor,
    response: &ResponseBehavior,
    envelope: &Value,
) -> Result<GrpcResponse, String> {
    let jmes = OnceCell::new();
    let rendered = render_grpc_response(response, &|| {
        jmes.get_or_init(|| to_rcvar(envelope)).clone()
    })
    .map_err(|error| error.to_string())?;
    let status = match &rendered.status {
        None => OK,
        Some(status) => status_code(status)?,
    };
    if status != OK {
        let message = match rendered.message {
            None => String::new(),
            Some(Value::String(text)) => text,
            Some(other) => return Err(format!("status message must be a string, got {other}")),
        };
        return Ok(GrpcResponse {
            metadata: rendered.metadata,
            ..GrpcResponse::error(status, message)
        });
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
