use crate::http::RuntimeConfig;
use crate::pipeline::{Pipeline, PipelineError};
use crate::workers::{
    Op,
    value::{to_json, to_msgpack},
};
use base64::Engine as _;
use prost_reflect::MessageDescriptor;
use rmpv::Value as Msgpack;
use serde_json::Value;

use super::DEFAULT_PIPELINE;
use crate::schema::{PipelineStep, Route};
use crate::transformers::{TransformError, grpc_frame, protobuf};

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

/// A named Pipeline gets the full gRPC frame. Each direction runs in authored order.
pub(super) async fn run_named_input(
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
                    grpc_frame::input(&bytes).map(|data| Msgpack::Binary(data.to_vec()))
                }
                PipelineStep::Protobuf => {
                    protobuf::input(&bytes, descriptor).map(|json| to_msgpack(&json))
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

/// Runs the output Pipeline for a unary reply, stream item, or Control API push.
pub async fn run_named_output(
    route: &Route,
    runtime: &RuntimeConfig,
    message: Value,
    descriptor: &MessageDescriptor,
) -> Result<Vec<u8>, TransformError> {
    let named = Pipeline::of(route, runtime).map_err(TransformError::Transform)?;
    let Some(named) = named.filter(Pipeline::has_output) else {
        return run_output(message, pipeline(route), descriptor);
    };
    let value = named
        .run_grpc(Op::Output, to_msgpack(&message), |step, value| {
            run_output_step(step, value, descriptor).map(Msgpack::Binary)
        })
        .await
        .map_err(|error| TransformError::Transform(error.to_string()))?;
    match value {
        Msgpack::Binary(bytes) => Ok(bytes),
        _ => Err(TransformError::Transform(
            "gRPC output Pipeline must return bytes".into(),
        )),
    }
}

fn run_output_step(
    step: PipelineStep,
    value: Msgpack,
    descriptor: &MessageDescriptor,
) -> Result<Vec<u8>, String> {
    match (step, value) {
        (PipelineStep::GrpcFrame, Msgpack::Binary(bytes)) => Ok(grpc_frame::output(&bytes)),
        (PipelineStep::GrpcFrame, _) => Err("gRPC output step `grpc-frame` requires bytes".into()),
        (PipelineStep::Protobuf, value) => {
            let json = match value {
                Msgpack::Binary(bytes) => {
                    serde_json::from_slice(&bytes).map_err(|error| error.to_string())?
                }
                value => to_json(&value)?,
            };
            protobuf::output(json, descriptor).map_err(|error| error.to_string())
        }
    }
}

/// Bytes as they flow through a Pipeline: steps turn one into the other.
enum Data {
    Bytes(Vec<u8>),
    Json(Value),
}

/// Run the input direction over the request body. Bytes left at the end become a base64 string.
pub(super) fn run_input(
    body: &[u8],
    steps: &[PipelineStep],
    input: &MessageDescriptor,
) -> Result<Value, TransformError> {
    // Borrowed throughout: the body is only read, and `protobuf` is the last step.
    let mut bytes = body;
    for (index, step) in steps.iter().enumerate() {
        match step {
            PipelineStep::GrpcFrame => bytes = grpc_frame::input(bytes)?,
            PipelineStep::Protobuf => {
                // `validate_pipeline` keeps `protobuf` last and each step once.
                assert_eq!(index, steps.len() - 1, "no step follows `protobuf`");
                return protobuf::input(bytes, input);
            }
        }
    }
    Ok(Value::String(base64_engine().encode(bytes)))
}

/// Run the output direction in reverse over the response message. Without a `protobuf` step the
/// message must be a base64 string.
pub(super) fn run_output(
    message: Value,
    steps: &[PipelineStep],
    output: &MessageDescriptor,
) -> Result<Vec<u8>, TransformError> {
    let mut data = Data::Json(message);
    for step in steps.iter().rev() {
        data = match (step, data) {
            (PipelineStep::Protobuf, Data::Json(value)) => {
                Data::Bytes(protobuf::output(value, output)?)
            }
            (PipelineStep::GrpcFrame, data) => Data::Bytes(grpc_frame::output(&bytes(data)?)),
            (PipelineStep::Protobuf, Data::Bytes(_)) => unreachable!("no step follows `protobuf`"),
        };
    }
    bytes(data)
}

fn bytes(data: Data) -> Result<Vec<u8>, TransformError> {
    match data {
        Data::Bytes(bytes) => Ok(bytes),
        Data::Json(Value::String(text)) => base64_engine()
            .decode(text)
            .map_err(TransformError::NotBase64),
        Data::Json(other) => Err(TransformError::NotAString(other)),
    }
}
