//! Reloading config into a running server (#293): everything but the listeners is swapped in.

use std::fs;

use std::path::Path;

use decoy_runtime::config::{ServeCliOptions, ServeConfig};
use decoy_runtime::{Reloaded, Server};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const USERS: &str = r#"
id: users
transport: http
match: { method: GET, path: '/users/{id}' }
cases:
  any:
    behaviors:
      ok: { status: 200, body: { name: Ada } }
      missing: { status: 404 }
"#;

const COLLECTIONS: &str =
    "- id: happy\n  routes: [users:any:ok]\n- id: errors\n  routes: [users:any:missing]\n";

struct Fixture {
    temp: tempfile::TempDir,
    server: Server,
    base: String,
}

impl Fixture {
    async fn start() -> Self {
        let temp = tempfile::TempDir::new().unwrap();
        fs::create_dir(temp.path().join("routes")).unwrap();
        fs::write(temp.path().join("routes/users.yaml"), USERS).unwrap();
        fs::write(temp.path().join("collections.yaml"), COLLECTIONS).unwrap();
        fs::write(
            temp.path().join("decoy.yaml"),
            "routes: routes\ncollections: collections.yaml\nhost: 127.0.0.1\nport: 0\n",
        )
        .unwrap();
        let server = decoy_runtime::serve(config(temp.path()), std::future::pending())
            .await
            .unwrap();
        let base = format!("http://{}", server.listeners[0].addr);
        Self { temp, server, base }
    }

    fn write(&self, file: &str, contents: &str) {
        fs::write(self.temp.path().join(file), contents).unwrap();
    }

    async fn reload(&self) -> Result<Reloaded, String> {
        self.server
            .reloader()
            .reload(config(self.temp.path()))
            .await
            .map_err(|error| error.to_string())
    }

    async fn collection(&self, session: &str) -> String {
        let controller = self.server.controller.read().await;
        controller.selection_snapshot(session).collection
    }
}

fn config(dir: &Path) -> ServeConfig {
    ServeConfig::from_options(
        ServeCliOptions {
            config_file: Some(dir.join("decoy.yaml")),
            ..Default::default()
        },
        dir,
    )
    .unwrap()
}

/// One request on an open keep-alive connection; the response body.
async fn request(connection: &mut TcpStream, head: &str, body: &str) -> (u16, String) {
    connection
        .write_all(
            format!(
                "{head}\r\nhost: decoy\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut read = Vec::new();
    loop {
        let mut chunk = [0; 4096];
        let n = connection.read(&mut chunk).await.unwrap();
        assert!(n > 0, "the connection closed");
        read.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&read);
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            continue;
        };
        let length: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse()
            .unwrap();
        if body.len() >= length {
            let status = head[9..12].parse().unwrap();
            return (status, body.to_owned());
        }
    }
}

async fn get(client: &reqwest::Client, url: String) -> (u16, String) {
    let response = client.get(url).send().await.unwrap();
    (response.status().as_u16(), response.text().await.unwrap())
}

