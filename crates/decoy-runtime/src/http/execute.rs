use std::collections::BTreeMap;

use bytes::Bytes;

use super::forward::{
    ForwardRequest, ForwardResponse, ForwardingError, forward_passthrough, target_url,
};
use super::pipeline::{
    PassthroughOutputError, PreparedInput, PreparedRoute, describes_original_body, mock_output,
    passthrough_output, worker_request,
};
use super::resolve::{HttpRequest, ResolveOutcome};
use crate::collections::Activation;
use crate::engine::{Catalog, MissDiagnostic};
use crate::events::{Match, Outcome};
use crate::http::{BodyPlan, HttpResponsePlan, PassthroughPlan, RequestMetadata, ResponsePlan};
use crate::pipeline::Pipeline;
use crate::request::RequestEnvelope;
use crate::schema::{Behavior, HttpMethod};
use crate::workers::CallRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpExecutionRequest {
    pub method: HttpMethod,
    pub path: String,
    pub path_and_query: String,
    pub headers: BTreeMap<String, String>,
    pub body: Bytes,
    pub metadata: RequestMetadata,
}

#[derive(Debug)]
pub enum HttpExecutionOutcome {
    Response {
        activation: Option<Activation>,
        response: ForwardResponse,
    },
    ForwardingError {
        activation: Activation,
        source: ForwardingError,
    },
}

impl Catalog {
    pub async fn execute_http(
        &self,
        collection_id: &str,
        request: HttpExecutionRequest,
    ) -> HttpExecutionOutcome {
        self.execute_http_with_overrides(collection_id, &[], request, false)
            .await
            .0
    }

    /// With `explain`, also returns the event [`Outcome`]; without it, serving skips building one.
    pub(crate) async fn execute_http_with_overrides(
        &self,
        collection_id: &str,
        overrides: &[Activation],
        request: HttpExecutionRequest,
        explain: bool,
    ) -> (HttpExecutionOutcome, Option<Outcome>) {
        let resolve_request = HttpRequest {
            method: request.method.clone(),
            path: request.path.clone(),
            envelope: RequestEnvelope::from_parts(
                request.metadata.original_base_url.as_deref(),
                &request.path_and_query,
                &request.headers,
                request.body.clone(),
            ),
        };

        let mut prepared = BTreeMap::new();
        let selection = self
            .select_http(
                collection_id,
                overrides,
                &resolve_request,
                &request.metadata,
                explain,
                &mut prepared,
            )
            .await;
        let (resolved, render_failed) = match selection {
            Ok(resolved) => resolved,
            Err(()) => {
                let response = transform_failure();
                return (
                    HttpExecutionOutcome::Response {
                        activation: None,
                        response,
                    },
                    explain.then(|| {
                        Outcome::Miss(MissDiagnostic {
                            collection: collection_id.to_owned(),
                            reason: PIPELINE_FAILURE.into(),
                            candidates: Vec::new(),
                        })
                    }),
                );
            }
        };
        match resolved {
            ResolveOutcome::Matched {
                activation,
                path_params,
                plan,
                delay,
                skipped,
                conditions,
            } => {
                if let Some(delay) = delay {
                    tokio::time::sleep(delay).await;
                }
                let matched = explain.then(|| Match {
                    activation: activation.clone(),
                    skipped,
                    conditions: conditions.clone(),
                    path_params,
                });
                let (input, output) = prepared.remove(&activation.route).map_or_else(
                    || (None, None),
                    |PreparedRoute { input, pipeline }| {
                        (
                            input.ok().flatten(),
                            pipeline.has_output().then_some(pipeline),
                        )
                    },
                );
                match plan {
                    ResponsePlan::Response(plan) => {
                        let response = self
                            .execute_mock_output(
                                &activation,
                                plan,
                                output.as_ref().filter(|_| !render_failed),
                                worker_request(&resolve_request),
                            )
                            .await;
                        (
                            HttpExecutionOutcome::Response {
                                activation: Some(activation),
                                response,
                            },
                            matched.map(Outcome::Hit),
                        )
                    }
                    ResponsePlan::Passthrough(plan) => {
                        let upstream_url = matched.as_ref().and_then(|_| {
                            target_url(plan.base_url.as_deref()?, &request.path_and_query)
                                .ok()
                                .map(String::from)
                        });
                        let (outcome, upstream_status) = execute_passthrough(
                            &plan,
                            activation,
                            forwarded_request(request, input),
                            output.as_ref(),
                            self.runtime
                                .max_body_bytes
                                .unwrap_or(crate::config::DEFAULT_MAX_BODY_BYTES),
                            worker_request(&resolve_request),
                        )
                        .await;
                        (
                            outcome,
                            matched.map(|matched| Outcome::Passthrough {
                                matched,
                                upstream_url,
                                upstream_status,
                            }),
                        )
                    }
                }
            }
            ResolveOutcome::Miss(miss) => (
                HttpExecutionOutcome::Response {
                    activation: None,
                    response: HttpResponsePlan::explained_miss(&miss).into(),
                },
                explain.then_some(Outcome::Miss(miss)),
            ),
        }
    }

