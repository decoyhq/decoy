use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::response::{
    control_response, grpc_response, json_response, response_from_execution_outcome,
    response_from_forward_response,
};
use crate::connections::Connections;
use crate::control_api::{
    ControlApiRequest, HEALTH_PATH, SEND_PATH, handle_control_request, handle_send_request,
    is_control_path, session_id,
};
use crate::engine::Controller;
use crate::grpc::{GrpcRequest, GrpcResponse, RESOURCE_EXHAUSTED, UNIMPLEMENTED};
use crate::http::{HttpExecutionRequest, HttpResponsePlan, RequestMetadata};
use crate::schema::HttpMethod;
use axum::Extension;
use axum::body::{Body, to_bytes};
use axum::extract::{OriginalUri, State};
use axum::http::{Request, Response, StatusCode};
use tokio::sync::RwLock;

/// `maxBodyBytes`: larger requests get `413` (or gRPC `RESOURCE_EXHAUSTED`) instead of buffering.
#[derive(Debug, Clone)]
pub(super) struct MaxBodyBytes(pub(super) Arc<AtomicUsize>);

/// Marks requests that arrived on a TLS listener.
#[derive(Debug, Clone, Copy)]
pub(super) struct Secure;

pub(super) async fn handle_request(
    State(controller): State<Arc<RwLock<Controller>>>,
    Extension(events): Extension<crate::events::Sender>,
    Extension(connections): Extension<Connections>,
    OriginalUri(uri): OriginalUri,
    request: Request<Body>,
) -> Response<Body> {
    crate::events::logging_to(events, respond(controller, connections, uri, request)).await
}

async fn respond(
    controller: Arc<RwLock<Controller>>,
    connections: Connections,
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
        original_base_url: original_base_url(&headers, request.uri(), secure),
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
        let request = ControlApiRequest {
            method: request_method.as_str().to_owned(),
            path: uri.path().to_owned(),
            headers,
            body: Some(String::from_utf8_lossy(&body).into_owned()),
        };
        if uri.path() == SEND_PATH && request_method == axum::http::Method::POST {
            // `send` may wait for a connection; it must not hold the Controller meanwhile.
            let catalog = controller.read().await.catalog();
            return control_response(handle_send_request(&catalog, &connections, request).await);
        }
        let response = handle_control_request(&mut *controller.write().await, request);
        return control_response(response);
    }

    let Some(method) = http_method(&request_method) else {
        let miss = HttpResponsePlan::fail_closed_miss("unsupported http method");
        return response_from_forward_response(miss.into());
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

/// `Host` for HTTP/1; HTTP/2 sends `:authority`, which lands in the request URI instead.
fn original_base_url(
    headers: &BTreeMap<String, String>,
    uri: &axum::http::Uri,
    secure: bool,
) -> Option<String> {
    let scheme = if secure { "https" } else { "http" };
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.as_str())
        .or_else(|| uri.authority().map(|authority| authority.as_str()))
        .map(|host| format!("{scheme}://{host}"))
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
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn http2_base_url_comes_from_the_uri_authority() {
        let uri: axum::http::Uri = "https://api.example.test:8443/users".parse().unwrap();

        assert_eq!(
            original_base_url(&BTreeMap::new(), &uri, true).as_deref(),
            Some("https://api.example.test:8443")
        );
    }

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
}
