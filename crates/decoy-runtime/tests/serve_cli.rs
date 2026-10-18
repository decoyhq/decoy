use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

#[tokio::test]
async fn serve_loads_yaml_routes_collections_and_responds_over_http() {
    let fixture = Fixture::new();
    fixture.write_route(
        "z/fallback.yaml",
        r#"
id: users
transport: http
match:
  method: GET
  path: /users/{id}
cases:
  any:
    match:
      pathParams:
        id: "*"
    behaviors:
      fallback:
        status: 200
        headers:
          x-route: fallback
        body: fallback text
"#,
    );
    fixture.write_route(
        "a/user.yaml",
        r#"
id: users-by-id
transport: http
match:
  method: GET
  path: /users/{id}
cases:
  user-123:
    match:
      pathParams:
        id: "123"
    behaviors:
      success:
        status: 201
        headers:
          x-route: user
        body:
          id: "123"
          name: Ada
      missing:
        status: 404
        body: nope
"#,
    );
    fixture.write_collections(
        r#"
- id: z-default
  routes:
    - users:any:fallback
    - users-by-id:user-123:success

- id: a-inherited
  from: z-default
  routes:
    - users-by-id:user-123:missing
"#,
    );

    let port = unused_port();
    let _server = fixture.spawn(port, &[]);
    wait_for_server(port).await;

    let response = reqwest::get(format!("http://127.0.0.1:{port}/users/123"))
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    assert_eq!(response.headers()["x-route"], "user");
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({ "id": "123", "name": "Ada" })
    );

    let fallback = reqwest::get(format!("http://127.0.0.1:{port}/users/456"))
        .await
        .unwrap();
    assert_eq!(fallback.status(), 200);
    assert_eq!(fallback.text().await.unwrap(), "fallback text");

    let miss = reqwest::get(format!("http://127.0.0.1:{port}/missing"))
        .await
        .unwrap();
    assert_eq!(miss.status(), 501);
    assert_eq!(miss.headers()["x-mock-miss"], "true");
    assert_eq!(
        miss.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({ "error": "no active route case matched request" })
    );
}

#[tokio::test]
async fn serve_uses_cli_collection_override_and_accepts_session_header() {
    let fixture = Fixture::new();
    fixture.write_route(
        "users.yml",
        r#"
id: users-by-id
transport: http
match:
  method: GET
  path: /users/{id}
cases:
  user-123:
    match:
      pathParams:
        id: "123"
    behaviors:
      success:
        status: 200
        body: ok
      missing:
        status: 404
        body: missing
"#,
    );
    fixture.write_collections(
        r#"
- id: default
  routes:
    - users-by-id:user-123:success

- id: not-found
  from: default
  routes:
    - users-by-id:user-123:missing
"#,
    );

    let port = unused_port();
    let _server = fixture.spawn(port, &["--collection", "not-found"]);
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://127.0.0.1:{port}/users/123"))
        .header("x-mock-session", "session-a")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    assert_eq!(response.text().await.unwrap(), "missing");
}

