//! `${ }` templating in response Behaviors: every string at any depth of `status`, `headers`, and
//! `body` is rendered against the request; keys are not. See the spec in issue #215.

use jmespath::Rcvar;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::expression::{ExprError, Segment, evaluate, segments, validate_expression};
use crate::schema::{ResponseBehavior, StreamItem};

/// Whether rendering could change anything (`\${` escapes count, so they get unescaped).
pub fn has_templates(response: &ResponseBehavior) -> bool {
    response
        .status
        .iter()
        .chain(response.body.iter())
        .any(value_has_templates)
        || response.headers.values().any(|value| value.contains("${"))
}

fn value_has_templates(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains("${"),
        Value::Array(items) => items.iter().any(value_has_templates),
        Value::Object(fields) => fields.values().any(value_has_templates),
        _ => false,
    }
}

/// Startup check: every template parses, and `status` is an integer 100–599 or a template.
pub fn validate_response(response: &ResponseBehavior) -> Result<(), String> {
    if response.message.is_some() || !response.metadata.is_empty() {
        return Err(
            "`message` and `metadata` are for gRPC Routes; use `body` and `headers`".to_owned(),
        );
    }
    if response.stream.is_some() {
        return Err("`stream` is for server-streaming gRPC Routes".to_owned());
    }
    if response.messages.is_some() || response.close.is_some() {
        return Err("`messages` and `close` are for WebSocket Routes".to_owned());
    }
    match &response.status {
        None => {}
        Some(Value::Number(number)) => {
            status_code(number.as_u64())
                .ok_or_else(|| format!("status {number} is not 100–599"))?;
        }
        Some(Value::String(text)) if text.contains("${") => validate_string(text)?,
        Some(other) => {
            return Err(format!(
                "status must be an integer or a `${{ }}` template, got {other}"
            ));
        }
    }
    for (name, value) in &response.headers {
        axum::http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("`{name}` is not a valid HTTP header name"))?;
        // A templated value is checked once rendered, when the request comes in.
        if !value.contains("${") {
            axum::http::HeaderValue::from_str(value)
                .map_err(|_| format!("header `{name}` has a value HTTP doesn't allow"))?;
        }
        validate_string(value)?;
    }
    response.body.iter().try_for_each(validate_value)
}

pub(crate) fn validate_value(value: &Value) -> Result<(), String> {
    match value {
        Value::String(text) => validate_string(text),
        Value::Array(items) => items.iter().try_for_each(validate_value),
        Value::Object(fields) => fields.values().try_for_each(validate_value),
        _ => Ok(()),
    }
}

