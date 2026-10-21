use crate::control_api::ControlApiResponse;
use crate::grpc::{GrpcResponse, StreamPart};
use crate::http::HttpExecutionOutcome;
use crate::http::forward::ForwardResponse;
use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Response, StatusCode};
use bytes::Bytes;
use futures_util::StreamExt as _;
use futures_util::stream::{self, BoxStream};
use http_body::Frame;
use http_body_util::StreamBody;

/// A gRPC answer: HTTP 200, the framed message, then `grpc-status`/`grpc-message` and metadata as
/// trailers.
pub(super) fn grpc_response(response: GrpcResponse) -> Response<Body> {
    grpc_body(stream::iter(frames(response)).boxed())
}

/// A server-streaming answer: one data frame per message as it comes, then the trailers.
pub(super) fn grpc_stream_response(parts: BoxStream<'static, StreamPart>) -> Response<Body> {
    // hyper polls the body outside the request's log scope, so the trailers' warnings take it along.
    let events = crate::events::current();
    grpc_body(
        parts
            .flat_map(move |part| {
                stream::iter(match part {
                    StreamPart::Message(message) => vec![Frame::data(Bytes::from(message))],
                    StreamPart::End(response) => {
                        crate::events::logging_to_sync(events.clone(), || frames(response))
                    }
                })
            })
            .boxed(),
    )
}

fn grpc_body(frames: BoxStream<'static, Frame<Bytes>>) -> Response<Body> {
    let frames = frames.map(Ok::<_, std::convert::Infallible>);
    let mut http = Response::new(Body::new(StreamBody::new(frames)));
    http.headers_mut()
        .insert("content-type", HeaderValue::from_static("application/grpc"));
    http
}

/// The response's message, if any, then its status and metadata as trailers.
fn frames(response: GrpcResponse) -> Vec<Frame<Bytes>> {
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
        frames.push(Frame::data(Bytes::from(response.body)));
    }
    frames.push(Frame::trailers(trailers));
    frames
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

    #[tokio::test]
    async fn a_stream_end_warning_reaches_the_server_that_built_the_stream() {
        use http_body_util::BodyExt;

        let (events, mut log) = tokio::sync::broadcast::channel(8);
        let end = GrpcResponse {
            metadata: BTreeMap::from([("x-tab".to_owned(), "a\tb".to_owned())]),
            ..GrpcResponse::error(0, "")
        };
        let response = crate::events::logging_to(events, async {
            grpc_stream_response(stream::iter([StreamPart::End(end)]).boxed())
        })
        .await;
        // hyper polls the body outside the request's scope.
        response.into_body().collect().await.unwrap();

        assert!(matches!(
            log.try_recv(),
            Ok(crate::events::Event::Log(crate::events::LogEvent { message, .. }))
                if message.contains("x-tab")
        ));
    }
}
