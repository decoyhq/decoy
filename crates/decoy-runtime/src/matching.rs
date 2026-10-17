//! Literal Case matching: `match` is an object whose fields (`pathParams`, `query`, `headers`,
//! `cookies`, `body`) must all match the request. See the spec in issue #215.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::request::RequestContext;

const FIELDS: [&str; 5] = ["pathParams", "query", "headers", "cookies", "body"];

/// Pattern value meaning "present, with any value" in the string-valued fields.
const ANY: &str = "*";

/// Reject `match` shapes the matcher would silently misread.
pub fn validate_case_match(case_match: &Value) -> Result<(), String> {
    let fields = match case_match {
        Value::Null => return Ok(()),
        Value::Object(fields) => fields,
        other => return Err(format!("`match` must be a mapping, got {}", kind(other))),
    };
    for (field, pattern) in fields {
        if !FIELDS.contains(&field.as_str()) {
            return Err(format!(
                "unknown match field `{field}`; expected one of {}",
                FIELDS.join(", ")
            ));
        }
        if field != "body" && !pattern.is_object() {
            return Err(format!(
                "match field `{field}` must be a mapping, got {}",
                kind(pattern)
            ));
        }
    }
    Ok(())
}

pub fn case_matches(
    case_match: &Value,
    path_params: &BTreeMap<String, String>,
    context: &RequestContext,
) -> bool {
    let Value::Object(fields) = case_match else {
        return case_match.is_null();
    };
    fields.iter().all(|(field, pattern)| match field.as_str() {
        "pathParams" => strings_match(pattern, |key| path_params.get(key).into_iter().collect()),
        "query" => strings_match(pattern, |key| {
            context.query.get(key).into_iter().flatten().collect()
        }),
        "headers" => strings_match(pattern, |key| {
            context
                .headers
                .get(&key.to_ascii_lowercase())
                .into_iter()
                .collect()
        }),
        "cookies" => strings_match(pattern, |key| {
            context.cookies.get(key).into_iter().collect()
        }),
        "body" => value_matches(pattern, &context.body),
        _ => false,
    })
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

    fn context(path_and_query: &str, headers: &[(&str, &str)], body: Value) -> RequestContext {
        RequestContext {
            query: RequestContext::from_parts(None, path_and_query, &BTreeMap::new(), b"").query,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            cookies: BTreeMap::from([("sid".to_owned(), "abc".to_owned())]),
            body,
            ..RequestContext::default()
        }
    }

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn matches(pattern: Value, request: &RequestContext) -> bool {
        case_matches(&pattern, &params(&[("id", "7")]), request)
    }

    #[test]
    fn no_match_or_empty_match_accepts_everything() {
        let request = context("/", &[], Value::Null);
        assert!(matches(Value::Null, &request));
        assert!(matches(json!({}), &request));
    }

    #[test]
    fn path_params_compare_as_strings_with_wildcard() {
        let request = context("/", &[], Value::Null);
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
        let request = context("/?page=5&tag=a&tag=b&extra=1", &[], Value::Null);
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
        let request = context("/", &[("x-role", "admin")], Value::Null);
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
        let request = context("/", &[], Value::Null);
        assert!(matches(json!({ "cookies": { "sid": "abc" } }), &request));
        assert!(!matches(json!({ "cookies": { "sid": "xyz" } }), &request));
    }

    #[test]
    fn body_is_a_deep_partial_match() {
        let request = context(
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
        let request = context(
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
        let request = context("/", &[], json!("ping"));
        assert!(matches(json!({ "body": "ping" }), &request));
        assert!(!matches(json!({ "body": { "a": 1 } }), &request));
    }

    #[test]
    fn fields_are_anded() {
        let request = context("/?page=1", &[("x-role", "admin")], Value::Null);
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
    fn validation_rejects_unknown_fields_and_non_mapping_shapes() {
        assert!(validate_case_match(&json!({ "body": [1] })).is_ok());
        assert!(
            validate_case_match(&json!({ "querry": {} }))
                .unwrap_err()
                .contains("unknown match field `querry`")
        );
        assert!(
            validate_case_match(&json!({ "query": "x" }))
                .unwrap_err()
                .contains("must be a mapping")
        );
        assert!(validate_case_match(&json!("${ true }")).is_err());
    }
}
