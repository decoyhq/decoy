//! `protobuf.*` host calls: workers use the protos Decoy compiled instead of loading their own.

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor};
use rmpv::Value as Msgpack;

use super::host::Host;
use super::value::{to_json, to_msgpack};

#[derive(Debug, Default)]
pub(super) struct ProtoHost {
    pool: RwLock<DescriptorPool>,
    /// Startup `check`: `encode` only validates, skipping `${ }` leaves.
    checking: AtomicBool,
}

impl ProtoHost {
    pub(super) fn set_pool(&self, pool: DescriptorPool) {
        *self.pool.write().expect("protos lock") = pool;
    }

    pub(super) fn set_checking(&self, checking: bool) {
        self.checking.store(checking, Ordering::Relaxed);
    }

    fn message(&self, args: &[Msgpack]) -> Result<MessageDescriptor, String> {
        let Some(name) = args.first().and_then(Msgpack::as_str) else {
            return Err("the first argument must be a message type name".to_owned());
        };
        self.pool
            .read()
            .expect("protos lock")
            .get_message_by_name(name)
            .ok_or_else(|| format!("`{name}` is not a message in the loaded protos"))
    }
}

impl Host for ProtoHost {
    fn call(&self, op: &str, args: &[Msgpack]) -> Result<Msgpack, String> {
        match op {
            "protobuf.has" => Ok(Msgpack::Boolean(self.message(args).is_ok())),
            "protobuf.decode" => {
                let message = self.message(args)?;
                let Some(Msgpack::Binary(bytes)) = args.get(1) else {
                    return Err("`protobuf.decode` takes bytes".to_owned());
                };
                let decoded = DynamicMessage::decode(message.clone(), bytes.as_slice())
                    .map_err(|error| format!("not a valid {}: {error}", message.full_name()))?;
                let json = crate::grpc::schema::request_json(&decoded)
                    .map_err(|error| error.to_string())?;
                Ok(to_msgpack(&json))
            }
            "protobuf.encode" => {
                let message = self.message(args)?;
                let value = to_json(args.get(1).unwrap_or(&Msgpack::Nil))?;
                if self.checking.load(Ordering::Relaxed) {
                    // The worker knows where in its message this value sits; paths start here.
                    crate::grpc::schema::check_response_message(&value, &message, "")
                        .map_err(|error| error.replace("`.", "`"))?;
                    return Ok(Msgpack::Binary(Vec::new()));
                }
                let encoded = DynamicMessage::deserialize(message.clone(), value)
                    .map_err(|error| format!("does not fit {}: {error}", message.full_name()))?;
                Ok(Msgpack::Binary(encoded.encode_to_vec()))
            }
            other => Err(format!("unknown host call `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> ProtoHost {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("users.proto"),
            "syntax = \"proto3\";\npackage users.v1;\nmessage User { string name = 1; int32 age = 2; }\n",
        )
        .unwrap();
        let pool = crate::protos::load_descriptor_pool(&crate::config::ProtoSources {
            roots: vec![dir.path().to_path_buf()],
            ..Default::default()
        })
        .unwrap();
        let host = ProtoHost::default();
        host.set_pool(pool);
        host
    }

    fn map(fields: &[(&str, Msgpack)]) -> Msgpack {
        Msgpack::Map(
            fields
                .iter()
                .map(|(key, value)| (Msgpack::from(*key), value.clone()))
                .collect(),
        )
    }

    #[test]
    fn encodes_and_decodes_with_the_loaded_protos() {
        let host = host();
        let user = map(&[("name", "Ada".into()), ("age", 36.into())]);
        let bytes = host
            .call("protobuf.encode", &["users.v1.User".into(), user])
            .unwrap();
        let decoded = host
            .call("protobuf.decode", &["users.v1.User".into(), bytes])
            .unwrap();
        assert_eq!(
            to_json(&decoded).unwrap(),
            serde_json::json!({ "name": "Ada", "age": 36 })
        );
        assert_eq!(
            host.call("protobuf.has", &["users.v1.User".into()]),
            Ok(true.into())
        );
        assert_eq!(
            host.call("protobuf.has", &["users.v1.Nobody".into()]),
            Ok(false.into())
        );
    }

    #[test]
    fn unknown_fields_fail_and_checking_skips_templates() {
        let host = host();
        let typo = map(&[("nmae", "Ada".into())]);
        let error = host
            .call("protobuf.encode", &["users.v1.User".into(), typo.clone()])
            .unwrap_err();
        assert!(error.contains("nmae"), "{error}");

        host.set_checking(true);
        let templated = map(&[
            ("name", "${ message.name }".into()),
            ("age", "${ x }".into()),
        ]);
        assert_eq!(
            host.call("protobuf.encode", &["users.v1.User".into(), templated]),
            Ok(Msgpack::Binary(Vec::new()))
        );
        let error = host
            .call("protobuf.encode", &["users.v1.User".into(), typo])
            .unwrap_err();
        assert_eq!(error, "unknown field `nmae` in users.v1.User");
    }
}
