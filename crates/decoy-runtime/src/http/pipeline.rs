//! HTTP's boundary between wire bodies and the values passed to Transformers.

use bytes::Bytes;
use futures_util::TryStreamExt as _;
use rmpv::Value as Msgpack;
use serde_json::Value;

use super::forward::{ForwardResponse, ForwardingError};
use super::{BodyPlan, HttpRequest, HttpResponsePlan, RuntimeConfig};
use crate::pipeline::{Pipeline, PipelineError};
use crate::request::RequestEnvelope;
use crate::schema::Route;
use crate::workers::value::{to_json, to_msgpack};
use crate::workers::{CallRequest, Op};

pub(super) fn worker_request(request: &HttpRequest) -> CallRequest<'_> {
    CallRequest {
        method: request.method.as_str(),
        path: &request.path,
        headers: request.envelope.headers(),
    }
}

pub(super) struct PreparedRoute {
    pub pipeline: Pipeline,
    /// An explicit worker miss rejects every Case on this Route.
    pub input: Result<Option<PreparedInput>, String>,
}

pub(super) struct PreparedInput {
    pub envelope: RequestEnvelope,
    /// Media type override for Passthrough only; Cases see the original headers.
    pub content_type: Option<&'static str>,
}

impl PreparedRoute {
    pub async fn prepare(
        route: &Route,
        runtime: &RuntimeConfig,
        request: &HttpRequest,
    ) -> Result<Self, String> {
        let original = &request.envelope;
        let pipeline = Pipeline::of(route, runtime)?.expect("Route selects a named Pipeline");
        let input = if pipeline.has_input() {
            let content_type = original.headers.get("content-type").map(String::as_str);
            let encoding = original.headers.get("content-encoding").map(String::as_str);
            let value = decode(content_type, encoding, &original.raw_body);
            match pipeline
                .run_http(Op::Input, value, worker_request(request))
                .await
            {
                Ok(value) => Ok(Some(PreparedInput::from_transformed(
                    original.clone(),
                    value,
                    content_type,
                    pipeline.input_ends_in_command(),
                )?)),
                Err(error @ PipelineError::Miss { .. }) => Err(error.to_string()),
                Err(error) => return Err(error.to_string()),
            }
        } else {
            Ok(None)
        };
        Ok(Self { pipeline, input })
    }
}

impl PreparedInput {
    fn from_transformed(
        mut envelope: RequestEnvelope,
        value: Msgpack,
        content_type: Option<&str>,
        from_command: bool,
    ) -> Result<Self, String> {
        let (bytes, json, content_type) = match value {
            Msgpack::Binary(bytes) => {
                let json = if from_command {
                    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                (Bytes::from(bytes), json, None)
            }
            value => {
                let json = to_json(&value)?;
                let forwarded_type = (!json.is_string() && !is_json(mime(content_type)))
                    .then_some("application/json");
                (json_bytes(&json, content_type), json, forwarded_type)
            }
        };
        envelope.parsed_body = json.into();
        envelope.raw_body = bytes;
        Ok(Self {
            envelope,
            content_type,
        })
    }
}

/// Give the Pipeline the original bytes when normal body decoding does not apply or fails.
fn decode(content_type: Option<&str>, encoding: Option<&str>, bytes: &[u8]) -> Msgpack {
    if encoding.is_some_and(|header| {
        header
            .split(',')
            .any(|coding| !coding.trim().eq_ignore_ascii_case("identity"))
    }) {
        return Msgpack::Binary(bytes.to_vec());
    }
    let mime = mime(content_type);
    if is_json(mime) {
        if bytes.is_empty() {
            return Msgpack::Nil;
        }
        if let Ok(value) = serde_json::from_slice(bytes) {
            return to_msgpack(&value);
        }
    } else if mime
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("text/"))
        && let Ok(text) = std::str::from_utf8(bytes)
    {
        return Msgpack::from(text);
    }
    Msgpack::Binary(bytes.to_vec())
}

/// These describe or validate the sender's original bytes. Passthrough can keep them only
/// when the transformed body is byte-for-byte identical.
pub(super) fn describes_original_body(name: &str) -> bool {
    [
        "content-encoding",
        "content-md5",
        "digest",
        "content-digest",
        "repr-digest",
        "etag",
    ]
    .iter()
    .any(|header| name.eq_ignore_ascii_case(header))
}

fn mime(content_type: Option<&str>) -> &str {
    content_type
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
}

fn is_json(mime: &str) -> bool {
    mime.eq_ignore_ascii_case("application/json")
        || mime
            .rsplit_once('+')
            .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("json"))
}

