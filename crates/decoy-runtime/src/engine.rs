use std::collections::BTreeMap;
use std::sync::Arc;

use thiserror::Error;

use crate::collections::{Activation, CollectionError, CollectionsFile};
use crate::grpc::{GrpcRequest, GrpcResponse};
use crate::http::{BodyPlan, RequestMetadata, ResponsePlan, RuntimeConfig};
use crate::http_forward::{ForwardRequest, ForwardResponse, ForwardingError, forward_passthrough};
use crate::matching::{ConditionFailure, MatchInput, case_mismatch};
use crate::request::RequestEnvelope;
use crate::schema::{Behavior, HttpMethod, Route, RouteMatch};
use crate::template::{has_templates, render_response};

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
pub enum ResolveOutcome {
    Matched {
        activation: Activation,
        path_params: BTreeMap<String, String>,
        plan: ResponsePlan,
        /// The selected Behavior's `delay`, applied before responding or forwarding.
        delay: Option<std::time::Duration>,
        /// Candidates rejected before the winner, in evaluation order.
        skipped: Vec<Candidate>,
        /// The winning Case's `match`; every condition in it passed.
        conditions: serde_json::Value,
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    routes: BTreeMap<String, Route>,
    collections: CollectionsFile,
    /// Each Collection resolved once (inheritance, validation) instead of on every request.
    resolved: BTreeMap<String, Result<Vec<Activation>, String>>,
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
        Ok(Self {
            routes: by_id,
            collections,
            resolved,
            runtime,
        })
    }

    pub fn resolve_http(&self, collection_id: &str, request: &HttpRequest) -> ResolveOutcome {
        self.resolve_http_with_metadata(collection_id, request, &RequestMetadata::default())
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

    pub fn resolve_http_with_metadata(
        &self,
        collection_id: &str,
        request: &HttpRequest,
        metadata: &RequestMetadata,
    ) -> ResolveOutcome {
        self.resolve_http_with_overrides(collection_id, request, &BTreeMap::new(), metadata)
    }

    pub async fn execute_http(
        &self,
        collection_id: &str,
        request: HttpExecutionRequest,
    ) -> HttpExecutionOutcome {
        self.execute_http_with_overrides(collection_id, &BTreeMap::new(), request)
            .await
    }

    async fn execute_http_with_overrides(
        &self,
        collection_id: &str,
        overrides: &BTreeMap<(String, String), String>,
        request: HttpExecutionRequest,
    ) -> HttpExecutionOutcome {
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
        ) {
            ResolveOutcome::Matched {
                activation,
                plan,
                delay,
                ..
            } => {
                if let Some(delay) = delay {
                    tokio::time::sleep(delay).await;
                }
                match plan {
                    ResponsePlan::Response(response) => HttpExecutionOutcome::Response {
                        activation: Some(activation),
                        response: response.into(),
                    },
                    ResponsePlan::Passthrough(plan) => {
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

                        match source {
                            Ok(response) => HttpExecutionOutcome::Response {
                                activation: Some(activation),
                                response,
                            },
                            Err(source) => {
                                HttpExecutionOutcome::ForwardingError { activation, source }
                            }
                        }
                    }
                }
            }
            ResolveOutcome::Miss(miss) => HttpExecutionOutcome::Response {
                activation: None,
                response: match ResponsePlan::explained_miss(&miss) {
                    ResponsePlan::Response(response) => response.into(),
                    ResponsePlan::Passthrough(_) => unreachable!("fail-closed miss is a response"),
                },
            },
        }
    }

