use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use thiserror::Error;

use crate::control_api::SESSION_HEADER;
use crate::http::PassthroughPlan;
use crate::schema::HttpMethod;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardRequest {
    pub method: HttpMethod,
    pub path_and_query: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardResponse {
    pub status: u16,
    /// In order, repeats kept (e.g. several `set-cookie`).
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl ForwardResponse {
    /// The first value of a header, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Error)]
pub enum ForwardingError {
    #[error("passthrough behavior selected but no upstream target was resolved")]
    MissingTarget,
    #[error("invalid passthrough target URL `{url}`: {source}")]
    InvalidTargetUrl {
        url: String,
        #[source]
        source: url::ParseError,
    },
    #[error("invalid forwarded request header `{name}`")]
    InvalidRequestHeaderName {
        name: String,
        #[source]
        source: reqwest::header::InvalidHeaderName,
    },
    #[error("invalid forwarded request header `{name}` value")]
    InvalidRequestHeaderValue {
        name: String,
        #[source]
        source: reqwest::header::InvalidHeaderValue,
    },
    #[error("passthrough loop: this Decoy already forwarded the request (`{VIA_HEADER}`)")]
    Loop,
    #[error("passthrough upstream timed out: {0}")]
    Timeout(#[source] reqwest::Error),
    #[error("passthrough request failed: {0}")]
    Request(#[source] reqwest::Error),
}

impl ForwardingError {
    /// What Decoy answers when forwarding fails.
    pub fn response(&self) -> ForwardResponse {
        let status = match self {
            Self::Loop => 508,
            Self::Timeout(_) => 504,
            _ => 502,
        };
        let body = serde_json::json!({
            "error": "passthrough forwarding failed",
            "detail": self.to_string(),
        });
        ForwardResponse {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: serde_json::to_vec(&body).expect("JSON response serializes"),
        }
    }
}

impl From<reqwest::Error> for ForwardingError {
    fn from(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout(error)
        } else {
            Self::Request(error)
        }
    }
}

/// Lists the Decoy processes a request passed through, so one that comes back is refused instead
/// of forwarded forever (a target that points at Decoy itself). Chains of distinct Decoys still
/// forward, but `x-mock-session` isn't passed on, so a downstream Decoy uses its default Session.
pub const VIA_HEADER: &str = "x-decoy-via";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

fn instance_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| format!("{:016x}", fastrand::u64(..)))
}

/// One pooled client for every passthrough, with timeouts so a hung upstream can't hang Decoy.
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("the passthrough client config is valid")
    })
}

const ALWAYS_STRIPPED_REQUEST_HEADERS: &[&str] = &[
    "host",
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
];

const STRIPPED_RESPONSE_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
];

pub async fn forward_passthrough(
    plan: &PassthroughPlan,
    request: ForwardRequest,
) -> Result<ForwardResponse, ForwardingError> {
    let Some(base_url) = &plan.base_url else {
        return Err(ForwardingError::MissingTarget);
    };

    let via = request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(VIA_HEADER))
        .map(|(_, value)| value.as_str());
    if via.is_some_and(|via| via.split(',').any(|id| id.trim() == instance_id())) {
        return Err(ForwardingError::Loop);
    }
    let via = match via {
        Some(via) => format!("{via}, {}", instance_id()),
        None => instance_id().to_owned(),
    };

    let url = target_url(base_url, &request.path_and_query)?;
    let mut headers = forwardable_request_headers(&request.headers)?;
    headers.insert(
        HeaderName::from_static(VIA_HEADER),
        HeaderValue::from_str(&via).map_err(|source| {
            ForwardingError::InvalidRequestHeaderValue {
                name: VIA_HEADER.to_owned(),
                source,
            }
        })?,
    );
    let response = client()
        .request(reqwest_method(&request.method), url)
        .headers(headers)
        .body(request.body)
        .send()
        .await?;

    let status = response.status().as_u16();
    let headers = forwardable_response_headers(response.headers());
    let body = response.bytes().await?.to_vec();

    Ok(ForwardResponse {
        status,
        headers,
        body,
    })
}

pub(crate) fn target_url(
    base_url: &str,
    path_and_query: &str,
) -> Result<url::Url, ForwardingError> {
    let normalized_path = if path_and_query.starts_with('/') {
        path_and_query.to_owned()
    } else {
        format!("/{path_and_query}")
    };
    let joined = format!("{}{}", base_url.trim_end_matches('/'), normalized_path);
    url::Url::parse(&joined).map_err(|source| ForwardingError::InvalidTargetUrl {
        url: joined,
        source,
    })
}

