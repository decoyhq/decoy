//! gRPC Route checks. Shape checks run with the rest of Route validation; proto checks run at
//! startup against the loaded `DescriptorPool` (design note `docs/design/transports.md` §4, Q3):
//! the service and method exist, and literal `match.message` / response `message` and
//! `stream[].message` values fit the method's input / output types as protobuf JSON. `${ }` strings
//! are skipped (checked at render).
//!
//! Response literals may use any form protobuf JSON accepts. Match literals are compared with the
//! decoded request as [`request_json`] renders it, so they must use that canonical form:
//! camelCase field names, 64-bit integers as strings, enums as names.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use prost_reflect::{
    DescriptorPool, DynamicMessage, FieldDescriptor, Kind, MessageDescriptor, MethodDescriptor,
    SerializeOptions,
};
use serde_json::Value;

use crate::matching::value_matches;
use crate::schema::{Behavior, PipelineStep, ResponseBehavior, Route, RouteMatch};
use crate::template::{validate_string, validate_value};

/// gRPC status codes by name, indexed by numeric code.
pub const STATUS_CODES: [&str; 17] = [
    "OK",
    "CANCELLED",
    "UNKNOWN",
    "INVALID_ARGUMENT",
    "DEADLINE_EXCEEDED",
    "NOT_FOUND",
    "ALREADY_EXISTS",
    "PERMISSION_DENIED",
    "RESOURCE_EXHAUSTED",
    "FAILED_PRECONDITION",
    "ABORTED",
    "OUT_OF_RANGE",
    "UNIMPLEMENTED",
    "INTERNAL",
    "UNAVAILABLE",
    "DATA_LOSS",
    "UNAUTHENTICATED",
];

/// How a decoded request message becomes the envelope's `message`: protobuf JSON with default
/// fields included, so a match on a zero value (`priority: 0`, `state: STATE_UNSPECIFIED`) works.
pub fn request_json(message: &DynamicMessage) -> Result<Value, serde_json::Error> {
    // Through text, not `value::Serializer`: that widens f32 (`0.1` -> 0.10000000149011612).
    let mut text = Vec::new();
    let options = SerializeOptions::new().skip_default_fields(false);
    message.serialize_with_options(&mut serde_json::Serializer::new(&mut text), &options)?;
    serde_json::from_slice(&text)
}

/// A Route's Pipeline: steps run in order, at most once each. gRPC bodies are always framed, so
/// `grpc-frame` comes first. With only the two built-ins, that leaves `protobuf` last.
pub fn validate_pipeline(steps: &[PipelineStep]) -> Result<(), String> {
    if steps.first() != Some(&PipelineStep::GrpcFrame) {
        return Err("gRPC calls are framed, so `grpc-frame` must be the first step".to_owned());
    }
    for (index, step) in steps.iter().enumerate() {
        if steps[..index].contains(step) {
            return Err(format!("step `{}` appears twice", step_name(*step)));
        }
    }
    Ok(())
}

fn step_name(step: PipelineStep) -> &'static str {
    match step {
        PipelineStep::GrpcFrame => "grpc-frame",
        PipelineStep::Protobuf => "protobuf",
    }
}