    async fn execute_mock_output(
        &self,
        activation: &Activation,
        plan: HttpResponsePlan,
        output: Option<&Pipeline>,
        context: CallRequest<'_>,
    ) -> ForwardResponse {
        let Some(output) = output else {
            return plan.into();
        };
        let behavior =
            &self.routes[&activation.route].cases[&activation.case].behaviors[&activation.behavior];
        let explicit_type = matches!(behavior, Behavior::Response(response) if response.headers.keys().any(|key| key.eq_ignore_ascii_case("content-type")));
        mock_output(output, plan, explicit_type, context)
            .await
            .unwrap_or_else(|_| pipeline_failure(&activation.route, "output"))
    }

    /// Run input I/O only when resolution reaches that Route. The diagnostic pass on a miss
    /// reuses the prepared inputs so a stateful Transformer runs at most once per Route.
    pub(super) async fn select_http(
        &self,
        collection: &str,
        overrides: &[Activation],
        request: &HttpRequest,
        metadata: &RequestMetadata,
        mut explain: bool,
        prepared: &mut BTreeMap<String, PreparedRoute>,
    ) -> Result<(ResolveOutcome<'_>, bool), ()> {
        loop {
            let mut candidates = explain.then(Vec::new);
            let mut entries = match self.activations(collection, overrides) {
                Ok(entries) => entries.rev(),
                Err(error) => {
                    return Ok((
                        ResolveOutcome::Miss(MissDiagnostic {
                            collection: collection.into(),
                            reason: error.to_string(),
                            candidates: vec![],
                        }),
                        false,
                    ));
                }
            };
            let mut pending = None;
            loop {
                let remaining = pending.take().into_iter().chain(&mut entries);
                match self.resolve_candidates(
                    remaining,
                    request,
                    metadata,
                    prepared,
                    &mut candidates,
                ) {
                    Ok(Some(hit)) => return Ok(hit),
                    Ok(None) => break,
                    Err((route, activation)) => {
                        let input = PreparedRoute::prepare(route, &self.runtime, request)
                            .await
                            .map_err(|_| log_pipeline_failure(&route.id, "input"))?;
                        prepared.insert(route.id.clone(), input);
                        pending = Some(activation);
                    }
                }
            }
            if let Some(candidates) = candidates {
                return Ok((
                    ResolveOutcome::Miss(MissDiagnostic {
                        collection: collection.into(),
                        reason: "no active route case matched request".into(),
                        candidates,
                    }),
                    false,
                ));
            }
            explain = true;
        }
    }
}

fn forwarded_request(
    request: HttpExecutionRequest,
    input: Option<PreparedInput>,
) -> ForwardRequest {
    let HttpExecutionRequest {
        method,
        path_and_query,
        mut headers,
        mut body,
        ..
    } = request;
    if let Some(input) = input {
        if input.envelope.raw_body != body {
            headers.retain(|name, _| !describes_original_body(name));
        }
        if let Some(content_type) = input.content_type {
            headers.retain(|name, _| !name.eq_ignore_ascii_case("content-type"));
            headers.insert("content-type".into(), content_type.into());
        }
        body = input.envelope.raw_body;
    }
    ForwardRequest {
        method,
        path_and_query,
        headers,
        body,
    }
}