fn reqwest_method(method: &HttpMethod) -> reqwest::Method {
    match method {
        HttpMethod::Get => reqwest::Method::GET,
        HttpMethod::Post => reqwest::Method::POST,
        HttpMethod::Put => reqwest::Method::PUT,
        HttpMethod::Patch => reqwest::Method::PATCH,
        HttpMethod::Delete => reqwest::Method::DELETE,
        HttpMethod::Head => reqwest::Method::HEAD,
        HttpMethod::Options => reqwest::Method::OPTIONS,
    }
}

fn forwardable_request_headers(
    headers: &BTreeMap<String, String>,
) -> Result<HeaderMap, ForwardingError> {
    let stripped = stripped_request_headers(headers);
    let mut out = HeaderMap::new();

    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        // The Session header is Decoy's own; the via list is rebuilt by the caller.
        if stripped.contains(&lower) || lower == SESSION_HEADER || lower == VIA_HEADER {
            continue;
        }
        // Non-UTF-8 bytes were replaced with U+FFFD for matching; forwarding that would send bytes
        // the client never did, so the header stays behind (as before), but not silently.
        if value.contains('\u{fffd}') {
            eprintln!("decoy serve: passthrough left out header `{name}`: not valid UTF-8");
            continue;
        }
        let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|source| {
            ForwardingError::InvalidRequestHeaderName {
                name: name.clone(),
                source,
            }
        })?;
        let header_value = HeaderValue::from_str(value).map_err(|source| {
            ForwardingError::InvalidRequestHeaderValue {
                name: name.clone(),
                source,
            }
        })?;
        out.insert(header_name, header_value);
    }

    Ok(out)
}

fn stripped_request_headers(headers: &BTreeMap<String, String>) -> BTreeSet<String> {
    let mut stripped = ALWAYS_STRIPPED_REQUEST_HEADERS
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();

    for (name, value) in headers {
        if name.eq_ignore_ascii_case("connection") {
            for token in value.split(',') {
                let token = token.trim().to_ascii_lowercase();
                if !token.is_empty() {
                    stripped.insert(token);
                }
            }
        }
    }

    stripped
}