#[tokio::test]
async fn a_changed_behavior_serves_on_the_next_request_over_the_same_connection() {
    let fixture = Fixture::start().await;
    let mut connection = TcpStream::connect(fixture.server.listeners[0].addr)
        .await
        .unwrap();
    assert_eq!(
        request(&mut connection, "GET /users/1 HTTP/1.1", "").await,
        (200, r#"{"name":"Ada"}"#.into())
    );

    fixture.write("routes/users.yaml", &USERS.replace("Ada", "Grace"));
    let reloaded = fixture.reload().await.unwrap();

    assert!(reloaded.warnings.is_empty(), "{:?}", reloaded.warnings);
    assert_eq!(reloaded.default_collection, "happy");
    assert_eq!(reloaded.catalog.routes().count(), 1);
    assert_eq!(
        request(&mut connection, "GET /users/1 HTTP/1.1", "").await,
        (200, r#"{"name":"Grace"}"#.into())
    );
}

#[tokio::test]
async fn max_body_bytes_applies_on_reload() {
    let fixture = Fixture::start().await;
    let mut connection = TcpStream::connect(fixture.server.listeners[0].addr)
        .await
        .unwrap();

    fixture.write(
        "decoy.yaml",
        "routes: routes\ncollections: collections.yaml\nhost: 127.0.0.1\nport: 0\nmaxBodyBytes: 10\n",
    );
    fixture.reload().await.unwrap();

    let (status, _) = request(&mut connection, "POST /users/1 HTTP/1.1", "eleven byte").await;
    assert_eq!(status, 413);
}

#[tokio::test]
async fn a_collection_naming_a_missing_behavior_keeps_the_old_config_live() {
    let fixture = Fixture::start().await;

    fixture.write(
        "collections.yaml",
        "- id: happy\n  routes: [users:any:nope]\n",
    );
    let error = fixture.reload().await.unwrap_err();

    assert!(error.contains("users:any:nope"), "{error}");
    assert_eq!(fixture.collection("default").await, "happy");
    let catalog = fixture.server.controller.read().await.catalog();
    assert_eq!(
        catalog.collection_ids().collect::<Vec<_>>(),
        ["errors", "happy"]
    );
}

#[tokio::test]
async fn an_override_whose_behavior_is_gone_is_dropped_with_a_warning() {
    let fixture = Fixture::start().await;
    fixture
        .server
        .controller
        .write()
        .await
        .try_use_route("e2e-1", "users", "any", "missing")
        .unwrap();

    fixture.write(
        "routes/users.yaml",
        &USERS.replace("      missing: { status: 404 }\n", ""),
    );
    fixture.write(
        "collections.yaml",
        "- id: happy\n  routes: [users:any:ok]\n",
    );
    let reloaded = fixture.reload().await.unwrap();

    assert_eq!(
        reloaded.warnings,
        ["session e2e-1: override `users:any:missing` dropped, it no longer exists"]
    );
    let controller = fixture.server.controller.read().await;
    assert!(controller.selection_snapshot("e2e-1").overrides.is_empty());
}

#[tokio::test]
async fn a_session_on_a_removed_collection_falls_back_and_others_follow_the_new_default() {
    let fixture = Fixture::start().await;
    {
        let mut controller = fixture.server.controller.write().await;
        controller.try_use_collection("picked", "errors").unwrap();
        controller.reset("unpicked");
    }

    fixture.write(
        "collections.yaml",
        "- id: calm\n  routes: [users:any:ok]\n- id: happy\n  routes: [users:any:ok]\n",
    );
    let reloaded = fixture.reload().await.unwrap();

    assert_eq!(
        reloaded.warnings,
        ["session picked: collection `errors` is gone, back to `calm`"]
    );
    assert_eq!(fixture.collection("picked").await, "calm");
    assert_eq!(fixture.collection("unpicked").await, "calm");
    assert_eq!(fixture.collection("never-seen").await, "calm");
}

#[tokio::test]
async fn a_changed_port_warns_and_applies_everything_else() {
    let fixture = Fixture::start().await;
    let client = reqwest::Client::new();

    fixture.write(
        "decoy.yaml",
        "routes: routes\ncollections: collections.yaml\nhost: 127.0.0.1\nport: 1\ncollection: errors\n",
    );
    let reloaded = fixture.reload().await.unwrap();

    assert_eq!(reloaded.warnings, ["listener changes need restart"]);
    // Still on the old port, now serving the new default Collection.
    let (status, _) = get(&client, format!("{}/users/1", fixture.base)).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn broken_yaml_keeps_the_old_config_live() {
    let fixture = Fixture::start().await;
    let client = reqwest::Client::new();

    fixture.write("routes/users.yaml", "id: users\ncases: [");
    let error = fixture.reload().await.unwrap_err();

    assert!(error.contains("users.yaml"), "{error}");
    assert_eq!(
        get(&client, format!("{}/users/1", fixture.base)).await,
        (200, r#"{"name":"Ada"}"#.into())
    );
}
