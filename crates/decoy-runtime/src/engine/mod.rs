pub mod controller;
pub mod session;
#[cfg(test)]
pub(crate) mod test_support;
pub mod view;

use std::cell::OnceCell;
use std::collections::BTreeMap;

use jmespath::Rcvar;
use thiserror::Error;

use crate::collections::{Activation, CollectionError, CollectionsFile};
use crate::events::{Match, Outcome};
use crate::expression::{set_field, to_rcvar};
use crate::http::{BodyPlan, HttpResponsePlan, RequestMetadata, ResponsePlan, RuntimeConfig};
use crate::http_forward::{
    ForwardRequest, ForwardResponse, ForwardingError, forward_passthrough, target_url,
};
use crate::matching::{ConditionFailure, MatchInput, case_matches, case_mismatch};
use crate::request::RequestEnvelope;
use crate::schema::{Behavior, HttpMethod, Route, RouteMatch};
use crate::template::{has_templates, render_response};

pub use controller::{ControlError, Controller, MAX_SESSIONS};
pub use session::{
    DEFAULT_SESSION, RouteOverrideSnapshot, SESSION_HEADER, Selection, SelectionSnapshot,
};
pub use view::SessionView;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub path: String,
    pub envelope: RequestEnvelope,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissDiagnostic {
    pub collection: String,
    pub reason: String,
    /// Every active route entry evaluated, bottom-to-top, with why it was rejected.
    pub candidates: Vec<Candidate>,
}

/// One evaluated active route entry and why it did not match.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Candidate {
    pub address: String,
    #[serde(flatten)]
    pub rejection: Rejection,
}

/// Why a Candidate was rejected. `method`/`path` hold the Route's value; the request's is known.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "reason", rename_all = "camelCase")]
pub enum Rejection {
    MissingRoute,
    MissingCase,
    MissingBehavior,
    /// The Route serves another transport (an HTTP request against a gRPC Route, or back).
    Transport,
    Method {
        expected: String,
    },
    Path {
        expected: String,
    },
    Condition(ConditionFailure),
    /// The request did not decode with the Route's Pipeline.
    Decode {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    routes: BTreeMap<String, Route>,
    collections: CollectionsFile,
    /// Each Collection resolved once (inheritance, validation) instead of on every request.
    resolved: BTreeMap<String, Result<Vec<Activation>, String>>,
    /// HTTP Response Behaviors without templates, planned once with the body serialized:
    /// route → case → behavior.
    static_plans: BTreeMap<String, BTreeMap<String, BTreeMap<String, HttpResponsePlan>>>,
    runtime: RuntimeConfig,
}

impl Catalog {
    pub fn new(
        routes: Vec<Route>,
        collections: CollectionsFile,
        runtime: RuntimeConfig,
    ) -> Result<Self, CatalogError> {
        for route in &routes {
            route.validate()?;
            crate::grpc_schema::check_route(route, &runtime.protos).map_err(CatalogError::Grpc)?;
        }
        Self::from_checked(routes, collections, runtime)
    }

    /// `new` for Routes startup already validated and checked against the protos (with file
    /// paths in its diagnostics), so the expensive proto check runs once.
    pub(crate) fn from_checked(
        routes: Vec<Route>,
        collections: CollectionsFile,
        runtime: RuntimeConfig,
    ) -> Result<Self, CatalogError> {
        let mut by_id = BTreeMap::new();

        for route in routes {
            if by_id.insert(route.id.clone(), route).is_some() {
                return Err(CatalogError::DuplicateRoute);
            }
        }

        let resolved = collections
            .ids()
            .map(|id| {
                let activations = collections.resolve(id).map_err(|error| error.to_string());
                (id.to_owned(), activations)
            })
            .collect();
        let static_plans = by_id
            .values()
            .filter(|route| matches!(route.route_match, RouteMatch::Http(_)))
            .map(|route| {
                let cases = route.cases.iter().map(|(case_id, case)| {
                    let behaviors = case.behaviors.iter().filter_map(|(name, behavior)| {
                        let Behavior::Response(response) = behavior else {
                            return None;
                        };
                        if has_templates(response) {
                            return None;
                        }
                        let metadata = RequestMetadata::default();
                        let ResponsePlan::Response(mut plan) =
                            ResponsePlan::from_behavior(route, behavior, &runtime, &metadata)
                        else {
                            unreachable!("a Response Behavior plans a response")
                        };
                        plan.body = plan.body.map(|body| BodyPlan::Bytes(body.into_bytes()));
                        Some((name.clone(), plan))
                    });
                    (case_id.clone(), behaviors.collect())
                });
                (route.id.clone(), cases.collect())
            })
            .collect();
        Ok(Self {
            routes: by_id,
            collections,
            resolved,
            static_plans,
            runtime,
        })
    }

    /// Resolves with the full explanation; serving uses `execute_http`, which skips it on hits.
    #[cfg(test)]
    pub fn resolve_http(&self, collection_id: &str, request: &HttpRequest) -> ResolveOutcome<'_> {
        self.resolve_http_with_metadata(collection_id, request, &RequestMetadata::default())
    }

