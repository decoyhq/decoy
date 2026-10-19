use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use prost::Message as _;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use serde_json::Value;

use super::DEFAULT_PIPELINE;
use super::schema::request_json_options;
use crate::schema::{PipelineStep, Route};

pub(super) fn pipeline(route: &Route) -> &[PipelineStep] {
    route.pipeline.as_deref().unwrap_or(&DEFAULT_PIPELINE)
}

/// Bytes as they flow through a Pipeline: steps turn one into the other.
enum Data {
    Bytes(Vec<u8>),
    Json(Value),
}

/// Run the Pipeline top to bottom over the request body. Bytes left at the end become a base64
/// string.
pub(super) fn decode(
    body: &[u8],
    steps: &[PipelineStep],
    input: &MessageDescriptor,
) -> Result<Value, String> {
    let mut data = Data::Bytes(body.to_vec());
    for step in steps {
        data = match (step, data) {
            (PipelineStep::GrpcFrame, Data::Bytes(bytes)) => Data::Bytes(unframe(&bytes)?),
            (PipelineStep::Protobuf, Data::Bytes(bytes)) => {
                let message = DynamicMessage::decode(input.clone(), bytes.as_slice())
                    .map_err(|error| format!("not a valid {}: {error}", input.full_name()))?;
                let value = message
                    .serialize_with_options(serde_json::value::Serializer, &request_json_options())
                    .map_err(|error| error.to_string())?;
                Data::Json(value)
            }
            (step, Data::Json(_)) => return Err(format!("step `{step:?}` needs bytes, got JSON")),
        };
    }
    Ok(match data {
        Data::Json(value) => value,
        Data::Bytes(bytes) => Value::String(BASE64.encode(bytes)),
    })
}

/// Run the Pipeline bottom to top over the response message. Without a `protobuf` step the
/// message must be a base64 string.
pub(super) fn encode(
    message: Value,
    steps: &[PipelineStep],
    output: &MessageDescriptor,
) -> Result<Vec<u8>, String> {
    let mut data = Data::Json(message);
    for step in steps.iter().rev() {
        data = match (step, data) {
            (PipelineStep::Protobuf, Data::Json(value)) => {
                let message =
                    DynamicMessage::deserialize(output.clone(), value).map_err(|error| {
                        format!("response does not fit {}: {error}", output.full_name())
                    })?;
                Data::Bytes(message.encode_to_vec())
            }
            (PipelineStep::GrpcFrame, data) => Data::Bytes(frame(&bytes(data)?)),
            (PipelineStep::Protobuf, Data::Bytes(_)) => {
                return Err("step `Protobuf` needs JSON, got bytes".to_owned());
            }
        };
    }
    bytes(data)
}

fn bytes(data: Data) -> Result<Vec<u8>, String> {
    match data {
        Data::Bytes(bytes) => Ok(bytes),
        Data::Json(Value::String(text)) => BASE64
            .decode(text)
            .map_err(|error| format!("message is not base64: {error}")),
        Data::Json(other) => Err(format!("expected a base64 string, got {other}")),
    }
}

/// One uncompressed length-prefixed message.
pub(super) fn unframe(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let [compressed, a, b, c, d, rest @ ..] = bytes else {
        return Err("gRPC frame shorter than its 5-byte header".to_owned());
    };
    if *compressed != 0 {
        return Err("compressed gRPC messages are not supported".to_owned());
    }
    let length = u32::from_be_bytes([*a, *b, *c, *d]) as usize;
    if rest.len() != length {
        return Err(format!(
            "gRPC frame declares {length} bytes but carries {}; unary calls take one message",
            rest.len()
        ));
    }
    Ok(rest.to_vec())
}

pub(super) fn frame(message: &[u8]) -> Vec<u8> {
    let length = u32::try_from(message.len()).unwrap_or(u32::MAX);
    let mut framed = Vec::with_capacity(message.len() + 5);
    framed.push(0);
    framed.extend_from_slice(&length.to_be_bytes());
    framed.extend_from_slice(message);
    framed
}
