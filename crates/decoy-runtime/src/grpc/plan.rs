//! Prepare unary replies and the status and metadata shared with streaming replies.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap};

use prost_reflect::{DescriptorPool, MethodDescriptor};
use serde_json::{Value, json};

use super::codec::{encode, pipeline};
use super::schema::{STATUS_CODES, find_method};
use super::{GrpcResponse, OK};
use crate::collections::Activation;
use crate::expression::to_rcvar;
use crate::schema::{Behavior, ResponseBehavior, Route, RouteMatch};
use crate::template::{render_grpc_response, value_has_templates};

/// Cache unary replies whose rendered fields are independent of the request.
pub(crate) fn static_unary_replies<'a>(
    routes: impl Iterator<Item = &'a Route>,
    pool: &DescriptorPool,
) -> HashMap<Activation, GrpcResponse> {
    let mut replies = HashMap::new();
    for route in routes {
        let RouteMatch::Grpc(address) = &route.route_match else {
            continue;
        };
        let Ok(method) = find_method(pool, &address.service, &address.method) else {
            continue;
        };
        if method.is_server_streaming() || route.pipeline_name.is_some() {
            continue;
        }
        for (case_name, case) in &route.cases {
            for (name, behavior) in &case.behaviors {
                let Behavior::Response(response) = behavior else {
                    continue;
                };
                if response
                    .status
                    .iter()
                    .chain(&response.message)
                    .any(value_has_templates)
                    || response.metadata.values().any(|value| value.contains("${"))
                {
                    continue;
                }
                if let Ok(reply) = respond(route, &method, response, &Value::Null) {
                    replies.insert(
                        Activation {
                            route: route.id.clone(),
                            case: case_name.clone(),
                            behavior: name.clone(),
                        },
                        reply,
                    );
                }
            }
        }
    }
    replies
}

pub(super) fn respond(
    route: &Route,
    method: &MethodDescriptor,
    response: &ResponseBehavior,
    envelope: &Value,
) -> Result<GrpcResponse, String> {
    let (mut reply, message) = prepare_unary(response, envelope)?;
    if let Some(message) = message {
        reply.body = encode(message, pipeline(route), &method.output())
            .map_err(|error| error.to_string())?;
    }
    Ok(reply)
}

pub(super) fn prepare_unary(
    response: &ResponseBehavior,
    envelope: &Value,
) -> Result<(GrpcResponse, Option<Value>), String> {
    let rendered = render(response, envelope)?;
    let status = match &rendered.status {
        None => OK,
        Some(status) => status_code(status)?,
    };
    if status != OK {
        return Ok((
            status_only(status, rendered.message, rendered.metadata)?,
            None,
        ));
    }
    let message = rendered.message.unwrap_or_else(|| json!({}));
    Ok((
        GrpcResponse {
            body: Vec::new(),
            status: OK,
            status_message: String::new(),
            metadata: rendered.metadata,
        },
        Some(message),
    ))
}

pub(super) fn render(
    response: &ResponseBehavior,
    envelope: &Value,
) -> Result<ResponseBehavior, String> {
    let jmes = OnceCell::new();
    render_grpc_response(response, &|| {
        jmes.get_or_init(|| to_rcvar(envelope)).clone()
    })
    .map_err(|error| error.to_string())
}

/// A bodyless answer: `status`, with `message` as its text.
pub(super) fn status_only(
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
pub(super) fn status_code(status: &Value) -> Result<u8, String> {
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
