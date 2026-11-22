//! Serving gRPC Routes: decode the call through the Route's Pipeline, match Cases over the message
//! envelope, render the Behavior, and encode the answer. Transport framing (HTTP/2, trailers) is
//! the server's job; this module works on bytes. See `docs/guide/grpc.md`.

mod codec;
pub mod execute;
mod plan;
pub mod resolve;
pub mod schema;
mod view;

use std::collections::BTreeMap;

use bytes::Bytes;

use crate::schema::PipelineStep;

pub use codec::GrpcCodecError;
pub use codec::encode_response_async;
pub(crate) use plan::static_unary_replies;
pub use resolve::GrpcResolution;

/// Encodes `message` with the Route's inline or default built-in codec. Named Pipelines
/// need the configured Transformers supplied to [`encode_response_async`].
pub fn encode_response(
    route: &crate::schema::Route,
    method: &prost_reflect::MethodDescriptor,
    message: serde_json::Value,
) -> Result<Vec<u8>, GrpcCodecError> {
    if route.pipeline_name.is_some() {
        return Err(GrpcCodecError::Transform(
            "named Pipelines require asynchronous encoding".into(),
        ));
    }
    codec::encode(message, codec::pipeline(route), &method.output())
}

/// How the server answers a call: one response, or a server-streaming body.
pub enum GrpcReply {
    Unary(GrpcResponse),
    Stream(futures_util::stream::BoxStream<'static, StreamPart>),
}

/// One piece of a server-streaming body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamPart {
    /// A framed response message.
    Message(Vec<u8>),
    /// The end of the call: its status and metadata (its `body` is empty).
    End(GrpcResponse),
}

/// The default Pipeline for gRPC Routes (Q2).
pub const DEFAULT_PIPELINE: [PipelineStep; 2] = [PipelineStep::GrpcFrame, PipelineStep::Protobuf];

/// gRPC status codes Decoy sends itself.
pub const OK: u8 = 0;
pub const INVALID_ARGUMENT: u8 = 3;
pub const RESOURCE_EXHAUSTED: u8 = 8;
pub const UNIMPLEMENTED: u8 = 12;
pub const INTERNAL: u8 = 13;
pub const UNAVAILABLE: u8 = 14;

