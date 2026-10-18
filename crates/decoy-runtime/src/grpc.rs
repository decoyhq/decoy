//! Serving gRPC Routes: decode the call through the Route's Pipeline, match Cases over the message
//! envelope, render the Behavior, and encode the answer. Transport framing (HTTP/2, trailers) is
//! the server's job; this module works on bytes. See `docs/design/transports.md` §4.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use prost::Message as _;
use prost_reflect::{DynamicMessage, MessageDescriptor, MethodDescriptor};
use serde_json::{Value, json};

use crate::collections::Activation;
use crate::engine::{Candidate, Catalog, MissDiagnostic, Rejection, reject};
use crate::events::{Match, Outcome};
use crate::grpc_schema::{STATUS_CODES, find_method, request_json_options};
use crate::matching::{grpc_case_matches, grpc_case_mismatch};
use crate::schema::{Behavior, PipelineStep, ResponseBehavior, Route, RouteMatch};
use crate::template::render_grpc_response;

/// The default Pipeline for gRPC Routes (Q2).
pub const DEFAULT_PIPELINE: [PipelineStep; 2] = [PipelineStep::GrpcFrame, PipelineStep::Protobuf];

/// gRPC status codes Decoy sends itself.
pub const OK: u8 = 0;
pub const INVALID_ARGUMENT: u8 = 3;
pub const RESOURCE_EXHAUSTED: u8 = 8;
pub const UNIMPLEMENTED: u8 = 12;
pub const INTERNAL: u8 = 13;

/// One unary call as the server received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcRequest {
    pub service: String,
    pub method: String,
    /// Request headers, lowercased.
    pub metadata: BTreeMap<String, String>,
    /// The HTTP/2 request body.
    pub body: Vec<u8>,
}

impl GrpcRequest {
    /// Split a `/package.Service/Method` request path.
    pub fn parse_path(path: &str) -> Option<(String, String)> {
        let (service, method) = path.strip_prefix('/')?.split_once('/')?;
        (!service.is_empty() && !method.is_empty() && !method.contains('/'))
            .then(|| (service.to_owned(), method.to_owned()))
    }
}

/// What to send back: the response body (empty on errors) and the trailers' status and metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcResponse {
    pub body: Vec<u8>,
    pub status: u8,
    pub status_message: String,
    pub metadata: BTreeMap<String, String>,
}

impl GrpcResponse {
    pub fn error(status: u8, message: impl Into<String>) -> Self {
        Self {
            body: Vec::new(),
            status,
            status_message: message.into(),
            metadata: BTreeMap::new(),
        }
    }

    /// Fail-closed miss: `UNIMPLEMENTED` with the same text as an HTTP miss.
    pub fn miss(reason: impl Into<String>) -> Self {
        let mut response = Self::error(UNIMPLEMENTED, reason);
        response
            .metadata
            .insert("x-mock-miss".to_owned(), "true".to_owned());
        response
    }
}

