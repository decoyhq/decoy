//! The runtime server: binds the configured listeners and serves every transport.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::config::{ServeConfig, TlsSource};
use crate::control_api::{
    ControlApiRequest, ControlApiResponse, HEALTH_PATH, handle_control_request, is_control_path,
    session_id,
};
use crate::engine::{Catalog, Controller, HttpExecutionOutcome, HttpExecutionRequest};
use crate::grpc::{GrpcRequest, GrpcResponse, RESOURCE_EXHAUSTED, UNIMPLEMENTED};
use crate::http::{BodyPlan, HttpResponsePlan, RequestMetadata, ResponsePlan, RuntimeConfig};
use crate::http_forward::ForwardResponse;
use crate::protos::load_descriptor_pool;
use crate::schema::{HttpMethod, PassthroughTarget};
use crate::startup::{Startup, load_catalog_from_files};
use crate::tls;
use axum::body::{Body, to_bytes};
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderName, HeaderValue, Request, Response, StatusCode};
use axum::routing::any;
use axum::{Extension, Router};
use bytes::Bytes;
use http_body::Frame;
use http_body_util::StreamBody;
use tokio::net::TcpListener;
use tokio::sync::RwLock;

/// One bound socket: the config key it serves (`port` or `listeners.<name>`), TLS, address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundListener {
    pub name: String,
    pub tls: bool,
    pub addr: SocketAddr,
}

impl BoundListener {
    pub fn url(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}", self.addr)
    }

    /// The URL, plus the config key unless it is the main `port`: what `listening on` shows.
    pub fn label(&self) -> String {
        match self.name.as_str() {
            "port" => self.url(),
            name => format!("{} ({name})", self.url()),
        }
    }
}

/// A started runtime: every listener is bound and serving until the shutdown signal fires.
pub struct Server {
    pub listeners: Vec<BoundListener>,
    /// Startup messages for the caller to show, e.g. where the auto TLS CA lives. `serve` prints
    /// nothing itself, not even the listen addresses.
    pub notices: Vec<String>,
    pub controller: Arc<RwLock<Controller>>,
    reloader: Reloader,
    events: crate::events::Sender,
    shutdown: Pin<Box<dyn Future<Output = ()> + Send>>,
    stop_all: tokio::sync::watch::Sender<()>,
    servers: tokio::task::JoinSet<std::io::Result<()>>,
}

/// Error returned by [`serve`] and [`Server::wait`]; `Send + Sync` so `wait` can be spawned.
pub type ServeError = Box<dyn std::error::Error + Send + Sync>;

