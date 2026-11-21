use crate::http::RuntimeConfig;
use crate::pipeline::{Pipeline, PipelineError};
use crate::plugins::{
    Op,
    value::{to_json, to_msgpack},
};
use base64::Engine as _;
use prost::Message as _;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use rmpv::Value as Msgpack;
use serde_json::Value;
use thiserror::Error;

use super::DEFAULT_PIPELINE;
use super::schema::request_json;
use crate::schema::{PipelineStep, Route};

fn base64_engine() -> &'static impl base64::Engine {
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_feature = "neon")
    ))]
    {
        use base64::engine::{Simd, general_purpose::PAD};
        use std::sync::LazyLock;
        static ENGINE: LazyLock<Simd> = LazyLock::new(|| Simd::standard(PAD));
        &*ENGINE
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_feature = "neon")
    )))]
    {
        &base64::engine::general_purpose::STANDARD
    }
}

pub(super) fn pipeline(route: &Route) -> &[PipelineStep] {
    route.pipeline.as_deref().unwrap_or(&DEFAULT_PIPELINE)
}

/// Why a gRPC message didn't decode or encode. The text reaches grpc-message and miss reasons.
#[derive(Debug, Error)]
pub enum GrpcCodecError {
    #[error("{0}")]
    Transform(String),
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

/// A named Pipeline gets the full gRPC frame. Each direction runs in authored order.
pub(super) async fn decode_named(
    body: &[u8],
    pipeline: &Pipeline,
    descriptor: &MessageDescriptor,
) -> Result<Value, PipelineError> {
    let value = pipeline
        .run_grpc(Op::Input, Msgpack::Binary(body.to_vec()), |step, value| {
            let Msgpack::Binary(bytes) = value else {
                return Err(format!("gRPC input step `{step}` requires bytes"));
            };
            match step {
                PipelineStep::GrpcFrame => {
                    unframe(&bytes).map(|data| Msgpack::Binary(data.to_vec()))
                }
                PipelineStep::Protobuf => {
                    decode(&bytes, &[step], descriptor).map(|json| to_msgpack(&json))
                }
            }
            .map_err(|error| error.to_string())
        })
        .await?;
    match value {
        Msgpack::Binary(bytes) if pipeline.input_ends_in_command() => {
            serde_json::from_slice(&bytes)
                .map_err(|error| format!("input command did not return JSON: {error}"))
        }
        Msgpack::Binary(bytes) => Ok(Value::String(base64_engine().encode(bytes))),
        value => to_json(&value),
    }
    .map_err(|reason| pipeline.invalid_result(Op::Input, reason))
}

pub(crate) async fn encode_for_route(
    route: &Route,
    runtime: &RuntimeConfig,
    message: Value,
    descriptor: &MessageDescriptor,
) -> Result<Vec<u8>, GrpcCodecError> {
    let Some(named) = Pipeline::of(route, runtime).map_err(GrpcCodecError::Transform)? else {
        return encode(message, pipeline(route), descriptor);
    };
    let value = named
        .run_grpc(Op::Output, to_msgpack(&message), |step, value| {
            let bytes = match step {
                PipelineStep::GrpcFrame => {
                    let Msgpack::Binary(bytes) = value else {
                        return Err("gRPC output step `grpc-frame` requires bytes".into());
                    };
                    frame(&bytes)
                }
                PipelineStep::Protobuf => {
                    let json = match value {
                        Msgpack::Binary(bytes) => {
                            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?
                        }
                        value => to_json(&value)?,
                    };
                    encode(json, &[step], descriptor).map_err(|error| error.to_string())?
                }
            };
            Ok(Msgpack::Binary(bytes))
        })
        .await
        .map_err(|error| GrpcCodecError::Transform(error.to_string()))?;
    match value {
        Msgpack::Binary(bytes) => Ok(bytes),
        _ => Err(GrpcCodecError::Transform(
            "gRPC output Pipeline must return bytes".into(),
        )),
    }
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
    // Borrowed throughout: the body is only read, and `protobuf` is the last step.
    let mut bytes = body;
    for (index, step) in steps.iter().enumerate() {
        match step {
            PipelineStep::GrpcFrame => bytes = unframe(bytes)?,
            PipelineStep::Protobuf => {
                // `validate_pipeline` keeps `protobuf` last and each step once.
                assert_eq!(index, steps.len() - 1, "no step follows `protobuf`");
                let message = DynamicMessage::decode(input.clone(), bytes).map_err(|error| {
                    GrpcCodecError::InvalidMessage {
                        message: input.full_name().to_owned(),
                        error,
                    }
                })?;
                return request_json(&message).map_err(GrpcCodecError::ToJson);
            }
        }
    }
    Ok(Value::String(base64_engine().encode(bytes)))
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
        Data::Json(Value::String(text)) => base64_engine()
            .decode(text)
            .map_err(GrpcCodecError::NotBase64),
        Data::Json(other) => Err(GrpcCodecError::NotAString(other)),
    }
}

/// One uncompressed length-prefixed message.
pub(super) fn unframe(bytes: &[u8]) -> Result<&[u8], GrpcCodecError> {
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
    Ok(rest)
}

pub(super) fn frame(message: &[u8]) -> Vec<u8> {
    let length = u32::try_from(message.len()).unwrap_or(u32::MAX);
    let mut framed = Vec::with_capacity(message.len() + 5);
    framed.push(0);
    framed.extend_from_slice(&length.to_be_bytes());
    framed.extend_from_slice(message);
    framed
}