/// Which active route entry answers a unary call, and why the others did not.
#[derive(Debug)]
pub enum GrpcResolution<'a> {
    Matched {
        activation: Activation,
        /// Candidates rejected before the winner, in evaluation order; empty when serving.
        skipped: Vec<Candidate>,
        /// The winning Case's `match`; every condition in it passed.
        conditions: &'a Value,
        route: &'a Route,
        method: MethodDescriptor,
        response: &'a ResponseBehavior,
        envelope: Value,
    },
    Miss(MissDiagnostic),
    /// The call failed before matching finished, e.g. the request did not decode.
    Failed(GrpcResponse),
}

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
        overrides: &BTreeMap<(String, String), String>,
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
            GrpcResolution::Failed(response) => GrpcCall {
                outcome: capture.then(|| {
                    Outcome::Miss(MissDiagnostic {
                        collection: collection_id.to_owned(),
                        reason: response.status_message.clone(),
                        candidates: Vec::new(),
                    })
                }),
                response,
                request: None,
                message: None,
            },
        }
    }

    /// Resolves with the full explanation; serving uses `execute_grpc`, which skips it.
    pub fn resolve_grpc(
        &self,
        collection_id: &str,
        overrides: &BTreeMap<(String, String), String>,
        request: &GrpcRequest,
    ) -> GrpcResolution<'_> {
        self.resolve_grpc_with(collection_id, overrides, request, true)
    }

    /// Unexplained (`explain: false`) leaves `skipped` and a miss's `candidates` empty: a gRPC
    /// miss answers with its reason only.
    fn resolve_grpc_with(
        &self,
        collection_id: &str,
        overrides: &BTreeMap<(String, String), String>,
        request: &GrpcRequest,
        explain: bool,
    ) -> GrpcResolution<'_> {
        let activations = match self.activations(collection_id, overrides) {
            Ok(activations) => activations,
            Err(reason) => {
                return GrpcResolution::Miss(MissDiagnostic {
                    collection: collection_id.to_owned(),
                    reason,
                    candidates: Vec::new(),
                });
            }
        };

        let mut candidates = explain.then(Vec::new);
        for activation in activations.iter().rev() {
            let Some(route) = self.route(&activation.route) else {
                reject(&mut candidates, activation, || Rejection::MissingRoute);
                continue;
            };
            let RouteMatch::Grpc(address) = &route.route_match else {
                reject(&mut candidates, activation, || Rejection::Transport);
                continue;
            };
            if address.service != request.service || address.method != request.method {
                reject(&mut candidates, activation, || Rejection::Method {
                    expected: format!("{}/{}", address.service, address.method),
                });
                continue;
            }
            // Startup checks every gRPC Route against the protos, so this only guards the lookup.
            let Ok(method) = find_method(&self.runtime().protos, &address.service, &address.method)
            else {
                reject(&mut candidates, activation, || Rejection::MissingRoute);
                continue;
            };
            let message = match decode(&request.body, pipeline(route), &method.input()) {
                Ok(message) => message,
                Err(reason) => {
                    return GrpcResolution::Failed(GrpcResponse::error(
                        INVALID_ARGUMENT,
                        format!(
                            "route `{}` could not decode the request: {reason}",
                            route.id
                        ),
                    ));
                }
            };
            let envelope = json!({
                "service": request.service,
                "method": request.method,
                "metadata": request.metadata,
                "message": message,
            });
            let Some(case) = route.cases.get(&activation.case) else {
                reject(&mut candidates, activation, || Rejection::MissingCase);
                continue;
            };
            if explain {
                if let Some(failure) = grpc_case_mismatch(&case.case_match, &envelope) {
                    reject(&mut candidates, activation, || {
                        Rejection::Condition(failure)
                    });
                    continue;
                }
            } else if !grpc_case_matches(&case.case_match, &envelope) {
                continue;
            }
            let Some(Behavior::Response(response)) = case.behaviors.get(&activation.behavior)
            else {
                reject(&mut candidates, activation, || Rejection::MissingBehavior);
                continue;
            };
            return GrpcResolution::Matched {
                activation: activation.clone(),
                skipped: candidates.unwrap_or_default(),
                conditions: &case.case_match,
                route,
                method,
                response,
                envelope,
            };
        }

        GrpcResolution::Miss(MissDiagnostic {
            collection: collection_id.to_owned(),
            reason: "no active route case matched request".to_owned(),
            candidates: candidates.unwrap_or_default(),
        })
    }
}

fn pipeline(route: &Route) -> &[PipelineStep] {
    route.pipeline.as_deref().unwrap_or(&DEFAULT_PIPELINE)
}