/// Loads `config`, binds every listener and starts serving. `shutdown` is awaited by
/// [`Server::wait`]; when it resolves, open requests get [`DRAIN_TIMEOUT`] to finish.
pub async fn serve(
    config: ServeConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<Server, ServeError> {
    let startup = load(&config)?;
    let controller = Controller::new(startup.catalog, startup.default_collection);
    let events = controller.events();
    let mut notices = Vec::new();
    let controller = Arc::new(RwLock::new(controller));
    let max_body_bytes = Arc::new(AtomicUsize::new(config.max_body_bytes));
    let app = Router::new()
        .fallback(any(handle_request))
        .with_state(controller.clone())
        .layer(Extension(MaxBodyBytes(max_body_bytes.clone())))
        .layer(Extension(events.clone()));
    let tls_config = match &config.tls {
        None => None,
        Some(TlsSource::Files(files)) => Some(tls::server_config(files)?),
        Some(TlsSource::Auto { dir }) => {
            let (tls_config, ca) = tls::auto_server_config(dir)?;
            notices.push(format!(
                "TLS cert signed by {}; trust it with e.g. NODE_EXTRA_CA_CERTS",
                ca.display()
            ));
            Some(tls_config)
        }
    };
    // Bind every listener before serving any, so a taken port fails startup cleanly (Q1).
    let mut listeners = Vec::new();
    let named = std::iter::once(("port".to_owned(), &config.port)).chain(
        config
            .listeners
            .iter()
            .map(|(name, listener)| (format!("listeners.{name}"), listener)),
    );
    for (name, listener) in named {
        for bound in bind(&name, &listener.host, listener.port, &mut notices).await? {
            listeners.push((name.clone(), listener.tls, bound));
        }
    }

    let (stop_all, _) = tokio::sync::watch::channel(());
    let mut bound = Vec::new();
    let mut servers = tokio::task::JoinSet::new();
    for (name, secure, listener) in listeners {
        let addr = listener.local_addr()?;
        bound.push(BoundListener {
            name: name.clone(),
            tls: secure,
            addr,
        });
        let mut stop = stop_all.subscribe();
        match tls_config.clone().filter(|_| secure) {
            Some(tls_config) => {
                let app = app.clone().layer(Extension(Secure));
                servers.spawn(crate::events::logging_to(
                    events.clone(),
                    tls::serve(listener, app, tls_config, stop),
                ));
            }
            None => {
                servers.spawn(
                    axum::serve(listener, app.clone())
                        .with_graceful_shutdown(async move {
                            let _ = stop.changed().await;
                        })
                        .into_future(),
                );
            }
        }
    }
    let reloader = Reloader {
        controller: controller.clone(),
        max_body_bytes,
        listeners: Arc::new(ListenerFields::of(&config)),
        running: Arc::default(),
    };
    Ok(Server {
        listeners: bound,
        notices,
        controller,
        reloader,
        events,
        shutdown: Box::pin(shutdown),
        stop_all,
        servers,
    })
}

/// Builds the Catalog from `config`'s protos, Routes and Collections.
fn load(config: &ServeConfig) -> Result<Startup, ServeError> {
    let protos = load_descriptor_pool(&config.protos)?;
    Ok(load_catalog_from_files(
        &config.routes,
        &config.collections,
        RuntimeConfig {
            passthrough: config
                .passthrough_base_url
                .clone()
                .map(|base_url| PassthroughTarget { base_url }),
            protos,
        },
        config.collection.as_deref(),
    )?)
}

/// Applies a re-read config to a running server (`/reload`, `--watch`): everything but the
/// listeners, which stay bound as they are.
#[derive(Clone)]
pub struct Reloader {
    controller: Arc<RwLock<Controller>>,
    max_body_bytes: Arc<AtomicUsize>,
    listeners: Arc<ListenerFields>,
    /// One reload at a time (`/reload` and `--watch`).
    running: Arc<tokio::sync::Mutex<()>>,
}

/// What a successful reload applied.
#[derive(Debug, Clone)]
pub struct Reloaded {
    /// The Catalog now served.
    pub catalog: Arc<Catalog>,
    /// What a Session that never picked a Collection now uses.
    pub default_collection: String,
    /// `listener changes need restart`, then one line per Selection fallback or dropped override.
    pub warnings: Vec<String>,
}

impl Reloaded {
    /// `2 routes, 1 collection`.
    pub fn summary(&self) -> String {
        let count = |count: usize, noun: &str| {
            format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
        };
        format!(
            "{}, {}",
            count(self.catalog.routes().count(), "route"),
            count(self.catalog.collection_ids().count(), "collection")
        )
    }
}

/// The config fields a reload can't apply without rebinding.
#[derive(Debug, PartialEq, Eq)]
struct ListenerFields {
    port: crate::config::Listener,
    listeners: BTreeMap<String, crate::config::Listener>,
    tls: Option<TlsSource>,
}

impl ListenerFields {
    fn of(config: &ServeConfig) -> Self {
        Self {
            port: config.port.clone(),
            listeners: config.listeners.clone(),
            tls: config.tls.clone(),
        }
    }
}

impl Reloader {
    /// Reads the config with `read_config`, builds the new Catalog before taking the Controller's
    /// lock, then swaps it in. On an error nothing changes. Requests already running finish
    /// against the old Catalog. Reloads queue, and each one reads the files only once it runs,
    /// so the last one always applies the newest files.
    pub async fn reload(
        &self,
        read_config: impl FnOnce() -> Result<ServeConfig, ServeError>,
    ) -> Result<Reloaded, ServeError> {
        let _running = self.running.lock().await;
        let config = read_config()?;
        let startup = load(&config)?;
        let mut warnings = Vec::new();
        if ListenerFields::of(&config) != *self.listeners {
            warnings.push("listener changes need restart".to_owned());
        }
        let mut controller = self.controller.write().await;
        warnings.extend(controller.reload(startup.catalog, startup.default_collection));
        self.max_body_bytes
            .store(config.max_body_bytes, Ordering::Relaxed);
        Ok(Reloaded {
            catalog: controller.catalog(),
            default_collection: controller.default_collection().to_owned(),
            warnings,
        })
    }
}

impl Server {
    pub fn reloader(&self) -> Reloader {
        self.reloader.clone()
    }

    /// Every [`crate::events::Event`] from now on; see [`crate::events`].
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<crate::events::Event> {
        self.events.subscribe()
    }

    /// Serves until the shutdown signal fires or a listener fails, then drains.
    pub async fn wait(self) -> Result<(), ServeError> {
        let Self {
            events,
            shutdown,
            stop_all,
            mut servers,
            ..
        } = self;
        tokio::select! {
            _ = shutdown => {}
            Some(result) = servers.join_next() => result??,
        }
        drop(stop_all);
        // Let open requests finish, but never wait on a long `delay` or a hung upstream forever.
        let drained = tokio::time::timeout(DRAIN_TIMEOUT, async {
            while let Some(result) = servers.join_next().await {
                result??;
            }
            Ok::<_, ServeError>(())
        })
        .await;
        match drained {
            Ok(result) => result?,
            Err(_) => {
                crate::events::logging_to(events, async {
                    crate::events::warn(format!(
                        "closed connections still open after {}s",
                        DRAIN_TIMEOUT.as_secs()
                    ));
                })
                .await;
                servers.abort_all();
            }
        }

        Ok(())
    }
}

pub const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// `maxBodyBytes`: larger requests get `413` (or gRPC `RESOURCE_EXHAUSTED`) instead of buffering.
#[derive(Debug, Clone)]
struct MaxBodyBytes(Arc<AtomicUsize>);

/// Marks requests that arrived on a TLS listener.
#[derive(Debug, Clone, Copy)]
struct Secure;

/// Binds every address `host` resolves to on one port. `localhost` is both loopbacks (`127.0.0.1`,
/// `::1`) without asking the resolver, which often maps it to `127.0.0.1` only. An address the
/// machine can't bind (e.g. `::1` with IPv6 off) is skipped while another one binds; a taken port
/// always fails; a skipped address adds a notice.
async fn bind(
    name: &str,
    host: &str,
    port: u16,
    notices: &mut Vec<String>,
) -> Result<Vec<TcpListener>, String> {
    let mut addrs: Vec<SocketAddr> = if host.eq_ignore_ascii_case("localhost") {
        vec![
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)),
            SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)),
        ]
    } else {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|error| format!("cannot resolve `{name}` host `{host}`: {error}"))?
            .collect()
    };
    addrs.sort();
    addrs.dedup();
    let mut bound = Vec::new();
    let mut skipped = None;
    for mut addr in addrs {
        // Port 0: the first address picks a free port; the others share it.
        if let Some(first) = bound
            .first()
            .and_then(|first: &TcpListener| first.local_addr().ok())
        {
            addr.set_port(first.port());
        }
        match TcpListener::bind(addr).await {
            Ok(listener) => bound.push(listener),
            Err(error)
                if !matches!(
                    error.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                notices.push(format!("skipped `{name}` on {addr}: {error}"));
                skipped = Some(format!("cannot listen on `{name}` ({addr}): {error}"));
            }
            Err(error) => return Err(format!("cannot listen on `{name}` ({addr}): {error}")),
        }
    }
    if bound.is_empty() {
        return Err(skipped.unwrap_or_else(|| format!("`{name}` host `{host}` has no addresses")));
    }
    Ok(bound)
}

