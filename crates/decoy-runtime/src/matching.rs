//! Case matching. `match` is either a `${ <jmespath> }` predicate string or an object whose fields
//! (`pathParams`, `query`, `headers`, `cookies`, `body`) must all match: each field is a literal
//! pattern or a predicate. Predicates always see the whole request. See the spec in issue #215.

use std::cell::OnceCell;
use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::expression::{predicate_holds, validate_predicate};
use crate::request::RequestEnvelope;
use crate::schema::{HttpMethod, Transport};

/// The request as Case matching sees it.
pub struct MatchInput<'a> {
    pub method: &'a HttpMethod,
    pub path: &'a str,
    pub path_params: &'a BTreeMap<String, String>,
    pub envelope: &'a RequestEnvelope,
    /// The envelope's JSON, built on first predicate and shared across candidates; when set, its
    /// `pathParams` must be this input's.
    pub json: &'a OnceCell<Value>,
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

/// The first Case condition a request fails: the field (`pathParams.id`, `body.user.name`, or
/// `match` for a whole-Case predicate), the pattern, and what the request had (`null` if absent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConditionFailure {
    pub field: String,
    pub expected: Value,
    pub actual: Value,
}

impl ConditionFailure {
    fn new(field: impl Into<String>, expected: &Value, actual: Value) -> Self {
        Self {
            field: field.into(),
            expected: expected.clone(),
            actual,
        }
    }

    fn predicate(field: &str, predicate: &str) -> Self {
        Self::new(
            field,
            &Value::String(predicate.to_owned()),
            Value::Bool(false),
        )
    }
}

/// What a failed condition reports: the whole [`ConditionFailure`], or `()` when only the verdict
/// matters (the serving fast path), so nothing is formatted or cloned.
trait Failure: Sized {
    fn build(make: impl FnOnce() -> ConditionFailure) -> Self;
    fn edit(self, edit: impl FnOnce(&mut ConditionFailure)) -> Self;
}

impl Failure for ConditionFailure {
    fn build(make: impl FnOnce() -> ConditionFailure) -> Self {
        make()
    }

    fn edit(mut self, edit: impl FnOnce(&mut ConditionFailure)) -> Self {
        edit(&mut self);
        self
    }
}

impl Failure for () {
    fn build(_: impl FnOnce() -> ConditionFailure) -> Self {}

    fn edit(self, _: impl FnOnce(&mut ConditionFailure)) -> Self {}
}

/// `None` when the request satisfies the Case's `match`, else its first failed condition.
pub fn case_mismatch(case_match: &Value, input: &MatchInput) -> Option<ConditionFailure> {
    case_failure(case_match, input)
}

/// [`case_mismatch`] as a verdict only, building no failure.
pub fn case_matches(case_match: &Value, input: &MatchInput) -> bool {
    case_failure::<()>(case_match, input).is_none()
}

fn case_failure<F: Failure>(case_match: &Value, input: &MatchInput) -> Option<F> {
    let request = || {
        input.json.get_or_init(|| {
            input
                .envelope
                .to_json(input.method, input.path, input.path_params)
        })
    };
    let fields = match case_match {
        Value::Null => return None,
        Value::String(predicate) => {
            return (!predicate_holds(predicate, request()))
                .then(|| F::build(|| ConditionFailure::predicate("match", predicate)));
        }
        Value::Object(fields) => fields,
        other => {
            return Some(F::build(|| {
                ConditionFailure::new("match", other, Value::Null)
            }));
        }
    };
    let envelope = input.envelope;
    fields
        .iter()
        .find_map(|(field, pattern)| match (field.as_str(), pattern) {
            (_, Value::String(text)) if is_predicate(text) => (!predicate_holds(text, request()))
                .then(|| F::build(|| ConditionFailure::predicate(field, text))),
            ("pathParams", _) => strings_mismatch(field, pattern, |key| {
                input.path_params.get(key).into_iter().collect()
            }),
            ("query", _) => strings_mismatch(field, pattern, |key| {
                envelope.query.get(key).into_iter().flatten().collect()
            }),
            ("headers", _) => strings_mismatch(field, pattern, |key| {
                envelope
                    .headers
                    .get(&key.to_ascii_lowercase())
                    .into_iter()
                    .collect()
            }),
            ("cookies", _) => strings_mismatch(field, pattern, |key| {
                envelope.cookies.get(key).into_iter().collect()
            }),
            ("body", _) => value_mismatch(field, pattern, &envelope.body),
            _ => Some(F::build(|| {
                ConditionFailure::new(field.as_str(), pattern, Value::Null)
            })),
        })
}