    /// The Collection's activations with Session overrides appended; the last one wins.
    pub(crate) fn activations(
        &self,
        collection_id: &str,
        overrides: &BTreeMap<(String, String), String>,
    ) -> Result<Vec<Activation>, String> {
        let mut activations = match self.resolved.get(collection_id) {
            Some(resolved) => resolved.clone()?,
            None => self
                .collections
                .resolve(collection_id)
                .map_err(|error| error.to_string())?,
        };
        for ((route, case), behavior) in overrides {
            activations.push(Activation {
                route: route.clone(),
                case: case.clone(),
                behavior: behavior.clone(),
            });
        }
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
        overrides: &BTreeMap<(String, String), String>,
        metadata: &RequestMetadata,
    ) -> ResolveOutcome {
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

        let mut candidates = Vec::new();

        for activation in activations.iter().rev() {
            let reject = |rejection| Candidate {
                address: activation.address(),
                rejection,
            };

            let Some(route) = self.routes.get(&activation.route) else {
                candidates.push(reject(Rejection::MissingRoute));
                continue;
            };

            let RouteMatch::Http(route_match) = &route.route_match else {
                candidates.push(reject(Rejection::Transport));
                continue;
            };

            if route_match.method != request.method {
                candidates.push(reject(Rejection::Method {
                    expected: route_match.method.as_str().to_owned(),
                }));
                continue;
            }

            let Some(path_params) = match_path(&route_match.path, &request.path) else {
                candidates.push(reject(Rejection::Path {
                    expected: route_match.path.clone(),
                }));
                continue;
            };

            let Some(case) = route.cases.get(&activation.case) else {
                candidates.push(reject(Rejection::MissingCase));
                continue;
            };

            let input = MatchInput {
                method: &request.method,
                path: &request.path,
                path_params: &path_params,
                envelope: &request.envelope,
            };
            if let Some(failure) = case_mismatch(&case.case_match, &input) {
                candidates.push(reject(Rejection::Condition(failure)));
                continue;
            }

            let Some(behavior) = case.behaviors.get(&activation.behavior) else {
                candidates.push(reject(Rejection::MissingBehavior));
                continue;
            };

            let plan = match behavior {
                Behavior::Response(response) if has_templates(response) => {
                    let envelope =
                        request
                            .envelope
                            .to_json(&request.method, &request.path, &path_params);
                    match render_response(response, &envelope) {
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
                skipped: candidates,
                conditions: case.case_match.clone(),
            };
        }

        ResolveOutcome::Miss(MissDiagnostic {
            collection: collection_id.to_owned(),
            reason: "no active route case matched request".to_owned(),
            candidates,
        })
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
        let body = response.body.map(response_body_bytes).unwrap_or_default();
        Self {
            status: response.status,
            headers: response.headers.into_iter().collect(),
            body,
        }
    }
}

fn response_body_bytes(body: BodyPlan) -> Vec<u8> {
    match body {
        BodyPlan::Json(value) => serde_json::to_vec(&value).expect("JSON response body serializes"),
        BodyPlan::Text(text) => text.into_bytes(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    collection: String,
    route_overrides: BTreeMap<(String, String), String>,
}

impl Selection {
    fn new(collection: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            route_overrides: BTreeMap::new(),
        }
    }

    pub fn snapshot(&self) -> SelectionSnapshot {
        SelectionSnapshot {
            collection: self.collection.clone(),
            overrides: self
                .route_overrides
                .iter()
                .map(|((route, case_id), behavior)| RouteOverrideSnapshot {
                    route: route.clone(),
                    case_id: case_id.clone(),
                    behavior: behavior.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SelectionSnapshot {
    pub collection: String,
    pub overrides: Vec<RouteOverrideSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RouteOverrideSnapshot {
    pub route: String,
    #[serde(rename = "case")]
    pub case_id: String,
    pub behavior: String,
}

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("unknown collection `{id}`")]
    UnknownCollection { id: String },
    #[error("unknown route `{route}`")]
    UnknownRoute { route: String },
    #[error("unknown case `{route}:{case}`")]
    UnknownCase { route: String, case: String },
    #[error("unknown behavior `{route}:{case}:{behavior}`")]
    UnknownBehavior {
        route: String,
        case: String,
        behavior: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Controller {
    /// Immutable after startup; requests share it instead of copying Routes and Collections.
    catalog: Arc<Catalog>,
    default_collection: String,
    sessions: BTreeMap<String, Selection>,
}

/// What one request needs: the shared Catalog and a copy of its Session's Selection, taken under
/// the read lock so the lock isn't held across `delay`s or passthrough.
#[derive(Debug, Clone)]
pub struct SessionView {
    catalog: Arc<Catalog>,
    selection: Selection,
}

impl SessionView {
    pub async fn execute_http(&self, request: HttpExecutionRequest) -> HttpExecutionOutcome {
        self.catalog
            .execute_http_with_overrides(
                &self.selection.collection,
                &self.selection.route_overrides,
                request,
            )
            .await
    }

    pub async fn execute_grpc(&self, request: GrpcRequest) -> GrpcResponse {
        self.catalog
            .execute_grpc(
                &self.selection.collection,
                &self.selection.route_overrides,
                request,
            )
            .await
    }
}

impl Controller {
    pub fn new(catalog: Catalog, default_collection: impl Into<String>) -> Self {
        Self {
            catalog: Arc::new(catalog),
            default_collection: default_collection.into(),
            sessions: BTreeMap::new(),
        }
    }

    pub fn use_collection(&mut self, session: &str, collection: impl Into<String>) {
        self.selection_mut(session).collection = collection.into();
    }

    pub fn try_use_collection(
        &mut self,
        session: &str,
        collection: impl Into<String>,
    ) -> Result<SelectionSnapshot, ControlError> {
        let collection = collection.into();
        self.catalog.validate_collection(&collection)?;
        self.selection_mut(session).collection = collection;
        Ok(self.selection(session).snapshot())
    }

    pub fn use_route(
        &mut self,
        session: &str,
        route: impl Into<String>,
        case: impl Into<String>,
        behavior: impl Into<String>,
    ) {
        self.selection_mut(session)
            .route_overrides
            .insert((route.into(), case.into()), behavior.into());
    }

    pub fn try_use_route(
        &mut self,
        session: &str,
        route: impl Into<String>,
        case: impl Into<String>,
        behavior: impl Into<String>,
    ) -> Result<SelectionSnapshot, ControlError> {
        let route = route.into();
        let case = case.into();
        let behavior = behavior.into();
        self.catalog
            .validate_behavior_address(&route, &case, &behavior)?;
        self.selection_mut(session)
            .route_overrides
            .insert((route, case), behavior);
        Ok(self.selection(session).snapshot())
    }

    pub fn reset(&mut self, session: &str) -> SelectionSnapshot {
        self.selection_mut(session).route_overrides.clear();
        self.selection(session).snapshot()
    }

    /// Forgets the Session's Selection, freeing it; a later request starts from the defaults.
    pub fn end_session(&mut self, session: &str) -> SelectionSnapshot {
        self.sessions.remove(session);
        self.selection(session).snapshot()
    }

    pub fn resolve_http(&self, session: &str, request: &HttpRequest) -> ResolveOutcome {
        let selection = self.selection(session);
        self.catalog.resolve_http_with_overrides(
            &selection.collection,
            request,
            &selection.route_overrides,
            &RequestMetadata::default(),
        )
    }

    pub fn session_view(&self, session: &str) -> SessionView {
        SessionView {
            catalog: Arc::clone(&self.catalog),
            selection: self.selection(session),
        }
    }

    fn selection(&self, session: &str) -> Selection {
        self.sessions
            .get(session)
            .cloned()
            .unwrap_or_else(|| Selection::new(self.default_collection.clone()))
    }

    fn selection_mut(&mut self, session: &str) -> &mut Selection {
        self.sessions
            .entry(session.to_owned())
            .or_insert_with(|| Selection::new(self.default_collection.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::collections::CollectionsFile;
    use crate::http::{BodyPlan, HttpResponsePlan, PassthroughPlan};
    use crate::schema::PassthroughTarget;

    #[derive(Debug)]
    struct CapturedRequest {
        request_line: String,
        headers: BTreeMap<String, String>,
        body: String,
    }

    fn spawn_upstream() -> (
        String,
        mpsc::Receiver<CapturedRequest>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0; 1024];
            loop {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    let text = String::from_utf8_lossy(&buffer);
                    let headers_end = text.find("\r\n\r\n").unwrap();
                    let head = &text[..headers_end];
                    let content_length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    let body_start = headers_end + 4;
                    if buffer.len() >= body_start + content_length {
                        break;
                    }
                }
            }

            let header_end = buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap();
            let head = String::from_utf8_lossy(&buffer[..header_end]);
            let mut lines = head.lines();
            let request_line = lines.next().unwrap().to_owned();
            let mut headers = BTreeMap::new();
            for line in lines {
                let (name, value) = line.split_once(':').unwrap();
                headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
            }
            let body = String::from_utf8_lossy(&buffer[header_end + 4..]).to_string();
            tx.send(CapturedRequest {
                request_line,
                headers,
                body,
            })
            .unwrap();

            stream
                .write_all(
                    b"HTTP/1.1 202 Accepted\r\ncontent-type: text/plain\r\nx-upstream: yes\r\ncontent-length: 8\r\n\r\naccepted",
                )
                .unwrap();
        });

        (format!("http://{addr}"), rx, handle)
    }

    fn execution_request(path: &str, path_and_query: &str) -> HttpExecutionRequest {
        HttpExecutionRequest {
            method: HttpMethod::Get,
            path: path.to_owned(),
            path_and_query: path_and_query.to_owned(),
            headers: BTreeMap::new(),
            body: Vec::new(),
            metadata: RequestMetadata::default(),
        }
    }

    fn get_user_route() -> Route {
        Route::from_yaml(
            r#"
id: get-user
transport: http
match:
  method: GET
  path: /users/{id}
passthrough:
  baseUrl: https://route.example.test
cases:
  any:
    match:
      pathParams:
        id: "*"
    behaviors:
      fallback:
        status: 200
        body:
          kind: fallback
  user-123:
    match:
      pathParams:
        id: "123"
    behaviors:
      success:
        status: 200
        body:
          id: "123"
      missing:
        status: 404
        body:
          error: missing
      real:
        kind: passthrough
"#,
        )
        .unwrap()
    }

    fn catalog(collections: &str) -> Catalog {
        Catalog::new(
            vec![get_user_route()],
            CollectionsFile::from_yaml(collections).unwrap(),
            RuntimeConfig {
                passthrough: Some(PassthroughTarget {
                    base_url: "https://runtime.example.test".to_owned(),
                }),
                ..RuntimeConfig::default()
            },
        )
        .unwrap()
    }

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
                body: Some(BodyPlan::Json(serde_json::json!({ "kind": "fallback" }))),
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
    fn golden_per_session_selection_is_isolated() {
        let mut controller = Controller::new(
            catalog(
                r#"
- id: local
  routes:
    - get-user:user-123:success

- id: not-found
  routes:
    - get-user:user-123:missing
"#,
            ),
            "local",
        );

        controller.use_collection("session-a", "not-found");

        let request = HttpRequest {
            method: HttpMethod::Get,
            path: "/users/123".to_owned(),
            ..Default::default()
        };

        let ResolveOutcome::Matched {
            activation: session_a,
            ..
        } = controller.resolve_http("session-a", &request)
        else {
            panic!("expected session-a match")
        };
        let ResolveOutcome::Matched {
            activation: session_b,
            ..
        } = controller.resolve_http("session-b", &request)
        else {
            panic!("expected session-b match")
        };

        assert_eq!(session_a.address(), "get-user:user-123:missing");
        assert_eq!(session_b.address(), "get-user:user-123:success");
    }

    #[test]
    fn golden_control_verbs_use_collection_use_route_and_reset() {
        let mut controller = Controller::new(
            catalog(
                r#"
- id: local
  routes:
    - get-user:user-123:success

- id: not-found
  routes:
    - get-user:user-123:missing
"#,
            ),
            "local",
        );
        let request = HttpRequest {
            method: HttpMethod::Get,
            path: "/users/123".to_owned(),
            ..Default::default()
        };

        controller.use_collection("session", "not-found");
        let ResolveOutcome::Matched { activation, .. } =
            controller.resolve_http("session", &request)
        else {
            panic!("expected useCollection match")
        };
        assert_eq!(activation.address(), "get-user:user-123:missing");

        controller.use_route("session", "get-user", "user-123", "success");
        let ResolveOutcome::Matched { activation, .. } =
            controller.resolve_http("session", &request)
        else {
            panic!("expected useRoute match")
        };
        assert_eq!(activation.address(), "get-user:user-123:success");

        controller.reset("session");
        let ResolveOutcome::Matched { activation, .. } =
            controller.resolve_http("session", &request)
        else {
            panic!("expected reset match")
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
        assert_eq!(response.body, b"accepted");

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
        assert_eq!(response.body, br#"{"id":"123"}"#);
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

    fn candidate(address: &str, rejection: Rejection) -> Candidate {
        Candidate {
            address: address.to_owned(),
            rejection,
        }
    }

    fn explain_catalog() -> Catalog {
        let create_user = Route::from_yaml(
            "id: create-user\ntransport: http\nmatch: { method: POST, path: '/users/{id}' }\ncases:\n  any:\n    behaviors:\n      ok: { status: 201 }\n",
        )
        .unwrap();
        let list_users = Route::from_yaml(
            "id: list-users\ntransport: http\nmatch: { method: GET, path: /users }\ncases:\n  any:\n    behaviors:\n      ok: { status: 200 }\n",
        )
        .unwrap();
        Catalog::new(
            vec![get_user_route(), create_user, list_users],
            CollectionsFile::from_yaml(
                r#"
- id: local
  routes:
    - get-user:any:fallback
    - get-user:any:ghost
    - get-user:user-123:success
    - get-user:ghost:ok
    - list-users:any:ok
    - create-user:any:ok
    - ghost:any:ok
"#,
            )
            .unwrap(),
            RuntimeConfig::default(),
        )
        .unwrap()
    }

    fn get(path: &str) -> HttpRequest {
        HttpRequest {
            method: HttpMethod::Get,
            path: path.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn a_hit_explains_skipped_candidates_and_the_winner() {
        let ResolveOutcome::Matched {
            activation,
            path_params,
            skipped,
            conditions,
            ..
        } = explain_catalog().resolve_http("local", &get("/users/456"))
        else {
            panic!("expected match")
        };

        assert_eq!(activation.address(), "get-user:any:fallback");
        assert_eq!(path_params["id"], "456");
        assert_eq!(
            conditions,
            serde_json::json!({ "pathParams": { "id": "*" } })
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
}