async fn execute_passthrough(
    plan: &PassthroughPlan,
    activation: Activation,
    request: ForwardRequest,
    output: Option<&Pipeline>,
    limit: usize,
    context: CallRequest<'_>,
) -> (HttpExecutionOutcome, Option<u16>) {
    let is_head = request.method == HttpMethod::Head;
    let source = forward_passthrough(plan, request).await;
    let upstream_status = source.as_ref().ok().map(|response| response.status);
    let source = match (source, output) {
        (Ok(response), Some(output))
            if !is_head && !matches!(response.status, 100..=199 | 204 | 205 | 304) =>
        {
            passthrough_output(output, response, limit, context)
                .await
                .or_else(|error| match error {
                    PassthroughOutputError::Upstream(source) => Err(source),
                    PassthroughOutputError::Transform(_) => {
                        Ok(pipeline_failure(&activation.route, "output"))
                    }
                })
        }
        (source, _) => source,
    };
    let outcome = match source {
        Ok(response) => HttpExecutionOutcome::Response {
            activation: Some(activation),
            response,
        },
        Err(source) => HttpExecutionOutcome::ForwardingError { activation, source },
    };
    (outcome, upstream_status)
}

const PIPELINE_FAILURE: &str = "HTTP Pipeline failed";

fn log_pipeline_failure(route: &str, direction: &str) {
    crate::events::error(format!("route `{route}` {direction} Pipeline failed"));
}

fn pipeline_failure(route: &str, direction: &str) -> ForwardResponse {
    log_pipeline_failure(route, direction);
    transform_failure()
}

fn transform_failure() -> ForwardResponse {
    HttpResponsePlan {
        status: 500,
        headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
        body: Some(BodyPlan::Json(
            serde_json::json!({ "error": PIPELINE_FAILURE }),
        )),
    }
    .into()
}