/// Shape of a gRPC response Behavior: `message` (success), or `status` plus an optional `message`
/// string (error); `metadata` for trailers. No HTTP fields. Without a `protobuf` step (`decoded`
/// false), a success `message` is base64 bytes instead of a mapping.
pub fn validate_response(response: &ResponseBehavior, decoded: bool) -> Result<(), String> {
    if response.body.is_some() || !response.headers.is_empty() {
        return Err("gRPC Behaviors use `message` and `metadata`, not `body` and `headers`".into());
    }
    if response.messages.is_some() || response.close.is_some() {
        return Err(
            "`messages` and `close` are for WebSocket Routes; gRPC streams use `stream`".into(),
        );
    }
    match &response.status {
        None => {}
        Some(Value::String(text)) if text.contains("${") => validate_string(text)?,
        Some(Value::String(name)) if STATUS_CODES.contains(&name.as_str()) => {}
        Some(other) => {
            return Err(format!(
                "status must be a gRPC status name such as `NOT_FOUND` or a `${{ }}` template, got {other}"
            ));
        }
    }
    for name in response.metadata.keys() {
        let valid = !name.is_empty()
            && name
                .bytes()
                .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.'));
        if !valid {
            return Err(format!(
                "metadata name `{name}` must be lowercase letters, digits, `-`, `_`, or `.`"
            ));
        }
        if name.starts_with("grpc-") {
            return Err(format!("metadata name `{name}` is reserved by gRPC"));
        }
    }
    if !decoded {
        match (&response.message, status_kind(response)) {
            (Some(Value::String(text)), StatusKind::Ok) if !text.contains("${") => {
                BASE64
                    .decode(text)
                    .map_err(|error| format!("`message` must be base64 bytes: {error}"))?;
            }
            (Some(Value::Object(_)), StatusKind::Ok) => {
                return Err(
                    "without a `protobuf` step, `message` is base64 bytes, not a mapping".into(),
                );
            }
            _ => {}
        }
    }
    match (&response.message, status_kind(response)) {
        (None, _)
        | (Some(Value::String(_)), StatusKind::Error | StatusKind::Templated)
        | (Some(Value::Object(_)), StatusKind::Ok | StatusKind::Templated) => {}
        (Some(Value::String(text)), StatusKind::Ok) if !decoded || text.contains("${") => {}
        (Some(_), StatusKind::Error) => {
            return Err("an error status takes a string `message`".into());
        }
        (Some(_), _) => return Err("`message` must be a mapping of response fields".into()),
    }
    for (index, item) in response.stream.iter().flatten().enumerate() {
        let path = format!("stream[{index}].message");
        match &item.message {
            Value::String(text) if text.contains("${") => {}
            Value::String(text) if !decoded => {
                BASE64
                    .decode(text)
                    .map_err(|error| format!("`{path}` must be base64 bytes: {error}"))?;
            }
            Value::Object(_) if decoded => {}
            _ if decoded => return Err(format!("`{path}` must be a mapping of response fields")),
            _ => return Err(format!("`{path}` is base64 bytes, not a mapping")),
        }
        validate_value(&item.message)?;
    }
    for (name, value) in &response.metadata {
        // A templated value is checked once rendered, when the request comes in.
        if !value.contains("${") && !valid_metadata_value(value) {
            return Err(format!("metadata `{name}` has a value gRPC doesn't allow"));
        }
        validate_string(value)?;
    }
    response.message.iter().try_for_each(validate_value)
}

/// gRPC ASCII metadata: printable `0x20..=0x7E` only, so no tab, which HTTP headers would allow.
pub(crate) fn valid_metadata_value(value: &str) -> bool {
    value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    /// No `status`, or `OK`: `message` is the response message.
    Ok,
    /// Any other status name: `message` is the status message.
    Error,
    /// Known only at render time; a mapping `message` is a response, a string one a status message.
    Templated,
}

fn status_kind(response: &ResponseBehavior) -> StatusKind {
    match response.status.as_ref().and_then(Value::as_str) {
        None | Some("OK") => StatusKind::Ok,
        Some(text) if text.contains("${") => StatusKind::Templated,
        Some(_) => StatusKind::Error,
    }
}

/// Check a gRPC Route against the loaded protos. Errors name the Route, Case, or Behavior and the
/// field path.
pub fn check_route(route: &Route, pool: &DescriptorPool) -> Result<(), String> {
    let RouteMatch::Grpc(address) = &route.route_match else {
        return Ok(());
    };
    if route.passthrough.is_some() {
        return Err(format!(
            "route `{}`: gRPC Routes do not support passthrough",
            route.id
        ));
    }
    let method = find_method(pool, &address.service, &address.method)
        .map_err(|reason| format!("route `{}`: {reason}", route.id))?;
    if method.is_client_streaming() {
        return Err(format!(
            "route `{}`: streaming method `{}` is not supported yet",
            route.id,
            method.full_name()
        ));
    }

    // Literals are protobuf JSON only where a `protobuf` step runs (Q3).
    let decoded = route.decodes_protobuf();

    for (case_id, case) in &route.cases {
        if decoded
            && let Some(pattern) = case.case_match.get("message")
            && !is_template(pattern)
        {
            check_message(pattern, &method.input(), "message", Form::Canonical)
                .map_err(|reason| format!("case `{}:{case_id}` match: {reason}", route.id))?;
        }
        for (behavior_id, behavior) in &case.behaviors {
            let Behavior::Response(response) = behavior else {
                continue;
            };
            check_behavior(response, &method, decoded).map_err(|reason| {
                format!("behavior `{}:{case_id}:{behavior_id}`: {reason}", route.id)
            })?;
        }
    }
    Ok(())
}

