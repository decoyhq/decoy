//! `decoy serve` answering unary and server-streaming gRPC calls from a real client (tonic) on the
//! HTTP port.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bytes::{Buf, BufMut};
use decoy_runtime::config::ProtoSources;
use decoy_runtime::protos::load_descriptor_pool;
use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor};
use serde_json::{Value, json};
use tempfile::TempDir;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig};
use tonic::{Code, Request, Status};

const ROUTE: &str = r#"
id: approvals-get
transport: grpc
match:
  service: approvals.v1.Approvals
  method: GetApproval
cases:
  pending:
    match:
      message: { id: a-1 }
    behaviors:
      ok:
        message: { id: a-1, state: PENDING, approvers: [alice] }
      approved:
        message: { id: a-1, state: APPROVED, approvers: [alice, bob] }
  any:
    behaviors:
      echo:
        message: { id: '${ message.id }', state: PENDING, approvers: ['${ metadata."x-user" }'] }
      not-found:
        status: NOT_FOUND
        message: 'approval ${ message.id } not found ✗'
        metadata: { x-reason: missing }
"#;

const COLLECTIONS: &str = r#"
- id: happy-path
  routes:
    - approvals-get:any:echo
    - approvals-get:pending:ok
- id: approvals-down
  from: happy-path
  routes:
    - approvals-get:any:not-found
"#;

#[cfg(unix)]
const NORMALIZE_PIPELINE: &str = r#"
transformers:
  normalize:
    input: [sed, 's/incoming/a-1/g']
    output: [sed, 's/a-1/outgoing/g']
pipelines:
  normalize-replies:
    input: [grpc-frame, protobuf, normalize]
    output: [normalize, protobuf, grpc-frame]
"#;

#[tokio::test]
async fn empty_shared_pipeline_uses_raw_grpc_frames() {
    let route = r#"
id: approvals-get
transport: grpc
pipeline: raw
match:
  service: approvals.v1.Approvals
  method: GetApproval
cases:
  raw:
    match:
      message: AAAAAAUKA2EtMQ==
    behaviors:
      ok:
        message: AAAAAAUKA3gtMQ==
"#;
    let server = Server::start_with_config(
        route,
        "- id: raw\n  routes: [approvals-get:raw:ok]\n",
        false,
        "pipelines: { raw: [] }\n",
    )
    .await;
    let mut client = server.client().await;
    let reply = client.get(json!({ "id": "a-1" }), &[]).await.unwrap();
    assert_eq!(reply, json!({ "id": "x-1" }));
}

#[cfg(unix)]
#[tokio::test]
async fn shorthand_pipeline_reverses_output_and_skips_missing_directions() {
    let route = ROUTE.replace("transport: grpc", "transport: grpc\npipeline: shorthand");
    let config = r#"
transformers:
  read: { input: [sed, 's/incoming/a-1/g'] }
  write: { output: [sed, 's/a-1/outgoing/g'] }
pipelines:
  shorthand: [grpc-frame, protobuf, read, write]
"#;
    let server = Server::start_with_config(&route, COLLECTIONS, false, config).await;
    let mut client = server.client().await;
    let reply = client.get(json!({ "id": "incoming" }), &[]).await.unwrap();
    assert_eq!(
        reply,
        json!({ "id": "outgoing", "state": "PENDING", "approvers": ["alice"] })
    );
}

