use std::cell::OnceCell;
use std::collections::BTreeMap;

use jmespath::Rcvar;

use super::path::match_path;
use crate::collections::Activation;
use crate::engine::{Candidate, Catalog, MissDiagnostic, Rejection, reject};
use crate::expression::{set_field, to_rcvar};
use crate::http::{HttpResponsePlan, RequestMetadata, ResponsePlan};
use crate::matching::{MatchInput, case_matches, case_mismatch};
use crate::request::RequestEnvelope;
use crate::schema::{Behavior, HttpMethod, RouteMatch};
use crate::template::render_response;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub path: String,
    pub envelope: RequestEnvelope,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolveOutcome<'a> {
    Matched {
        activation: Activation,
        path_params: BTreeMap<String, String>,
        plan: ResponsePlan,
        /// The selected Behavior's `delay`, applied before responding or forwarding.
        delay: Option<std::time::Duration>,
        /// Candidates rejected before the winner, in evaluation order; empty on the serving
        /// fast path, which does not explain hits.
        skipped: Vec<Candidate>,
        /// The winning Case's `match`; every condition in it passed.
        conditions: &'a serde_json::Value,
    },
    Miss(MissDiagnostic),
}

impl Catalog {
    /// Resolves with the full explanation; serving uses `execute_http`, which skips it on hits.
    #[cfg(test)]
    pub fn resolve_http(&self, collection_id: &str, request: &HttpRequest) -> ResolveOutcome<'_> {
        self.resolve_http_with_metadata(collection_id, request, &RequestMetadata::default())
    }

    #[cfg(test)]
    pub fn resolve_http_with_metadata(
        &self,
        collection_id: &str,
        request: &HttpRequest,
        metadata: &RequestMetadata,
    ) -> ResolveOutcome<'_> {
        self.resolve_http_with_overrides(collection_id, request, &[], metadata, true)
    }

    pub(crate) fn resolve_http_with_overrides(
        &self,
        collection_id: &str,
        request: &HttpRequest,
        overrides: &[Activation],
        metadata: &RequestMetadata,
        explain: bool,
    ) -> ResolveOutcome<'_> {
        let activations = match self.activations(collection_id, overrides) {
            Ok(activations) => activations,
            Err(error) => {
                return ResolveOutcome::Miss(MissDiagnostic {
                    collection: collection_id.to_owned(),
                    reason: error.to_string(),
                    candidates: Vec::new(),
                });
            }
        };

        let mut candidates = explain.then(Vec::new);
        // The envelope for JMESPath, built once per request; only `pathParams` changes per
        // candidate.
        let mut json = OnceCell::<Rcvar>::new();

        for activation in activations.iter().copied().rev() {
            let Some(route) = self.routes.get(&activation.route) else {
                reject(&mut candidates, activation, || Rejection::MissingRoute);
                continue;
            };

            let RouteMatch::Http(route_match) = &route.route_match else {
                reject(&mut candidates, activation, || Rejection::Transport);
                continue;
            };

            if route_match.method != request.method {
                reject(&mut candidates, activation, || Rejection::Method {
                    expected: route_match.method.as_str().to_owned(),
                });
                continue;
            }

            let Some(path_params) = match_path(&route_match.path, &request.path) else {
                reject(&mut candidates, activation, || Rejection::Path {
                    expected: route_match.path.clone(),
                });
                continue;
            };

            let Some(case) = route.cases.get(&activation.case) else {
                reject(&mut candidates, activation, || Rejection::MissingCase);
                continue;
            };

            if let Some(json) = json.get_mut() {
                set_field(json, "pathParams", &serde_json::json!(path_params));
            }
            let input = MatchInput {
                method: &request.method,
                path: &request.path,
                path_params: &path_params,
                envelope: &request.envelope,
                json: &json,
            };
            if explain {
                if let Some(failure) = case_mismatch(&case.case_match, &input) {
                    reject(&mut candidates, activation, || {
                        Rejection::Condition(failure)
                    });
                    continue;
                }
            } else if !case_matches(&case.case_match, &input) {
                continue;
            }

            let Some(behavior) = case.behaviors.get(&activation.behavior) else {
                reject(&mut candidates, activation, || Rejection::MissingBehavior);
                continue;
            };

            let static_plan = self
                .static_plans
                .get(&activation.route)
                .and_then(|cases| cases.get(&activation.case))
                .and_then(|behaviors| behaviors.get(&activation.behavior));
            let plan = match behavior {
                _ if let Some(plan) = static_plan => ResponsePlan::Response(plan.clone()),
                Behavior::Response(response) => {
                    let envelope = json.get_or_init(|| {
                        to_rcvar(&request.envelope.to_json(
                            &request.method,
                            &request.path,
                            &path_params,
                        ))
                    });
                    match render_response(response, &|| envelope.clone()) {
                        Ok(rendered) => ResponsePlan::from_behavior(
                            route,
                            &Behavior::Response(rendered),
                            &self.runtime,
                            metadata,
                        ),
                        Err(reason) => ResponsePlan::Response(HttpResponsePlan::template_error(
                            &activation.address(),
                            &reason.to_string(),
                        )),
                    }
                }
                behavior => ResponsePlan::from_behavior(route, behavior, &self.runtime, metadata),
            };

            return ResolveOutcome::Matched {
                activation: activation.clone(),
                path_params,
                plan,
                delay: behavior.delay(),
                skipped: candidates.unwrap_or_default(),
                conditions: &case.case_match,
            };
        }

        let Some(candidates) = candidates else {
            // Misses are rare: only now pay for the candidates the fail-closed body lists.
            return self.resolve_http_with_overrides(
                collection_id,
                request,
                overrides,
                metadata,
                true,
            );
        };
        ResolveOutcome::Miss(MissDiagnostic {
            collection: collection_id.to_owned(),
            reason: "no active route case matched request".to_owned(),
            candidates,
        })
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::collections::CollectionsFile;
    use crate::engine::Rejection;
    use crate::engine::test_support::{candidate, catalog, explain_catalog, get};
    use crate::http::{BodyPlan, HttpResponsePlan, PassthroughPlan, RuntimeConfig};
    use crate::matching::ConditionFailure;
    use crate::schema::{PassthroughTarget, Route};

    #[test]
    fn golden_route_case_behavior_selection_prefers_last_matching_activation() {
        let catalog = catalog(
            r#"
- id: local
  routes:
    - get-user:user-123:missing
    - get-user:any:fallback
"#,
        );

        let outcome = catalog.resolve_http(
            "local",
            &HttpRequest {
                method: HttpMethod::Get,
                path: "/users/123".to_owned(),
                ..Default::default()
            },
        );

        let ResolveOutcome::Matched {
            activation,
            path_params,
            plan,
            ..
        } = outcome
        else {
            panic!("expected match")
        };

        assert_eq!(activation.address(), "get-user:any:fallback");
        assert_eq!(path_params["id"], "123");
        assert_eq!(
            plan,
            ResponsePlan::Response(HttpResponsePlan {
                status: 200,
                headers: BTreeMap::from([(
                    "content-type".to_owned(),
                    "application/json".to_owned()
                )]),
                // Static, so served as the bytes serialized at load.
                body: Some(BodyPlan::Bytes(r#"{"kind":"fallback"}"#.into())),
            })
        );
    }

    #[test]
    fn golden_collection_inheritance_child_overrides_parent_selection() {
        let catalog = catalog(
            r#"
- id: base
  routes:
    - get-user:user-123:success

- id: not-found
  from: base
  routes:
    - get-user:user-123:missing
"#,
        );

        let outcome = catalog.resolve_http(
            "not-found",
            &HttpRequest {
                method: HttpMethod::Get,
                path: "/users/123".to_owned(),
                ..Default::default()
            },
        );

        let ResolveOutcome::Matched { activation, .. } = outcome else {
            panic!("expected match")
        };
        assert_eq!(activation.address(), "get-user:user-123:missing");
    }

    #[test]
    fn golden_passthrough_plan_uses_route_target_before_runtime_target() {
        let catalog = catalog(
            r#"
- id: local
  routes:
    - get-user:user-123:real
"#,
        );

        let outcome = catalog.resolve_http(
            "local",
            &HttpRequest {
                method: HttpMethod::Get,
                path: "/users/123".to_owned(),
                ..Default::default()
            },
        );

        let ResolveOutcome::Matched { plan, .. } = outcome else {
            panic!("expected match")
        };
        assert_eq!(
            plan,
            ResponsePlan::Passthrough(PassthroughPlan {
                base_url: Some("https://route.example.test".to_owned())
            })
        );
    }

    #[test]
    fn golden_passthrough_plan_uses_behavior_target_before_route_target() {
        let route = Route::from_yaml(
            r#"
id: get-user
transport: http
match:
  method: GET
  path: /users/{id}
passthrough:
  baseUrl: https://route.example.test
cases:
  user-123:
    match:
      pathParams:
        id: "123"
    behaviors:
      real:
        kind: passthrough
        target:
          baseUrl: https://behavior.example.test
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
            RuntimeConfig {
                passthrough: Some(PassthroughTarget {
                    base_url: "https://runtime.example.test".to_owned(),
                }),
                ..RuntimeConfig::default()
            },
        )
        .unwrap();

        let ResolveOutcome::Matched { plan, .. } = catalog.resolve_http(
            "local",
            &HttpRequest {
                method: HttpMethod::Get,
                path: "/users/123".to_owned(),
                ..Default::default()
            },
        ) else {
            panic!("expected match")
        };

        assert_eq!(
            plan,
            ResponsePlan::Passthrough(PassthroughPlan {
                base_url: Some("https://behavior.example.test".to_owned())
            })
        );
    }

    #[test]
    fn golden_passthrough_plan_uses_runtime_before_request_metadata() {
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
            RuntimeConfig {
                passthrough: Some(PassthroughTarget {
                    base_url: "https://runtime.example.test".to_owned(),
                }),
                ..RuntimeConfig::default()
            },
        )
        .unwrap();

        let ResolveOutcome::Matched { plan, .. } = catalog.resolve_http_with_metadata(
            "local",
            &HttpRequest {
                method: HttpMethod::Get,
                path: "/users/123".to_owned(),
                ..Default::default()
            },
            &RequestMetadata {
                original_base_url: Some("https://request.example.test".to_owned()),
            },
        ) else {
            panic!("expected match")
        };

        assert_eq!(
            plan,
            ResponsePlan::Passthrough(PassthroughPlan {
                base_url: Some("https://runtime.example.test".to_owned())
            })
        );
    }

    #[test]
    fn golden_passthrough_plan_falls_back_to_request_metadata() {
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

        let ResolveOutcome::Matched { plan, .. } = catalog.resolve_http_with_metadata(
            "local",
            &HttpRequest {
                method: HttpMethod::Get,
                path: "/users/123".to_owned(),
                ..Default::default()
            },
            &RequestMetadata {
                original_base_url: Some("https://request.example.test".to_owned()),
            },
        ) else {
            panic!("expected match")
        };

        assert_eq!(
            plan,
            ResponsePlan::Passthrough(PassthroughPlan {
                base_url: Some("https://request.example.test".to_owned())
            })
        );
    }

    #[test]
    fn golden_global_passthrough_target_does_not_forward_unmatched_requests() {
        let catalog = catalog(
            r#"
- id: local
  routes:
    - get-user:user-123:success
"#,
        );

        let outcome = catalog.resolve_http(
            "local",
            &HttpRequest {
                method: HttpMethod::Get,
                path: "/unknown".to_owned(),
                ..Default::default()
            },
        );

        assert!(matches!(outcome, ResolveOutcome::Miss(_)));
    }

    #[test]
    fn a_hit_explains_skipped_candidates_and_the_winner() {
        let catalog = explain_catalog();
        let ResolveOutcome::Matched {
            activation,
            path_params,
            skipped,
            conditions,
            ..
        } = catalog.resolve_http("local", &get("/users/456"))
        else {
            panic!("expected match")
        };

        assert_eq!(activation.address(), "get-user:any:fallback");
        assert_eq!(path_params["id"], "456");
        assert_eq!(
            conditions,
            &serde_json::json!({ "pathParams": { "id": "*" } })
        );
        assert_eq!(
            skipped,
            vec![
                candidate("ghost:any:ok", Rejection::MissingRoute),
                candidate(
                    "create-user:any:ok",
                    Rejection::Method {
                        expected: "POST".to_owned()
                    }
                ),
                candidate(
                    "list-users:any:ok",
                    Rejection::Path {
                        expected: "/users".to_owned()
                    }
                ),
                candidate("get-user:ghost:ok", Rejection::MissingCase),
                candidate(
                    "get-user:user-123:success",
                    Rejection::Condition(ConditionFailure {
                        field: "pathParams.id".to_owned(),
                        expected: serde_json::json!("123"),
                        actual: serde_json::json!("456"),
                        error: None,
                    })
                ),
                candidate("get-user:any:ghost", Rejection::MissingBehavior),
            ]
        );
    }

    #[test]
    fn a_broken_collection_misses_with_its_load_error() {
        assert_eq!(
            catalog("- id: a\n  from: b\n- id: b\n  from: a\n")
                .resolve_http("a", &get("/users/456")),
            ResolveOutcome::Miss(MissDiagnostic {
                collection: "a".to_owned(),
                reason: "collection inheritance cycle at `a`".to_owned(),
                candidates: Vec::new(),
            })
        );
    }

    #[test]
    fn an_unknown_collection_misses_with_no_candidates() {
        assert_eq!(
            explain_catalog().resolve_http("nope", &get("/users/456")),
            ResolveOutcome::Miss(MissDiagnostic {
                collection: "nope".to_owned(),
                reason: "unknown collection `nope`".to_owned(),
                candidates: Vec::new(),
            })
        );
    }

    #[test]
    fn a_case_that_does_not_read_the_body_leaves_it_unparsed() {
        let catalog = catalog("- id: local\n  routes:\n    - get-user:any:fallback\n");
        let request = HttpRequest {
            method: HttpMethod::Get,
            path: "/users/123".to_owned(),
            envelope: RequestEnvelope::from_parts(
                None,
                "/users/123",
                &BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]),
                "{not json".into(),
            ),
        };

        let ResolveOutcome::Matched { activation, .. } = catalog.resolve_http("local", &request)
        else {
            panic!("expected match")
        };
        assert_eq!(activation.address(), "get-user:any:fallback");
        assert_eq!(request.envelope.parsed_body.get(), None);
    }
}