fn check_behavior(
    response: &ResponseBehavior,
    method: &MethodDescriptor,
    decoded: bool,
) -> Result<(), String> {
    if method.is_server_streaming() {
        streamed_shape(response)?;
    } else if response.stream.is_some() {
        return Err(format!(
            "`stream` needs a server-streaming method; `{}` is unary",
            method.full_name()
        ));
    }
    if !decoded {
        return Ok(());
    }
    if let Some(message @ Value::Object(_)) = &response.message {
        check_response_message(message, &method.output(), "message")?;
    }
    for (index, item) in response.stream.iter().flatten().enumerate() {
        if item.message.is_object() {
            let path = format!("stream[{index}].message");
            check_response_message(&item.message, &method.output(), &path)?;
        }
    }
    Ok(())
}

/// A server-streaming answer sends its messages in `stream`; `message` and `metadata` belong to
/// the `status` that ends the call.
fn streamed_shape(response: &ResponseBehavior) -> Result<(), String> {
    match (&response.message, status_kind(response)) {
        (None, _) | (Some(Value::String(_)), StatusKind::Error | StatusKind::Templated) => {}
        _ => {
            return Err(
                "server-streaming messages go in `stream`; `message` is only the text of an error `status`"
                    .into(),
            );
        }
    }
    if response.status.is_none() && !response.metadata.is_empty() {
        return Err(
            "`metadata` is sent with `status`; without one the stream stays open and never sends it"
                .into(),
        );
    }
    Ok(())
}

pub fn find_method(
    pool: &DescriptorPool,
    service: &str,
    method: &str,
) -> Result<MethodDescriptor, String> {
    let service_descriptor = pool
        .get_service_by_name(service)
        .ok_or_else(|| format!("unknown gRPC service `{service}` (not in the loaded protos)"))?;
    service_descriptor
        .methods()
        .find(|candidate| candidate.name() == method)
        .ok_or_else(|| format!("unknown method `{method}` on gRPC service `{service}`"))
}

fn is_template(value: &Value) -> bool {
    value.as_str().is_some_and(|text| text.contains("${"))
}

fn has_template(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains("${"),
        Value::Array(items) => items.iter().any(has_template),
        Value::Object(fields) => fields.values().any(has_template),
        _ => false,
    }
}

/// Which protobuf JSON spellings a literal may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// Anything protobuf JSON parsing accepts (response literals).
    Any,
    /// Only what [`request_json`] produces (match literals).
    Canonical,
}

/// The field walk gives paths for the common mistakes; a fully literal message is then parsed
/// for real, which also catches oneof conflicts, bad base64, map keys, and well-known types.
fn check_response_message(
    value: &Value,
    message: &MessageDescriptor,
    path: &str,
) -> Result<(), String> {
    check_message(value, message, path, Form::Any)?;
    if !has_template(value) {
        DynamicMessage::deserialize(message.clone(), value.clone())
            .map_err(|error| format!("`{path}` does not fit {}: {error}", message.full_name()))?;
    }
    Ok(())
}

fn check_message(
    value: &Value,
    message: &MessageDescriptor,
    path: &str,
    form: Form,
) -> Result<(), String> {
    let Value::Object(fields) = value else {
        return Err(format!(
            "`{path}` must be a mapping for {}, got {value}",
            message.full_name()
        ));
    };
    for (key, field_value) in fields {
        let field = message.get_field_by_json_name(key).or_else(|| match form {
            Form::Any => message.get_field_by_name(key),
            Form::Canonical => None,
        });
        let Some(field) = field else {
            let hint = match message.get_field_by_name(key) {
                Some(field) => format!("; match uses the JSON name `{}`", field.json_name()),
                None => String::new(),
            };
            return Err(format!(
                "unknown field `{path}.{key}` in {}{hint}",
                message.full_name()
            ));
        };
        check_field(field_value, &field, &format!("{path}.{key}"), form)?;
    }
    Ok(())
}

fn check_field(
    value: &Value,
    field: &FieldDescriptor,
    path: &str,
    form: Form,
) -> Result<(), String> {
    if value.is_null() || is_template(value) {
        return Ok(());
    }
    if field.is_map() {
        let Value::Object(entries) = value else {
            return Err(format!("`{path}` must be a mapping, got {value}"));
        };
        let value_field = field
            .kind()
            .as_message()
            .expect("map fields are messages")
            .map_entry_value_field();
        return entries.iter().try_for_each(|(key, entry)| {
            check_kind(entry, &value_field.kind(), &format!("{path}.{key}"), form)
        });
    }
    if field.is_list() {
        let Value::Array(items) = value else {
            return Err(format!("`{path}` must be a list, got {value}"));
        };
        return items.iter().enumerate().try_for_each(|(index, item)| {
            check_kind(item, &field.kind(), &format!("{path}[{index}]"), form)
        });
    }
    check_kind(value, &field.kind(), path, form)
}

