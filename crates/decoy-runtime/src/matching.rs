//! Case matching. `match` is either a `${ <jmespath> }` predicate string or an object whose fields
//! (`pathParams`, `query`, `headers`, `cookies`, `body`) must all match: each field is a literal
//! pattern or a predicate. Predicates always see the whole request. See the spec in issue #215.

use std::cell::OnceCell;
use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::expression::{predicate_holds, validate_predicate};
use crate::request::RequestEnvelope;
use crate::schema::{HttpMethod, Transport};

/// The request as Case matching sees it.
pub struct MatchInput<'a> {
    pub method: &'a HttpMethod,
    pub path: &'a str,
    pub path_params: &'a BTreeMap<String, String>,
    pub envelope: &'a RequestEnvelope,
}

const HTTP_FIELDS: [&str; 5] = ["pathParams", "query", "headers", "cookies", "body"];
const GRPC_FIELDS: [&str; 2] = ["message", "metadata"];

/// Pattern value meaning "present, with any value" in the string-valued fields.
const ANY: &str = "*";

/// Reject `match` shapes the matcher would silently misread.
pub fn validate_case_match(transport: &Transport, case_match: &Value) -> Result<(), String> {
    let (allowed, free_form): (&[&str], &str) = match transport {
        Transport::Http => (&HTTP_FIELDS, "body"),
        Transport::Grpc => (&GRPC_FIELDS, "message"),
    };
    let fields = match case_match {
        Value::Null => return Ok(()),
        Value::String(predicate) => return validate_predicate(predicate),
        Value::Object(fields) => fields,
        other => {
            return Err(format!(
                "`match` must be a mapping or a `${{ }}` predicate, got {}",
                kind(other)
            ));
        }
    };
    for (field, pattern) in fields {
        if !allowed.contains(&field.as_str()) {
            return Err(format!(
                "unknown match field `{field}`; expected one of {}",
                allowed.join(", ")
            ));
        }
        match pattern {
            Value::String(text) if is_predicate(text) => validate_predicate(text)?,
            _ if field == free_form || pattern.is_object() => {}
            _ => {
                return Err(format!(
                    "match field `{field}` must be a mapping or a `${{ }}` predicate, got {}",
                    kind(pattern)
                ));
            }
        }
    }
    Ok(())
}

pub fn case_matches(case_match: &Value, input: &MatchInput) -> bool {
    let request = OnceCell::new();
    let request = || {
        request.get_or_init(|| {
            input
                .envelope
                .to_json(input.method, input.path, input.path_params)
        })
    };
    let fields = match case_match {
        Value::Null => return true,
        Value::String(predicate) => return predicate_holds(predicate, request()),
        Value::Object(fields) => fields,
        _ => return false,
    };
    let envelope = input.envelope;
    fields
        .iter()
        .all(|(field, pattern)| match (field.as_str(), pattern) {
            (_, Value::String(text)) if is_predicate(text) => predicate_holds(text, request()),
            ("pathParams", _) => strings_match(pattern, |key| {
                input.path_params.get(key).into_iter().collect()
            }),
            ("query", _) => strings_match(pattern, |key| {
                envelope.query.get(key).into_iter().flatten().collect()
            }),
            ("headers", _) => strings_match(pattern, |key| {
                envelope
                    .headers
                    .get(&key.to_ascii_lowercase())
                    .into_iter()
                    .collect()
            }),
            ("cookies", _) => strings_match(pattern, |key| {
                envelope.cookies.get(key).into_iter().collect()
            }),
            ("body", _) => value_matches(pattern, &envelope.body),
            _ => false,
        })
}

/// A field string is a predicate when it is exactly one `${ … }`; any other `body` string is
/// literal text to compare.
fn is_predicate(text: &str) -> bool {
    let text = text.trim();
    text.starts_with("${") && text.ends_with('}')
}

/// Every pattern key must be present, and one of its request values must equal the pattern value
/// compared as a string (`'*'` accepts any value).
fn strings_match<'a>(pattern: &Value, actual: impl Fn(&str) -> Vec<&'a String>) -> bool {
    let Value::Object(pattern) = pattern else {
        return false;
    };
    pattern.iter().all(|(key, expected)| {
        let values = actual(key);
        match as_string(expected) {
            Some(expected) if expected == ANY => !values.is_empty(),
            Some(expected) => values.iter().any(|value| **value == expected),
            None => false,
        }
    })
}

