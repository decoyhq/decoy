use crate::control_api::ControlApiResponse;
use crate::grpc::GrpcResponse;
use crate::http::HttpExecutionOutcome;
use crate::http::forward::ForwardResponse;
use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Response, StatusCode};
use bytes::Bytes;
use http_body::Frame;
use http_body_util::StreamBody;

/// A gRPC answer: HTTP 200, the framed message, then `grpc-status`/`grpc-message` and metadata as
/// trailers.
pub(super) fn grpc_response(response: GrpcResponse) -> Response<Body> {
    let mut trailers = axum::http::HeaderMap::new();
    trailers.insert("grpc-status", HeaderValue::from(u16::from(response.status)));
    if !response.status_message.is_empty()
        && let Ok(value) = HeaderValue::from_str(&percent_encode(&response.status_message))
    {
        trailers.insert("grpc-message", value);
    }
    for (name, value) in response.metadata {
        // A rendered template value skips the startup check; a tab is a valid header but not gRPC.
        let valid = crate::grpc::schema::valid_metadata_value(&value);
        match (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            (Ok(name), Ok(value)) if valid => {
                trailers.insert(name, value);
            }
            _ => crate::events::warn(format!(
                "dropped response metadata `{name}`: invalid name or value"
            )),
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

pub(super) fn response_from_execution_outcome(outcome: HttpExecutionOutcome) -> Response<Body> {
    match outcome {
        HttpExecutionOutcome::Response { response, .. } => response_from_forward_response(response),
        HttpExecutionOutcome::ForwardingError { source, .. } => {
            response_from_forward_response(source.response())
        }
    }
}

pub(super) fn control_response(control: ControlApiResponse) -> Response<Body> {
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

pub(super) fn response_from_forward_response(plan: ForwardResponse) -> Response<Body> {
    let mut response = Response::new(match plan.stream {
        Some(stream) => Body::from_stream(stream.0),
        None => Body::from(plan.body),
    });
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

pub(super) fn json_response(status: StatusCode, value: serde_json::Value) -> Response<Body> {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[tokio::test]
    async fn grpc_response_drops_metadata_grpc_does_not_allow() {
        use http_body_util::BodyExt;

        let response = grpc_response(GrpcResponse {
            metadata: BTreeMap::from([
                ("x-ok".to_owned(), "fine".to_owned()),
                ("x-tab".to_owned(), "a\tb".to_owned()),
                ("bad name".to_owned(), "x".to_owned()),
            ]),
            ..GrpcResponse::error(0, "")
        });
        let trailers = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .trailers()
            .cloned()
            .unwrap();

        assert_eq!(trailers["x-ok"], "fine");
        assert!(!trailers.contains_key("x-tab"));
        assert_eq!(trailers.len(), 2, "{trailers:?}"); // grpc-status + x-ok
    }
}
