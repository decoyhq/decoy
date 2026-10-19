//! Fixtures shared by the engine and transport unit tests.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use super::{Candidate, Catalog, Rejection};
use crate::collections::CollectionsFile;
use crate::http::{HttpRequest, RuntimeConfig};
use crate::schema::{HttpMethod, PassthroughTarget, Route};

#[derive(Debug)]
pub(crate) struct CapturedRequest {
    pub(crate) request_line: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: String,
}

pub(crate) fn spawn_upstream() -> (
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
                b"HTTP/1.1 202 Accepted\r\ncontent-type: text/plain\r\nx-upstream: yes\r\ncontent-length: 8\r\n\r\naccepted",
            )
            .unwrap();
    });

    (format!("http://{addr}"), rx, handle)
}

pub(crate) fn get_user_route() -> Route {
    Route::from_yaml(
        r#"
id: get-user
transport: http
match:
  method: GET
  path: /users/{id}
passthrough:
  baseUrl: https://route.example.test
cases:
  any:
    match:
      pathParams:
        id: "*"
    behaviors:
      fallback:
        status: 200
        body:
          kind: fallback
  user-123:
    match:
      pathParams:
        id: "123"
    behaviors:
      success:
        status: 200
        body:
          id: "123"
      missing:
        status: 404
        body:
          error: missing
      real:
        kind: passthrough
"#,
    )
    .unwrap()
}

pub(crate) fn catalog(collections: &str) -> Catalog {
    Catalog::new(
        vec![get_user_route()],
        CollectionsFile::from_yaml(collections).unwrap(),
        RuntimeConfig {
            passthrough: Some(PassthroughTarget {
                base_url: "https://runtime.example.test".to_owned(),
            }),
            ..RuntimeConfig::default()
        },
    )
    .unwrap()
}

pub(crate) fn candidate(address: &str, rejection: Rejection) -> Candidate {
    Candidate {
        address: address.to_owned(),
        rejection,
    }
}

pub(crate) fn explain_catalog() -> Catalog {
    let create_user = Route::from_yaml(
        "id: create-user\ntransport: http\nmatch: { method: POST, path: '/users/{id}' }\ncases:\n  any:\n    behaviors:\n      ok: { status: 201 }\n",
    )
    .unwrap();
    let list_users = Route::from_yaml(
        "id: list-users\ntransport: http\nmatch: { method: GET, path: /users }\ncases:\n  any:\n    behaviors:\n      ok: { status: 200 }\n",
    )
    .unwrap();
    Catalog::new(
        vec![get_user_route(), create_user, list_users],
        CollectionsFile::from_yaml(
            r#"
- id: local
  routes:
    - get-user:any:fallback
    - get-user:any:ghost
    - get-user:user-123:success
    - get-user:ghost:ok
    - list-users:any:ok
    - create-user:any:ok
    - ghost:any:ok
"#,
        )
        .unwrap(),
        RuntimeConfig::default(),
    )
    .unwrap()
}

pub(crate) fn get(path: &str) -> HttpRequest {
    HttpRequest {
        method: HttpMethod::Get,
        path: path.to_owned(),
        ..Default::default()
    }
}
