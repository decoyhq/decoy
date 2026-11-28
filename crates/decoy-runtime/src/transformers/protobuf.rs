use prost::Message as _;
use prost_reflect::{DynamicMessage, MessageDescriptor, SerializeOptions};
use serde_json::Value;

use super::TransformError;

pub(crate) fn input(bytes: &[u8], descriptor: &MessageDescriptor) -> Result<Value, TransformError> {
    let message = DynamicMessage::decode(descriptor.clone(), bytes).map_err(|error| {
        TransformError::InvalidMessage {
            message: descriptor.full_name().to_owned(),
            error,
        }
    })?;
    message_json(&message).map_err(TransformError::ToJson)
}

pub(crate) fn output(
    value: Value,
    descriptor: &MessageDescriptor,
) -> Result<Vec<u8>, TransformError> {
    let message = DynamicMessage::deserialize(descriptor.clone(), value).map_err(|error| {
        TransformError::DoesNotFit {
            message: descriptor.full_name().to_owned(),
            error,
        }
    })?;
    Ok(message.encode_to_vec())
}

/// Render Protobuf JSON with default fields included, so a Case can match a zero value.
pub fn message_json(message: &DynamicMessage) -> Result<Value, serde_json::Error> {
    // Through text, not `value::Serializer`: that widens f32 (`0.1` -> 0.10000000149011612).
    let mut text = Vec::new();
    let options = SerializeOptions::new().skip_default_fields(false);
    message.serialize_with_options(&mut serde_json::Serializer::new(&mut text), &options)?;
    serde_json::from_slice(&text)
}
