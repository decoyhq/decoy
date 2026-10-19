use std::collections::BTreeMap;

use super::forward::{
    ForwardRequest, ForwardResponse, ForwardingError, forward_passthrough, target_url,
};
use super::resolve::{HttpRequest, ResolveOutcome};
use crate::collections::Activation;
use crate::engine::Catalog;
use crate::events::{Match, Outcome};
use crate::http::{BodyPlan, RequestMetadata, ResponsePlan};
use crate::request::RequestEnvelope;
use crate::schema::HttpMethod;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpExecutionRequest {
    pub method: HttpMethod,
    pub path: String,
    pub path_and_query: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
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
                &request.body,
            ),
        };

        match self.resolve_http_with_overrides(
            collection_id,
            &resolve_request,
            overrides,
            &request.metadata,
            explain,
        ) {
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
                match plan {
                    ResponsePlan::Response(response) => (
                        HttpExecutionOutcome::Response {
                            activation: Some(activation),
                            response: response.into(),
                        },
                        matched.map(Outcome::Hit),
                    ),
                    ResponsePlan::Passthrough(plan) => {
                        let upstream_url =
                            plan.base_url
                                .as_deref()
                                .filter(|_| explain)
                                .and_then(|base_url| {
                                    target_url(base_url, &request.path_and_query)
                                        .ok()
                                        .map(String::from)
                                });
                        let source = forward_passthrough(
                            &plan,
                            ForwardRequest {
                                method: request.method,
                                path_and_query: request.path_and_query,
                                headers: request.headers,
                                body: request.body,
                            },
                        )
                        .await;

                        let upstream_status = source.as_ref().ok().map(|response| response.status);
                        let outcome = match source {
                            Ok(response) => HttpExecutionOutcome::Response {
                                activation: Some(activation),
                                response,
                            },
                            Err(source) => {
                                HttpExecutionOutcome::ForwardingError { activation, source }
                            }
                        };
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
                    response: match ResponsePlan::explained_miss(&miss) {
                        ResponsePlan::Response(response) => response.into(),
                        ResponsePlan::Passthrough(_) => {
                            unreachable!("fail-closed miss is a response")
                        }
                    },
                },
                explain.then_some(Outcome::Miss(miss)),
            ),
        }
    }
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
    use crate::engine::test_support::{
        candidate, catalog, execution_request, explain_catalog, get, spawn_upstream,
    };
    use crate::engine::{MissDiagnostic, Rejection};
    use crate::http::{BodyPlan, ResolveOutcome, RuntimeConfig};
    use crate::matching::ConditionFailure;
    use crate::schema::Route;

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
        request.body = b"body-from-caller".to_vec();
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
            "id: by-id\ntransport: http\nmatch: { method: GET, path: '/users/{id}' }\ncases:\n  eight:\n    match: \"${ pathParams.id == '8' }\"\n    behaviors:\n      ok: { status: 200 }\n",
        )
        .unwrap();
        let by_name = Route::from_yaml(
            "id: by-name\ntransport: http\nmatch: { method: GET, path: '/users/{name}' }\ncases:\n  named:\n    match: \"${ pathParams.name == '7' }\"\n    behaviors:\n      echo: { status: 200, body: { name: '${ pathParams.name }' } }\n",
        )
        .unwrap();
        let catalog = Catalog::new(
            vec![by_id, by_name],
            CollectionsFile::from_yaml(
                "- id: local\n  routes:\n    - by-name:named:echo\n    - by-id:eight:ok\n",
            )
            .unwrap(),
            RuntimeConfig::default(),
        )
        .unwrap();

        let ResolveOutcome::Matched {
            activation, plan, ..
        } = catalog.resolve_http("local", &get("/users/7"))
        else {
            panic!("expected match")
        };

        assert_eq!(activation.address(), "by-name:named:echo");
        let ResponsePlan::Response(response) = plan else {
            panic!("expected response")
        };
        assert_eq!(
            response.body,
            Some(BodyPlan::Json(serde_json::json!({ "name": "7" })))
        );

        // The serving fast path shares the same envelope.
        let HttpExecutionOutcome::Response { response, .. } = catalog
            .execute_http("local", execution_request("/users/7", "/users/7"))
            .await
        else {
            panic!("expected a response")
        };
        assert_eq!(response.body, &br#"{"name":"7"}"#[..]);
    }
}