fn check_kind(value: &Value, kind: &Kind, path: &str, form: Form) -> Result<(), String> {
    if value.is_null() || is_template(value) {
        return Ok(());
    }
    let fits = match kind {
        // Well-known types have special JSON forms (timestamps as strings, etc.); a match literal
        // must be the exact form the request renders.
        Kind::Message(message) if message.full_name().starts_with("google.protobuf.") => {
            return match form {
                Form::Any => Ok(()),
                Form::Canonical => canonical_well_known(value, message, path),
            };
        }
        Kind::Message(message) => return check_message(value, message, path, form),
        Kind::Enum(descriptor) => match value {
            Value::String(name) => {
                if descriptor.get_value_by_name(name).is_some() {
                    true
                } else {
                    let names = descriptor
                        .values()
                        .map(|value| value.name().to_owned())
                        .collect::<Vec<_>>();
                    return Err(format!(
                        "`{path}` has unknown {} value `{name}`; expected one of {}",
                        descriptor.full_name(),
                        names.join(", ")
                    ));
                }
            }
            Value::Number(number) => {
                form == Form::Any
                    && number
                        .as_i64()
                        .is_some_and(|number| i32::try_from(number).is_ok())
            }
            _ => false,
        },
        Kind::String => value.is_string(),
        // Canonical bytes are padded standard base64, exactly as the request renders them.
        Kind::Bytes => value.as_str().is_some_and(|text| {
            form == Form::Any
                || BASE64
                    .decode(text)
                    .is_ok_and(|bytes| BASE64.encode(bytes) == text)
        }),
        Kind::Bool => value.is_boolean(),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
            (value.is_number() || form == Form::Any)
                && integer(value).is_some_and(|number| i32::try_from(number).is_ok())
        }
        Kind::Uint32 | Kind::Fixed32 => {
            (value.is_number() || form == Form::Any)
                && integer(value).is_some_and(|number| u32::try_from(number).is_ok())
        }
        // Canonical protobuf JSON writes 64-bit integers as strings.
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
            (value.is_string() || form == Form::Any)
                && integer(value).is_some_and(|number| {
                    i64::try_from(number).is_ok() && canonical_integer(value, number, form)
                })
        }
        Kind::Uint64 | Kind::Fixed64 => {
            (value.is_string() || form == Form::Any)
                && integer(value).is_some_and(|number| {
                    u64::try_from(number).is_ok() && canonical_integer(value, number, form)
                })
        }
        Kind::Float | Kind::Double => match value {
            Value::Number(_) => true,
            Value::String(text) => {
                matches!(text.as_str(), "NaN" | "Infinity" | "-Infinity")
                    || (form == Form::Any && text.parse::<f64>().is_ok())
            }
            _ => false,
        },
    };
    if fits {
        Ok(())
    } else {
        Err(format!("`{path}` expects {}, got {value}", kind_name(kind)))
    }
}

fn canonical_well_known(
    value: &Value,
    message: &MessageDescriptor,
    path: &str,
) -> Result<(), String> {
    let rendered = DynamicMessage::deserialize(message.clone(), value.clone())
        .map_err(|error| error.to_string())
        .and_then(|parsed| request_json(&parsed).map_err(|error| error.to_string()))
        .map_err(|error| format!("`{path}` does not fit {}: {error}", message.full_name()))?;
    if value_matches(value, &rendered) {
        Ok(())
    } else {
        Err(format!(
            "`{path}` must be written as the request renders it: {rendered}, got {value}"
        ))
    }
}

/// A canonical 64-bit string has no sign, padding, or leading zeros (`"5"`, not `"+05"`).
fn canonical_integer(value: &Value, number: i128, form: Form) -> bool {
    form == Form::Any || value.as_str() == Some(number.to_string().as_str())
}

/// Protobuf JSON integers: a JSON integer, or a decimal string (the 64-bit form).
fn integer(value: &Value) -> Option<i128> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from)),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

