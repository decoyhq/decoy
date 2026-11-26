//! Built-in Transformer operations. Transports provide the Pipeline and any type information.

pub(crate) mod grpc_frame;
pub(crate) mod protobuf;

use serde_json::Value;
use thiserror::Error;

/// Why a gRPC Pipeline could not transform a message. The text reaches grpc-message and miss reasons.
#[derive(Debug, Error)]
pub enum TransformError {
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