/// Deep partial match: objects need every pattern key, arrays need every pattern element somewhere
/// in the request array, scalars must be equal (numbers compared by value).
fn value_matches(pattern: &Value, actual: &Value) -> bool {
    match (pattern, actual) {
        (Value::Object(pattern), Value::Object(actual)) => object_matches(pattern, actual),
        (Value::Array(pattern), Value::Array(actual)) => pattern
            .iter()
            .all(|expected| actual.iter().any(|value| value_matches(expected, value))),
        (Value::Number(pattern), Value::Number(actual)) => pattern.as_f64() == actual.as_f64(),
        (pattern, actual) => pattern == actual,
    }
}

fn object_matches(pattern: &Map<String, Value>, actual: &Map<String, Value>) -> bool {
    pattern.iter().all(|(key, expected)| {
        actual
            .get(key)
            .is_some_and(|value| value_matches(expected, value))
    })
}

fn as_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "a mapping",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn envelope(path_and_query: &str, headers: &[(&str, &str)], body: Value) -> RequestEnvelope {
        RequestEnvelope {
            query: RequestEnvelope::from_parts(None, path_and_query, &BTreeMap::new(), b"").query,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            cookies: BTreeMap::from([("sid".to_owned(), "abc".to_owned())]),
            body,
            ..RequestEnvelope::default()
        }
    }

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn matches(pattern: Value, request: &RequestEnvelope) -> bool {
        case_matches(
            &pattern,
            &MatchInput {
                method: &HttpMethod::Post,
                path: "/orders/7",
                path_params: &params(&[("id", "7")]),
                envelope: request,
            },
        )
    }

    #[test]
    fn no_match_or_empty_match_accepts_everything() {
        let request = envelope("/", &[], Value::Null);
        assert!(matches(Value::Null, &request));
        assert!(matches(json!({}), &request));
    }

    #[test]
    fn path_params_compare_as_strings_with_wildcard() {
        let request = envelope("/", &[], Value::Null);
        assert!(matches(json!({ "pathParams": { "id": 7 } }), &request));
        assert!(matches(json!({ "pathParams": { "id": "*" } }), &request));
        assert!(!matches(json!({ "pathParams": { "id": "8" } }), &request));
        assert!(!matches(
            json!({ "pathParams": { "other": "*" } }),
            &request
        ));
    }

    #[test]
    fn query_is_a_subset_match_and_repeated_keys_match_any_value() {
        let request = envelope("/?page=5&tag=a&tag=b&extra=1", &[], Value::Null);
        assert!(matches(json!({ "query": { "page": 5 } }), &request));
        assert!(matches(json!({ "query": { "tag": "b" } }), &request));
        assert!(matches(
            json!({ "query": { "page": "*", "tag": "a" } }),
            &request
        ));
        assert!(!matches(json!({ "query": { "tag": "c" } }), &request));
        assert!(!matches(json!({ "query": { "missing": "*" } }), &request));
    }

    #[test]
    fn header_names_are_case_insensitive_and_values_exact() {
        let request = envelope("/", &[("x-role", "admin")], Value::Null);
        assert!(matches(
            json!({ "headers": { "X-Role": "admin" } }),
            &request
        ));
        assert!(matches(json!({ "headers": { "x-role": "*" } }), &request));
        assert!(!matches(
            json!({ "headers": { "x-role": "Admin" } }),
            &request
        ));
    }

    #[test]
    fn cookies_match_by_name() {
        let request = envelope("/", &[], Value::Null);
        assert!(matches(json!({ "cookies": { "sid": "abc" } }), &request));
        assert!(!matches(json!({ "cookies": { "sid": "xyz" } }), &request));
    }

    #[test]
    fn body_is_a_deep_partial_match() {
        let request = envelope(
            "/",
            &[],
            json!({ "user": { "name": "Ada", "age": 36 }, "active": true }),
        );
        assert!(matches(
            json!({ "body": { "user": { "name": "Ada" } } }),
            &request
        ));
        assert!(matches(
            json!({ "body": { "user": { "age": 36.0 } } }),
            &request
        ));
        assert!(!matches(
            json!({ "body": { "user": { "age": "36" } } }),
            &request
        ));
        assert!(!matches(
            json!({ "body": { "user": { "email": "x" } } }),
            &request
        ));
    }

    #[test]
    fn body_arrays_contain_each_pattern_element_in_any_order() {
        let request = envelope(
            "/",
            &[],
            json!({ "tags": ["beta", "x", "admin"], "items": [{ "id": 1 }, { "id": 2, "qty": 3 }] }),
        );
        assert!(matches(
            json!({ "body": { "tags": ["admin", "beta"] } }),
            &request
        ));
        assert!(matches(
            json!({ "body": { "items": [{ "id": 2 }] } }),
            &request
        ));
        assert!(!matches(json!({ "body": { "tags": ["gamma"] } }), &request));
        assert!(!matches(
            json!({ "body": { "items": [{ "id": 3 }] } }),
            &request
        ));
    }

    #[test]
    fn text_bodies_match_exact_strings() {
        let request = envelope("/", &[], json!("ping"));
        assert!(matches(json!({ "body": "ping" }), &request));
        assert!(!matches(json!({ "body": { "a": 1 } }), &request));
    }

    #[test]
    fn fields_are_anded() {
        let request = envelope("/?page=1", &[("x-role", "admin")], Value::Null);
        assert!(matches(
            json!({ "query": { "page": "1" }, "headers": { "x-role": "admin" } }),
            &request
        ));
        assert!(!matches(
            json!({ "query": { "page": "1" }, "headers": { "x-role": "user" } }),
            &request
        ));
    }

    #[test]
    fn grpc_validation_allows_message_and_metadata_only() {
        let grpc = Transport::Grpc;
        assert!(validate_case_match(&grpc, &json!({ "message": { "id": "a-1" } })).is_ok());
        assert!(validate_case_match(&grpc, &json!({ "metadata": { "x-tenant": "a" } })).is_ok());
        assert!(
            validate_case_match(&grpc, &json!({ "body": {} }))
                .unwrap_err()
                .contains("expected one of message, metadata")
        );
    }

    #[test]
    fn validation_rejects_unknown_fields_and_non_mapping_shapes() {
        assert!(validate_case_match(&Transport::Http, &json!({ "body": [1] })).is_ok());
        assert!(
            validate_case_match(&Transport::Http, &json!({ "querry": {} }))
                .unwrap_err()
                .contains("unknown match field `querry`")
        );
        assert!(
            validate_case_match(&Transport::Http, &json!({ "query": "x" }))
                .unwrap_err()
                .contains("must be a mapping")
        );
        assert!(validate_case_match(&Transport::Http, &json!("${ `true` }")).is_ok());
        assert!(validate_case_match(&Transport::Http, &json!("plain")).is_err());
        assert!(validate_case_match(&Transport::Http, &json!({ "query": "${ query.x }" })).is_ok());
        assert!(
            validate_case_match(&Transport::Http, &json!({ "query": "${ query.[ }" }))
                .unwrap_err()
                .contains("invalid JMESPath")
        );
    }

    #[test]
    fn whole_case_predicates_see_the_full_request() {
        let request = envelope(
            "/?verbose=true",
            &[("x-tier", "gold")],
            json!({ "amount": 1500 }),
        );
        assert!(matches(
            json!("${ method == 'POST' && path == '/orders/7' && pathParams.id == '7' }"),
            &request
        ));
        assert!(matches(
            json!(
                "${ body.amount > `1000` && headers.\"x-tier\" == 'gold' && query.verbose == 'true' }"
            ),
            &request
        ));
        assert!(matches(json!("${ cookies.sid == 'abc' }"), &request));
        assert!(!matches(json!("${ body.amount < `1000` }"), &request));
    }

    #[test]
    fn field_predicates_are_anded_with_literal_fields() {
        let request = envelope("/?page=1", &[], json!({ "operationName": "GetUser" }));
        assert!(matches(
            json!({ "query": { "page": "1" }, "body": "${ body.operationName == 'GetUser' }" }),
            &request
        ));
        assert!(!matches(
            json!({ "query": { "page": "2" }, "body": "${ body.operationName == 'GetUser' }" }),
            &request
        ));
        // A field predicate still sees the whole request, not only its own field.
        assert!(matches(
            json!({ "headers": "${ query.page == '1' }" }),
            &request
        ));
    }

    #[test]
    fn repeated_query_keys_are_arrays_for_predicates() {
        let request = envelope("/?tag=a&tag=b", &[], Value::Null);
        assert!(matches(json!("${ contains(query.tag, 'b') }"), &request));
    }
}
