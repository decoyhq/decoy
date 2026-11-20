//! Одноразовая команда для измерения нативного transformer.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, Read, Write};
use std::path::Path;

use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage};
use rmpv::Value;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Читает одно сообщение из stdin и передаёт результат в stdout.
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [direction, map_path, proto_dir, output_topic] = args.as_slice() else {
        return Err("expected direction, schema map, proto directory and output topic".into());
    };
    let schemas: BTreeMap<String, String> = serde_json::from_slice(&std::fs::read(map_path)?)?;
    let dir = Path::new(proto_dir);
    let descriptors = protox::compile(
        [
            dir.join("approvals/v1/approvals.proto"),
            dir.join("users/v1/users.proto"),
        ],
        [dir],
    )?;
    let pool = DescriptorPool::from_file_descriptor_set(descriptors)?;
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    let output = if direction == "inbound" {
        let (topic, payload) = unpack(&input)?;
        let descriptor = message_type(&pool, &schemas, &topic)?;
        serde_json::to_vec(&DynamicMessage::decode(descriptor, payload.as_slice())?)?
    } else if direction == "outbound" {
        let descriptor = message_type(&pool, &schemas, output_topic)?;
        let mut json = serde_json::Deserializer::from_slice(&input);
        let payload = DynamicMessage::deserialize(descriptor, &mut json)?.encode_to_vec();
        pack(output_topic, &payload)?
    } else {
        return Err(format!("unknown direction: {direction}").into());
    };
    io::stdout().write_all(&output)?;
    Ok(())
}

/// Выбирает protobuf-дескриптор по теме протокола.
fn message_type(
    pool: &DescriptorPool,
    schemas: &BTreeMap<String, String>,
    topic: &str,
) -> Result<prost_reflect::MessageDescriptor> {
    let name = schemas
        .get(topic)
        .ok_or_else(|| format!("unknown topic: {topic}"))?;
    pool.get_message_by_name(name)
        .ok_or_else(|| format!("unknown schema: {name}").into())
}

/// Извлекает тему и protobuf из MessagePack и снимает заголовок gRPC.
fn unpack(input: &[u8]) -> Result<(String, Vec<u8>)> {
    let envelope = rmpv::decode::read_value(&mut io::Cursor::new(input))?;
    let topic = envelope["meta"]["topic"]
        .as_str()
        .ok_or("expected meta.topic")?;
    let Value::Binary(frame) = &envelope["payload"] else {
        return Err("expected binary payload".into());
    };
    let [0, a, b, c, d, body @ ..] = frame.as_slice() else {
        return Err("expected an uncompressed gRPC message".into());
    };
    if u32::from_be_bytes([*a, *b, *c, *d]) as usize != body.len() {
        return Err("gRPC message length does not match payload".into());
    }
    Ok((topic.to_owned(), body.to_vec()))
}

/// Добавляет заголовок gRPC и упаковывает protobuf вместе с темой.
fn pack(topic: &str, payload: &[u8]) -> Result<Vec<u8>> {
    let mut frame = vec![0];
    frame.extend(u32::try_from(payload.len())?.to_be_bytes());
    frame.extend(payload);
    let envelope = Value::Map(vec![
        (
            "meta".into(),
            Value::Map(vec![("topic".into(), topic.into())]),
        ),
        ("payload".into(), Value::Binary(frame)),
    ]);
    let mut bytes = Vec::new();
    rmpv::encode::write_value(&mut bytes, &envelope)?;
    Ok(bytes)
}