/// gRPC Case matching over the message envelope `{ service, method, metadata, message }`:
/// `message` is a deep partial match like HTTP `body`, `metadata` matches like `headers`.
pub fn grpc_case_mismatch(case_match: &Value, envelope: &Value) -> Option<ConditionFailure> {
    grpc_case_failure(case_match, envelope)
}

/// [`grpc_case_mismatch`] as a verdict only, building no failure.
pub fn grpc_case_matches(case_match: &Value, envelope: &Value) -> bool {
    grpc_case_failure::<()>(case_match, envelope).is_none()
}

fn grpc_case_failure<F: Failure>(case_match: &Value, envelope: &Value) -> Option<F> {
    let fields = match case_match {
        Value::Null => return None,
        Value::String(predicate) => {
            return (!predicate_holds(predicate, envelope))
                .then(|| F::build(|| ConditionFailure::predicate("match", predicate)));
        }
        Value::Object(fields) => fields,
        other => {
            return Some(F::build(|| {
                ConditionFailure::new("match", other, Value::Null)
            }));
        }
    };
    fields
        .iter()
        .find_map(|(field, pattern)| match (field.as_str(), pattern) {
            (_, Value::String(text)) if is_predicate(text) => (!predicate_holds(text, envelope))
                .then(|| F::build(|| ConditionFailure::predicate(field, text))),
            ("message", _) => value_mismatch(field, pattern, &envelope["message"]),
            ("metadata", _) => strings_mismatch(field, pattern, |key| {
                envelope["metadata"]
                    .get(key.to_ascii_lowercase())
                    .and_then(Value::as_str)
                    .into_iter()
                    .collect()
            }),
            _ => Some(F::build(|| {
                ConditionFailure::new(field.as_str(), pattern, Value::Null)
            })),
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
fn strings_mismatch<'a, S: AsRef<str> + ?Sized + 'a, F: Failure>(
    field: &str,
    pattern: &Value,
    actual: impl Fn(&str) -> Vec<&'a S>,
) -> Option<F> {
    let Value::Object(pattern) = pattern else {
        return Some(F::build(|| {
            ConditionFailure::new(field, pattern, Value::Null)
        }));
    };
    pattern.iter().find_map(|(key, expected)| {
        let values = actual(key);
        let holds = match as_string(expected) {
            Some(expected) if expected == ANY => !values.is_empty(),
            Some(expected) => values.iter().any(|value| value.as_ref() == expected),
            None => false,
        };
        (!holds).then(|| {
            F::build(|| {
                let mut values = values
                    .iter()
                    .map(|value| Value::String(value.as_ref().to_owned()));
                let actual = match (values.next(), values.next()) {
                    (None, _) => Value::Null,
                    (Some(only), None) => only,
                    (Some(first), Some(second)) => {
                        Value::Array([first, second].into_iter().chain(values).collect())
                    }
                };
                ConditionFailure::new(format!("{field}.{key}"), expected, actual)
            })
        })
    })
}

/// Deep partial match: objects need every pattern key, arrays need every pattern element somewhere
/// in the request array, scalars must be equal (numbers compared by value).
fn value_mismatch<F: Failure>(field: &str, pattern: &Value, actual: &Value) -> Option<F> {
    Some(value_failure::<F>(pattern, actual)?.edit(|failure| failure.field.insert_str(0, field)))
}

/// The failure's `field` is the `.key` path below the matched value, built only on failure.
fn value_failure<F: Failure>(pattern: &Value, actual: &Value) -> Option<F> {
    let holds = match (pattern, actual) {
        (Value::Object(pattern), Value::Object(actual)) => {
            return pattern.iter().find_map(|(key, expected)| {
                let failure = match actual.get(key) {
                    Some(value) => value_failure::<F>(expected, value)?,
                    None => {
                        F::build(|| ConditionFailure::new(String::new(), expected, Value::Null))
                    }
                };
                Some(failure.edit(|failure| failure.field.insert_str(0, &format!(".{key}"))))
            });
        }
        (pattern, actual) => value_matches(pattern, actual),
    };
    (!holds).then(|| F::build(|| ConditionFailure::new(String::new(), pattern, actual.clone())))
}

/// `value_failure` without building the failure, for array elements tried one by one.
fn value_matches(pattern: &Value, actual: &Value) -> bool {
    match (pattern, actual) {
        (Value::Object(pattern), Value::Object(actual)) => pattern.iter().all(|(key, expected)| {
            actual
                .get(key)
                .is_some_and(|value| value_matches(expected, value))
        }),
        (Value::Array(pattern), Value::Array(actual)) => pattern
            .iter()
            .all(|expected| actual.iter().any(|value| value_matches(expected, value))),
        (Value::Number(pattern), Value::Number(actual)) => pattern.as_f64() == actual.as_f64(),
        (pattern, actual) => pattern == actual,
    }
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
        mismatch(pattern, request).is_none()
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
    fn grpc_matches_message_metadata_and_predicates() {
        let envelope = json!({
            "service": "approvals.v1.Approvals",
            "method": "GetApproval",
            "metadata": { "x-tenant": "acme" },
            "message": { "id": "a-1", "approvers": ["alice", "bob"] },
        });
        let matches = |case_match: Value| grpc_mismatch(&case_match, &envelope).is_none();
        assert!(matches(Value::Null));
        assert!(matches(json!({ "message": { "id": "a-1" } })));
        assert!(matches(json!({ "message": { "approvers": ["bob"] } })));
        assert!(!matches(json!({ "message": { "id": "a-2" } })));
        assert!(matches(json!({ "metadata": { "X-Tenant": "acme" } })));
        assert!(matches(json!({ "metadata": { "x-tenant": "*" } })));
        assert!(!matches(json!({ "metadata": { "x-other": "*" } })));
        assert!(matches(json!("${ message.id == 'a-1' }")));
        assert!(matches(
            json!({ "message": "${ length(message.approvers) == `2` }" })
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

    fn failure(field: &str, expected: Value, actual: Value) -> Option<ConditionFailure> {
        Some(ConditionFailure {
            field: field.to_owned(),
            expected,
            actual,
        })
    }

    /// Also checks the fast-path verdict agrees with the explained one.
    fn mismatch(pattern: Value, request: &RequestEnvelope) -> Option<ConditionFailure> {
        let input = MatchInput {
            method: &HttpMethod::Post,
            path: "/orders/7",
            path_params: &params(&[("id", "7")]),
            envelope: request,
            json: &OnceCell::new(),
        };
        let failure = case_mismatch(&pattern, &input);
        assert_eq!(
            case_matches(&pattern, &input),
            failure.is_none(),
            "{pattern}"
        );
        failure
    }

    fn grpc_mismatch(case_match: &Value, envelope: &Value) -> Option<ConditionFailure> {
        let failure = grpc_case_mismatch(case_match, envelope);
        assert_eq!(
            grpc_case_matches(case_match, envelope),
            failure.is_none(),
            "{case_match}"
        );
        failure
    }

    #[test]
    fn a_failed_condition_names_field_expected_and_actual() {
        let request = envelope(
            "/?page=5&tag=a&tag=b",
            &[("x-role", "user")],
            json!({ "user": { "name": "Ada" }, "tags": ["x"] }),
        );
        assert_eq!(
            mismatch(json!({ "pathParams": { "id": "7" } }), &request),
            None
        );
        assert_eq!(
            mismatch(json!({ "pathParams": { "id": "123" } }), &request),
            failure("pathParams.id", json!("123"), json!("7"))
        );
        assert_eq!(
            mismatch(json!({ "pathParams": { "slug": "*" } }), &request),
            failure("pathParams.slug", json!("*"), Value::Null)
        );
        assert_eq!(
            mismatch(json!({ "query": { "tag": "c" } }), &request),
            failure("query.tag", json!("c"), json!(["a", "b"]))
        );
        assert_eq!(
            mismatch(json!({ "headers": { "X-Role": "admin" } }), &request),
            failure("headers.X-Role", json!("admin"), json!("user"))
        );
        assert_eq!(
            mismatch(json!({ "cookies": { "sid": "xyz" } }), &request),
            failure("cookies.sid", json!("xyz"), json!("abc"))
        );
        assert_eq!(
            mismatch(json!({ "body": { "user": { "name": "Bob" } } }), &request),
            failure("body.user.name", json!("Bob"), json!("Ada"))
        );
        assert_eq!(
            mismatch(json!({ "body": { "user": { "email": "x" } } }), &request),
            failure("body.user.email", json!("x"), Value::Null)
        );
        assert_eq!(
            mismatch(json!({ "body": { "tags": ["y"] } }), &request),
            failure("body.tags", json!(["y"]), json!(["x"]))
        );
        assert_eq!(
            mismatch(json!("${ query.page == '6' }"), &request),
            failure("match", json!("${ query.page == '6' }"), json!(false))
        );
        assert_eq!(
            mismatch(json!({ "body": "${ body.user.name == 'Bob' }" }), &request),
            failure("body", json!("${ body.user.name == 'Bob' }"), json!(false))
        );
    }

    #[test]
    fn grpc_failed_condition_names_field_expected_and_actual() {
        let envelope = json!({
            "service": "approvals.v1.Approvals",
            "method": "GetApproval",
            "metadata": { "x-tenant": "acme" },
            "message": { "id": "a-1" },
        });
        let mismatch = |case_match: Value| grpc_mismatch(&case_match, &envelope);
        assert_eq!(mismatch(json!({ "message": { "id": "a-1" } })), None);
        assert_eq!(
            mismatch(json!({ "message": { "id": "a-2" } })),
            failure("message.id", json!("a-2"), json!("a-1"))
        );
        assert_eq!(
            mismatch(json!({ "metadata": { "x-tenant": "other" } })),
            failure("metadata.x-tenant", json!("other"), json!("acme"))
        );
        assert_eq!(
            mismatch(json!("${ message.id == 'a-2' }")),
            failure("match", json!("${ message.id == 'a-2' }"), json!(false))
        );
    }

    #[test]
    fn a_malformed_match_fails_naming_the_unusable_part() {
        let request = envelope("/", &[], Value::Null);
        assert_eq!(
            mismatch(json!(7), &request),
            failure("match", json!(7), Value::Null)
        );
        assert_eq!(
            mismatch(json!({ "method": "GET" }), &request),
            failure("method", json!("GET"), Value::Null)
        );
        assert_eq!(
            mismatch(json!({ "query": "page" }), &request),
            failure("query", json!("page"), Value::Null)
        );

        let envelope = json!({ "message": {}, "metadata": {} });
        let mismatch = |case_match: Value| grpc_mismatch(&case_match, &envelope);
        assert_eq!(
            mismatch(json!([1])),
            failure("match", json!([1]), Value::Null)
        );
        assert_eq!(
            mismatch(json!({ "service": "x" })),
            failure("service", json!("x"), Value::Null)
        );
    }

    #[test]
    fn repeated_query_keys_are_arrays_for_predicates() {
        let request = envelope("/?tag=a&tag=b", &[], Value::Null);
        assert!(matches(json!("${ contains(query.tag, 'b') }"), &request));
    }
}
