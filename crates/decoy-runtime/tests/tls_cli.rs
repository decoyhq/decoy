//! `decoy serve` with TLS listeners next to plain ones.

use std::fs;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const ROUTE: &str = r#"
id: who
transport: http
match: { method: GET, path: /who }
cases:
  any:
    behaviors:
      ok: { body: hi }
"#;

#[tokio::test]
async fn tls_and_plain_listeners_serve_the_same_routes() {
    let fixture = Fixture::new();
    let cert = fixture.write_cert();
    let (secure, extra, plain) = (unused_port(), unused_port(), unused_port());
    fixture.write_config(&format!(
        "tls: {{ cert: certs/dev.pem, key: certs/dev-key.pem }}\nport: {secure}\nports:\n  payments: {extra}\n  legacy: {{ port: {plain}, tls: false }}\n"
    ));
    let _server = fixture.spawn();
    let client = trusting_client(&cert);
    wait_until_up(&client, &format!("https://localhost:{secure}")).await;

    for port in [secure, extra] {
        let response = client
            .get(format!("https://localhost:{port}/who"))
            .send()
            .await
            .unwrap();
        // ALPN picks h2, which gRPC needs, on every TLS listener.
        assert_eq!(response.version(), reqwest::Version::HTTP_2);
        assert_eq!(response.text().await.unwrap(), "hi");
    }

    let http1 = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(cert.as_bytes()).unwrap())
        .http1_only()
        .build()
        .unwrap();
    let response = http1
        .get(format!("https://localhost:{secure}/who"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.version(), reqwest::Version::HTTP_11);

    let body = reqwest::get(format!("http://127.0.0.1:{plain}/who"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "hi");
    assert!(
        client
            .get(format!("http://127.0.0.1:{secure}/who"))
            .send()
            .await
            .is_err(),
        "a TLS listener must not answer plain http"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_stops_a_tls_listener_with_a_client_stuck_before_its_handshake() {
    let fixture = Fixture::new();
    let cert = fixture.write_cert();
    let port = unused_port();
    fixture.write_config(&format!(
        "tls: {{ cert: certs/dev.pem, key: certs/dev-key.pem }}\nport: {port}\n"
    ));
    let mut server = fixture.spawn();
    wait_until_up(
        &trusting_client(&cert),
        &format!("https://localhost:{port}"),
    )
    .await;
    let _stuck = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    std::thread::sleep(Duration::from_millis(100));

    let status = Command::new("kill")
        .args(["-INT", &server.0.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(3);
    while server.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "decoy did not stop");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn auto_tls_writes_a_ca_that_clients_trust_across_restarts() {
    let fixture = Fixture::new();
    let port = unused_port();
    fixture.write_config(&format!("tls: auto\nport: {port}\n"));
    let ca_path = fixture.temp.path().join(".decoy/ca.pem");

    let server = fixture.spawn();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ca_path.exists() {
        assert!(Instant::now() < deadline, "CA was not written");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let ca = fs::read_to_string(&ca_path).unwrap();
    let client = trusting_client(&ca);
    wait_until_up(&client, &format!("https://localhost:{port}")).await;
    for host in ["localhost", "127.0.0.1"] {
        let body = client
            .get(format!("https://{host}:{port}/who"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "hi", "{host}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let key = fs::metadata(fixture.temp.path().join(".decoy/ca-key.pem")).unwrap();
        assert_eq!(key.permissions().mode() & 0o777, 0o600);
    }
    drop(server);

    // A restart signs a new localhost cert with the same CA, so the old trust still holds.
    let _server = fixture.spawn();
    let client = trusting_client(&ca);
    wait_until_up(&client, &format!("https://localhost:{port}")).await;
    assert_eq!(fs::read_to_string(&ca_path).unwrap(), ca);

    // Losing ca.pem rewrites it from the kept key; old trust still holds.
    drop(_server);
    fs::remove_file(&ca_path).unwrap();
    let server = fixture.spawn();
    wait_until_up(&trusting_client(&ca), &format!("https://localhost:{port}")).await;

    // Losing the key makes a new CA, and ca.pem follows it instead of going stale.
    drop(server);
    let stale = fs::read_to_string(&ca_path).unwrap();
    fs::remove_file(fixture.temp.path().join(".decoy/ca-key.pem")).unwrap();
    let _server = fixture.spawn();
    let deadline = Instant::now() + Duration::from_secs(10);
    while fs::read_to_string(&ca_path).is_ok_and(|current| current == stale) {
        assert!(Instant::now() < deadline, "ca.pem was not rebuilt");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let renewed = fs::read_to_string(&ca_path).unwrap();
    wait_until_up(
        &trusting_client(&renewed),
        &format!("https://localhost:{port}"),
    )
    .await;
    assert_eq!(
        fs::read_to_string(fixture.temp.path().join(".decoy/.gitignore")).unwrap(),
        "*\n"
    );
}

#[tokio::test]
async fn two_first_starts_share_one_auto_ca() {
    let fixture = Fixture::new();
    let ports = [unused_port(), unused_port()];
    for (name, port) in ["a", "b"].iter().zip(ports) {
        fs::write(
            fixture.temp.path().join(format!("{name}.yaml")),
            format!("routes: routes\ncollections: collections.yaml\ntls: auto\nport: {port}\n"),
        )
        .unwrap();
    }
    let _servers = ["a.yaml", "b.yaml"].map(|config| fixture.spawn_config(config));

    let ca_path = fixture.temp.path().join(".decoy/ca.pem");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ca_path.exists() {
        assert!(Instant::now() < deadline, "CA was not written");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let client = trusting_client(&fs::read_to_string(&ca_path).unwrap());
    for port in ports {
        wait_until_up(&client, &format!("https://localhost:{port}")).await;
    }
}

#[test]
fn unreadable_or_mismatched_cert_fails_startup() {
    let fixture = Fixture::new();
    fixture.write_cert();
    fs::write(fixture.temp.path().join("certs/empty.pem"), "").unwrap();
    let other = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    fs::write(
        fixture.temp.path().join("certs/other-key.pem"),
        other.signing_key.serialize_pem(),
    )
    .unwrap();

    for (tls, expected) in [
        (
            "{ cert: certs/missing.pem, key: certs/dev-key.pem }",
            "certs/missing.pem: cannot read TLS cert",
        ),
        (
            "{ cert: certs/empty.pem, key: certs/dev-key.pem }",
            "certs/empty.pem: TLS cert holds no PEM certificates",
        ),
        (
            "{ cert: certs/dev.pem, key: certs/empty.pem }",
            "certs/empty.pem: cannot read TLS key",
        ),
        (
            "{ cert: certs/dev.pem, key: certs/other-key.pem }",
            "certs/other-key.pem: TLS key doesn't fit cert",
        ),
    ] {
        fixture.write_config(&format!("tls: {tls}\nport: {}\n", unused_port()));
        let output = Command::new(env!("CARGO_BIN_EXE_decoy"))
            .args(["serve", "--config", "decoy.yaml"])
            .current_dir(fixture.temp.path())
            .output()
            .unwrap();
        assert!(!output.status.success(), "{tls}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{tls}: {stderr}");
    }
}

struct Fixture {
    temp: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("routes")).unwrap();
        fs::write(temp.path().join("routes/who.yaml"), ROUTE).unwrap();
        fs::write(
            temp.path().join("collections.yaml"),
            "- id: main\n  routes: [who:any:ok]\n",
        )
        .unwrap();
        Self { temp }
    }

    /// Writes a self-signed cert for localhost and returns its PEM.
    fn write_cert(&self) -> String {
        let generated = rcgen::generate_simple_self_signed(vec![
            "localhost".to_owned(),
            "127.0.0.1".to_owned(),
        ])
        .unwrap();
        let certs = self.temp.path().join("certs");
        fs::create_dir_all(&certs).unwrap();
        let pem = generated.cert.pem();
        fs::write(certs.join("dev.pem"), &pem).unwrap();
        fs::write(
            certs.join("dev-key.pem"),
            generated.signing_key.serialize_pem(),
        )
        .unwrap();
        pem
    }

    fn write_config(&self, listeners: &str) {
        fs::write(
            self.temp.path().join("decoy.yaml"),
            format!("routes: routes\ncollections: collections.yaml\n{listeners}"),
        )
        .unwrap();
    }

    fn spawn(&self) -> Server {
        self.spawn_config("decoy.yaml")
    }

    fn spawn_config(&self, config: &str) -> Server {
        Server(
            Command::new(env!("CARGO_BIN_EXE_decoy"))
                .args(["serve", "--config", config])
                .current_dir(self.temp.path())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    }
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn trusting_client(cert: &str) -> reqwest::Client {
    reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(cert.as_bytes()).unwrap())
        .build()
        .unwrap()
}

async fn wait_until_up(client: &reqwest::Client, base: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while client
        .get(format!("{base}/__decoy__/health"))
        .send()
        .await
        .is_err()
    {
        assert!(Instant::now() < deadline, "decoy did not start");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn unused_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