impl From<crate::http::HttpResponsePlan> for ForwardResponse {
    fn from(response: crate::http::HttpResponsePlan) -> Self {
        let body = response.body.map(BodyPlan::into_bytes).unwrap_or_default();
        Self {
            status: response.status,
            headers: response.headers.into_iter().collect(),
            body,
            stream: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::collections::CollectionsFile;
    use crate::engine::test_support::{candidate, catalog, explain_catalog, get, spawn_upstream};
    use crate::engine::{MissDiagnostic, Rejection};
    use crate::http::{BodyPlan, ResolveOutcome, RuntimeConfig};
    use crate::matching::ConditionFailure;
    use crate::schema::Route;

    fn execution_request(path: &str, path_and_query: &str) -> HttpExecutionRequest {
        HttpExecutionRequest {
            method: HttpMethod::Get,
            path: path.to_owned(),
            path_and_query: path_and_query.to_owned(),
            headers: BTreeMap::new(),
            body: Bytes::new(),
            metadata: RequestMetadata::default(),
        }
    }

    #[tokio::test]
    async fn execution_forwards_only_when_selected_behavior_is_passthrough() {
        let (target, received, handle) = spawn_upstream();
        let route = Route::from_yaml(&format!(
            r#"
id: get-user
transport: http
match:
  method: GET
  path: /users/{{id}}
cases:
  user-123:
    match:
      pathParams:
        id: "123"
    behaviors:
      success:
        status: 200
        body:
          ok: true
      real:
        kind: passthrough
        target:
          baseUrl: {target}
"#
        ))
        .unwrap();
        let catalog = Catalog::new(
            vec![route],
            CollectionsFile::from_yaml(
                r#"
- id: local
  routes:
    - get-user:user-123:real
"#,
            )
            .unwrap(),
            RuntimeConfig::default(),
        )
        .unwrap();

        let mut request = execution_request("/users/123", "/users/123?expand=true");
        request.headers = BTreeMap::from([("x-keep".to_owned(), "yes".to_owned())]);
        request.body = Bytes::from_static(b"body-from-caller");
        let outcome = catalog.execute_http("local", request).await;

        let HttpExecutionOutcome::Response {
            activation,
            response,
        } = outcome
        else {
            panic!("expected forwarded response")
        };
        assert_eq!(
            activation.map(|activation| activation.address()),
            Some("get-user:user-123:real".to_owned())
        );
        assert_eq!(response.status, 202);
        assert_eq!(response.header("x-upstream"), Some("yes"));
        let body: Vec<bytes::Bytes> =
            futures_util::TryStreamExt::try_collect(response.stream.unwrap().0)
                .await
                .unwrap();
        assert_eq!(body.concat(), b"accepted");

        let request = received.recv().unwrap();
        handle.join().unwrap();
        assert_eq!(request.request_line, "GET /users/123?expand=true HTTP/1.1");
        assert_eq!(request.headers.get("x-keep"), Some(&"yes".to_owned()));
        assert_eq!(request.body, "body-from-caller");
    }

    #[tokio::test]
    async fn execution_returns_mock_response_without_forwarding_when_response_behavior_selected() {
        let catalog = catalog(
            r#"
- id: local
  routes:
    - get-user:user-123:success
"#,
        );

        let outcome = catalog
            .execute_http("local", execution_request("/users/123", "/users/123"))
            .await;

        let HttpExecutionOutcome::Response {
            activation,
            response,
        } = outcome
        else {
            panic!("expected mock response")
        };
        assert_eq!(
            activation.map(|activation| activation.address()),
            Some("get-user:user-123:success".to_owned())
        );
        assert_eq!(response.status, 200);
        assert_eq!(response.header("content-type"), Some("application/json"));
        assert_eq!(response.body, &br#"{"id":"123"}"#[..]);
    }

    #[tokio::test]
    async fn execution_surfaces_forwarding_errors_instead_of_mock_misses() {
        let route = Route::from_yaml(
            r#"
id: get-user
transport: http
match:
  method: GET
  path: /users/{id}
cases:
  user-123:
    match:
      pathParams:
        id: "123"
    behaviors:
      real:
        kind: passthrough
"#,
        )
        .unwrap();
        let catalog = Catalog::new(
            vec![route],
            CollectionsFile::from_yaml(
                r#"
- id: local
  routes:
    - get-user:user-123:real
"#,
            )
            .unwrap(),
            RuntimeConfig::default(),
        )
        .unwrap();

        let outcome = catalog
            .execute_http("local", execution_request("/users/123", "/users/123"))
            .await;

        let HttpExecutionOutcome::ForwardingError { activation, source } = outcome else {
            panic!("expected forwarding error")
        };
        assert_eq!(activation.address(), "get-user:user-123:real");
        assert!(matches!(source, ForwardingError::MissingTarget));
    }

    #[tokio::test]
    async fn execution_keeps_unmatched_requests_fail_closed() {
        let catalog = catalog(
            r#"
- id: local
  routes:
    - get-user:user-123:success
"#,
        );

        let outcome = catalog
            .execute_http("local", execution_request("/unknown", "/unknown"))
            .await;

        let HttpExecutionOutcome::Response {
            activation,
            response,
        } = outcome
        else {
            panic!("expected fail-closed response")
        };
        assert_eq!(activation, None);
        assert_eq!(response.status, 501);
        assert_eq!(response.header("x-mock-miss"), Some("true"));
    }

    #[tokio::test]
    async fn serving_skips_explanations_yet_picks_the_same_winner_and_miss_body() {
        let catalog = explain_catalog();
        let HttpExecutionOutcome::Response { activation, .. } = catalog
            .execute_http("local", execution_request("/users/456", "/users/456"))
            .await
        else {
            panic!("expected a response")
        };
        assert_eq!(
            activation.map(|activation| activation.address()).as_deref(),
            Some("get-user:any:fallback")
        );

        let ResolveOutcome::Miss(miss) = catalog.resolve_http("local", &get("/nope")) else {
            panic!("expected miss")
        };
        assert_eq!(miss.candidates.len(), 7);
        let HttpExecutionOutcome::Response { response, .. } = catalog
            .execute_http("local", execution_request("/nope", "/nope"))
            .await
        else {
            panic!("expected fail-closed response")
        };
        let body: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["candidates"], serde_json::json!(miss.candidates));
    }

    #[tokio::test]
    async fn a_miss_lists_every_candidate_in_the_fail_closed_body() {
        let catalog = catalog(
            r#"
- id: local
  routes:
    - get-user:user-123:success
"#,
        );

        assert_eq!(
            catalog.resolve_http("local", &get("/users/456")),
            ResolveOutcome::Miss(MissDiagnostic {
                collection: "local".to_owned(),
                reason: "no active route case matched request".to_owned(),
                candidates: vec![candidate(
                    "get-user:user-123:success",
                    Rejection::Condition(ConditionFailure {
                        field: "pathParams.id".to_owned(),
                        expected: serde_json::json!("123"),
                        actual: serde_json::json!("456"),
                        error: None,
                    })
                )],
            })
        );

        let HttpExecutionOutcome::Response { response, .. } = catalog
            .execute_http("local", execution_request("/users/456", "/users/456"))
            .await
        else {
            panic!("expected fail-closed response")
        };
        assert_eq!(response.status, 501);
        assert_eq!(response.header("x-mock-miss"), Some("true"));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&response.body).unwrap(),
            serde_json::json!({
                "error": "no active route case matched request",
                "candidates": [{
                    "address": "get-user:user-123:success",
                    "reason": "condition",
                    "field": "pathParams.id",
                    "expected": "123",
                    "actual": "456",
                }],
            })
        );
    }

