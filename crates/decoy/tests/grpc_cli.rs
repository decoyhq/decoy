//! `decoy serve` answering unary gRPC calls from a real client (tonic) on the HTTP port.

use std::fs;
use std::net::TcpListener;
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

    server
        .control(
            "useRoute",
            r#"{"route":"approvals-get","case":"pending","behavior":"approved"}"#,
            "s1",
        )
        .await;
    let reply = client
        .get(json!({ "id": "a-1" }), &[("x-mock-session", "s1")])
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
        .get(json!({ "id": "a-1" }), &[("x-mock-session", "s1")])
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::NotFound);
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
        let temp = TempDir::new().unwrap();
        let protos =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../decoy-runtime/tests/fixtures/proto");
        fs::create_dir_all(temp.path().join("routes")).unwrap();
        fs::write(temp.path().join("routes/approvals.yaml"), route).unwrap();
        fs::write(temp.path().join("collections.yaml"), collections).unwrap();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        fs::write(
            temp.path().join("decoy.yaml"),
            format!(
                "routes: routes\ncollections: collections.yaml\nport: {port}\nprotos:\n  roots: [{}]\n{}",
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
        let child = Command::new(env!("CARGO_BIN_EXE_decoy"))
            .args(["serve", "--config", "decoy.yaml"])
            .current_dir(temp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let pool = load_descriptor_pool(&ProtoSources {
            roots: vec![protos],
            descriptor_sets: vec![],
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
            .header("x-mock-session", session)
            .body(body.to_owned())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
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
