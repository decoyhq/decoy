use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::schema::HttpMethod;

/// What Case matching (and later predicates and templates) can see about an HTTP request, besides
/// method, path, and path params.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RequestContext {
    /// Full URL as Decoy received it, e.g. `http://127.0.0.1:8080/api/users?verbose=true`.
    pub url: String,
    /// Decoded query parameters; a repeated key keeps every value in order.
    pub query: BTreeMap<String, Vec<String>>,
    /// Lowercased header names; repeated headers are joined with `", "`.
    pub headers: BTreeMap<String, String>,
    /// Parsed from the `cookie` header.
    pub cookies: BTreeMap<String, String>,
    /// JSON when the content type is JSON and it parses, text otherwise, `null` when empty.
    pub body: Value,
}

impl RequestContext {
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
            "body": self.body,
        })
    }

    pub fn from_parts(
        base_url: Option<&str>,
        path_and_query: &str,
        headers: &BTreeMap<String, String>,
        body: &[u8],
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
            body: parse_body(headers.get("content-type").map(String::as_str), body),
            headers,
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
        let context = RequestContext::from_parts(
            Some("http://127.0.0.1:8080"),
            "/api/users?tag=a&tag=b&name=Ada%20L&flag",
            &headers(&[
                ("Content-Type", "text/plain"),
                ("cookie", "sid=abc; theme=dark"),
            ]),
            b"",
        );

        assert_eq!(
            context.url,
            "http://127.0.0.1:8080/api/users?tag=a&tag=b&name=Ada%20L&flag"
        );
        assert_eq!(
            context.query,
            BTreeMap::from([
                ("flag".to_owned(), vec![String::new()]),
                ("name".to_owned(), vec!["Ada L".to_owned()]),
                ("tag".to_owned(), vec!["a".to_owned(), "b".to_owned()]),
            ])
        );
        assert_eq!(context.headers["content-type"], "text/plain");
        assert_eq!(context.cookies["sid"], "abc");
        assert_eq!(context.cookies["theme"], "dark");
        assert_eq!(context.body, Value::Null);
    }

    #[test]
    fn parses_json_bodies_by_content_type_and_falls_back_to_text() {
        let json_body = |content_type: &str, body: &str| {
            RequestContext::from_parts(
                None,
                "/",
                &headers(&[("content-type", content_type)]),
                body.as_bytes(),
            )
            .body
        };

        assert_eq!(
            json_body("application/json; charset=utf-8", r#"{"a":1}"#),
            json!({ "a": 1 })
        );
        assert_eq!(json_body("application/vnd.api+json", r#"[1]"#), json!([1]));
        assert_eq!(json_body("application/json", "{oops"), json!("{oops"));
        assert_eq!(json_body("text/plain", r#"{"a":1}"#), json!(r#"{"a":1}"#));
    }
}