fn forwardable_response_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, value) in headers {
        if STRIPPED_RESPONSE_HEADERS
            .iter()
            .any(|stripped| name.as_str().eq_ignore_ascii_case(stripped))
        {
            continue;
        }
        match value.to_str() {
            Ok(value) => out.push((name.as_str().to_owned(), value.to_owned())),
            Err(_) => eprintln!(
                "decoy serve: passthrough left out upstream header `{name}`: not valid UTF-8"
            ),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use pretty_assertions::assert_eq;

    use super::*;

    #[derive(Debug)]
    struct CapturedRequest {
        request_line: String,
        headers: BTreeMap<String, String>,
        body: String,
    }

    fn spawn_upstream() -> (
        String,
        mpsc::Receiver<CapturedRequest>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = Vec::new();
            let mut chunk = [0; 1024];
            loop {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    let text = String::from_utf8_lossy(&buffer);
                    let headers_end = text.find("\r\n\r\n").unwrap();
                    let head = &text[..headers_end];
                    let content_length = head
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    let body_start = headers_end + 4;
                    if buffer.len() >= body_start + content_length {
                        break;
                    }
                }
            }

            let header_end = buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap();
            let head = String::from_utf8_lossy(&buffer[..header_end]);
            let mut lines = head.lines();
            let request_line = lines.next().unwrap().to_owned();
            let mut headers = BTreeMap::new();
            for line in lines {
                let (name, value) = line.split_once(':').unwrap();
                headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
            }
            let body = String::from_utf8_lossy(&buffer[header_end + 4..]).to_string();
            tx.send(CapturedRequest {
                request_line,
                headers,
                body,
            })
            .unwrap();

            stream
                .write_all(
                    b"HTTP/1.1 207 Multi-Status\r\ncontent-type: application/json\r\nx-from-upstream: yes\r\nset-cookie: a=1\r\nset-cookie: b=2\r\nconnection: close\r\ncontent-length: 11\r\n\r\n{\"ok\":true}",
                )
                .unwrap();
        });

        (format!("http://{addr}"), rx, handle)
    }

    #[tokio::test]
    async fn forwards_method_path_query_headers_and_body_to_resolved_target() {
        let (target, received, handle) = spawn_upstream();
        let response = forward_passthrough(
            &PassthroughPlan {
                base_url: Some(format!("{target}/")),
            },
            ForwardRequest {
                method: HttpMethod::Post,
                path_and_query: "/orders?page=2".to_owned(),
                headers: BTreeMap::from([
                    ("content-type".to_owned(), "application/json".to_owned()),
                    ("x-keep".to_owned(), "yes".to_owned()),
                    ("host".to_owned(), "decoy.local".to_owned()),
                    ("connection".to_owned(), "x-remove, keep-alive".to_owned()),
                    ("x-remove".to_owned(), "no".to_owned()),
                    ("content-length".to_owned(), "999".to_owned()),
                    ("x-mock-session".to_owned(), "s1".to_owned()),
                    (VIA_HEADER.to_owned(), "other-decoy".to_owned()),
                ]),
                body: br#"{"item":"x"}"#.to_vec(),
            },
        )
        .await
        .unwrap();

        assert_eq!(response.status, 207);
        assert_eq!(response.header("x-from-upstream"), Some("yes"));
        assert_eq!(response.header("content-length"), None);
        let cookies: Vec<&str> = response
            .headers
            .iter()
            .filter(|(name, _)| name == "set-cookie")
            .map(|(_, value)| value.as_str())
            .collect();
        assert_eq!(cookies, ["a=1", "b=2"]);
        assert_eq!(response.body, br#"{"ok":true}"#);

        let request = received.recv().unwrap();
        handle.join().unwrap();
        assert_eq!(request.request_line, "POST /orders?page=2 HTTP/1.1");
        assert_eq!(request.headers.get("x-keep"), Some(&"yes".to_owned()));
        assert_eq!(request.headers.get("x-remove"), None);
        assert_eq!(request.headers.get("x-mock-session"), None);
        assert_eq!(
            request.headers.get(VIA_HEADER),
            Some(&format!("other-decoy, {}", instance_id()))
        );
        assert_ne!(request.headers.get("host"), Some(&"decoy.local".to_owned()));
        assert_ne!(
            request.headers.get("content-length"),
            Some(&"999".to_owned())
        );
        assert_eq!(request.body, r#"{"item":"x"}"#);
    }

    #[tokio::test]
    async fn a_request_this_decoy_already_forwarded_is_a_loop() {
        let error = forward_passthrough(
            &PassthroughPlan {
                base_url: Some("http://127.0.0.1:1".to_owned()),
            },
            ForwardRequest {
                method: HttpMethod::Get,
                path_and_query: "/".to_owned(),
                headers: BTreeMap::from([(
                    VIA_HEADER.to_owned(),
                    format!("other-decoy, {}", instance_id()),
                )]),
                body: Vec::new(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ForwardingError::Loop), "{error}");
        assert_eq!(error.response().status, 508);
    }

    #[tokio::test]
    async fn an_upstream_that_never_answers_is_a_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _hold = thread::spawn(move || listener.accept().map(|(stream, _)| stream));
        let error = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(100))
            .build()
            .unwrap()
            .get(format!("http://{addr}/"))
            .send()
            .await
            .unwrap_err();

        let error = ForwardingError::from(error);
        assert!(matches!(error, ForwardingError::Timeout(_)));
        assert_eq!(error.response().status, 504);
    }

    #[tokio::test]
    async fn missing_target_is_forwarding_error() {
        let error = forward_passthrough(
            &PassthroughPlan { base_url: None },
            ForwardRequest {
                method: HttpMethod::Get,
                path_and_query: "/users/123".to_owned(),
                headers: BTreeMap::new(),
                body: Vec::new(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ForwardingError::MissingTarget));
    }

    #[tokio::test]
    async fn request_failure_is_forwarding_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let error = forward_passthrough(
            &PassthroughPlan {
                base_url: Some(format!("http://{addr}")),
            },
            ForwardRequest {
                method: HttpMethod::Get,
                path_and_query: "/users/123".to_owned(),
                headers: BTreeMap::new(),
                body: Vec::new(),
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ForwardingError::Request(_)));
        let response = error.response();
        assert_eq!(response.status, 502);
        assert_eq!(response.header("content-type"), Some("application/json"));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&response.body).unwrap(),
            serde_json::json!({
                "error": "passthrough forwarding failed",
                "detail": error.to_string(),
            })
        );
    }

    #[tokio::test]
    async fn redirects_are_returned_instead_of_followed() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 1024];
            let _ = stream.read(&mut buffer).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:9/elsewhere\r\ncontent-length: 0\r\n\r\n",
                )
                .unwrap();
        });

        let response = forward_passthrough(
            &PassthroughPlan {
                base_url: Some(format!("http://{addr}")),
            },
            ForwardRequest {
                method: HttpMethod::Get,
                path_and_query: "/users/123".to_owned(),
                headers: BTreeMap::new(),
                body: Vec::new(),
            },
        )
        .await
        .unwrap();

        handle.join().unwrap();
        assert_eq!(response.status, 302);
        assert_eq!(
            response.header("location"),
            Some("http://127.0.0.1:9/elsewhere")
        );
    }
}