/// One unary call as the server received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcRequest {
    pub service: String,
    pub method: String,
    /// Request headers, lowercased.
    pub metadata: BTreeMap<String, String>,
    /// The HTTP/2 request body.
    pub body: Bytes,
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use prost::Message as _;
    use prost_reflect::{DynamicMessage, MessageDescriptor};
    use serde_json::{Value, json};

    use super::codec::{decode, encode, frame, unframe};
    use super::schema::find_method;
    use super::*;
    use crate::collections::CollectionsFile;
    use crate::config::ProtoSources;
    use crate::engine::{Candidate, Catalog, MissDiagnostic, Rejection};
    use crate::events::Outcome;
    use crate::http::RuntimeConfig;
    use crate::protos::load_descriptor_pool;
    use crate::schema::Route;

    fn catalog(cases: &str, collection: &str) -> Catalog {
        catalog_with_pipeline(None, cases, collection)
    }

    fn runtime() -> RuntimeConfig {
        let protos = load_descriptor_pool(&ProtoSources {
            roots: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proto")],
            descriptor_sets: vec![],
            files: vec![],
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
            body: frame(&message.encode_to_vec()).into(),
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
        let message = DynamicMessage::decode(output, unframe(&response.body).unwrap()).unwrap();
        serde_json::to_value(&message).unwrap()
    }

    #[test]
    fn named_pipelines_validate_the_default_protobuf_direction() {
        for (input, output, conditions, message, invalid_field) in [
            (
                vec![],
                vec!["protobuf", "grpc-frame"],
                "{ unknownInput: true }",
                "{ id: ok }",
                "unknownInput",
            ),
            (
                vec!["grpc-frame", "protobuf"],
                vec![],
                "{ id: ok }",
                "{ unknownOutput: true }",
                "unknownOutput",
            ),
        ] {
            let route = Route::from_yaml(&format!("id: get\ntransport: grpc\npipeline: named\nmatch: {{ service: approvals.v1.Approvals, method: GetApproval }}\ncases: {{ any: {{ match: {{ message: {conditions} }}, behaviors: {{ ok: {{ message: {message} }} }} }} }}")).unwrap();
            let mut runtime = runtime();
            runtime.pipelines.insert(
                "named".into(),
                crate::config::PipelineConfig {
                    input: input.into_iter().map(str::to_owned).collect(),
                    output: output.into_iter().map(str::to_owned).collect(),
                },
            );
            let collections =
                CollectionsFile::from_yaml("- id: default\n  routes: [get:any:ok]").unwrap();
            let error = Catalog::new(vec![route], collections, runtime).unwrap_err();
            assert!(error.to_string().contains(invalid_field), "{error}");
        }
    }

    #[test]
    fn named_grpc_builtins_reject_impossible_transitions_without_reordering_custom_steps() {
        let route = Route::from_yaml("id: get\ntransport: grpc\npipeline: named\nmatch: { service: approvals.v1.Approvals, method: GetApproval }\ncases: { any: { behaviors: { ok: { message: { id: ok } } } } }").unwrap();
        let collections =
            CollectionsFile::from_yaml("- id: default\n  routes: [get:any:ok]").unwrap();
        for (input, output, valid) in [
            (vec!["protobuf", "grpc-frame"], vec![], false),
            (vec!["grpc-frame", "protobuf", "protobuf"], vec![], false),
            (vec![], vec!["grpc-frame"], false),
            (vec![], vec!["protobuf", "grpc-frame", "protobuf"], false),
            (
                vec!["custom", "grpc-frame", "custom", "protobuf"],
                vec!["protobuf", "grpc-frame"],
                true,
            ),
            (
                vec!["grpc-frame", "protobuf", "custom"],
                vec!["protobuf", "grpc-frame", "custom"],
                true,
            ),
        ] {
            let mut runtime = runtime();
            runtime.commands.insert(
                "custom".into(),
                crate::config::CommandsConfig {
                    input: Some(vec!["not-run-during-validation".into()]),
                    output: Some(vec!["not-run-during-validation".into()]),
                    dir: std::env::temp_dir(),
                },
            );
            runtime.pipelines.insert(
                "named".into(),
                crate::config::PipelineConfig {
                    input: input.into_iter().map(str::to_owned).collect(),
                    output: output.into_iter().map(str::to_owned).collect(),
                },
            );
            let result = Catalog::new(vec![route.clone()], collections.clone(), runtime);
            assert_eq!(result.is_ok(), valid, "{:?}", result.err());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn named_input_misses_try_other_routes_and_worker_errors_end_the_call() {
        use crate::{
            config::{PipelineConfig, WorkerConfig},
            workers,
        };

        // A worker replying to the handshake and three consecutive input calls.
        let dir = tempfile::tempdir().unwrap();
        let frames = [
            (
                "ready",
                json!({"kind":"ready","protocol":1,"name":"filter","version":"1","directions":["input"]}),
            ),
            (
                "miss1",
                json!({"kind":"result","id":1,"ok":false,"miss":"unknown record"}),
            ),
            (
                "miss2",
                json!({"kind":"result","id":2,"ok":false,"miss":"unknown record"}),
            ),
            (
                "failed",
                json!({"kind":"result","id":3,"ok":false,"error":"broken transformer"}),
            ),
        ];
        for (name, message) in frames {
            let mut body = Vec::new();
            rmpv::encode::write_value(&mut body, &workers::value::to_msgpack(&message)).unwrap();
            let mut frame = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
            frame.extend(body);
            std::fs::write(dir.path().join(name), frame).unwrap();
        }
        let worker = WorkerConfig {
            command: vec!["sh".into(), "-c".into(), "for reply in ready miss1 miss2 failed; do dd bs=4096 count=1 of=/dev/null 2>/dev/null; cat \"$reply\"; done; dd bs=4096 count=1 of=/dev/null 2>/dev/null".into()],
            dir: dir.path().to_owned(),
            timeout: crate::config::DEFAULT_TRANSFORM_TIMEOUT,
        };
        let (workers, supervisor) = workers::start(
            &BTreeMap::from([("filter".into(), worker)]),
            &crate::events::channel(),
        )
        .await
        .unwrap();
        let mut runtime = runtime();
        runtime.workers = workers;
        runtime.pipelines.insert(
            "filtered".into(),
            PipelineConfig {
                input: vec!["filter".into()],
                output: vec!["protobuf".into(), "grpc-frame".into()],
            },
        );
        let plain = Route::from_yaml("id: plain\ntransport: grpc\nmatch: { service: approvals.v1.Approvals, method: GetApproval }\ncases: { any: { behaviors: { ok: { message: { id: fallback } } } } }").unwrap();
        let mut filtered = plain.clone();
        filtered.id = "filtered".into();
        filtered.pipeline_name = Some("filtered".into());
        let collections = CollectionsFile::from_yaml("- id: only\n  routes: [filtered:any:ok]\n- id: both\n  routes: [plain:any:ok, filtered:any:ok]").unwrap();
        let catalog = Catalog::new(vec![plain, filtered], collections, runtime).unwrap();
        let call = request(&catalog, json!({}));
        let miss = catalog
            .execute_grpc("only", &[], call.clone(), false)
            .await
            .response;
        assert_eq!(miss.status, UNIMPLEMENTED);
        assert_eq!(miss.metadata["x-mock-miss"], "true");
        let fallback = catalog
            .execute_grpc("both", &[], call.clone(), false)
            .await
            .response;
        assert_eq!(decoded(&catalog, &fallback)["id"], "fallback");
        let failed = catalog
            .execute_grpc("both", &[], call, false)
            .await
            .response;
        assert_eq!(failed.status, INVALID_ARGUMENT);
        assert!(failed.status_message.contains("broken transformer"));
        supervisor.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protobuf_output_step_reads_json_from_the_preceding_command() {
        use crate::config::{CommandsConfig, PipelineConfig};

        let route = Route::from_yaml("id: get\ntransport: grpc\npipeline: named\nmatch: { service: approvals.v1.Approvals, method: GetApproval }\ncases: { any: { behaviors: { ok: { message: { id: original } } } } }").unwrap();
        let collections =
            CollectionsFile::from_yaml("- id: default\n  routes: [get:any:ok]").unwrap();
        for (command_output, valid) in [(r#"{"id":"transformed"}"#, true), ("not JSON", false)] {
            let mut runtime = runtime();
            runtime.commands.insert(
                "replace".into(),
                CommandsConfig {
                    input: None,
                    output: Some(
                        ["sh", "-c", "printf '%s' \"$1\"", "replace", command_output]
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                    ),
                    dir: std::env::temp_dir(),
                },
            );
            runtime.pipelines.insert(
                "named".into(),
                PipelineConfig {
                    input: vec![],
                    output: vec!["replace".into(), "protobuf".into(), "grpc-frame".into()],
                },
            );
            let catalog = Catalog::new(vec![route.clone()], collections.clone(), runtime).unwrap();
            let response = catalog
                .execute_grpc("default", &[], request(&catalog, json!({})), false)
                .await
                .response;
            if valid {
                assert_eq!(response.status, OK, "{response:?}");
                assert_eq!(decoded(&catalog, &response), json!({"id": "transformed"}));
            } else {
                assert_eq!(response.status, INTERNAL, "{response:?}");
                assert!(response.body.is_empty());
                assert!(
                    response
                        .status_message
                        .contains("transformer `protobuf` output failed:"),
                    "{response:?}"
                );
            }
        }
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
                &[],
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
    async fn static_message_with_templated_metadata_reads_each_request() {
        let catalog = catalog(
            "  any:\n    behaviors:\n      ok:\n        message: { id: literal }\n        metadata: { x-id: '${ message.id }' }\n",
            "get:any:ok",
        );
        for id in ["a-1", "a-2"] {
            let call = catalog
                .execute_grpc("default", &[], request(&catalog, json!({ "id": id })), true)
                .await;
            assert_eq!(call.response.status, OK);
            assert_eq!(call.response.metadata["x-id"], id);
            assert_eq!(call.message.unwrap()["id"], "literal");
        }
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
                &[],
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
                &[],
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
        } = catalog.resolve_grpc("default", &[], &call)
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
                        error: None,
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
        let GrpcResolution::Miss(miss) = catalog.resolve_grpc("default", &[], &call) else {
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
        let GrpcResolution::Miss(miss) = catalog.resolve_grpc("nope", &[], &call) else {
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
        let request = crate::http::HttpRequest {
            path: "/y".to_owned(),
            ..Default::default()
        };
        let crate::http::ResolveOutcome::Miss(miss) =
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
                &[],
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
            .execute_grpc("default", &[], request(&catalog, json!({})), false)
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
                &[],
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
                &[],
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
    async fn numeric_status_template_checks_the_rendered_code_on_each_call() {
        let catalog = catalog(
            "  any:\n    behaviors:\n      pick: { status: '${ to_number(message.id) }' }\n",
            "get:any:pick",
        );
        for (id, status) in [("5", 5), ("17", INTERNAL), ("unknown", INTERNAL)] {
            let response = catalog
                .execute_grpc(
                    "default",
                    &[],
                    request(&catalog, json!({ "id": id })),
                    false,
                )
                .await
                .response;
            assert_eq!(response.status, status);
            if status == INTERNAL {
                assert!(response.status_message.contains("not a gRPC status name"));
            } else {
                assert!(response.status_message.is_empty());
            }
        }
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
            .execute_grpc("default", &[], call, false)
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
        let call = catalog.execute_grpc("default", &[], call, true).await;
        let Some(Outcome::Miss(miss)) = call.outcome else {
            panic!("expected a miss")
        };
        assert_eq!(miss.collection, "default");
        assert_eq!(miss.reason, call.response.status_message);
        assert!(
            matches!(
                &miss.candidates[..],
                [Candidate {
                    rejection: Rejection::Decode { .. },
                    ..
                }]
            ),
            "{:?}",
            miss.candidates
        );
    }

    #[test]
    fn a_decode_failure_rejects_its_route_and_matching_goes_on() {
        let route = |id: &str, pipeline: &str, message: &str| {
            Route::from_yaml(&format!(
                "id: {id}\ntransport: grpc\nmatch: {{ service: approvals.v1.Approvals, method: GetApproval }}\n{pipeline}cases:\n  any:\n    behaviors:\n      ok: {{ message: {message} }}\n"
            ))
            .unwrap()
        };
        let catalog = Catalog::new(
            vec![
                route("raw", "pipeline: [grpc-frame]\n", "CgNhLTE="),
                route("proto", "", "{}"),
            ],
            CollectionsFile::from_yaml("- id: default\n  routes: [raw:any:ok, proto:any:ok]\n")
                .unwrap(),
            runtime(),
        )
        .unwrap();
        let mut call = request(&catalog, json!({}));
        // A truncated varint: a valid frame whose payload is not an `ApprovalRequest`.
        call.body = frame(&[0x08]).into();
        let GrpcResolution::Matched {
            activation,
            skipped,
            ..
        } = catalog.resolve_grpc("default", &[], &call)
        else {
            panic!("expected the raw Route to match")
        };
        assert_eq!(activation.address(), "raw:any:ok");
        assert!(
            matches!(&skipped[..], [Candidate { address, rejection: Rejection::Decode { .. } }] if address == "proto:any:ok"),
            "{skipped:?}"
        );
    }

    #[test]
    fn a_cached_decode_failure_rejects_every_route_on_that_pipeline() {
        let route = |id: &str| {
            Route::from_yaml(&format!(
                "id: {id}\ntransport: grpc\nmatch: {{ service: approvals.v1.Approvals, method: GetApproval }}\ncases:\n  any:\n    behaviors:\n      ok: {{ message: {{}} }}\n"
            ))
            .unwrap()
        };
        let catalog = Catalog::new(
            vec![route("first"), route("second")],
            CollectionsFile::from_yaml("- id: default\n  routes: [first:any:ok, second:any:ok]\n")
                .unwrap(),
            runtime(),
        )
        .unwrap();
        let mut call = request(&catalog, json!({}));
        call.body = frame(&[0x08]).into();
        let GrpcResolution::Failed {
            response,
            candidates,
        } = catalog.resolve_grpc("default", &[], &call)
        else {
            panic!("expected a decode failure")
        };
        // Later entries are tried first; the answer names the first Route that failed.
        assert!(
            response
                .status_message
                .starts_with("route `second` could not decode"),
            "{response:?}"
        );
        assert!(
            matches!(
                &candidates[..],
                [
                    Candidate { rejection: Rejection::Decode { error: a }, .. },
                    Candidate { rejection: Rejection::Decode { error: b }, .. },
                ] if a == b
            ),
            "{candidates:?}"
        );
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
                &[],
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
                &[],
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
            files: vec![],
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

    fn approval_request() -> MessageDescriptor {
        load_descriptor_pool(&ProtoSources {
            roots: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proto")],
            descriptor_sets: vec![],
            files: vec![],
        })
        .unwrap()
        .get_message_by_name("approvals.v1.GetApprovalRequest")
        .unwrap()
    }

    // `validate_pipeline` rejects these at load, so reaching the codec with one is a bug.
    #[test]
    #[should_panic(expected = "no step follows `protobuf`")]
    fn decode_panics_on_a_step_after_protobuf() {
        let _ = decode(
            &[],
            &[PipelineStep::Protobuf, PipelineStep::GrpcFrame],
            &approval_request(),
        );
    }

    #[test]
    #[should_panic(expected = "no step follows `protobuf`")]
    fn encode_panics_on_a_repeated_protobuf_step() {
        let _ = encode(
            json!({}),
            &[PipelineStep::Protobuf, PipelineStep::Protobuf],
            &approval_request(),
        );
    }

    #[test]
    fn float_fields_decode_as_written() {
        let pool = load_descriptor_pool(&ProtoSources {
            roots: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proto")],
            descriptor_sets: vec![],
            files: vec![],
        })
        .unwrap();
        let input = pool.get_message_by_name("approvals.v1.Sample").unwrap();
        let sample = json!({
            "ratio": 0.1,
            "ratios": [0.1, 2.5],
            "weights": { "a": 0.3 },
            "boxedRatio": 0.7,
        });
        let bytes = DynamicMessage::deserialize(input.clone(), sample)
            .unwrap()
            .encode_to_vec();

        let message = decode(&frame(&bytes), &DEFAULT_PIPELINE, &input).unwrap();
        assert_eq!(message["ratio"], json!(0.1));
        assert_eq!(message["ratios"], json!([0.1, 2.5]));
        assert_eq!(message["weights"], json!({ "a": 0.3 }));
        assert_eq!(message["boxedRatio"], json!(0.7));
    }

    #[test]
    fn codec_error_messages_are_stable() {
        // These reach grpc-message and miss reasons on the wire.
        let pool = load_descriptor_pool(&ProtoSources {
            roots: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proto")],
            descriptor_sets: vec![],
            files: vec![],
        })
        .unwrap();
        let input = pool
            .get_message_by_name("approvals.v1.GetApprovalRequest")
            .unwrap();
        let message = |error: &dyn std::fmt::Display| error.to_string();

        assert_eq!(
            message(&unframe(&[0, 0]).unwrap_err()),
            "gRPC frame shorter than its 5-byte header"
        );
        assert_eq!(
            message(&unframe(&[1, 0, 0, 0, 0]).unwrap_err()),
            "compressed gRPC messages are not supported"
        );
        assert_eq!(
            message(&unframe(&[0, 0, 0, 0, 2, 9]).unwrap_err()),
            "gRPC frame declares 2 bytes but carries 1; unary calls take one message"
        );
        assert_eq!(
            message(&decode(&[0xff], &[PipelineStep::Protobuf], &input).unwrap_err()),
            "not a valid approvals.v1.GetApprovalRequest: failed to decode Protobuf message: invalid varint"
        );
        assert_eq!(
            message(&encode(json!({ "nope": 1 }), &DEFAULT_PIPELINE, &input).unwrap_err()),
            "response does not fit approvals.v1.GetApprovalRequest: unrecognized field name 'nope'"
        );
        assert_eq!(
            message(&encode(json!(1), &[PipelineStep::GrpcFrame], &input).unwrap_err()),
            "expected a base64 string, got 1"
        );
        assert_eq!(
            message(&encode(json!("!"), &[PipelineStep::GrpcFrame], &input).unwrap_err()),
            "message is not base64: Invalid symbol 33, offset 0."
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