#[tokio::test]
async fn serve_forwards_selected_passthrough_over_the_cli_request_path() {
    let (upstream_url, captured, upstream) = spawn_upstream();
    let fixture = Fixture::new();
    fixture.write_route(
        "users.yml",
        format!(
            r#"
id: create-user
transport: http
match:
  method: POST
  path: /users
cases:
  any:
    match: {{}}
    behaviors:
      forward:
        kind: passthrough
        target:
          baseUrl: {upstream_url}
"#,
        ),
    );
    fixture.write_collections(
        r#"
- id: default
  routes:
    - create-user:any:forward
"#,
    );

    let port = unused_port();
    let _server = fixture.spawn(port, &[]);
    wait_for_server(port).await;

    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/users?verbose=true"))
        .header("x-decoy-test", "yes")
        .header("connection", "x-strip")
        .header("x-strip", "no")
        .body(r#"{"name":"Ada"}"#)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 202);
    assert_eq!(response.headers()["x-upstream"], "yes");
    assert_eq!(response.text().await.unwrap(), "accepted");
    let captured = captured.recv().unwrap();
    assert_eq!(captured.request_line, "POST /users?verbose=true HTTP/1.1");
    assert_eq!(
        captured.headers.get("x-decoy-test"),
        Some(&"yes".to_owned())
    );
    assert!(!captured.headers.contains_key("connection"));
    assert!(!captured.headers.contains_key("x-strip"));
    assert_eq!(captured.body, r#"{"name":"Ada"}"#);
    upstream.join().unwrap();
}

#[tokio::test]
async fn serve_uses_cli_global_passthrough_target_for_selected_passthrough() {
    let (upstream_url, captured, upstream) = spawn_upstream();
    let fixture = Fixture::new();
    fixture.write_route(
        "users.yml",
        r#"
id: users
transport: http
match:
  method: GET
  path: /users/{id}
cases:
  any:
    match:
      pathParams:
        id: "*"
    behaviors:
      forward:
        kind: passthrough
"#,
    );
    fixture.write_collections(
        r#"
- id: default
  routes:
    - users:any:forward
"#,
    );

    let port = unused_port();
    let _server = fixture.spawn(port, &["--passthrough-base-url", &upstream_url]);
    wait_for_server(port).await;

    let response = reqwest::get(format!("http://127.0.0.1:{port}/users/456?source=cli"))
        .await
        .unwrap();

    assert_eq!(response.status(), 202);
    assert_eq!(response.text().await.unwrap(), "accepted");
    let captured = captured.recv().unwrap();
    assert_eq!(captured.request_line, "GET /users/456?source=cli HTTP/1.1");
    upstream.join().unwrap();
}

#[tokio::test]
async fn serve_reads_native_config_file_and_uses_first_collection_by_default() {
    let fixture = Fixture::new();
    fixture.write_route("users.yaml", route_with_id("users"));
    fixture.write_collections(
        r#"
- id: local
  routes:
    - users:any:ok

- id: default
  routes:
    - users:any:ok
"#,
    );
    let port = unused_port();
    fixture.write_config(format!(
        r#"
routes: routes
collections: collections.yaml
port: {port}
"#
    ));

    let _server = fixture.spawn_raw(&["serve", "--config", "decoy.yaml"]);
    wait_for_server(port).await;

    let response = reqwest::get(format!("http://127.0.0.1:{port}/users/123"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn serve_cli_flags_override_native_config_file_values_during_startup() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.temp.path().join("config/routes")).unwrap();
    fs::create_dir_all(fixture.temp.path().join("flag/routes")).unwrap();
    fs::write(
        fixture.temp.path().join("config/routes/who.yaml"),
        route_response("who", "CONFIG"),
    )
    .unwrap();
    fs::write(
        fixture.temp.path().join("flag/routes/who.yaml"),
        route_response("who", "FLAG"),
    )
    .unwrap();
    fs::write(
        fixture.temp.path().join("config/collections.yaml"),
        "- id: config
  routes:
    - who:any:ok
",
    )
    .unwrap();
    fs::write(
        fixture.temp.path().join("flag/collections.yaml"),
        "- id: flag
  routes:
    - who:any:ok
",
    )
    .unwrap();
    fixture.write_config(
        r#"
routes: config/routes
collections: config/collections.yaml
collection: config
port: 1
"#,
    );

    let port = unused_port();
    let _server = fixture.spawn_raw(&[
        "serve",
        "--config",
        "decoy.yaml",
        "--routes",
        "flag/routes",
        "--collections",
        "flag/collections.yaml",
        "--collection",
        "flag",
        "--port",
        &port.to_string(),
    ]);
    wait_for_server(port).await;

    let response = reqwest::get(format!("http://127.0.0.1:{port}/who"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "FLAG");
}

#[test]
fn duplicate_route_ids_fail_startup_with_diagnostic() {
    let fixture = Fixture::new();
    fixture.write_route("a.yml", route_with_id("dupe"));
    fixture.write_route("nested/b.yaml", route_with_id("dupe"));
    fixture.write_collections("- id: default\n  routes: []\n");

    let output = fixture.output(&[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("duplicate route id `dupe`"), "{stderr}");
    assert!(stderr.contains("a.yml"), "{stderr}");
    assert!(stderr.contains("nested/b.yaml"), "{stderr}");
}

#[test]
fn missing_startup_collection_fails_startup() {
    let fixture = Fixture::new();
    fixture.write_route("users.yaml", route_with_id("users"));
    fixture.write_collections("- id: default\n  routes: []\n");

    let output = fixture.output(&["--collection", "missing"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown startup collection `missing`"),
        "{stderr}"
    );
}

#[tokio::test]
async fn serve_starts_with_configured_proto_root() {
    let fixture = Fixture::new();
    fixture.write_route("users.yaml", route_with_id("users"));
    fixture.write_collections("- id: default\n  routes: [users:any:ok]\n");
    let port = unused_port();
    fixture.write_config(format!(
        r#"
routes: routes
collections: collections.yaml
port: {port}
protos:
  roots: [{}]
"#,
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/proto")
            .display()
    ));

    let _server = fixture.spawn_raw(&["serve", "--config", "decoy.yaml"]);
    wait_for_server(port).await;

    let response = reqwest::get(format!("http://127.0.0.1:{port}/users/123"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[test]
fn broken_proto_fails_startup_with_file_and_reason() {
    let fixture = Fixture::new();
    fixture.write_route("users.yaml", route_with_id("users"));
    fixture.write_collections("- id: default\n  routes: []\n");
    fs::create_dir_all(fixture.temp.path().join("proto")).unwrap();
    fs::write(
        fixture.temp.path().join("proto/broken.proto"),
        "syntax = \"proto3\";\nmessage Broken { string id = 1 }\n",
    )
    .unwrap();
    fixture.write_config(
        r#"
routes: routes
collections: collections.yaml
protos:
  roots: [proto]
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_decoy"))
        .args(["serve", "--config", "decoy.yaml"])
        .current_dir(fixture.temp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("proto/broken.proto"), "{stderr}");
    assert!(stderr.contains("expected"), "{stderr}");
}

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
        let headers = lines
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                Some((name.to_ascii_lowercase(), value.trim().to_owned()))
            })
            .collect();
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

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    temp: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("routes")).unwrap();
        Self { temp }
    }

    fn write_route(&self, relative: impl AsRef<Path>, contents: impl AsRef<str>) {
        let path = self.temp.path().join("routes").join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents.as_ref()).unwrap();
    }

    fn write_collections(&self, contents: impl AsRef<str>) {
        fs::write(self.temp.path().join("collections.yaml"), contents.as_ref()).unwrap();
    }

    fn write_config(&self, contents: impl AsRef<str>) {
        fs::write(self.temp.path().join("decoy.yaml"), contents.as_ref()).unwrap();
    }

    fn spawn(&self, port: u16, extra: &[&str]) -> Server {
        let mut command = Command::new(env!("CARGO_BIN_EXE_decoy"));
        command.args(self.args(port, extra));
        Server(
            command
                .current_dir(self.temp.path())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn spawn_raw(&self, args: &[&str]) -> Server {
        Server(
            Command::new(env!("CARGO_BIN_EXE_decoy"))
                .args(args)
                .current_dir(self.temp.path())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn output(&self, extra: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_decoy"))
            .args(self.args(unused_port(), extra))
            .current_dir(self.temp.path())
            .output()
            .unwrap()
    }

    fn args(&self, port: u16, extra: &[&str]) -> Vec<String> {
        let mut args = vec![
            "serve".to_owned(),
            "--routes".to_owned(),
            self.temp.path().join("routes").display().to_string(),
            "--collections".to_owned(),
            self.temp
                .path()
                .join("collections.yaml")
                .display()
                .to_string(),
            "--port".to_owned(),
            port.to_string(),
        ];
        args.extend(extra.iter().map(|arg| (*arg).to_owned()));
        args
    }
}

fn route_response(id: &str, body: &str) -> String {
    format!(
        r#"
id: {id}
transport: http
match:
  method: GET
  path: /{id}
cases:
  any:
    behaviors:
      ok:
        status: 200
        body: {body}
"#
    )
}

fn route_with_id(id: &str) -> String {
    format!(
        r#"
id: {id}
transport: http
match:
  method: GET
  path: /users/{{id}}
cases:
  any:
    behaviors:
      ok:
        status: 200
"#
    )
}

fn unused_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn wait_for_server(port: u16) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if client
            .get(format!("http://127.0.0.1:{port}/__decoy_wait"))
            .send()
            .await
            .is_ok()
        {
            return;
        }
        assert!(Instant::now() < deadline, "server did not start");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

const CONTROL_ROUTE: &str = r#"
id: get-user
transport: http
match:
  method: GET
  path: /users/{id}
cases:
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
      broken:
        status: 500
        body:
          error: broken
"#;

const CONTROL_COLLECTIONS: &str = r#"
- id: happy
  routes:
    - get-user:user-123:success
- id: not-found
  routes:
    - get-user:user-123:missing
"#;

struct ControlServer {
    _server: Server,
    _fixture: Fixture,
    port: u16,
    client: reqwest::Client,
}

impl ControlServer {
    async fn start() -> Self {
        let fixture = Fixture::new();
        fixture.write_route("user.yaml", CONTROL_ROUTE);
        fixture.write_collections(CONTROL_COLLECTIONS);
        let port = unused_port();
        let server = fixture.spawn(port, &[]);
        wait_for_server(port).await;
        Self {
            _server: server,
            _fixture: fixture,
            port,
            client: reqwest::Client::new(),
        }
    }

    async fn get_user(&self, session: Option<&str>) -> u16 {
        let mut request = self
            .client
            .get(format!("http://127.0.0.1:{}/users/123", self.port));
        if let Some(session) = session {
            request = request.header("x-mock-session", session);
        }
        request.send().await.unwrap().status().as_u16()
    }

    async fn post(&self, path: &str, session: Option<&str>, body: &str) -> (u16, String) {
        let mut request = self
            .client
            .post(format!("http://127.0.0.1:{}{path}", self.port))
            .body(body.to_owned());
        if let Some(session) = session {
            request = request.header("x-mock-session", session);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.text().await.unwrap())
    }
}

#[tokio::test]
async fn serve_control_verbs_mutate_only_targeted_session_and_reset_independently() {
    let server = ControlServer::start().await;

    assert_eq!(server.get_user(Some("a")).await, 200);
    assert_eq!(server.get_user(Some("b")).await, 200);

    let (status, body) = server
        .post(
            "/__decoy__/control/useCollection",
            Some("a"),
            r#"{"collection":"not-found"}"#,
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(server.get_user(Some("a")).await, 404);
    assert_eq!(server.get_user(Some("b")).await, 200);

    let (status, _) = server
        .post(
            "/__decoy__/control/useRoute",
            Some("b"),
            r#"{"route":"get-user","case":"user-123","behavior":"broken"}"#,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(server.get_user(Some("a")).await, 404);
    assert_eq!(server.get_user(Some("b")).await, 500);

    let (status, _) = server.post("/__decoy__/control/reset", Some("b"), "").await;
    assert_eq!(status, 200);
    assert_eq!(server.get_user(Some("a")).await, 404);
    assert_eq!(server.get_user(Some("b")).await, 200);
}

#[tokio::test]
async fn serve_control_without_session_header_targets_the_default_session() {
    let server = ControlServer::start().await;

    let (status, _) = server
        .post(
            "/__decoy__/control/useCollection",
            None,
            r#"{"collection":"not-found"}"#,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(server.get_user(None).await, 404);
    assert_eq!(server.get_user(Some("other")).await, 200);
}

#[tokio::test]
async fn serve_control_returns_clear_errors_for_invalid_addresses() {
    let server = ControlServer::start().await;

    let (status, body) = server
        .post(
            "/__decoy__/control/useCollection",
            None,
            r#"{"collection":"ghost"}"#,
        )
        .await;
    assert_eq!(status, 400);
    assert_eq!(body, r#"{"error":"unknown collection `ghost`"}"#);

    let (status, body) = server
        .post(
            "/__decoy__/control/useRoute",
            None,
            r#"{"route":"get-user","case":"user-123","behavior":"ghost"}"#,
        )
        .await;
    assert_eq!(status, 400);
    assert_eq!(
        body,
        r#"{"error":"unknown behavior `get-user:user-123:ghost`"}"#
    );
}

#[tokio::test]
async fn serve_control_rejects_malformed_reset_json_without_mutating_selection() {
    let server = ControlServer::start().await;

    let (status, _) = server
        .post(
            "/__decoy__/control/useRoute",
            None,
            r#"{"route":"get-user","case":"user-123","behavior":"broken"}"#,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(server.get_user(None).await, 500);

    let (status, body) = server.post("/__decoy__/control/reset", None, "{").await;
    assert_eq!(status, 400);
    assert!(body.contains("failed to parse control JSON body"), "{body}");
    assert_eq!(server.get_user(None).await, 500);
}

#[tokio::test]
async fn serve_reserves_decoy_namespace_but_not_lookalike_paths() {
    let server = ControlServer::start().await;

    let (status, body) = server.post("/__decoy__/control/ghost", None, "").await;
    assert_eq!(status, 404);
    assert!(body.contains("is not defined"), "{body}");

    let lookalike = server
        .client
        .get(format!("http://127.0.0.1:{}/__decoy__foo", server.port))
        .send()
        .await
        .unwrap();
    assert_eq!(lookalike.status(), 501);
    assert_eq!(lookalike.headers()["x-mock-miss"], "true");
}

#[tokio::test]
async fn serve_health_reports_liveness_without_touching_selection() {
    let server = ControlServer::start().await;
    let (status, _) = server
        .post(
            "/__decoy__/control/useCollection",
            None,
            r#"{"collection":"not-found"}"#,
        )
        .await;
    assert_eq!(status, 200);

    let health = server
        .client
        .get(format!("http://127.0.0.1:{}/__decoy__/health", server.port))
        .header("x-mock-session", "probe")
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(
        health.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({ "status": "ok" })
    );

    assert_eq!(server.get_user(None).await, 404);
    assert_eq!(server.get_user(Some("probe")).await, 200);
}

#[test]
fn version_flag_prints_the_crate_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_decoy"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("decoy {}", env!("CARGO_PKG_VERSION"))
    );
}

#[tokio::test]
async fn serve_matches_cases_on_query_headers_and_json_body() {
    let fixture = Fixture::new();
    fixture.write_route(
        "orders.yaml",
        r#"
id: orders
transport: http
match:
  method: POST
  path: /orders
cases:
  fallback:
    behaviors:
      ok:
        body: fallback
  vip:
    match:
      query: { priority: high }
      headers: { X-Tier: '*' }
      body: { items: [{ sku: gold }] }
    behaviors:
      ok:
        body: vip
  tagged:
    match:
      headers: { x-tag: 'a, b' }
    behaviors:
      ok:
        body: tagged
"#,
    );
    fixture.write_collections(
        r#"
- id: default
  routes:
    - orders:fallback:ok
    - orders:tagged:ok
    - orders:vip:ok
"#,
    );
    let port = unused_port();
    let _server = fixture.spawn(port, &[]);
    wait_for_server(port).await;
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/orders");

    let vip = client
        .post(format!("{url}?priority=high"))
        .header("x-tier", "gold")
        .header("content-type", "application/json")
        .body(r#"{"items":[{"sku":"basic"},{"sku":"gold","qty":2}]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(vip.text().await.unwrap(), "vip");

    let wrong_body = client
        .post(format!("{url}?priority=high"))
        .header("x-tier", "gold")
        .header("content-type", "application/json")
        .body(r#"{"items":[{"sku":"basic"}]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_body.text().await.unwrap(), "fallback");

    let repeated_header = client
        .post(&url)
        .header("x-tag", "a")
        .header("x-tag", "b")
        .send()
        .await
        .unwrap();
    assert_eq!(repeated_header.text().await.unwrap(), "tagged");
}

#[test]
fn unknown_match_field_fails_startup() {
    let fixture = Fixture::new();
    fixture.write_route(
        "users.yaml",
        r#"
id: users
transport: http
match:
  method: GET
  path: /users
cases:
  typo:
    match:
      querry: { page: 1 }
    behaviors:
      ok:
        status: 200
"#,
    );
    fixture.write_collections("- id: default\n  routes:\n    - users:typo:ok\n");

    let output = fixture.output(&[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("users:typo") && stderr.contains("unknown match field `querry`"),
        "{stderr}"
    );
}

#[tokio::test]
async fn serve_matches_cases_with_jmespath_predicates() {
    let fixture = Fixture::new();
    fixture.write_route(
        "graphql.yaml",
        r#"
id: graphql
transport: http
match:
  method: POST
  path: /graphql
cases:
  get-user:
    match:
      headers: { x-client: web }
      body: "${ body.operationName == 'GetUser' }"
    behaviors:
      ok:
        body: { data: { user: { name: Ada } } }
  big-order:
    match: "${ body.variables.amount > `1000` }"
    behaviors:
      ok:
        body: { data: { order: big } }
"#,
    );
    fixture.write_collections(
        "- id: default\n  routes:\n    - graphql:get-user:ok\n    - graphql:big-order:ok\n",
    );
    let port = unused_port();
    let _server = fixture.spawn(port, &[]);
    wait_for_server(port).await;
    let client = reqwest::Client::new();
    let post = |body: &'static str| {
        client
            .post(format!("http://127.0.0.1:{port}/graphql"))
            .header("content-type", "application/json")
            .header("x-client", "web")
            .body(body)
            .send()
    };

    let user = post(r#"{"operationName":"GetUser"}"#).await.unwrap();
    assert_eq!(
        user.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({ "data": { "user": { "name": "Ada" } } })
    );
    let order = post(r#"{"operationName":"Order","variables":{"amount":1500}}"#)
        .await
        .unwrap();
    assert_eq!(
        order.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({ "data": { "order": "big" } })
    );
    let miss = post(r#"{"operationName":"Order","variables":{"amount":10}}"#)
        .await
        .unwrap();
    assert_eq!(miss.status(), 501);
}

#[test]
fn invalid_predicate_fails_startup() {
    let fixture = Fixture::new();
    fixture.write_route(
        "users.yaml",
        r#"
id: users
transport: http
match:
  method: GET
  path: /users
cases:
  broken:
    match: "${ query.[ }"
    behaviors:
      ok:
        status: 200
"#,
    );
    fixture.write_collections("- id: default\n  routes:\n    - users:broken:ok\n");

    let output = fixture.output(&[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("users:broken") && stderr.contains("invalid JMESPath"),
        "{stderr}"
    );
}

#[tokio::test]
async fn serve_renders_templated_responses() {
    let fixture = Fixture::new();
    fixture.write_route(
        "users.yaml",
        r#"
id: users
transport: http
match:
  method: POST
  path: /users/{id}
cases:
  any:
    behaviors:
      echo:
        status: "${ query.status || `201` }"
        headers:
          x-user: "user-${ pathParams.id }"
        body:
          id: "${ pathParams.id }"
          greeting: "Hello, ${ body.name }!"
          age: "${ body.age }"
          requestId: "${ uuid() }"
          literal: '\${ not a template }'
"#,
    );
    fixture.write_collections("- id: default\n  routes:\n    - users:any:echo\n");
    let port = unused_port();
    let _server = fixture.spawn(port, &[]);
    wait_for_server(port).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://127.0.0.1:{port}/users/42"))
        .header("content-type", "application/json")
        .body(r#"{"name":"Ada","age":36}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    assert_eq!(response.headers()["x-user"], "user-42");
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["id"], "42");
    assert_eq!(body["greeting"], "Hello, Ada!");
    assert_eq!(body["age"], 36);
    assert_eq!(body["requestId"].as_str().unwrap().len(), 36);
    assert_eq!(body["literal"], "${ not a template }");

    let teapot = client
        .post(format!("http://127.0.0.1:{port}/users/1?status=418"))
        .send()
        .await
        .unwrap();
    assert_eq!(teapot.status(), 418);
}

#[test]
fn invalid_response_template_fails_startup() {
    let fixture = Fixture::new();
    fixture.write_route(
        "users.yaml",
        r#"
id: users
transport: http
match:
  method: GET
  path: /users
cases:
  any:
    behaviors:
      broken:
        body: { name: "${ body.[ }" }
"#,
    );
    fixture.write_collections("- id: default\n  routes:\n    - users:any:broken\n");

    let output = fixture.output(&[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("users:any:broken") && stderr.contains("invalid JMESPath"),
        "{stderr}"
    );
}

#[tokio::test]
async fn serve_delays_behaviors_without_blocking_other_requests() {
    let fixture = Fixture::new();
    fixture.write_route(
        "timing.yaml",
        r#"
id: timing
transport: http
match:
  method: GET
  path: /timing/{speed}
cases:
  slow:
    match: { pathParams: { speed: slow } }
    behaviors:
      wait:
        delay: 300ms
        body: slow
  fast:
    match: { pathParams: { speed: fast } }
    behaviors:
      now:
        body: fast
"#,
    );
    fixture.write_collections(
        "- id: default\n  routes:\n    - timing:slow:wait\n    - timing:fast:now\n",
    );
    let port = unused_port();
    let _server = fixture.spawn(port, &[]);
    wait_for_server(port).await;
    let client = reqwest::Client::new();
    let get = |speed: &str| {
        client
            .get(format!("http://127.0.0.1:{port}/timing/{speed}"))
            .send()
    };

    let started = Instant::now();
    let slow = tokio::spawn(get("slow"));
    let fast = get("fast").await.unwrap();
    let fast_elapsed = started.elapsed();
    assert_eq!(fast.text().await.unwrap(), "fast");
    let slow = slow.await.unwrap().unwrap();
    let slow_elapsed = started.elapsed();

    assert_eq!(slow.text().await.unwrap(), "slow");
    assert!(
        slow_elapsed >= Duration::from_millis(300),
        "{slow_elapsed:?}"
    );
    assert!(
        fast_elapsed < Duration::from_millis(300),
        "{fast_elapsed:?}"
    );
}

#[test]
fn invalid_delay_fails_startup() {
    let fixture = Fixture::new();
    fixture.write_route(
        "users.yaml",
        r#"
id: users
transport: http
match:
  method: GET
  path: /users
cases:
  any:
    behaviors:
      slow:
        delay: soon
"#,
    );
    fixture.write_collections("- id: default\n  routes:\n    - users:any:slow\n");

    let output = fixture.output(&[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("must be integer milliseconds"), "{stderr}");
}