async fn handle_request(
    State(controller): State<Arc<RwLock<Controller>>>,
    Extension(events): Extension<crate::events::Sender>,
    OriginalUri(uri): OriginalUri,
    request: Request<Body>,
) -> Response<Body> {
    crate::events::logging_to(events, respond(controller, uri, request)).await
}

async fn respond(
    controller: Arc<RwLock<Controller>>,
    uri: axum::http::Uri,
    request: Request<Body>,
) -> Response<Body> {
    if uri.path() == HEALTH_PATH && request.method() == axum::http::Method::GET {
        return json_response(StatusCode::OK, serde_json::json!({ "status": "ok" }));
    }

    let headers = request_headers(request.headers());
    let session = session_id(&headers);
    let request_method = request.method().clone();
    let secure = request.extensions().get::<Secure>().is_some();
    let metadata = RequestMetadata {
        original_base_url: original_base_url(&headers, secure),
    };
    let max_body_bytes = request
        .extensions()
        .get::<MaxBodyBytes>()
        .map_or(crate::config::DEFAULT_MAX_BODY_BYTES, |max| {
            max.0.load(Ordering::Relaxed)
        });
    let body = match to_bytes(request.into_body(), max_body_bytes).await {
        Ok(body) => body.to_vec(),
        Err(error)
            if std::error::Error::source(&error)
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>()) =>
        {
            let message = format!("request body exceeds {max_body_bytes} bytes (`maxBodyBytes`)");
            return if is_grpc(&headers) {
                grpc_response(GrpcResponse::error(RESOURCE_EXHAUSTED, message))
            } else {
                json_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    serde_json::json!({ "error": message }),
                )
            };
        }
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                serde_json::json!({ "error": format!("failed to read request body: {error}") }),
            );
        }
    };

    if is_grpc(&headers) && !is_control_path(uri.path()) {
        let Some((service, method)) = GrpcRequest::parse_path(uri.path()) else {
            return grpc_response(GrpcResponse::error(
                UNIMPLEMENTED,
                format!("`{}` is not a gRPC method path", uri.path()),
            ));
        };
        let view = controller.read().await.session_view(&session);
        let response = view
            .execute_grpc(GrpcRequest {
                service,
                method,
                metadata: headers,
                body,
            })
            .await;
        return grpc_response(response);
    }

    if is_control_path(uri.path()) {
        let response = handle_control_request(
            &mut *controller.write().await,
            ControlApiRequest {
                method: request_method.as_str().to_owned(),
                path: uri.path().to_owned(),
                headers,
                body: Some(String::from_utf8_lossy(&body).into_owned()),
            },
        );
        return control_response(response);
    }

    let Some(method) = http_method(&request_method) else {
        return response_from_plan(ResponsePlan::fail_closed_miss("unsupported http method"));
    };
    let view = controller.read().await.session_view(&session);
    let outcome = view
        .execute_http(HttpExecutionRequest {
            method,
            path: uri.path().to_owned(),
            path_and_query: uri
                .path_and_query()
                .map(|path| path.as_str().to_owned())
                .unwrap_or_else(|| uri.path().to_owned()),
            headers,
            body,
            metadata,
        })
        .await;

    response_from_execution_outcome(outcome)
}