fn json_bytes(value: &Value, content_type: Option<&str>) -> Bytes {
    if let Value::String(text) = value
        && !is_json(mime(content_type))
    {
        return Bytes::copy_from_slice(text.as_bytes());
    }
    serde_json::to_vec(value).expect("JSON serializes").into()
}

fn encode(value: Msgpack, content_type: Option<&str>) -> Result<(Bytes, &'static str), String> {
    match value {
        Msgpack::Binary(bytes) => Ok((bytes.into(), "application/octet-stream")),
        Msgpack::String(text) if !is_json(mime(content_type)) => Ok((
            text.into_str().ok_or("invalid UTF-8 string")?.into(),
            "text/plain; charset=utf-8",
        )),
        value => Ok((
            json_bytes(&to_json(&value)?, content_type),
            "application/json",
        )),
    }
}

async fn output(
    pipeline: &Pipeline,
    response: &mut ForwardResponse,
    value: Msgpack,
    request: CallRequest<'_>,
) -> Result<&'static str, String> {
    let value = pipeline
        .run_http(Op::Output, value, request)
        .await
        .map_err(|error| error.to_string())?;
    let (bytes, content_type) = encode(value, response.header("content-type"))?;
    response.body = bytes;
    response.headers.retain(|(name, _)| {
        !name.eq_ignore_ascii_case("content-length")
            && !name.eq_ignore_ascii_case("transfer-encoding")
    });
    if response.header("content-type").is_none() {
        response
            .headers
            .push(("content-type".into(), content_type.into()));
    }
    Ok(content_type)
}

pub(super) async fn mock_output(
    pipeline: &Pipeline,
    mut plan: HttpResponsePlan,
    explicit_content_type: bool,
    request: CallRequest<'_>,
) -> Result<ForwardResponse, String> {
    let value = match plan.body.take() {
        None => Msgpack::Nil,
        Some(BodyPlan::Text(text)) => Msgpack::from(text),
        Some(BodyPlan::Json(value)) => to_msgpack(&value),
        Some(BodyPlan::Bytes(_)) => {
            unreachable!("named Pipeline retains the logical response body")
        }
    };
    if !explicit_content_type {
        plan.headers.remove("content-type");
    }
    let mut response = ForwardResponse::from(plan);
    output(pipeline, &mut response, value, request).await?;
    Ok(response)
}

#[derive(Debug, thiserror::Error)]
pub(super) enum PassthroughOutputError {
    #[error(transparent)]
    Upstream(#[from] ForwardingError),
    #[error("{0}")]
    Transform(String),
}

pub(super) async fn passthrough_output(
    pipeline: &Pipeline,
    mut response: ForwardResponse,
    limit: usize,
    request: CallRequest<'_>,
) -> Result<ForwardResponse, PassthroughOutputError> {
    let mut bytes = Vec::new();
    if let Some(mut stream) = response.stream.take() {
        while let Some(chunk) = stream.0.try_next().await.map_err(ForwardingError::from)? {
            if chunk.len() > limit - bytes.len() {
                return Err(PassthroughOutputError::Transform(format!(
                    "upstream body exceeds {limit} bytes (maxBodyBytes)"
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
    }
    let value = decode(
        response.header("content-type"),
        response.header("content-encoding"),
        &bytes,
    );
    let content_type = output(pipeline, &mut response, value, request)
        .await
        .map_err(PassthroughOutputError::Transform)?;
    if response.body.as_ref() != bytes {
        if response.status == 206 {
            return Err(PassthroughOutputError::Transform(
                "cannot change the body of an upstream partial response".into(),
            ));
        }
        response
            .headers
            .retain(|(name, _)| !describes_original_body(name));
        if content_type == "application/json" && !is_json(mime(response.header("content-type"))) {
            response
                .headers
                .retain(|(name, _)| !name.eq_ignore_ascii_case("content-type"));
            response
                .headers
                .push(("content-type".into(), content_type.into()));
        }
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_scalar_strings_round_trip_and_returned_bytes_are_verbatim() {
        let content_type = Some("application/vnd.orders+json; charset=utf-8");
        let scalar = decode(content_type, None, br#""abc""#);
        assert_eq!(scalar.as_str(), Some("abc"));
        assert_eq!(encode(scalar, content_type).unwrap().0, &br#""abc""#[..]);
        assert_eq!(encode("abc".into(), Some("text/plain")).unwrap().0, "abc");
        assert_eq!(
            encode(Msgpack::Binary(b"abc".to_vec()), content_type)
                .unwrap()
                .0,
            "abc"
        );
    }
}