fn respond(
    route: &Route,
    method: &MethodDescriptor,
    response: &ResponseBehavior,
    envelope: &Value,
) -> Result<GrpcResponse, String> {
    let rendered = render_grpc_response(response, envelope)?;
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
        body: encode(message, pipeline(route), &method.output())?,
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

/// Bytes as they flow through a Pipeline: steps turn one into the other.
enum Data {
    Bytes(Vec<u8>),
    Json(Value),
}

/// Run the Pipeline top to bottom over the request body. Bytes left at the end become a base64
/// string.
fn decode(body: &[u8], steps: &[PipelineStep], input: &MessageDescriptor) -> Result<Value, String> {
    let mut data = Data::Bytes(body.to_vec());
    for step in steps {
        data = match (step, data) {
            (PipelineStep::GrpcFrame, Data::Bytes(bytes)) => Data::Bytes(unframe(&bytes)?),
            (PipelineStep::Protobuf, Data::Bytes(bytes)) => {
                let message = DynamicMessage::decode(input.clone(), bytes.as_slice())
                    .map_err(|error| format!("not a valid {}: {error}", input.full_name()))?;
                let value = message
                    .serialize_with_options(serde_json::value::Serializer, &request_json_options())
                    .map_err(|error| error.to_string())?;
                Data::Json(value)
            }
            (step, Data::Json(_)) => return Err(format!("step `{step:?}` needs bytes, got JSON")),
        };
    }
    Ok(match data {
        Data::Json(value) => value,
        Data::Bytes(bytes) => Value::String(BASE64.encode(bytes)),
    })
}

/// Run the Pipeline bottom to top over the response message. Without a `protobuf` step the
/// message must be a base64 string.
fn encode(
    message: Value,
    steps: &[PipelineStep],
    output: &MessageDescriptor,
) -> Result<Vec<u8>, String> {
    let mut data = Data::Json(message);
    for step in steps.iter().rev() {
        data = match (step, data) {
            (PipelineStep::Protobuf, Data::Json(value)) => {
                let message =
                    DynamicMessage::deserialize(output.clone(), value).map_err(|error| {
                        format!("response does not fit {}: {error}", output.full_name())
                    })?;
                Data::Bytes(message.encode_to_vec())
            }
            (PipelineStep::GrpcFrame, data) => Data::Bytes(frame(&bytes(data)?)),
            (PipelineStep::Protobuf, Data::Bytes(_)) => {
                return Err("step `Protobuf` needs JSON, got bytes".to_owned());
            }
        };
    }
    bytes(data)
}

fn bytes(data: Data) -> Result<Vec<u8>, String> {
    match data {
        Data::Bytes(bytes) => Ok(bytes),
        Data::Json(Value::String(text)) => BASE64
            .decode(text)
            .map_err(|error| format!("message is not base64: {error}")),
        Data::Json(other) => Err(format!("expected a base64 string, got {other}")),
    }
}

/// One uncompressed length-prefixed message.
fn unframe(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let [compressed, a, b, c, d, rest @ ..] = bytes else {
        return Err("gRPC frame shorter than its 5-byte header".to_owned());
    };
    if *compressed != 0 {
        return Err("compressed gRPC messages are not supported".to_owned());
    }
    let length = u32::from_be_bytes([*a, *b, *c, *d]) as usize;
    if rest.len() != length {
        return Err(format!(
            "gRPC frame declares {length} bytes but carries {}; unary calls take one message",
            rest.len()
        ));
    }
    Ok(rest.to_vec())
}

fn frame(message: &[u8]) -> Vec<u8> {
    let length = u32::try_from(message.len()).unwrap_or(u32::MAX);
    let mut framed = Vec::with_capacity(message.len() + 5);
    framed.push(0);
    framed.extend_from_slice(&length.to_be_bytes());
    framed.extend_from_slice(message);
    framed
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::collections::CollectionsFile;
    use crate::config::ProtoSources;
    use crate::http::RuntimeConfig;
    use crate::protos::load_descriptor_pool;

    fn catalog(cases: &str, collection: &str) -> Catalog {
        catalog_with_pipeline(None, cases, collection)
    }

    fn runtime() -> RuntimeConfig {
        let protos = load_descriptor_pool(&ProtoSources {
            roots: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proto")],
            descriptor_sets: vec![],
        })
        .unwrap();
        RuntimeConfig {
            protos,
            ..RuntimeConfig::default()
        }
    }

    fn catalog_with_pipeline(pipeline: Option<&str>, cases: &str, collection: &str) -> Catalog {
        let route = Route::from_yaml(&format!(
            "id: get\ntransport: grpc\nmatch: {{ service: approvals.v1.Approvals, method: GetApproval }}\n{}cases:\n{cases}",
            pipeline.map(|steps| format!("pipeline: {steps}\n")).unwrap_or_default()
        ))
        .unwrap();
        Catalog::new(
            vec![route],
            CollectionsFile::from_yaml(&format!("- id: default\n  routes: [{collection}]\n"))
                .unwrap(),
            runtime(),
        )
        .unwrap()
    }

    fn request(catalog: &Catalog, message: Value) -> GrpcRequest {
        let input = find_method(
            &catalog.runtime().protos,
            "approvals.v1.Approvals",
            "GetApproval",
        )
        .unwrap()
        .input();
        let message = DynamicMessage::deserialize(input, message).unwrap();
        GrpcRequest {
            service: "approvals.v1.Approvals".to_owned(),
            method: "GetApproval".to_owned(),
            metadata: BTreeMap::from([("x-tenant".to_owned(), "acme".to_owned())]),
            body: frame(&message.encode_to_vec()),
        }
    }

    fn decoded(catalog: &Catalog, response: &GrpcResponse) -> Value {
        let output = find_method(
            &catalog.runtime().protos,
            "approvals.v1.Approvals",
            "GetApproval",
        )
        .unwrap()
        .output();
        let message =
            DynamicMessage::decode(output, unframe(&response.body).unwrap().as_slice()).unwrap();
        serde_json::to_value(&message).unwrap()
    }

    #[tokio::test]
    async fn answers_a_matched_call_with_a_rendered_message() {
        let catalog = catalog(
            r#"
  pending:
    match:
      message: { id: a-1 }
      metadata: { x-tenant: acme }
    behaviors:
      echo:
        message: { id: '${ message.id }', state: PENDING, approvers: ['${ metadata."x-tenant" }'] }
"#,
            "get:pending:echo",
        );
        let response = catalog
            .execute_grpc(
                "default",
                &BTreeMap::new(),
                request(&catalog, json!({ "id": "a-1" })),
                false,
            )
            .await
            .response;
        assert_eq!(response.status, OK, "{response:?}");
        assert_eq!(
            decoded(&catalog, &response),
            json!({ "id": "a-1", "state": "PENDING", "approvers": ["acme"] })
        );
    }

    #[tokio::test]
    async fn answers_status_errors_with_metadata() {
        let catalog = catalog(
            r#"
  any:
    behaviors:
      not-found:
        status: NOT_FOUND
        message: 'approval ${ message.id } not found'
        metadata: { x-reason: missing }
"#,
            "get:any:not-found",
        );
        let response = catalog
            .execute_grpc(
                "default",
                &BTreeMap::new(),
                request(&catalog, json!({ "id": "a-9" })),
                false,
            )
            .await
            .response;
        assert_eq!(
            response,
            GrpcResponse {
                body: Vec::new(),
                status: 5,
                status_message: "approval a-9 not found".to_owned(),
                metadata: BTreeMap::from([("x-reason".to_owned(), "missing".to_owned())]),
            }
        );
    }

    #[tokio::test]
    async fn misses_fail_closed_as_unimplemented() {
        let catalog = catalog(
            "  one:\n    match: { message: { id: a-1 } }\n    behaviors:\n      ok: { message: {} }\n",
            "get:one:ok",
        );
        let response = catalog
            .execute_grpc(
                "default",
                &BTreeMap::new(),
                request(&catalog, json!({ "id": "a-2" })),
                false,
            )
            .await
            .response;
        assert_eq!(
            response,
            GrpcResponse::miss("no active route case matched request")
        );
        assert_eq!(response.status, UNIMPLEMENTED);
        assert_eq!(response.metadata["x-mock-miss"], "true");
    }

    fn explain_catalog() -> Catalog {
        let get = Route::from_yaml(
            r#"
id: get
transport: grpc
match: { service: approvals.v1.Approvals, method: GetApproval }
cases:
  any:
    behaviors:
      ok: { message: {} }
  one:
    match: { message: { id: a-1 } }
    behaviors:
      ok: { message: {} }
"#,
        )
        .unwrap();
        let http = Route::from_yaml(
            "id: http\ntransport: http\nmatch: { method: GET, path: /x }\ncases:\n  any:\n    behaviors:\n      ok: { status: 200 }\n",
        )
        .unwrap();
        Catalog::new(
            vec![get, http],
            CollectionsFile::from_yaml(
                "- id: default\n  routes: [get:any:ok, get:any:ghost, get:one:ok, get:ghost:ok, http:any:ok, ghost:any:ok]\n",
            )
            .unwrap(),
            runtime(),
        )
        .unwrap()
    }

    fn candidate(address: &str, rejection: Rejection) -> Candidate {
        Candidate {
            address: address.to_owned(),
            rejection,
        }
    }

    #[test]
    fn a_hit_explains_skipped_candidates_and_the_winner() {
        let catalog = explain_catalog();
        let call = request(&catalog, json!({ "id": "a-2" }));
        let GrpcResolution::Matched {
            activation,
            skipped,
            conditions,
            ..
        } = catalog.resolve_grpc("default", &BTreeMap::new(), &call)
        else {
            panic!("expected match")
        };
        assert_eq!(activation.address(), "get:any:ok");
        assert_eq!(conditions, &Value::Null);
        assert_eq!(
            skipped,
            vec![
                candidate("ghost:any:ok", Rejection::MissingRoute),
                candidate("http:any:ok", Rejection::Transport),
                candidate("get:ghost:ok", Rejection::MissingCase),
                candidate(
                    "get:one:ok",
                    Rejection::Condition(crate::matching::ConditionFailure {
                        field: "message.id".to_owned(),
                        expected: json!("a-1"),
                        actual: json!("a-2"),
                    })
                ),
                candidate("get:any:ghost", Rejection::MissingBehavior),
            ]
        );
    }

    #[test]
    fn a_miss_lists_every_candidate() {
        let catalog = explain_catalog();
        let mut call = request(&catalog, json!({ "id": "a-1" }));
        call.method = "ListApprovals".to_owned();
        let GrpcResolution::Miss(miss) = catalog.resolve_grpc("default", &BTreeMap::new(), &call)
        else {
            panic!("expected miss")
        };
        let method = || Rejection::Method {
            expected: "approvals.v1.Approvals/GetApproval".to_owned(),
        };
        assert_eq!(miss.reason, "no active route case matched request");
        assert_eq!(
            miss.candidates,
            vec![
                candidate("ghost:any:ok", Rejection::MissingRoute),
                candidate("http:any:ok", Rejection::Transport),
                candidate("get:ghost:ok", method()),
                candidate("get:one:ok", method()),
                candidate("get:any:ghost", method()),
                candidate("get:any:ok", method()),
            ]
        );
    }

    #[test]
    fn an_unknown_collection_misses_with_no_candidates() {
        let catalog = explain_catalog();
        let call = request(&catalog, json!({ "id": "a-1" }));
        let GrpcResolution::Miss(miss) = catalog.resolve_grpc("nope", &BTreeMap::new(), &call)
        else {
            panic!("expected miss")
        };
        assert_eq!(
            miss,
            MissDiagnostic {
                collection: "nope".to_owned(),
                reason: "unknown collection `nope`".to_owned(),
                candidates: Vec::new(),
            }
        );
    }

    #[test]
    fn http_resolution_rejects_grpc_candidates_by_transport() {
        let request = crate::engine::HttpRequest {
            path: "/y".to_owned(),
            ..Default::default()
        };
        let crate::engine::ResolveOutcome::Miss(miss) =
            explain_catalog().resolve_http("default", &request)
        else {
            panic!("expected miss")
        };
        assert_eq!(
            miss.candidates,
            vec![
                candidate("ghost:any:ok", Rejection::MissingRoute),
                candidate(
                    "http:any:ok",
                    Rejection::Path {
                        expected: "/x".to_owned()
                    }
                ),
                candidate("get:ghost:ok", Rejection::Transport),
                candidate("get:one:ok", Rejection::Transport),
                candidate("get:any:ghost", Rejection::Transport),
                candidate("get:any:ok", Rejection::Transport),
            ]
        );
    }

    #[tokio::test]
    async fn a_delayed_behavior_answers_after_its_delay() {
        let catalog = catalog(
            "  any:\n    behaviors:\n      slow: { delay: 20ms, message: { id: late } }\n",
            "get:any:slow",
        );
        let started = std::time::Instant::now();
        let response = catalog
            .execute_grpc(
                "default",
                &BTreeMap::new(),
                request(&catalog, json!({ "id": "a-1" })),
                false,
            )
            .await
            .response;
        assert!(started.elapsed() >= std::time::Duration::from_millis(20));
        assert_eq!(decoded(&catalog, &response), json!({ "id": "late" }));
    }

    #[tokio::test]
    async fn default_fields_are_matchable() {
        let catalog = catalog(
            "  empty:\n    match: { message: { id: '' } }\n    behaviors:\n      ok: { message: { id: empty } }\n",
            "get:empty:ok",
        );
        let response = catalog
            .execute_grpc(
                "default",
                &BTreeMap::new(),
                request(&catalog, json!({})),
                false,
            )
            .await
            .response;
        assert_eq!(decoded(&catalog, &response), json!({ "id": "empty" }));
    }

    #[tokio::test]
    async fn templated_status_and_render_failures() {
        let catalog = catalog(
            r#"
  any:
    behaviors:
      pick:
        status: '${ message.id }'
        message: picked
"#,
            "get:any:pick",
        );
        let response = catalog
            .execute_grpc(
                "default",
                &BTreeMap::new(),
                request(&catalog, json!({ "id": "PERMISSION_DENIED" })),
                false,
            )
            .await
            .response;
        assert_eq!(response.status, 7);
        assert_eq!(response.status_message, "picked");

        let response = catalog
            .execute_grpc(
                "default",
                &BTreeMap::new(),
                request(&catalog, json!({ "id": "NOPE" })),
                false,
            )
            .await
            .response;
        assert_eq!(response.status, INTERNAL);
        assert!(
            response.status_message.contains("not a gRPC status name"),
            "{response:?}"
        );
    }

    #[tokio::test]
    async fn undecodable_requests_are_invalid_arguments() {
        let catalog = catalog(
            "  any:\n    behaviors:\n      ok: { message: {} }\n",
            "get:any:ok",
        );
        let mut call = request(&catalog, json!({ "id": "a-1" }));
        call.body.truncate(3);
        let response = catalog
            .execute_grpc("default", &BTreeMap::new(), call, false)
            .await
            .response;
        assert_eq!(response.status, INVALID_ARGUMENT);
        assert!(
            response.status_message.contains("5-byte header"),
            "{response:?}"
        );
    }

    #[tokio::test]
    async fn a_captured_undecodable_request_is_a_miss_naming_the_decode_error() {
        let catalog = catalog(
            "  any:\n    behaviors:\n      ok: { message: {} }\n",
            "get:any:ok",
        );
        let mut call = request(&catalog, json!({ "id": "a-1" }));
        call.body.truncate(3);
        let call = catalog
            .execute_grpc("default", &BTreeMap::new(), call, true)
            .await;
        let Some(Outcome::Miss(miss)) = call.outcome else {
            panic!("expected a miss")
        };
        assert_eq!(miss.collection, "default");
        assert_eq!(miss.reason, call.response.status_message);
        assert!(miss.candidates.is_empty());
    }

    #[tokio::test]
    async fn captured_raw_pipelines_hand_back_no_decoded_messages() {
        let catalog = catalog_with_pipeline(
            Some("[grpc-frame]"),
            "  any:\n    behaviors:\n      ok: { message: CgNhLTE= }\n",
            "get:any:ok",
        );
        let call = catalog
            .execute_grpc(
                "default",
                &BTreeMap::new(),
                request(&catalog, json!({ "id": "x" })),
                true,
            )
            .await;
        assert_eq!(call.response.status, OK, "{:?}", call.response);
        assert!(matches!(call.outcome, Some(Outcome::Hit(_))));
        assert_eq!((call.request, call.message), (None, None));
    }

    #[tokio::test]
    async fn raw_pipelines_answer_with_literal_bytes() {
        let catalog = catalog_with_pipeline(
            Some("[grpc-frame]"),
            "  any:\n    behaviors:\n      ok: { message: CgNhLTE= }\n",
            "get:any:ok",
        );
        let response = catalog
            .execute_grpc(
                "default",
                &BTreeMap::new(),
                request(&catalog, json!({ "id": "x" })),
                false,
            )
            .await
            .response;
        assert_eq!(response.status, OK, "{response:?}");
        assert_eq!(decoded(&catalog, &response), json!({ "id": "a-1" }));
    }

    #[test]
    fn pipelines_run_in_written_order() {
        let pool = load_descriptor_pool(&ProtoSources {
            roots: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proto")],
            descriptor_sets: vec![],
        })
        .unwrap();
        let input = pool
            .get_message_by_name("approvals.v1.GetApprovalRequest")
            .unwrap();
        let bytes = DynamicMessage::deserialize(input.clone(), json!({ "id": "a-1" }))
            .unwrap()
            .encode_to_vec();

        assert_eq!(
            decode(&frame(&bytes), &DEFAULT_PIPELINE, &input).unwrap(),
            json!({ "id": "a-1" })
        );
        assert_eq!(
            decode(&frame(&bytes), &[PipelineStep::GrpcFrame], &input).unwrap(),
            Value::String(BASE64.encode(&bytes))
        );
        assert_eq!(
            encode(json!({ "id": "a-1" }), &DEFAULT_PIPELINE, &input).unwrap(),
            frame(&bytes)
        );
        assert_eq!(
            encode(
                Value::String(BASE64.encode(&bytes)),
                &[PipelineStep::GrpcFrame],
                &input
            )
            .unwrap(),
            frame(&bytes)
        );
    }

    #[test]
    fn parses_grpc_paths() {
        assert_eq!(
            GrpcRequest::parse_path("/approvals.v1.Approvals/GetApproval"),
            Some((
                "approvals.v1.Approvals".to_owned(),
                "GetApproval".to_owned()
            ))
        );
        assert_eq!(GrpcRequest::parse_path("/approvals.v1.Approvals"), None);
        assert_eq!(GrpcRequest::parse_path("/a/b/c"), None);
    }
}