/// One entry handler serves every transport (Q1): `application/grpc*` requests are gRPC calls.
fn is_grpc(headers: &BTreeMap<String, String>) -> bool {
    headers
        .get("content-type")
        .and_then(|value| value.strip_prefix("application/grpc"))
        // `application/grpc`, `+proto`, `;charset`; not grpc-web, which frames trailers in the body.
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['+', ';']))
}

/// A gRPC answer: HTTP 200, the framed message, then `grpc-status`/`grpc-message` and metadata as
/// trailers.
fn grpc_response(response: GrpcResponse) -> Response<Body> {
    let mut trailers = axum::http::HeaderMap::new();
    trailers.insert("grpc-status", HeaderValue::from(u16::from(response.status)));
    if !response.status_message.is_empty()
        && let Ok(value) = HeaderValue::from_str(&percent_encode(&response.status_message))
    {
        trailers.insert("grpc-message", value);
    }
    for (name, value) in response.metadata {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            trailers.insert(name, value);
        }
    }
    let mut frames = Vec::new();
    if !response.body.is_empty() {
        frames.push(Ok::<_, std::convert::Infallible>(Frame::data(Bytes::from(
            response.body,
        ))));
    }
    frames.push(Ok(Frame::trailers(trailers)));
    let mut http = Response::new(Body::new(StreamBody::new(futures_util::stream::iter(
        frames,
    ))));
    http.headers_mut()
        .insert("content-type", HeaderValue::from_static("application/grpc"));
    http
}

/// `grpc-message` is percent-encoded UTF-8 (gRPC over HTTP/2 spec).
fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b' '..=b'~' if byte != b'%' => char::from(byte).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn response_from_execution_outcome(outcome: HttpExecutionOutcome) -> Response<Body> {
    match outcome {
        HttpExecutionOutcome::Response { response, .. } => response_from_forward_response(response),
        HttpExecutionOutcome::ForwardingError { source, .. } => {
            response_from_forward_response(source.response())
        }
    }
}