#[cfg(unix)]
#[tokio::test]
async fn named_pipeline_matches_transformed_messages_and_encodes_each_reply() {
    let route = ROUTE.replace(
        "transport: grpc",
        "transport: grpc\npipeline: normalize-replies",
    );
    let server = Server::start_with_config(&route, COLLECTIONS, false, NORMALIZE_PIPELINE).await;
    let mut client = server.client().await;
    let reply = client.get(json!({ "id": "incoming" }), &[]).await.unwrap();
    assert_eq!(
        reply,
        json!({ "id": "outgoing", "state": "PENDING", "approvers": ["alice"] })
    );

    let route = WATCH_ROUTE.replace(
        "transport: grpc",
        "transport: grpc\npipeline: normalize-replies",
    );
    let server =
        Server::start_with_config(&route, WATCH_COLLECTIONS, false, NORMALIZE_PIPELINE).await;
    let mut client = server.client().await;
    let mut stream = client
        .watch(json!({ "ids": ["incoming"] }), &[])
        .await
        .unwrap();
    for state in ["PENDING", "APPROVED"] {
        assert_eq!(
            next(&mut stream).await.unwrap(),
            Some(json!({ "id": "outgoing", "state": state }))
        );
    }
    assert_eq!(next(&mut stream).await.unwrap(), None);

    server
        .control("useCollection", r#"{"collection":"open"}"#, "s1")
        .await;
    let mut open = client
        .watch(json!({ "ids": [] }), &[("x-mock-session", "s1")])
        .await
        .unwrap();
    assert_eq!(next(&mut open).await.unwrap().unwrap()["id"], "outgoing");
    let invalid = r#"{"route":"approvals-watch","message":{"unknownField":1}}"#;
    let (status, diagnostic) = server.send(invalid, "s1").await;
    assert_eq!(status, 400);
    assert!(
        diagnostic["error"]
            .as_str()
            .unwrap()
            .contains("unknownField")
    );
    let pushed = r#"{"route":"approvals-watch","message":{"id":"a-1","state":"APPROVED"}}"#;
    assert_eq!(
        server.send(pushed, "s1").await,
        (200, json!({ "delivered": 1 }))
    );
    assert_eq!(
        next(&mut open).await.unwrap(),
        Some(json!({ "id": "outgoing", "state": "APPROVED" }))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn named_pipeline_keeps_the_default_grpc_codec_for_each_omitted_direction() {
    let route = ROUTE.replace(
        "transport: grpc",
        "transport: grpc\npipeline: normalize-replies",
    );
    for (omitted, reply_id, approver) in [
        (
            "    output: [normalize, protobuf, grpc-frame]\n",
            "a-1",
            "alice",
        ),
        (
            "    input: [grpc-frame, protobuf, normalize]\n",
            "incoming",
            "carol",
        ),
    ] {
        let config = NORMALIZE_PIPELINE.replace(omitted, "");
        let server = Server::start_with_config(&route, COLLECTIONS, false, &config).await;
        let reply = server
            .client()
            .await
            .get(json!({ "id": "incoming" }), &[("x-user", "carol")])
            .await
            .unwrap();
        assert_eq!(
            reply,
            json!({ "id": reply_id, "state": "PENDING", "approvers": [approver] })
        );
    }
}

#[tokio::test]
async fn serves_unary_grpc_calls_with_matching_templates_statuses_and_sessions() {
    let server = Server::start(ROUTE, COLLECTIONS).await;
    let mut client = server.client().await;

    let reply = client.get(json!({ "id": "a-1" }), &[]).await.unwrap();
    assert_eq!(
        reply,
        json!({ "id": "a-1", "state": "PENDING", "approvers": ["alice"] })
    );

    let reply = client
        .get(json!({ "id": "a-7" }), &[("x-user", "carol")])
        .await
        .unwrap();
    assert_eq!(
        reply,
        json!({ "id": "a-7", "state": "PENDING", "approvers": ["carol"] })
    );

    // A Session switches Collection; the default Session is untouched.
    server
        .control("useCollection", r#"{"collection":"approvals-down"}"#, "s1")
        .await;
    let status = client
        .get(json!({ "id": "a-7" }), &[("x-mock-session", "s1")])
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::NotFound);
    assert_eq!(status.message(), "approval a-7 not found ✗");
    assert_eq!(status.metadata().get("x-reason").unwrap(), "missing");
    assert!(
        client
            .get(json!({ "id": "a-7" }), &[("x-user", "carol")])
            .await
            .is_ok()
    );

    // s2 stays on happy-path.
    server
        .control(
            "useRoute",
            r#"{"route":"approvals-get","case":"pending","behavior":"approved"}"#,
            "s2",
        )
        .await;
    let reply = client
        .get(json!({ "id": "a-1" }), &[("x-mock-session", "s2")])
        .await
        .unwrap();
    assert_eq!(reply["state"], "APPROVED");
}

#[tokio::test]
async fn unmatched_calls_fail_closed_as_unimplemented() {
    let server = Server::start(
        ROUTE,
        "- id: only-pending\n  routes: [approvals-get:pending:ok]\n",
    )
    .await;
    let mut client = server.client().await;

    let status = client.get(json!({ "id": "zzz" }), &[]).await.unwrap_err();
    assert_eq!(status.code(), Code::Unimplemented);
    assert_eq!(status.message(), "no active route case matched request");
    assert_eq!(status.metadata().get("x-mock-miss").unwrap(), "true");
}

#[tokio::test]
async fn http_routes_keep_working_over_http2_on_the_same_port() {
    let server = Server::start(ROUTE, COLLECTIONS).await;
    let response = reqwest::Client::builder()
        .http2_prior_knowledge()
        .build()
        .unwrap()
        .get(format!("http://127.0.0.1:{}/__decoy__/health", server.port))
        .send()
        .await
        .unwrap();
    assert_eq!(response.version(), reqwest::Version::HTTP_2);
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn an_oversized_call_gets_resource_exhausted() {
    let server = Server::start(ROUTE, COLLECTIONS).await;
    let mut client = server.client().await;

    let id = "x".repeat(33 * 1024 * 1024);
    let status = client.get(json!({ "id": id }), &[]).await.unwrap_err();
    assert_eq!(status.code(), Code::ResourceExhausted);
}

#[tokio::test]
async fn serves_grpc_over_tls_with_alpn_h2() {
    let server = Server::start_with(ROUTE, COLLECTIONS, true).await;
    let mut client = server.client().await;

    let reply = client.get(json!({ "id": "a-1" }), &[]).await.unwrap();
    assert_eq!(reply["state"], "PENDING");
    server
        .control("useCollection", r#"{"collection":"approvals-down"}"#, "s1")
        .await;
    let status = client
        .get(json!({ "id": "a-7" }), &[("x-mock-session", "s1")])
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::NotFound);
}

const WATCH_ROUTE: &str = r#"
id: approvals-watch
transport: grpc
match: { service: approvals.v1.Approvals, method: WatchApprovals }
cases:
  any:
    behaviors:
      then-ok:
        stream:
          - { payload: { id: '${ message.ids[0] }', state: PENDING } }
          - { payload: { id: '${ message.ids[0] }', state: APPROVED }, delay: 50ms }
        status: OK
      then-aborted:
        stream: [{ payload: { id: a-1, state: PENDING } }]
        status: ABORTED
        message: gave up
      open:
        stream: [{ payload: { id: a-1, state: PENDING } }]
"#;

const WATCH_COLLECTIONS: &str = r#"
- id: then-ok
  routes: [approvals-watch:any:then-ok]
- id: then-aborted
  routes: [approvals-watch:any:then-aborted]
- id: open
  routes: [approvals-watch:any:open]
"#;

#[tokio::test]
async fn streams_scripted_messages_then_ends_with_the_status() {
    let server = Server::start(WATCH_ROUTE, WATCH_COLLECTIONS).await;
    let mut client = server.client().await;

    let mut stream = client.watch(json!({ "ids": ["a-7"] }), &[]).await.unwrap();
    assert_eq!(
        next(&mut stream).await.unwrap(),
        Some(json!({ "id": "a-7", "state": "PENDING" }))
    );
    assert_eq!(
        next(&mut stream).await.unwrap(),
        Some(json!({ "id": "a-7", "state": "APPROVED" }))
    );
    assert_eq!(next(&mut stream).await.unwrap(), None);

    server
        .control("useCollection", r#"{"collection":"then-aborted"}"#, "s1")
        .await;
    let mut stream = client
        .watch(json!({ "ids": ["a-1"] }), &[("x-mock-session", "s1")])
        .await
        .unwrap();
    assert_eq!(
        next(&mut stream).await.unwrap(),
        Some(json!({ "id": "a-1", "state": "PENDING" }))
    );
    let status = next(&mut stream).await.unwrap_err();
    assert_eq!(status.code(), Code::Aborted);
    assert_eq!(status.message(), "gave up");
}

#[tokio::test]
async fn an_open_stream_takes_sends_from_its_session_until_the_client_cancels() {
    let server = Server::start(WATCH_ROUTE, WATCH_COLLECTIONS).await;
    server
        .control("useCollection", r#"{"collection":"open"}"#, "s1")
        .await;
    let mut client = server.client().await;
    let mut stream = client
        .watch(json!({ "ids": ["a-1"] }), &[("x-mock-session", "s1")])
        .await
        .unwrap();
    assert_eq!(
        next(&mut stream).await.unwrap(),
        Some(json!({ "id": "a-1", "state": "PENDING" }))
    );

    let approved =
        r#"{"route":"approvals-watch","message":{"id":"a-1","state":"APPROVED"},"timeout":200}"#;
    assert_eq!(
        server.send(approved, "s1").await,
        (200, json!({ "delivered": 1 }))
    );
    assert_eq!(
        next(&mut stream).await.unwrap(),
        Some(json!({ "id": "a-1", "state": "APPROVED" }))
    );
    assert_eq!(server.send(approved, "s2").await.0, 408);

    drop(stream);
    // The server learns of the cancel asynchronously; until then a send may still be delivered.
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.send(approved, "s1").await.0 != 408 {
        assert!(
            Instant::now() < deadline,
            "the cancelled stream still takes sends"
        );
    }
}

#[tokio::test]
async fn wait_resolves_once_that_sessions_stream_opens() {
    let server = Server::start(WATCH_ROUTE, WATCH_COLLECTIONS).await;
    server
        .control("useCollection", r#"{"collection":"open"}"#, "s1")
        .await;
    let connected = r#"{"route":"approvals-watch","connected":true,"timeout":100}"#;
    assert_eq!(server.post("wait", connected, "s1").await.0, 408);

    let mut client = server.client().await;
    let mut stream = client
        .watch(json!({ "ids": ["a-1"] }), &[("x-mock-session", "s1")])
        .await
        .unwrap();
    assert!(next(&mut stream).await.unwrap().is_some());

    assert_eq!(
        server.post("wait", connected, "s1").await,
        (200, json!({ "connections": 1 }))
    );
    assert_eq!(server.post("wait", connected, "s2").await.0, 408);
}

#[tokio::test]
async fn wait_on_a_unary_route_is_a_conflict() {
    let server = Server::start(ROUTE, COLLECTIONS).await;
    let (status, body) = server
        .post(
            "wait",
            r#"{"route":"approvals-get","connected":true}"#,
            "s1",
        )
        .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(
        body["error"],
        "route `approvals-get` never has live connections; `wait` needs a ws or streaming gRPC Route"
    );
}

#[tokio::test]
async fn a_send_that_does_not_fit_the_output_type_is_rejected_and_the_stream_stays_open() {
    let server = Server::start(WATCH_ROUTE, WATCH_COLLECTIONS).await;
    server
        .control("useCollection", r#"{"collection":"open"}"#, "s1")
        .await;
    let mut client = server.client().await;
    let mut stream = client
        .watch(json!({ "ids": ["a-1"] }), &[("x-mock-session", "s1")])
        .await
        .unwrap();
    assert!(next(&mut stream).await.unwrap().is_some());

    let (status, body) = server
        .send(r#"{"route":"approvals-watch","message":{"nope":1}}"#, "s1")
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("response does not fit approvals.v1.Approval"),
        "{body}"
    );

    let good = r#"{"route":"approvals-watch","message":{"id":"a-2"}}"#;
    assert_eq!(server.send(good, "s1").await.0, 200);
    assert_eq!(
        next(&mut stream).await.unwrap(),
        Some(json!({ "id": "a-2" }))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_ends_open_streams_with_unavailable() {
    let mut server = Server::start(WATCH_ROUTE, WATCH_COLLECTIONS).await;
    server
        .control("useCollection", r#"{"collection":"open"}"#, "default")
        .await;
    let mut client = server.client().await;
    let mut stream = client.watch(json!({ "ids": ["a-1"] }), &[]).await.unwrap();
    assert!(next(&mut stream).await.unwrap().is_some());

    let started = Instant::now();
    let killed = Command::new("kill")
        .args(["-TERM", &server.child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let status = next(&mut stream).await.unwrap_err();
    assert_eq!(status.code(), Code::Unavailable, "{status:?}");
    assert_eq!(status.message(), "decoy is shutting down");
    while server.child.try_wait().unwrap().is_none() {
        // Well under the 5s drain timeout: the open stream must not hold shutdown.
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "decoy did not stop"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[test]
fn stream_on_a_unary_method_fails_startup() {
    let temp = TempDir::new().unwrap();
    let protos =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../decoy-runtime/tests/fixtures/proto");
    fs::create_dir_all(temp.path().join("routes")).unwrap();
    fs::write(
        temp.path().join("routes/approvals.yaml"),
        "id: approvals-get\ntransport: grpc\nmatch: { service: approvals.v1.Approvals, method: GetApproval }\ncases:\n  any:\n    behaviors:\n      ok: { stream: [{ payload: { id: a-1 } }] }\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("collections.yaml"),
        "- id: default\n  routes: [approvals-get:any:ok]\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("decoy.yaml"),
        format!(
            "routes: routes\ncollections: collections.yaml\nprotos:\n  roots: [{}]\n",
            protos.display()
        ),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_decoy"))
        .args(["serve", "--config", "decoy.yaml"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "behavior `approvals-get:any:ok`: `stream` needs a server-streaming method; `approvals.v1.Approvals.GetApproval` is unary"
        ),
        "{stderr}"
    );
}

/// The startup error comes from checking the Route against the catalog service, so the service
/// compiled from `files`, its import resolved through the root, and the broken proto was skipped.
#[test]
fn proto_files_compile_only_the_matching_entry_files() {
    let temp = TempDir::new().unwrap();
    for (file, text) in [
        (
            "proto/shared/v1/money.proto",
            "syntax = \"proto3\";\npackage shared.v1;\nmessage Money { int64 units = 1; }\n",
        ),
        (
            "proto/services/catalog/v1/catalog_service.proto",
            "syntax = \"proto3\";\npackage catalog.v1;\nimport \"shared/v1/money.proto\";\n\
             message GetPriceRequest { string id = 1; }\n\
             service CatalogService { rpc GetPrice(GetPriceRequest) returns (shared.v1.Money); }\n",
        ),
        (
            "proto/legacy/broken.proto",
            "syntax = \"proto3\";\nmessage Broken { string id = 1 }\n",
        ),
        (
            "decoy/routes/catalog.yaml",
            "id: catalog-price\ntransport: grpc\nmatch: { service: catalog.v1.CatalogService, method: GetPrice }\ncases:\n  any:\n    behaviors:\n      ok: { stream: [{ payload: { units: 1 } }] }\n",
        ),
        (
            "decoy/collections.yaml",
            "- id: default\n  routes: [catalog-price:any:ok]\n",
        ),
        (
            "decoy/decoy.yaml",
            "routes: routes\ncollections: collections.yaml\nprotos:\n  roots: [../proto]\n  files: [../proto/services/catalog/**/*.proto]\n",
        ),
    ] {
        let path = temp.path().join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    let output = Command::new(env!("CARGO_BIN_EXE_decoy"))
        .args(["serve", "--config", "decoy/decoy.yaml"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("`catalog.v1.CatalogService.GetPrice` is unary"),
        "{stderr}"
    );
}

async fn next(stream: &mut tonic::Streaming<DynamicMessage>) -> Result<Option<Value>, Status> {
    let message = stream.message().await?;
    Ok(message.map(|message| serde_json::to_value(message).unwrap()))
}

struct Server {
    _temp: TempDir,
    child: Child,
    port: u16,
    pool: DescriptorPool,
    /// PEM of the self-signed cert when the server listens with TLS.
    cert: Option<String>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    async fn start(route: &str, collections: &str) -> Self {
        Self::start_with(route, collections, false).await
    }

    async fn start_with(route: &str, collections: &str, tls: bool) -> Self {
        Self::start_with_config(route, collections, tls, "").await
    }

    async fn start_with_config(route: &str, collections: &str, tls: bool, config: &str) -> Self {
        let temp = TempDir::new().unwrap();
        let protos =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../decoy-runtime/tests/fixtures/proto");
        fs::create_dir_all(temp.path().join("routes")).unwrap();
        fs::write(temp.path().join("routes/approvals.yaml"), route).unwrap();
        fs::write(temp.path().join("collections.yaml"), collections).unwrap();
        fs::write(
            temp.path().join("decoy.yaml"),
            format!(
                "routes: routes\ncollections: collections.yaml\nport: 0\nprotos:\n  roots: [{}]\n{}{config}",
                protos.display(),
                if tls { "tls: { cert: cert.pem, key: key.pem }\n" } else { "" }
            ),
        )
        .unwrap();
        let cert = tls.then(|| {
            let generated =
                rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
            fs::write(temp.path().join("cert.pem"), generated.cert.pem()).unwrap();
            fs::write(
                temp.path().join("key.pem"),
                generated.signing_key.serialize_pem(),
            )
            .unwrap();
            generated.cert.pem()
        });
        let mut child = Command::new(env!("CARGO_BIN_EXE_decoy"))
            .args(["serve", "--config", "decoy.yaml"])
            .current_dir(temp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Port 0 lets decoy pick the port itself; picking one here and rebinding it races other
        // tests. Read it from `listening on`, then keep echoing stderr so the pipe never fills.
        let (port_tx, port_rx) = std::sync::mpsc::channel();
        let stderr = BufReader::new(child.stderr.take().unwrap());
        std::thread::spawn(move || {
            for line in stderr.lines().map_while(Result::ok) {
                if let Some(addr) = line.strip_prefix("decoy serve listening on ") {
                    let port = addr.rsplit(':').next().unwrap().parse::<u16>().unwrap();
                    let _ = port_tx.send(port);
                }
                eprintln!("{line}");
            }
        });
        let port = port_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("decoy did not report its port");
        let pool = load_descriptor_pool(&ProtoSources {
            roots: vec![protos],
            descriptor_sets: vec![],
            files: vec![],
        })
        .unwrap();
        let server = Self {
            _temp: temp,
            child,
            port,
            pool,
            cert,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while server
            .http()
            .get(format!("{}/__decoy__/health", server.base_url()))
            .send()
            .await
            .is_err()
        {
            assert!(Instant::now() < deadline, "decoy did not start");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        server
    }

    fn base_url(&self) -> String {
        match self.cert {
            Some(_) => format!("https://localhost:{}", self.port),
            None => format!("http://127.0.0.1:{}", self.port),
        }
    }

    fn http(&self) -> reqwest::Client {
        let mut builder = reqwest::Client::builder();
        if let Some(cert) = &self.cert {
            builder = builder
                .add_root_certificate(reqwest::Certificate::from_pem(cert.as_bytes()).unwrap());
        }
        builder.build().unwrap()
    }

    async fn client(&self) -> ApprovalsClient {
        let mut endpoint = Channel::from_shared(self.base_url()).unwrap();
        if let Some(cert) = &self.cert {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().ca_certificate(Certificate::from_pem(cert)))
                .unwrap();
        }
        let channel = endpoint.connect().await.unwrap();
        ApprovalsClient {
            grpc: tonic::client::Grpc::new(channel),
            input: self
                .pool
                .get_message_by_name("approvals.v1.GetApprovalRequest")
                .unwrap(),
            output: self
                .pool
                .get_message_by_name("approvals.v1.Approval")
                .unwrap(),
        }
    }

    async fn control(&self, verb: &str, body: &str, session: &str) {
        let response = self
            .http()
            .post(format!("{}/__decoy__/control/{verb}", self.base_url()))
            .header("content-type", "application/json")
            .header("x-mock-session", session)
            .body(body.to_owned())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    }

    async fn send(&self, body: &str, session: &str) -> (u16, Value) {
        self.post("send", body, session).await
    }

    async fn post(&self, verb: &str, body: &str, session: &str) -> (u16, Value) {
        let response = self
            .http()
            .post(format!("{}/__decoy__/control/{verb}", self.base_url()))
            .header("content-type", "application/json")
            .header("x-mock-session", session)
            .body(body.to_owned())
            .send()
            .await
            .unwrap();
        (response.status().as_u16(), response.json().await.unwrap())
    }
}

/// A tonic client over runtime-loaded descriptors, so the test needs no codegen.
struct ApprovalsClient {
    grpc: tonic::client::Grpc<Channel>,
    input: MessageDescriptor,
    output: MessageDescriptor,
}

impl ApprovalsClient {
    async fn get(
        &mut self,
        message: Value,
        metadata: &[(&'static str, &str)],
    ) -> Result<Value, Status> {
        let mut request =
            Request::new(DynamicMessage::deserialize(self.input.clone(), message).unwrap());
        for (key, value) in metadata {
            request
                .metadata_mut()
                .insert(*key, MetadataValue::try_from(*value).unwrap());
        }
        self.grpc.ready().await.unwrap();
        let response = self
            .grpc
            .unary(
                request,
                "/approvals.v1.Approvals/GetApproval".parse().unwrap(),
                DynamicCodec(self.output.clone()),
            )
            .await?;
        Ok(serde_json::to_value(response.into_inner()).unwrap())
    }

    async fn watch(
        &mut self,
        message: Value,
        metadata: &[(&'static str, &str)],
    ) -> Result<tonic::Streaming<DynamicMessage>, Status> {
        let input = self
            .output
            .parent_pool()
            .get_message_by_name("approvals.v1.WatchRequest")
            .unwrap();
        let mut request = Request::new(DynamicMessage::deserialize(input, message).unwrap());
        for (key, value) in metadata {
            request
                .metadata_mut()
                .insert(*key, MetadataValue::try_from(*value).unwrap());
        }
        self.grpc.ready().await.unwrap();
        let response = self
            .grpc
            .server_streaming(
                request,
                "/approvals.v1.Approvals/WatchApprovals".parse().unwrap(),
                DynamicCodec(self.output.clone()),
            )
            .await?;
        Ok(response.into_inner())
    }
}

struct DynamicCodec(MessageDescriptor);

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicEncoder;
    type Decoder = DynamicDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynamicEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        DynamicDecoder(self.0.clone())
    }
}

struct DynamicEncoder;

impl Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn encode(&mut self, item: Self::Item, dst: &mut EncodeBuf<'_>) -> Result<(), Self::Error> {
        dst.put_slice(&item.encode_to_vec());
        Ok(())
    }
}

struct DynamicDecoder(MessageDescriptor);

impl Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        let bytes = src.copy_to_bytes(src.remaining());
        DynamicMessage::decode(self.0.clone(), bytes)
            .map(Some)
            .map_err(|error| Status::internal(error.to_string()))
    }
}