fn kind_name(kind: &Kind) -> String {
    match kind {
        Kind::Message(message) => message.full_name().to_owned(),
        Kind::Enum(descriptor) => descriptor.full_name().to_owned(),
        other => format!("{other:?}").to_ascii_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::ProtoSources;
    use crate::protos::load_descriptor_pool;

    fn pool() -> DescriptorPool {
        load_descriptor_pool(&ProtoSources {
            roots: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proto")],
            descriptor_sets: vec![],
            files: vec![],
        })
        .unwrap()
    }

    fn route(method: &str, cases: &str) -> Route {
        Route::from_yaml(&format!(
            r#"
id: approvals-get
transport: grpc
match:
  service: approvals.v1.Approvals
  method: {method}
cases:
{cases}
"#
        ))
        .unwrap()
    }

    fn check(cases: &str) -> Result<(), String> {
        check_route(&route("GetApproval", cases), &pool())
    }

    #[test]
    fn accepts_literals_that_fit_the_proto() {
        check(
            r#"
  pending:
    match:
      message: { id: a-1 }
    behaviors:
      ok:
        message:
          id: a-1
          state: PENDING
          approvers: [alice]
          labels: { team: core }
          reviewer: { name: Bob, admin: true }
          priority: 3
          createdAtMs: "1700000000000"
      by-proto-name:
        message: { created_at_ms: 5 }
      echo:
        message: { id: '${ message.id }', state: '${ message.state }' }
      not-found:
        status: NOT_FOUND
        message: 'approval ${ message.id } not found'
        metadata: { x-reason: missing }
"#,
        )
        .unwrap();
    }

    #[test]
    fn rejects_unknown_fields_with_address_and_path() {
        let error = check(
            r#"
  any:
    behaviors:
      ok:
        message: { reviewer: { nmae: Bob } }
"#,
        )
        .unwrap_err();
        assert_eq!(
            error,
            "behavior `approvals-get:any:ok`: unknown field `message.reviewer.nmae` in approvals.v1.Reviewer"
        );
    }

    #[test]
    fn rejects_bad_enum_names() {
        let error = check(
            r#"
  any:
    behaviors:
      ok:
        message: { state: DONE }
"#,
        )
        .unwrap_err();
        assert!(
            error.contains("`message.state` has unknown approvals.v1.State value `DONE`"),
            "{error}"
        );
    }

    #[test]
    fn rejects_wrong_scalar_types() {
        for (message, expected) in [
            ("{ id: 1 }", "`message.id` expects string, got 1"),
            ("{ priority: x }", "`message.priority` expects int32"),
            (
                "{ priority: 3000000000 }",
                "`message.priority` expects int32",
            ),
            ("{ approvers: alice }", "`message.approvers` must be a list"),
            (
                "{ approvers: [1] }",
                "`message.approvers[0]` expects string",
            ),
            (
                "{ labels: { team: 1 } }",
                "`message.labels.team` expects string",
            ),
            (
                "{ reviewer: { admin: yes } }",
                "`message.reviewer.admin` expects bool",
            ),
            (
                "{ createdAtMs: '99999999999999999999' }",
                "`message.createdAtMs` expects int64",
            ),
            (
                "{ state: 3000000000 }",
                "`message.state` expects approvals.v1.State",
            ),
        ] {
            let error = check(&format!(
                "  any:\n    behaviors:\n      ok:\n        message: {message}\n"
            ))
            .unwrap_err();
            assert!(error.contains(expected), "{message}: {error}");
        }
    }

    #[test]
    fn match_literals_must_use_the_canonical_request_form() {
        let pool = pool();
        let input = pool.get_message_by_name("approvals.v1.Approval").unwrap();
        let canonical = |value: Value| check_message(&value, &input, "message", Form::Canonical);
        canonical(serde_json::json!({ "createdAtMs": "5", "state": "PENDING", "priority": 0 }))
            .unwrap();
        assert!(
            canonical(serde_json::json!({ "created_at_ms": "5" }))
                .unwrap_err()
                .contains("match uses the JSON name `createdAtMs`")
        );
        assert!(
            canonical(serde_json::json!({ "createdAtMs": 5 }))
                .unwrap_err()
                .contains("expects int64")
        );
        for (literal, expected) in [
            (serde_json::json!({ "createdAtMs": "+05" }), "expects int64"),
            (serde_json::json!({ "evidence": "hello" }), "expects bytes"),
            (serde_json::json!({ "score": "1.5" }), "expects double"),
        ] {
            assert!(
                canonical(literal.clone()).unwrap_err().contains(expected),
                "{literal}"
            );
        }
        canonical(serde_json::json!({ "evidence": "aGVsbG8=", "score": 1.5 })).unwrap();
        canonical(serde_json::json!({ "score": "NaN" })).unwrap();
        assert!(
            canonical(serde_json::json!({ "state": 1 }))
                .unwrap_err()
                .contains("expects approvals.v1.State")
        );
    }

    #[test]
    fn request_json_keeps_doubles_exact() {
        let approval = pool().get_message_by_name("approvals.v1.Approval").unwrap();
        let literal = serde_json::json!({ "score": 3.8841428571428573 });
        let message = DynamicMessage::deserialize(approval, literal.clone()).unwrap();

        assert_eq!(request_json(&message).unwrap()["score"], literal["score"]);
    }

    #[test]
    fn well_known_match_literals_must_use_the_canonical_request_form() {
        let pool = pool();
        let input = pool.get_message_by_name("approvals.v1.Sample").unwrap();
        let canonical = |value: Value| check_message(&value, &input, "message", Form::Canonical);
        canonical(serde_json::json!({
            "limit": "5",
            "at": "2024-01-01T00:00:00Z",
            "boxedRatio": 0.1,
        }))
        .unwrap();
        for (literal, expected) in [
            (
                serde_json::json!({ "limit": 5 }),
                "`message.limit` must be written as the request renders it: \"5\", got 5",
            ),
            (
                serde_json::json!({ "at": "2024-01-01T00:00:00+00:00" }),
                "`message.at` must be written as the request renders it: \"2024-01-01T00:00:00Z\", got \"2024-01-01T00:00:00+00:00\"",
            ),
            (
                serde_json::json!({ "limit": "five" }),
                "`message.limit` does not fit google.protobuf.Int64Value",
            ),
        ] {
            let error = canonical(literal.clone()).unwrap_err();
            assert!(error.contains(expected), "{literal}: {error}");
        }
        // Response literals keep any form protobuf JSON parses.
        let response = serde_json::json!({ "limit": 5, "at": "2024-01-01T00:00:00+00:00" });
        check_message(&response, &input, "message", Form::Any).unwrap();
    }

    #[test]
    fn response_literals_are_parsed_as_protobuf_json() {
        let error = check(
            r#"
  any:
    behaviors:
      ok:
        message: { evidence: 'not base64!' }
"#,
        )
        .unwrap_err();
        assert!(
            error.contains("`message` does not fit approvals.v1.Approval"),
            "{error}"
        );
    }

    #[test]
    fn templated_status_accepts_either_message_form() {
        check(
            r#"
  any:
    behaviors:
      maybe:
        status: '${ metadata.status }'
        message: { id: a-1 }
      maybe-error:
        status: '${ metadata.status }'
        message: 'failed for ${ message.id }'
"#,
        )
        .unwrap();
        let error = check(
            r#"
  any:
    behaviors:
      maybe:
        status: '${ metadata.status }'
        message: { nope: 1 }
"#,
        )
        .unwrap_err();
        assert!(error.contains("unknown field `message.nope`"), "{error}");
    }

    #[test]
    fn pipelines_without_protobuf_skip_literal_checks() {
        let route = Route::from_yaml(
            r#"
id: raw
transport: grpc
match: { service: approvals.v1.Approvals, method: GetApproval }
pipeline: [grpc-frame]
cases:
  any:
    behaviors:
      ok: { message: CgNhLTE= }
"#,
        )
        .unwrap();
        check_route(&route, &pool()).unwrap();

        let raw = |behavior: &str| {
            Route::from_yaml(&format!(
                "id: r\ntransport: grpc\nmatch: {{ service: s, method: m }}\npipeline: [grpc-frame]\ncases:\n  any:\n    behaviors:\n      ok: {behavior}\n"
            ))
            .map_err(|error| error.to_string())
        };
        assert!(
            raw("{ message: { id: x } }")
                .unwrap_err()
                .contains("base64 bytes, not a mapping")
        );
        assert!(
            raw("{ message: 'not base64!' }")
                .unwrap_err()
                .contains("must be base64 bytes")
        );
        raw("{ message: '${ message }' }").unwrap();

        for (steps, expected) in [
            (
                "[protobuf, grpc-frame]",
                "`grpc-frame` must be the first step",
            ),
            (
                "[grpc-frame, protobuf, protobuf]",
                "step `protobuf` appears twice",
            ),
            ("[protobuf]", "`grpc-frame` must be the first step"),
            ("[]", "`grpc-frame` must be the first step"),
        ] {
            let error = Route::from_yaml(&format!(
                "id: r\ntransport: grpc\nmatch: {{ service: s, method: m }}\npipeline: {steps}\ncases:\n  any:\n    behaviors:\n      ok: {{}}\n"
            ))
            .unwrap_err()
            .to_string();
            assert!(error.contains(expected), "{steps}: {error}");
        }

        let unknown = Route::from_yaml(
            "id: r\ntransport: grpc\nmatch: { service: s, method: m }\npipeline: [msgpack]\ncases: {}\n",
        )
        .unwrap_err()
        .to_string();
        assert!(unknown.contains("unknown variant `msgpack`"), "{unknown}");

        let http = Route::from_yaml(
            "id: r\ntransport: http\nmatch: { method: GET, path: /x }\npipeline: [protobuf]\ncases:\n  any:\n    behaviors:\n      ok: {}\n",
        )
        .unwrap_err()
        .to_string();
        assert!(
            http.contains("HTTP Routes do not take a `pipeline` yet"),
            "{http}"
        );
    }

    #[test]
    fn rejects_reserved_and_invalid_metadata_names() {
        for (name, expected) in [
            ("grpc-status", "reserved by gRPC"),
            ("X-Reason", "must be lowercase"),
            ("'bad name'", "must be lowercase"),
        ] {
            let error = Route::from_yaml(&format!(
                "id: r\ntransport: grpc\nmatch: {{ service: s, method: m }}\ncases:\n  any:\n    behaviors:\n      ok: {{ metadata: {{ {name}: x }} }}\n"
            ))
            .unwrap_err()
            .to_string();
            assert!(error.contains(expected), "{name}: {error}");
        }
    }

    #[test]
    fn rejects_metadata_values_http_does_not_allow() {
        let yaml = |value: &str| {
            format!(
                "id: r\ntransport: grpc\nmatch: {{ service: s, method: m }}\ncases:\n  any:\n    behaviors:\n      ok: {{ metadata: {{ x-reason: \"{value}\" }} }}\n"
            )
        };
        for value in ["a\\nb", "a\\tb"] {
            let error = Route::from_yaml(&yaml(value)).unwrap_err().to_string();
            assert!(error.contains("metadata `x-reason`"), "{value}: {error}");
        }
        // A templated value is checked once rendered.
        assert!(Route::from_yaml(&yaml("${ 'a' }")).is_ok());
    }

    #[test]
    fn rejects_route_level_passthrough() {
        let mut route = route("GetApproval", "  any:\n    behaviors:\n      ok: {}\n");
        route.passthrough = Some(crate::schema::PassthroughTarget {
            base_url: "http://x".to_owned(),
        });
        assert!(
            check_route(&route, &pool())
                .unwrap_err()
                .contains("do not support passthrough")
        );
    }

    #[test]
    fn checks_match_literals_against_the_input_type() {
        let error = check(
            r#"
  pending:
    match:
      message: { approvalId: a-1 }
    behaviors:
      ok:
        message: { id: a-1 }
"#,
        )
        .unwrap_err();
        assert_eq!(
            error,
            "case `approvals-get:pending` match: unknown field `message.approvalId` in approvals.v1.GetApprovalRequest"
        );
        check(
            r#"
  pending:
    match: { message: "${ message.id == 'a-1' }" }
    behaviors:
      ok:
        message: { id: a-1 }
"#,
        )
        .unwrap();
    }

    #[test]
    fn rejects_unknown_services_methods_and_streaming() {
        let cases = "  any:\n    behaviors:\n      ok:\n        message: {}\n";
        let mut unknown_service = route("GetApproval", cases);
        if let RouteMatch::Grpc(address) = &mut unknown_service.route_match {
            address.service = "approvals.v1.Nope".to_owned();
        }
        assert_eq!(
            check_route(&unknown_service, &pool()).unwrap_err(),
            "route `approvals-get`: unknown gRPC service `approvals.v1.Nope` (not in the loaded protos)"
        );
        assert_eq!(
            check_route(&route("Get", cases), &pool()).unwrap_err(),
            "route `approvals-get`: unknown method `Get` on gRPC service `approvals.v1.Approvals`"
        );
        assert_eq!(
            check_route(&route("UploadEvidence", cases), &pool()).unwrap_err(),
            "route `approvals-get`: streaming method `approvals.v1.Approvals.UploadEvidence` is not supported yet"
        );
    }

    fn check_watch(cases: &str) -> Result<(), String> {
        check_route(&route("WatchApprovals", cases), &pool())
    }

    #[test]
    fn server_streaming_methods_take_a_stream_list() {
        check_watch(
            r#"
  any:
    behaviors:
      then-ok:
        stream:
          - { message: { id: a-1, state: PENDING } }
          - { message: { id: '${ message.ids[0] }' }, delay: 10ms }
        status: OK
      then-aborted:
        stream: [{ message: { id: a-1 } }]
        status: ABORTED
        message: 'gave up on ${ message.ids[0] }'
        metadata: { x-reason: gone }
      open: { stream: [{ message: { id: a-1 } }] }
      open-empty: {}
      fails-at-once: { status: UNAVAILABLE }
"#,
        )
        .unwrap();
    }

    #[test]
    fn stream_messages_are_checked_against_the_output_type() {
        let error = check_watch(
            "  any:
    behaviors:
      ok:
        stream: [{ message: { id: a-1 } }, { message: { nope: 1 } }]
",
        )
        .unwrap_err();
        assert_eq!(
            error,
            "behavior `approvals-get:any:ok`: unknown field `stream[1].message.nope` in approvals.v1.Approval"
        );
        let error = check_watch(
            "  any:
    behaviors:
      ok:
        stream: [{ message: { evidence: 'not base64!' } }]
",
        )
        .unwrap_err();
        assert!(
            error.contains("`stream[0].message` does not fit approvals.v1.Approval"),
            "{error}"
        );
    }

    #[test]
    fn streamed_answers_send_messages_only_in_stream() {
        for (behavior, expected) in [
            (
                "{ message: { id: a-1 } }",
                "`message` is only the text of an error `status`",
            ),
            (
                "{ stream: [], status: OK, message: { id: a-1 } }",
                "`message` is only the text of an error `status`",
            ),
            (
                "{ stream: [], metadata: { x-reason: gone } }",
                "`metadata` is sent with `status`",
            ),
        ] {
            let error = check_watch(&format!("  any:\n    behaviors:\n      ok: {behavior}\n"))
                .unwrap_err();
            assert!(error.contains(expected), "{behavior}: {error}");
        }
        let error =
            check("  any:\n    behaviors:\n      ok: { stream: [{ message: { id: a-1 } }] }\n")
                .unwrap_err();
        assert_eq!(
            error,
            "behavior `approvals-get:any:ok`: `stream` needs a server-streaming method; `approvals.v1.Approvals.GetApproval` is unary"
        );
    }

    #[test]
    fn stream_message_shapes_are_checked_with_the_route() {
        let invalid = |pipeline: &str, behavior: &str| {
            Route::from_yaml(&format!(
                "id: r\ntransport: grpc\nmatch: {{ service: s, method: m }}\n{pipeline}cases:\n  any:\n    behaviors:\n      ok: {behavior}\n"
            ))
            .unwrap_err()
            .to_string()
        };
        assert!(
            invalid("", "{ stream: [{ message: plain }] }")
                .contains("`stream[0].message` must be a mapping of response fields")
        );
        assert!(
            invalid(
                "pipeline: [grpc-frame]\n",
                "{ stream: [{ message: { id: x } }] }"
            )
            .contains("`stream[0].message` is base64 bytes, not a mapping")
        );
        assert!(
            invalid(
                "pipeline: [grpc-frame]\n",
                "{ stream: [{ message: 'not base64!' }] }"
            )
            .contains("`stream[0].message` must be base64 bytes")
        );
        // Templates in stream messages are parsed at load time too.
        invalid("", "{ stream: [{ message: { id: '${ nope(' } }] }");
    }

    #[test]
    fn behavior_shape_is_checked_with_the_route() {
        let invalid = |behavior: &str| {
            Route::from_yaml(&format!(
                r#"
id: approvals-get
transport: grpc
match: {{ service: approvals.v1.Approvals, method: GetApproval }}
cases:
  any:
    behaviors:
      b: {behavior}
"#
            ))
            .unwrap_err()
            .to_string()
        };
        assert!(invalid("{ body: {} }").contains("not `body` and `headers`"));
        assert!(invalid("{ status: 404 }").contains("gRPC status name"));
        assert!(invalid("{ status: NOPE }").contains("gRPC status name"));
        assert!(invalid("{ status: NOT_FOUND, message: { id: x } }").contains("string `message`"));
        assert!(invalid("{ message: plain }").contains("mapping of response fields"));
        assert!(invalid("{ kind: passthrough }").contains("do not support passthrough"));
    }
}
