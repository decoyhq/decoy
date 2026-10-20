use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::connections::{Connections, SendError};
pub use crate::engine::session::{DEFAULT_SESSION, SESSION_HEADER};
use crate::engine::{Catalog, ControlError, Controller, SelectionSnapshot};
use crate::events::SessionSource;
use crate::grpc::schema::find_method;
use crate::schema::{Route, RouteMatch};

pub const CONTROL_PREFIX: &str = "/__decoy__";
/// Liveness probe for tools waiting on a runtime. It reports only that Decoy is serving; it
/// exposes no diagnostics or identity and never reads or mutates Selection.
pub const HEALTH_PATH: &str = "/__decoy__/health";
/// Pushes a message to a Route's live connections; see [`handle_send_request`].
pub const SEND_PATH: &str = "/__decoy__/control/send";
/// How long `send` waits for a live connection when the body sets no `timeout` (Q5).
pub const DEFAULT_SEND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlApiRequest {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
}

impl ControlApiRequest {
    pub fn post(path: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            method: "POST".to_owned(),
            path: path.into(),
            headers: BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]),
            body: Some(body.into()),
        }
    }

    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.headers
            .insert(SESSION_HEADER.to_owned(), session.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlApiResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

impl ControlApiResponse {
    fn json(status: u16, value: impl Serialize) -> Self {
        Self {
            status,
            headers: BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]),
            body: serde_json::to_string(&value).expect("control response serializes"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct UseCollectionBody {
    collection: String,
}

#[derive(Debug, Deserialize)]
struct UseRouteBody {
    route: String,
    #[serde(rename = "case")]
    case_id: String,
    behavior: String,
}

#[derive(Debug, Deserialize)]
struct SendBody {
    route: String,
    message: serde_json::Value,
    /// Milliseconds.
    timeout: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SendResponseBody {
    delivered: usize,
}

#[derive(Debug, Serialize)]
struct ControlErrorBody {
    error: String,
}

#[derive(Debug, Error)]
pub enum ControlApiError {
    #[error("control endpoint `{method} {path}` is not defined")]
    UnknownEndpoint { method: String, path: String },
    #[error("control requests must be `content-type: application/json`")]
    NotJson,
    #[error("failed to parse control JSON body: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error("unknown route `{route}`")]
    SendUnknownRoute { route: String },
    #[error(
        "route `{route}` has no live connections to send to; `send` needs a ws, sse or streaming gRPC Route"
    )]
    SendNotLongLived { route: String },
    #[error("no live connection of route `{route}` in session `{session}` within {timeout_ms}ms")]
    SendTimeout {
        route: String,
        session: String,
        timeout_ms: u128,
    },
}

impl ControlApiError {
    fn status(&self) -> u16 {
        match self {
            Self::UnknownEndpoint { .. } => 404,
            Self::NotJson => 415,
            Self::InvalidJson(_) | Self::Control(_) => 400,
            Self::SendUnknownRoute { .. } => 404,
            Self::SendNotLongLived { .. } => 409,
            Self::SendTimeout { .. } => 408,
        }
    }
}

/// Handle the native HTTP Control API shape for the prototype runtime.
///
/// The endpoint decision is recorded in `docs/design/native-control-api-shape.md`.
/// Endpoints are command-shaped Controller verbs below [`CONTROL_PREFIX`]:
///
/// - `POST /__decoy__/control/useCollection` with `{ "collection": "..." }`
/// - `POST /__decoy__/control/useRoute` with `{ "route": "...", "case": "...", "behavior": "..." }`
/// - `POST /__decoy__/control/reset` with an empty or absent body
/// - `POST /__decoy__/control/endSession` with an empty or absent body: frees the Session
///
/// Every `POST`, bodiless ones too, needs `content-type: application/json`: a cross-site page can
/// send `text/plain` or no type without a CORS preflight, so this keeps it from changing Selection.
/// The target Session is selected with `x-mock-session`; absent or empty headers use the default
/// Session. Successful responses return the resulting Selection snapshot.
pub fn handle_control_request(
    controller: &mut Controller,
    request: ControlApiRequest,
) -> ControlApiResponse {
    match try_handle_control_request(controller, request) {
        Ok(selection) => ControlApiResponse::json(200, selection),
        Err(error) => error.into(),
    }
}

impl From<ControlApiError> for ControlApiResponse {
    fn from(error: ControlApiError) -> Self {
        Self::json(
            error.status(),
            ControlErrorBody {
                error: error.to_string(),
            },
        )
    }
}

/// `POST /__decoy__/control/send` with `{ "route": "...", "message": ..., "timeout"?: <ms> }`:
/// delivers `message` to every live connection of the Route in the request's Session and answers
/// `{ "delivered": <n> }`. With no live connection it waits up to `timeout` (default 5000) and
/// fails with `408` (Q5). Unknown Route: `404`; a Route without long-lived connections: `409`.
///
/// Unlike the other verbs it waits, so the server calls it without holding the Controller.
pub async fn handle_send_request(
    catalog: &Catalog,
    connections: &Connections,
    request: ControlApiRequest,
) -> ControlApiResponse {
    match try_send(catalog, connections, request).await {
        Ok(delivered) => ControlApiResponse::json(200, SendResponseBody { delivered }),
        Err(error) => error.into(),
    }
}

async fn try_send(
    catalog: &Catalog,
    connections: &Connections,
    request: ControlApiRequest,
) -> Result<usize, ControlApiError> {
    if !is_json(&request.headers) {
        return Err(ControlApiError::NotJson);
    }
    let session = session_id(&request.headers);
    let body: SendBody = parse_body(request.body)?;
    let Some(route) = catalog.route(&body.route) else {
        return Err(ControlApiError::SendUnknownRoute { route: body.route });
    };
    if !takes_send(catalog, route) {
        return Err(ControlApiError::SendNotLongLived { route: body.route });
    }
    let timeout = body
        .timeout
        .map_or(DEFAULT_SEND_TIMEOUT, Duration::from_millis);
    connections
        .send(&session, &body.route, body.message, timeout)
        .await
        .map_err(|SendError::Timeout| ControlApiError::SendTimeout {
            route: body.route,
            session,
            timeout_ms: timeout.as_millis(),
        })
}

/// Whether the Route holds long-lived connections: server-streaming gRPC so far; WebSocket and SSE
/// come with #218.
fn takes_send(catalog: &Catalog, route: &Route) -> bool {
    let RouteMatch::Grpc(address) = &route.route_match else {
        return false;
    };
    find_method(&catalog.runtime().protos, &address.service, &address.method)
        .is_ok_and(|method| method.is_server_streaming())
}

pub fn try_handle_control_request(
    controller: &mut Controller,
    request: ControlApiRequest,
) -> Result<SelectionSnapshot, ControlApiError> {
    controller.with_source(SessionSource::ControlApi, |controller| {
        dispatch(controller, request)
    })
}

fn dispatch(
    controller: &mut Controller,
    request: ControlApiRequest,
) -> Result<SelectionSnapshot, ControlApiError> {
    let session = session_id(&request.headers);
    let route = route_key(&request.method, &request.path);
    // Only the known verbs; an unknown `/__decoy__` path stays a 404 whatever its content type.
    let known_post = matches!(
        route.as_deref(),
        Some(
            "POST /control/useCollection"
                | "POST /control/useRoute"
                | "POST /control/reset"
                | "POST /control/endSession"
        )
    );
    if known_post && !is_json(&request.headers) {
        return Err(ControlApiError::NotJson);
    }

    match route.as_deref() {
        Some("POST /control/useCollection") => {
            let body: UseCollectionBody = parse_body(request.body)?;
            Ok(controller.try_use_collection(&session, body.collection)?)
        }
        Some("POST /control/useRoute") => {
            let body: UseRouteBody = parse_body(request.body)?;
            Ok(controller.try_use_route(&session, body.route, body.case_id, body.behavior)?)
        }
        Some("POST /control/reset") => {
            parse_optional_body_json(request.body)?;
            Ok(controller.reset(&session))
        }
        Some("POST /control/endSession") => {
            parse_optional_body_json(request.body)?;
            Ok(controller.end_session(&session))
        }
        _ => Err(ControlApiError::UnknownEndpoint {
            method: request.method,
            path: request.path,
        }),
    }
}

fn route_key(method: &str, path: &str) -> Option<String> {
    if !is_control_path(path) {
        return None;
    }
    let path = path.strip_prefix(CONTROL_PREFIX)?;
    Some(format!("{} {}", method.to_ascii_uppercase(), path))
}

pub fn is_control_path(path: &str) -> bool {
    path.strip_prefix(CONTROL_PREFIX)
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
}

fn parse_body<T: for<'de> Deserialize<'de>>(body: Option<String>) -> Result<T, serde_json::Error> {
    serde_json::from_str(body.as_deref().unwrap_or("{}"))
}

fn parse_optional_body_json(body: Option<String>) -> Result<(), serde_json::Error> {
    let Some(body) = body else {
        return Ok(());
    };

    if body.trim().is_empty() {
        return Ok(());
    }

    serde_json::from_str::<serde_json::Value>(&body).map(|_| ())
}

/// `application/json`, with or without parameters such as `; charset=utf-8`.
fn is_json(headers: &BTreeMap<String, String>) -> bool {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
        .and_then(|(_, value)| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

pub fn session_id(headers: &BTreeMap<String, String>) -> String {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(SESSION_HEADER))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_SESSION)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::collections::CollectionsFile;
    use crate::engine::Catalog;
    use crate::http::RuntimeConfig;
    use crate::http::{HttpRequest, ResolveOutcome};
    use crate::schema::{HttpMethod, Route};

    fn route() -> Route {
        Route::from_yaml(
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
      success:
        status: 200
        body:
          ok: true
      missing:
        status: 404
        body:
          error: missing
"#,
        )
        .unwrap()
    }

    fn controller() -> Controller {
        Controller::new(
            Catalog::new(
                vec![route()],
                CollectionsFile::from_yaml(
                    r#"
- id: local
  routes:
    - get-user:user-123:success

- id: not-found
  routes:
    - get-user:user-123:missing
"#,
                )
                .unwrap(),
                RuntimeConfig::default(),
            )
            .unwrap(),
            "local",
        )
    }

    fn selected(controller: &Controller, session: &str) -> String {
        let ResolveOutcome::Matched { activation, .. } = controller.resolve_http(
            session,
            &HttpRequest {
                method: HttpMethod::Get,
                path: "/users/123".to_owned(),
                ..Default::default()
            },
        ) else {
            panic!("expected match")
        };
        activation.address()
    }

    #[test]
    fn native_control_api_can_diverge_two_sessions_and_reset_one_independently() {
        let mut controller = controller();

        let a = handle_control_request(
            &mut controller,
            ControlApiRequest::post(
                "/__decoy__/control/useCollection",
                r#"{"collection":"not-found"}"#,
            )
            .with_session("a"),
        );
        assert_eq!(a.status, 200);
        assert_eq!(selected(&controller, "a"), "get-user:user-123:missing");
        assert_eq!(selected(&controller, "b"), "get-user:user-123:success");

        let b = handle_control_request(
            &mut controller,
            ControlApiRequest::post(
                "/__decoy__/control/useRoute",
                r#"{"route":"get-user","case":"user-123","behavior":"missing"}"#,
            )
            .with_session("b"),
        );
        assert_eq!(b.status, 200);
        assert_eq!(selected(&controller, "a"), "get-user:user-123:missing");
        assert_eq!(selected(&controller, "b"), "get-user:user-123:missing");

        let reset_b = handle_control_request(
            &mut controller,
            ControlApiRequest::post("/__decoy__/control/reset", "").with_session("b"),
        );
        assert_eq!(reset_b.status, 200);
        assert_eq!(selected(&controller, "a"), "get-user:user-123:missing");
        assert_eq!(selected(&controller, "b"), "get-user:user-123:success");
    }

    #[test]
    fn native_control_api_rejects_malformed_reset_json_without_mutating_selection() {
        let mut controller = controller();

        let use_route = handle_control_request(
            &mut controller,
            ControlApiRequest::post(
                "/__decoy__/control/useRoute",
                r#"{"route":"get-user","case":"user-123","behavior":"missing"}"#,
            ),
        );
        assert_eq!(use_route.status, 200);
        assert_eq!(
            selected(&controller, DEFAULT_SESSION),
            "get-user:user-123:missing"
        );

        let reset = handle_control_request(
            &mut controller,
            ControlApiRequest::post("/__decoy__/control/reset", "{"),
        );

        assert_eq!(reset.status, 400);
        assert!(reset.body.contains("failed to parse control JSON body"));
        assert_eq!(
            selected(&controller, DEFAULT_SESSION),
            "get-user:user-123:missing"
        );
    }

    #[test]
    fn native_control_api_takes_only_json_posts() {
        let mut controller = controller();
        let post = |content_type: Option<&str>| {
            let mut request = ControlApiRequest::post(
                "/__decoy__/control/useCollection",
                r#"{"collection":"not-found"}"#,
            );
            request.headers.remove("content-type");
            if let Some(content_type) = content_type {
                request
                    .headers
                    .insert("content-type".to_owned(), content_type.to_owned());
            }
            request
        };

        // A cross-site form or `fetch` can send these without a CORS preflight.
        for content_type in [Some("text/plain"), None] {
            let response = handle_control_request(&mut controller, post(content_type));
            assert_eq!(response.status, 415, "{content_type:?}");
            assert_eq!(
                selected(&controller, DEFAULT_SESSION),
                "get-user:user-123:success"
            );
        }
        let mut reset = ControlApiRequest::post("/__decoy__/control/reset", "");
        reset.headers.clear();
        assert_eq!(handle_control_request(&mut controller, reset).status, 415);
        let mut unknown = ControlApiRequest::post("/__decoy__/control/nope", "");
        unknown.headers.clear();
        assert_eq!(handle_control_request(&mut controller, unknown).status, 404);

        let response = handle_control_request(
            &mut controller,
            post(Some("Application/JSON; charset=utf-8")),
        );
        assert_eq!(response.status, 200, "{}", response.body);
    }

    #[test]
    fn native_control_api_returns_clear_errors_for_bad_addresses() {
        let mut controller = controller();

        let missing_collection = handle_control_request(
            &mut controller,
            ControlApiRequest::post(
                "/__decoy__/control/useCollection",
                r#"{"collection":"ghost"}"#,
            ),
        );
        assert_eq!(missing_collection.status, 400);
        assert_eq!(
            missing_collection.body,
            r#"{"error":"unknown collection `ghost`"}"#
        );

        let missing_behavior = handle_control_request(
            &mut controller,
            ControlApiRequest::post(
                "/__decoy__/control/useRoute",
                r#"{"route":"get-user","case":"user-123","behavior":"ghost"}"#,
            ),
        );
        assert_eq!(missing_behavior.status, 400);
        assert_eq!(
            missing_behavior.body,
            r#"{"error":"unknown behavior `get-user:user-123:ghost`"}"#
        );
    }

    fn send(body: &str) -> ControlApiRequest {
        ControlApiRequest::post(SEND_PATH, body)
    }

    async fn send_response(request: ControlApiRequest) -> ControlApiResponse {
        handle_send_request(&controller().catalog(), &Connections::default(), request).await
    }

    #[tokio::test]
    async fn send_rejects_unknown_routes_and_routes_without_live_connections() {
        let unknown = send_response(send(r#"{"route":"ghost","message":{}}"#)).await;
        assert_eq!(unknown.status, 404);
        assert_eq!(unknown.body, r#"{"error":"unknown route `ghost`"}"#);

        let http = send_response(send(r#"{"route":"get-user","message":{}}"#)).await;
        assert_eq!(http.status, 409, "{}", http.body);
    }

    #[tokio::test]
    async fn send_takes_only_a_well_formed_json_body() {
        let missing_message = send_response(send(r#"{"route":"get-user"}"#)).await;
        assert_eq!(missing_message.status, 400);
        assert!(missing_message.body.contains("missing field `message`"));

        assert_eq!(send_response(send("{")).await.status, 400);

        let mut plain = send(r#"{"route":"get-user","message":{}}"#);
        plain
            .headers
            .insert("content-type".to_owned(), "text/plain".to_owned());
        assert_eq!(send_response(plain).await.status, 415);
    }
}
