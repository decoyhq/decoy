use std::collections::BTreeMap;
use std::sync::OnceLock;

use bytes::Bytes;

use serde_json::{Value, json};

use crate::schema::HttpMethod;

/// The request envelope: what Case matching, predicates, and templates see about an HTTP request,
/// besides method, path, and path params.
#[derive(Debug, Clone, Default)]
pub struct RequestEnvelope {
    /// Full URL as Decoy received it, e.g. `http://127.0.0.1:8080/api/users?verbose=true`.
    pub url: String,
    /// Decoded query parameters; a repeated key keeps every value in order.
    pub query: BTreeMap<String, Vec<String>>,
    /// Lowercased header names; repeated headers are joined with `", "`. Read-only outside the
    /// crate: `content-type` feeds the cached [`Self::body`].
    pub(crate) headers: BTreeMap<String, String>,
    /// Parsed from the `cookie` header.
    pub cookies: BTreeMap<String, String>,
    /// The body as received; [`Self::body`] parses it on first use.
    pub(crate) raw_body: Bytes,
    pub(crate) parsed_body: OnceLock<Value>,
}

/// Compares the request inputs, not whether [`RequestEnvelope::body`] has run.
impl PartialEq for RequestEnvelope {
    fn eq(&self, other: &Self) -> bool {
        self.url == other.url
            && self.query == other.query
            && self.headers == other.headers
            && self.cookies == other.cookies
            && self.raw_body == other.raw_body
    }
}

impl RequestEnvelope {
    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }

    /// JSON when the content type is JSON and it parses, text otherwise, `null` when empty.
    /// Parsed once, on first use: passthrough and Cases that never read `body` skip it.
    pub fn body(&self) -> &Value {
        self.parsed_body.get_or_init(|| {
            parse_body(
                self.headers.get("content-type").map(String::as_str),
                &self.raw_body,
            )
        })
    }

    /// What predicates and templates see: `{ method, url, path, pathParams, query, headers,
    /// cookies, body }`. A query key is a string, or an array when it repeats.
    pub fn to_json(
        &self,
        method: &HttpMethod,
        path: &str,
        path_params: &BTreeMap<String, String>,
    ) -> Value {
        let query: serde_json::Map<String, Value> = self
            .query
            .iter()
            .map(|(key, values)| {
                let value = match values.as_slice() {
                    [single] => json!(single),
                    many => json!(many),
                };
                (key.clone(), value)
            })
            .collect();
        json!({
            "method": method,
            "url": self.url,
            "path": path,
            "pathParams": path_params,
            "query": query,
            "headers": self.headers,
            "cookies": self.cookies,
            "body": self.body(),
        })
    }

    pub fn from_parts(
        base_url: Option<&str>,
        path_and_query: &str,
        headers: &BTreeMap<String, String>,
        body: Bytes,
    ) -> Self {
        let headers: BTreeMap<String, String> = headers
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
            .collect();

        Self {
            url: format!("{}{path_and_query}", base_url.unwrap_or_default()),
            query: parse_query(path_and_query),
            cookies: headers
                .get("cookie")
                .map(|cookie| parse_cookies(cookie))
                .unwrap_or_default(),
            headers,
            raw_body: body,
            parsed_body: OnceLock::new(),
        }
    }
}

fn parse_query(path_and_query: &str) -> BTreeMap<String, Vec<String>> {
    let mut query: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some((_, raw)) = path_and_query.split_once('?') {
        for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
            query
                .entry(key.into_owned())
                .or_default()
                .push(value.into_owned());
        }
    }
    query
}

fn parse_cookies(header: &str) -> BTreeMap<String, String> {
    header
        .split(';')
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            Some((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

fn parse_body(content_type: Option<&str>, body: &[u8]) -> Value {
    if body.is_empty() {
        return Value::Null;
    }
    let is_json = content_type.is_some_and(|content_type| {
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        mime == "application/json" || mime.ends_with("+json")
    });
    if is_json && let Ok(value) = serde_json::from_slice(body) {
        return value;
    }
    Value::String(String::from_utf8_lossy(body).into_owned())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn parses_url_query_headers_and_cookies() {
        let envelope = RequestEnvelope::from_parts(
            Some("http://127.0.0.1:8080"),
            "/api/users?tag=a&tag=b&name=Ada%20L&flag",
            &headers(&[
                ("Content-Type", "text/plain"),
                ("cookie", "sid=abc; theme=dark"),
            ]),
            Bytes::new(),
        );

        assert_eq!(
            envelope.url,
            "http://127.0.0.1:8080/api/users?tag=a&tag=b&name=Ada%20L&flag"
        );
        assert_eq!(
            envelope.query,
            BTreeMap::from([
                ("flag".to_owned(), vec![String::new()]),
                ("name".to_owned(), vec!["Ada L".to_owned()]),
                ("tag".to_owned(), vec!["a".to_owned(), "b".to_owned()]),
            ])
        );
        assert_eq!(envelope.headers["content-type"], "text/plain");
        assert_eq!(envelope.cookies["sid"], "abc");
        assert_eq!(envelope.cookies["theme"], "dark");
        assert_eq!(envelope.body(), &Value::Null);
    }

    #[test]
    fn parses_json_bodies_by_content_type_and_falls_back_to_text() {
        let json_body = |content_type: &str, body: &str| {
            RequestEnvelope::from_parts(
                None,
                "/",
                &headers(&[("content-type", content_type)]),
                Bytes::copy_from_slice(body.as_bytes()),
            )
            .body()
            .clone()
        };

        assert_eq!(
            json_body("application/json; charset=utf-8", r#"{"a":1}"#),
            json!({ "a": 1 })
        );
        assert_eq!(json_body("application/vnd.api+json", r#"[1]"#), json!([1]));
        assert_eq!(json_body("application/json", "{oops"), json!("{oops"));
        assert_eq!(json_body("text/plain", r#"{"a":1}"#), json!(r#"{"a":1}"#));
    }

    #[test]
    fn parses_the_body_on_first_use_only() {
        let envelope = RequestEnvelope::from_parts(
            None,
            "/",
            &headers(&[("content-type", "application/json")]),
            r#"{"a":1}"#.into(),
        );
        assert_eq!(envelope.parsed_body.get(), None);

        let first: *const Value = envelope.body();
        assert_eq!(envelope.body(), &json!({ "a": 1 }));
        assert!(std::ptr::eq(first, envelope.body()));
    }

    #[test]
    fn equality_ignores_whether_the_body_was_parsed() {
        let envelope = || {
            RequestEnvelope::from_parts(
                None,
                "/",
                &headers(&[("content-type", "application/json")]),
                r#"{"a":1}"#.into(),
            )
        };
        let parsed = envelope();
        parsed.body();

        assert_eq!(parsed, envelope());
    }
}