    #[tokio::test]
    async fn a_template_builds_the_envelope_when_no_condition_did() {
        let route = Route::from_yaml(
            "id: echo\ntransport: http\nmatch: { method: GET, path: '/users/{id}' }\ncases:\n  any:\n    behaviors:\n      ok: { status: 200, body: { id: '${ pathParams.id }' } }\n",
        )
        .unwrap();
        let catalog = Catalog::new(
            vec![route],
            CollectionsFile::from_yaml("- id: local\n  routes: [echo:any:ok]\n").unwrap(),
            RuntimeConfig::default(),
        )
        .unwrap();

        let HttpExecutionOutcome::Response { response, .. } = catalog
            .execute_http("local", execution_request("/users/42", "/users/42"))
            .await
        else {
            panic!("expected a response")
        };
        assert_eq!(response.body, &br#"{"id":"42"}"#[..]);
    }

    #[tokio::test]
    async fn predicates_and_templates_see_each_candidates_own_path_params() {
        let by_id = Route::from_yaml(
            "id: by-id\ntransport: http\nmatch: { method: GET, path: '/users/{id}' }\ncases:\n  eight:\n    match: \"${ pathParams.id == '8' }\"\n    behaviors:\n      ok: { status: 200 }\n  nine:\n    match: \"${ pathParams.id == '9' }\"\n    behaviors:\n      ok: { status: 200 }\n",
        )
        .unwrap();
        let by_name = Route::from_yaml(
            "id: by-name\ntransport: http\nmatch: { method: GET, path: '/users/{name}' }\ncases:\n  named:\n    match: \"${ pathParams.name == '7/8' }\"\n    behaviors:\n      echo: { status: 200, body: { name: '${ pathParams.name }' } }\n",
        )
        .unwrap();
        let catalog = Catalog::new(
            vec![by_id, by_name],
            CollectionsFile::from_yaml(
                "- id: local\n  routes:\n    - by-name:named:echo\n    - by-id:eight:ok\n    - by-id:nine:ok\n",
            )
            .unwrap(),
            RuntimeConfig::default(),
        )
        .unwrap();

        let ResolveOutcome::Matched {
            activation, plan, ..
        } = catalog.resolve_http("local", &get("/users/7%2F8"))
        else {
            panic!("expected match")
        };

        assert_eq!(activation.address(), "by-name:named:echo");
        let ResponsePlan::Response(response) = plan else {
            panic!("expected response")
        };
        assert_eq!(
            response.body,
            Some(BodyPlan::Json(serde_json::json!({ "name": "7/8" })))
        );

        // The serving fast path shares the same envelope.
        let HttpExecutionOutcome::Response { response, .. } = catalog
            .execute_http("local", execution_request("/users/7%2F8", "/users/7%2F8"))
            .await
        else {
            panic!("expected a response")
        };
        assert_eq!(response.body, &br#"{"name":"7/8"}"#[..]);
    }
}