pub(crate) fn validate_string(text: &str) -> Result<(), String> {
    for segment in segments(text).map_err(|error| error.to_string())? {
        if let Segment::Expression(source) = segment {
            validate_expression(&source).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// Why a templated response didn't render. The text reaches the template-error body.
#[derive(Debug, Error)]
pub enum TemplateError {
    #[error(transparent)]
    Expr(#[from] ExprError),
    #[error("templated status rendered {rendered}, not 100–599")]
    Status { rendered: Value },
}

/// The request envelope for templates, converted for JMESPath only if an expression needs it.
pub type Request<'a> = &'a dyn Fn() -> Rcvar;

/// Render a templated response against the request envelope. A templated `status` must come out
/// as an integer 100–599.
pub fn render_response(
    response: &ResponseBehavior,
    request: Request,
) -> Result<ResponseBehavior, TemplateError> {
    let status = match &response.status {
        Some(status) => {
            let rendered = render_value(status, request)?;
            let code = rendered
                .as_u64()
                .or_else(|| rendered.as_str()?.trim().parse().ok());
            let code = status_code(code).ok_or(TemplateError::Status { rendered })?;
            Some(Value::from(code))
        }
        None => None,
    };
    let headers = response
        .headers
        .iter()
        .map(|(name, value)| Ok((name.clone(), render_text(value, request)?)))
        .collect::<Result<_, ExprError>>()?;
    let body = response
        .body
        .as_ref()
        .map(|body| render_value(body, request))
        .transpose()?;
    Ok(ResponseBehavior {
        status,
        headers,
        body,
        message: response.message.clone(),
        metadata: response.metadata.clone(),
        stream: response.stream.clone(),
        messages: response.messages.clone(),
        close: response.close.clone(),
        delay: response.delay,
    })
}

/// Render a gRPC response Behavior: `status`, `message`, `metadata`, and `stream` messages. Whether the result
/// is a valid status or message is checked when it is encoded.
pub fn render_grpc_response(
    response: &ResponseBehavior,
    request: Request,
) -> Result<ResponseBehavior, TemplateError> {
    let render = |value: &Option<Value>| {
        value
            .as_ref()
            .map(|value| render_value(value, request))
            .transpose()
    };
    Ok(ResponseBehavior {
        status: render(&response.status)?,
        message: render(&response.message)?,
        metadata: response
            .metadata
            .iter()
            .map(|(name, value)| Ok((name.clone(), render_text(value, request)?)))
            .collect::<Result<_, ExprError>>()?,
        stream: render_items(&response.stream, request)?,
        ..response.clone()
    })
}

/// Render a WebSocket Behavior: `message` and each `messages` item.
pub fn render_ws_response(
    response: &ResponseBehavior,
    request: Request,
) -> Result<ResponseBehavior, TemplateError> {
    Ok(ResponseBehavior {
        message: response
            .message
            .as_ref()
            .map(|value| render_value(value, request))
            .transpose()?,
        messages: render_items(&response.messages, request)?,
        ..response.clone()
    })
}

fn render_items(
    items: &Option<Vec<StreamItem>>,
    request: Request,
) -> Result<Option<Vec<StreamItem>>, ExprError> {
    items
        .as_ref()
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    Ok(StreamItem {
                        message: render_value(&item.message, request)?,
                        delay: item.delay,
                    })
                })
                .collect()
        })
        .transpose()
}

fn status_code(code: Option<u64>) -> Option<u16> {
    code.filter(|code| (100..=599).contains(code))
        .and_then(|code| u16::try_from(code).ok())
}

fn render_value(value: &Value, request: Request) -> Result<Value, ExprError> {
    match value {
        Value::String(text) => render_string(text, request),
        Value::Array(items) => items
            .iter()
            .map(|item| render_value(item, request))
            .collect::<Result<_, _>>()
            .map(Value::Array),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, item)| Ok((key.clone(), render_value(item, request)?)))
            .collect::<Result<Map<_, _>, ExprError>>()
            .map(Value::Object),
        other => Ok(other.clone()),
    }
}

/// Exactly one `${ }` keeps the result's JSON type; anything else interpolates into text.
fn render_string(text: &str, request: Request) -> Result<Value, ExprError> {
    let segments = segments(text)?;
    match segments.as_slice() {
        [Segment::Expression(source)] => evaluate(source, &request()),
        _ => interpolate(segments, request).map(Value::String),
    }
}

fn render_text(text: &str, request: Request) -> Result<String, ExprError> {
    interpolate(segments(text)?, request)
}

