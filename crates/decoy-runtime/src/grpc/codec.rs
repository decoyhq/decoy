use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use prost::Message as _;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use serde_json::Value;
use thiserror::Error;

use super::DEFAULT_PIPELINE;
use super::schema::request_json;
use crate::schema::{PipelineStep, Route};

pub(super) fn pipeline(route: &Route) -> &[PipelineStep] {
    route.pipeline.as_deref().unwrap_or(&DEFAULT_PIPELINE)
}

/// Why a gRPC message didn't decode or encode. The text reaches grpc-message and miss reasons.
#[derive(Debug, Error)]
pub enum GrpcCodecError {
    #[error("gRPC frame shorter than its 5-byte header")]
    ShortFrame,
    #[error("compressed gRPC messages are not supported")]
    Compressed,
    #[error(
        "gRPC frame declares {declared} bytes but carries {carried}; unary calls take one message"
    )]
    FrameLength { declared: usize, carried: usize },
    #[error("not a valid {message}: {error}")]
    InvalidMessage {
        message: String,
        error: prost::DecodeError,
    },
    #[error("{0}")]
    ToJson(serde_json::Error),
    #[error("response does not fit {message}: {error}")]
    DoesNotFit {
        message: String,
        error: serde_json::Error,
    },
    #[error("message is not base64: {0}")]
    NotBase64(base64::DecodeError),
    #[error("expected a base64 string, got {0}")]
    NotAString(Value),
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
) -> Result<Value, GrpcCodecError> {
    let mut data = Data::Bytes(body.to_vec());
    for step in steps {
        data = match (step, data) {
            (PipelineStep::GrpcFrame, Data::Bytes(bytes)) => Data::Bytes(unframe(&bytes)?),
            (PipelineStep::Protobuf, Data::Bytes(bytes)) => {
                let message =
                    DynamicMessage::decode(input.clone(), bytes.as_slice()).map_err(|error| {
                        GrpcCodecError::InvalidMessage {
                            message: input.full_name().to_owned(),
                            error,
                        }
                    })?;
                Data::Json(request_json(&message).map_err(GrpcCodecError::ToJson)?)
            }
            // `validate_pipeline` keeps `protobuf` last and each step once.
            (_, Data::Json(_)) => unreachable!("no step follows `protobuf`"),
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
) -> Result<Vec<u8>, GrpcCodecError> {
    let mut data = Data::Json(message);
    for step in steps.iter().rev() {
        data = match (step, data) {
            (PipelineStep::Protobuf, Data::Json(value)) => {
                let message =
                    DynamicMessage::deserialize(output.clone(), value).map_err(|error| {
                        GrpcCodecError::DoesNotFit {
                            message: output.full_name().to_owned(),
                            error,
                        }
                    })?;
                Data::Bytes(message.encode_to_vec())
            }
            (PipelineStep::GrpcFrame, data) => Data::Bytes(frame(&bytes(data)?)),
            (PipelineStep::Protobuf, Data::Bytes(_)) => unreachable!("no step follows `protobuf`"),
        };
    }
    bytes(data)
}

fn bytes(data: Data) -> Result<Vec<u8>, GrpcCodecError> {
    match data {
        Data::Bytes(bytes) => Ok(bytes),
        Data::Json(Value::String(text)) => BASE64.decode(text).map_err(GrpcCodecError::NotBase64),
        Data::Json(other) => Err(GrpcCodecError::NotAString(other)),
    }
}

/// One uncompressed length-prefixed message.
pub(super) fn unframe(bytes: &[u8]) -> Result<Vec<u8>, GrpcCodecError> {
    let [compressed, a, b, c, d, rest @ ..] = bytes else {
        return Err(GrpcCodecError::ShortFrame);
    };
    if *compressed != 0 {
        return Err(GrpcCodecError::Compressed);
    }
    let length = u32::from_be_bytes([*a, *b, *c, *d]) as usize;
    if rest.len() != length {
        return Err(GrpcCodecError::FrameLength {
            declared: length,
            carried: rest.len(),
        });
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