    /// Every Route, by id.
    pub fn routes(&self) -> impl Iterator<Item = &Route> {
        self.routes.values()
    }

    pub fn collection_ids(&self) -> impl Iterator<Item = &str> {
        self.collections.ids()
    }

    pub fn validate_collection(&self, collection_id: &str) -> Result<(), ControlError> {
        self.collections
            .get(collection_id)
            .map(|_| ())
            .ok_or_else(|| ControlError::UnknownCollection {
                id: collection_id.to_owned(),
            })
    }

    pub fn validate_behavior_address(
        &self,
        route_id: &str,
        case_id: &str,
        behavior_id: &str,
    ) -> Result<(), ControlError> {
        let route = self
            .routes
            .get(route_id)
            .ok_or_else(|| ControlError::UnknownRoute {
                route: route_id.to_owned(),
            })?;
        let case = route
            .cases
            .get(case_id)
            .ok_or_else(|| ControlError::UnknownCase {
                route: route_id.to_owned(),
                case: case_id.to_owned(),
            })?;
        case.behaviors
            .get(behavior_id)
            .map(|_| ())
            .ok_or_else(|| ControlError::UnknownBehavior {
                route: route_id.to_owned(),
                case: case_id.to_owned(),
                behavior: behavior_id.to_owned(),
            })
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
    async fn execute_http_with_overrides(
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

    /// The Collection's activations with Session overrides applied: an override replaces its
    /// `route+case` entry in place, or is appended after the Collection in call order.
    pub(crate) fn activations<'a>(
        &'a self,
        collection_id: &str,
        overrides: &'a [Activation],
    ) -> Result<Vec<&'a Activation>, String> {
        let collection = self
            .resolved
            .get(collection_id)
            .ok_or_else(|| {
                CollectionError::UnknownCollection {
                    id: collection_id.to_owned(),
                }
                .to_string()
            })?
            .as_ref()
            .map_err(Clone::clone)?;
        let same = |a: &Activation, b: &Activation| a.route == b.route && a.case == b.case;
        let mut activations: Vec<&Activation> = collection
            .iter()
            .map(|entry| overrides.iter().find(|o| same(o, entry)).unwrap_or(entry))
            .collect();
        activations.extend(
            overrides
                .iter()
                .filter(|o| !collection.iter().any(|entry| same(o, entry))),
        );
        Ok(activations)
    }

    pub(crate) fn route(&self, id: &str) -> Option<&Route> {
        self.routes.get(id)
    }

    pub(crate) fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }

    fn resolve_http_with_overrides(
        &self,
        collection_id: &str,
        request: &HttpRequest,
        overrides: &[Activation],
        metadata: &RequestMetadata,
        explain: bool,
    ) -> ResolveOutcome<'_> {
        let activations = match self.activations(collection_id, overrides) {
            Ok(activations) => activations,
            Err(reason) => {
                return ResolveOutcome::Miss(MissDiagnostic {
                    collection: collection_id.to_owned(),
                    reason,
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
                        Err(reason) => ResponsePlan::template_error(&activation.address(), &reason),
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

/// Records why `activation` was rejected when explaining (`Some`); the fast path builds nothing.
pub(crate) fn reject(
    candidates: &mut Option<Vec<Candidate>>,
    activation: &Activation,
    rejection: impl FnOnce() -> Rejection,
) {
    if let Some(candidates) = candidates {
        candidates.push(Candidate {
            address: activation.address(),
            rejection: rejection(),
        });
    }
}

fn match_path(pattern: &str, actual: &str) -> Option<BTreeMap<String, String>> {
    let pattern_parts = split_path(pattern);
    let actual_parts = split_path(actual);

    if pattern_parts.len() != actual_parts.len() {
        return None;
    }

    let mut params = BTreeMap::new();

    for (pattern_part, actual_part) in pattern_parts.iter().zip(actual_parts) {
        if pattern_part.starts_with('{') && pattern_part.ends_with('}') {
            let name = &pattern_part[1..pattern_part.len() - 1];
            // Decoded like query values, so `/users/a%20b` gives `a b`.
            let value = percent_encoding::percent_decode_str(actual_part).decode_utf8_lossy();
            params.insert(name.to_owned(), value.into_owned());
            continue;
        }

        if *pattern_part != percent_encoding::percent_decode_str(actual_part).decode_utf8_lossy() {
            return None;
        }
    }

    Some(params)
}

fn split_path(path: &str) -> Vec<&str> {
    path.trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect()
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

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error(transparent)]
    Validation(#[from] crate::schema::ValidationError),
    #[error("duplicate route id")]
    DuplicateRoute,
    #[error("{0}")]
    Grpc(String),
    #[error(transparent)]
    Collection(#[from] CollectionError),
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::collections::CollectionsFile;
    use crate::engine::test_support::{
        candidate, catalog, execution_request, explain_catalog, get, spawn_upstream,
    };
    use crate::http::{BodyPlan, HttpResponsePlan, PassthroughPlan};
    use crate::schema::PassthroughTarget;

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