fn interpolate(segments: Vec<Segment>, request: Request) -> Result<String, ExprError> {
    segments
        .into_iter()
        .map(|segment| match segment {
            Segment::Text(text) => Ok(text),
            Segment::Expression(source) => Ok(match evaluate(&source, &request())? {
                Value::Null => String::new(),
                Value::String(text) => text,
                other => other.to_string(),
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn response(status: Option<Value>, headers: &[(&str, &str)], body: Value) -> ResponseBehavior {
        ResponseBehavior {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect::<BTreeMap<_, _>>(),
            body: Some(body),
            message: None,
            metadata: BTreeMap::new(),
            stream: None,
            messages: None,
            close: None,
            delay: None,
        }
    }

    fn request() -> Rcvar {
        crate::expression::to_rcvar(&json!({
            "pathParams": { "id": "42" },
            "query": { "n": "3" },
            "body": { "name": "Ada", "age": 36, "admin": true, "tags": ["a"], "nested": { "k": 1 } }
        }))
    }

    fn render_body(body: Value) -> Value {
        render_response(&response(None, &[], body), &request)
            .unwrap()
            .body
            .unwrap()
    }

    #[test]
    fn whole_string_keeps_the_type() {
        assert_eq!(
            render_body(json!({
                "age": "${ body.age }",
                "admin": "${ body.admin }",
                "tags": "${ body.tags }",
                "nested": "${ body.nested }",
                "id": "${ pathParams.id }",
                "missing": "${ body.nope }"
            })),
            json!({
                "age": 36, "admin": true, "tags": ["a"], "nested": { "k": 1 }, "id": "42", "missing": null
            })
        );
    }

    #[test]
    fn interpolation_renders_text() {
        assert_eq!(
            render_body(json!(
                "Hi ${ body.name } (${ body.age }) ${ body.nope }${ body.nested }"
            )),
            json!(r#"Hi Ada (36) {"k":1}"#)
        );
        assert_eq!(
            render_body(json!("${ body.name }${ body.age }")),
            json!("Ada36")
        );
    }

    #[test]
    fn renders_nested_strings_but_not_keys_or_non_strings() {
        assert_eq!(
            render_body(json!({
                "${ body.name }": [1, { "who": "${ body.name }", "n": 2 }],
                "flag": false
            })),
            json!({ "${ body.name }": [1, { "who": "Ada", "n": 2 }], "flag": false })
        );
    }

    #[test]
    fn escape_produces_a_literal_dollar_brace() {
        assert_eq!(
            render_body(json!(r"\${ not } ${ body.name }")),
            json!("${ not } Ada")
        );
    }

    #[test]
    fn renders_headers_and_status() {
        let rendered = render_response(
            &response(
                Some(json!("${ `201` }")),
                &[("x-user", "user-${ pathParams.id }")],
                json!(null),
            ),
            &request,
        )
        .unwrap();
        assert_eq!(rendered.status, Some(json!(201)));
        assert_eq!(rendered.headers["x-user"], "user-42");

        let from_string = render_response(
            &response(Some(json!("20${ query.n }")), &[], json!(null)),
            &request,
        )
        .unwrap();
        assert_eq!(from_string.status, Some(json!(203)));
    }

    #[test]
    fn templated_status_must_be_a_valid_code() {
        let error = render_response(
            &response(Some(json!("${ body.name }")), &[], json!(null)),
            &request,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            r#"templated status rendered "Ada", not 100–599"#
        );
    }

    #[test]
    fn startup_validation_catches_bad_templates_and_statuses() {
        assert!(
            validate_response(&response(Some(json!(200)), &[], json!("${ body.name }"))).is_ok()
        );
        assert!(validate_response(&response(Some(json!(99)), &[], json!(null))).is_err());
        assert!(validate_response(&response(Some(json!("ok")), &[], json!(null))).is_err());
        assert!(
            validate_response(&response(None, &[], json!({ "a": ["${ body.[ }"] })))
                .unwrap_err()
                .contains("invalid JMESPath")
        );
        assert!(validate_response(&response(None, &[("x", "${ body.name")], json!(null))).is_err());
        // Headers that would be dropped at response time fail at startup instead.
        assert_eq!(
            validate_response(&response(None, &[("bad name", "v")], json!(null))).unwrap_err(),
            "`bad name` is not a valid HTTP header name"
        );
        assert_eq!(
            validate_response(&response(None, &[("x-a", "line\nbreak")], json!(null))).unwrap_err(),
            "header `x-a` has a value HTTP doesn't allow"
        );
        assert!(
            validate_response(&response(None, &[("x-a", "${ body.name }")], json!(null))).is_ok()
        );
    }

    #[test]
    fn detects_templates_for_the_fast_path() {
        assert!(!has_templates(&response(
            Some(json!(200)),
            &[("a", "b")],
            json!({ "x": [1, "y"] })
        )));
        assert!(has_templates(&response(
            None,
            &[],
            json!({ "x": ["${ y }"] })
        )));
        assert!(has_templates(&response(
            None,
            &[("a", "${ b }")],
            json!(null)
        )));
    }
}
