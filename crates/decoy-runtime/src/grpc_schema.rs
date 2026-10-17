//! gRPC Route checks. Shape checks run with the rest of Route validation; proto checks run at
//! startup against the loaded `DescriptorPool` (design note `docs/design/transports.md` §4, Q3):
//! the service and method exist, and literal `match.message` / response `message` values fit the
//! method's input / output types as protobuf JSON. `${ }` strings are skipped (checked at render).

use prost_reflect::{DescriptorPool, FieldDescriptor, Kind, MessageDescriptor, MethodDescriptor};
use serde_json::Value;

use crate::schema::{Behavior, ResponseBehavior, Route, RouteMatch};
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

/// Shape of a gRPC response Behavior: `message` (success), or `status` plus an optional `message`
/// string (error); `metadata` for trailers. No HTTP fields.
pub fn validate_response(response: &ResponseBehavior) -> Result<(), String> {
    if response.body.is_some() || !response.headers.is_empty() {
        return Err("gRPC Behaviors use `message` and `metadata`, not `body` and `headers`".into());
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
    match (&response.message, is_error(response)) {
        (None, _) | (Some(Value::String(_)), true) | (Some(Value::Object(_)), false) => {}
        (Some(Value::String(text)), false) if text.contains("${") => {}
        (Some(_), true) => return Err("an error status takes a string `message`".into()),
        (Some(_), false) => return Err("`message` must be a mapping of response fields".into()),
    }
    for value in response.metadata.values() {
        validate_string(value)?;
    }
    response.message.iter().try_for_each(validate_value)
}

/// A Behavior with a non-`OK` `status` answers with an error; templated statuses count as errors
/// here, so their `message` is a status string.
fn is_error(response: &ResponseBehavior) -> bool {
    response
        .status
        .as_ref()
        .is_some_and(|status| status.as_str() != Some("OK"))
}

/// Check a gRPC Route against the loaded protos. Errors name the Route, Case, or Behavior and the
/// field path.
pub fn check_route(route: &Route, pool: &DescriptorPool) -> Result<(), String> {
    let RouteMatch::Grpc(address) = &route.route_match else {
        return Ok(());
    };
    let method = find_method(pool, &address.service, &address.method)
        .map_err(|reason| format!("route `{}`: {reason}", route.id))?;
    if method.is_client_streaming() || method.is_server_streaming() {
        return Err(format!(
            "route `{}`: streaming method `{}` is not supported yet",
            route.id,
            method.full_name()
        ));
    }

    for (case_id, case) in &route.cases {
        if let Some(pattern) = case.case_match.get("message")
            && !is_template(pattern)
        {
            check_message(pattern, &method.input(), "message")
                .map_err(|reason| format!("case `{}:{case_id}` match: {reason}", route.id))?;
        }
        for (behavior_id, behavior) in &case.behaviors {
            let Behavior::Response(response) = behavior else {
                continue;
            };
            if is_error(response) {
                continue;
            }
            if let Some(message) = &response.message
                && !is_template(message)
            {
                check_message(message, &method.output(), "message").map_err(|reason| {
                    format!("behavior `{}:{case_id}:{behavior_id}`: {reason}", route.id)
                })?;
            }
        }
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

fn check_message(value: &Value, message: &MessageDescriptor, path: &str) -> Result<(), String> {
    let Value::Object(fields) = value else {
        return Err(format!(
            "`{path}` must be a mapping for {}, got {value}",
            message.full_name()
        ));
    };
    for (key, field_value) in fields {
        let field = message
            .get_field_by_json_name(key)
            .or_else(|| message.get_field_by_name(key))
            .ok_or_else(|| format!("unknown field `{path}.{key}` in {}", message.full_name()))?;
        check_field(field_value, &field, &format!("{path}.{key}"))?;
    }
    Ok(())
}

fn check_field(value: &Value, field: &FieldDescriptor, path: &str) -> Result<(), String> {
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
            check_kind(entry, &value_field.kind(), &format!("{path}.{key}"))
        });
    }
    if field.is_list() {
        let Value::Array(items) = value else {
            return Err(format!("`{path}` must be a list, got {value}"));
        };
        return items.iter().enumerate().try_for_each(|(index, item)| {
            check_kind(item, &field.kind(), &format!("{path}[{index}]"))
        });
    }
    check_kind(value, &field.kind(), path)
}

fn check_kind(value: &Value, kind: &Kind, path: &str) -> Result<(), String> {
    if value.is_null() || is_template(value) {
        return Ok(());
    }
    let fits = match kind {
        // Well-known types have special JSON forms (timestamps as strings, etc.); accept them.
        Kind::Message(message) if message.full_name().starts_with("google.protobuf.") => true,
        Kind::Message(message) => return check_message(value, message, path),
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
            Value::Number(number) => number.is_i64(),
            _ => false,
        },
        Kind::String | Kind::Bytes => value.is_string(),
        Kind::Bool => value.is_boolean(),
        Kind::Int32
        | Kind::Sint32
        | Kind::Sfixed32
        | Kind::Int64
        | Kind::Sint64
        | Kind::Sfixed64 => integer(value).is_some_and(|number| {
            let is_32 = matches!(kind, Kind::Int32 | Kind::Sint32 | Kind::Sfixed32);
            !is_32 || i32::try_from(number).is_ok()
        }),
        Kind::Uint32 | Kind::Fixed32 | Kind::Uint64 | Kind::Fixed64 => {
            integer(value).is_some_and(|number| {
                let is_32 = matches!(kind, Kind::Uint32 | Kind::Fixed32);
                number >= 0 && (!is_32 || u32::try_from(number).is_ok())
            })
        }
        Kind::Float | Kind::Double => match value {
            Value::Number(_) => true,
            Value::String(text) => {
                matches!(text.as_str(), "NaN" | "Infinity" | "-Infinity")
                    || text.parse::<f64>().is_ok()
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
        ] {
            let error = check(&format!(
                "  any:\n    behaviors:\n      ok:\n        message: {message}\n"
            ))
            .unwrap_err();
            assert!(error.contains(expected), "{message}: {error}");
        }
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
        assert!(
            check_route(&route("WatchApprovals", cases), &pool())
                .unwrap_err()
                .contains(
                    "streaming method `approvals.v1.Approvals.WatchApprovals` is not supported yet"
                )
        );
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