fn control_response(control: ControlApiResponse) -> Response<Body> {
    let mut response = Response::new(Body::from(control.body));
    *response.status_mut() =
        StatusCode::from_u16(control.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    for (name, value) in control.headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

fn response_from_plan(plan: ResponsePlan) -> Response<Body> {
    match plan {
        ResponsePlan::Response(response) => http_response(response),
        ResponsePlan::Passthrough(_) => response_from_plan(ResponsePlan::fail_closed_miss(
            "passthrough behavior is not implemented by native serve yet",
        )),
    }
}

fn response_from_forward_response(plan: ForwardResponse) -> Response<Body> {
    let mut response = Response::new(Body::from(plan.body));
    *response.status_mut() = StatusCode::from_u16(plan.status).unwrap_or(StatusCode::OK);

    for (name, value) in plan.headers {
        match (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            (Ok(name), Ok(value)) => {
                response.headers_mut().append(name, value);
            }
            _ => crate::events::warn(format!(
                "dropped response header `{name}`: invalid name or value"
            )),
        }
    }

    response
}

fn json_response(status: StatusCode, value: serde_json::Value) -> Response<Body> {
    let mut response = Response::new(Body::from(
        serde_json::to_vec(&value).expect("JSON response serializes"),
    ));
    *response.status_mut() = status;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn http_response(plan: HttpResponsePlan) -> Response<Body> {
    let body = match plan.body {
        Some(BodyPlan::Json(value)) => Body::from(serde_json::to_vec(&value).expect("json body")),
        Some(BodyPlan::Text(text)) => Body::from(text),
        None => Body::empty(),
    };
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::from_u16(plan.status).unwrap_or(StatusCode::OK);

    for (name, value) in plan.headers {
        match (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            (Ok(name), Ok(value)) => {
                response.headers_mut().insert(name, value);
            }
            _ => crate::events::warn(format!(
                "dropped response header `{name}`: invalid name or value"
            )),
        }
    }

    response
}

/// Lowercased names; a repeated header keeps every value, joined with `", "`.
fn request_headers(headers: &axum::http::HeaderMap) -> BTreeMap<String, String> {
    let mut joined: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in headers {
        // Non-UTF-8 bytes become U+FFFD instead of dropping the whole header.
        let value = &*String::from_utf8_lossy(value.as_bytes());
        // HTTP/2 splits `cookie` into one field per pair; they rejoin with `; ` (RFC 9113 §8.2.3).
        let separator = if name == axum::http::header::COOKIE {
            "; "
        } else {
            ", "
        };
        joined
            .entry(name.as_str().to_owned())
            .and_modify(|existing| {
                existing.push_str(separator);
                existing.push_str(value);
            })
            .or_insert_with(|| value.to_owned());
    }
    joined
}

fn original_base_url(headers: &BTreeMap<String, String>, secure: bool) -> Option<String> {
    let scheme = if secure { "https" } else { "http" };
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("host"))
        .map(|(_, value)| format!("{scheme}://{value}"))
}

fn http_method(method: &axum::http::Method) -> Option<HttpMethod> {
    match *method {
        axum::http::Method::GET => Some(HttpMethod::Get),
        axum::http::Method::POST => Some(HttpMethod::Post),
        axum::http::Method::PUT => Some(HttpMethod::Put),
        axum::http::Method::PATCH => Some(HttpMethod::Patch),
        axum::http::Method::DELETE => Some(HttpMethod::Delete),
        axum::http::Method::HEAD => Some(HttpMethod::Head),
        axum::http::Method::OPTIONS => Some(HttpMethod::Options),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_http2_cookies_rejoin_with_semicolons() {
        let mut headers = axum::http::HeaderMap::new();
        headers.append("cookie", HeaderValue::from_static("a=1"));
        headers.append("cookie", HeaderValue::from_static("b=2"));
        headers.append("accept", HeaderValue::from_static("text/html"));
        headers.append("accept", HeaderValue::from_static("*/*"));

        headers.append("x-latin1", HeaderValue::from_bytes(b"caf\xe9").unwrap());

        let joined = request_headers(&headers);
        assert_eq!(joined["cookie"], "a=1; b=2");
        assert_eq!(joined["x-latin1"], "caf\u{fffd}");
        assert_eq!(joined["accept"], "text/html, */*");
    }

    #[tokio::test]
    async fn port_zero_picks_one_port_for_every_address_of_a_host() {
        let bound = bind("t", "localhost", 0, &mut Vec::new()).await.unwrap();
        assert!(!bound.is_empty());
        let ports: Vec<u16> = bound
            .iter()
            .map(|listener| listener.local_addr().unwrap().port())
            .collect();
        assert!(ports.iter().all(|port| *port == ports[0]), "{ports:?}");
    }
}
