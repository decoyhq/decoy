//! Reloading config into a running server (#293): everything but the listeners is swapped in.

use std::fs;

use decoy_runtime::config::{ServeCliOptions, ServeConfig};
use decoy_runtime::{Reloaded, Server};

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
        let server = decoy_runtime::serve(config(&temp), std::future::pending())
            .await
            .unwrap();
        let base = format!("http://{}", server.listeners[0].addr);
        Self { temp, server, base }
    }

    fn write(&self, file: &str, contents: &str) {
        fs::write(self.temp.path().join(file), contents).unwrap();
    }

    async fn reload(&self) -> Result<Reloaded, String> {
        let config = ServeConfig::from_options(self.options(), self.temp.path())
            .map_err(|error| error.to_string())?;
        self.server
            .reloader()
            .reload(config)
            .await
            .map_err(|error| error.to_string())
    }

    fn options(&self) -> ServeCliOptions {
        ServeCliOptions {
            config_file: Some(self.temp.path().join("decoy.yaml")),
            ..Default::default()
        }
    }

    async fn collection(&self, session: &str) -> String {
        let controller = self.server.controller.read().await;
        controller.selection_snapshot(session).collection
    }
}

fn config(temp: &tempfile::TempDir) -> ServeConfig {
    ServeConfig::from_options(
        ServeCliOptions {
            config_file: Some(temp.path().join("decoy.yaml")),
            ..Default::default()
        },
        temp.path(),
    )
    .unwrap()
}

async fn get(client: &reqwest::Client, url: String) -> (u16, String) {
    let response = client.get(url).send().await.unwrap();
    (response.status().as_u16(), response.text().await.unwrap())
}

#[tokio::test]
async fn a_changed_behavior_serves_on_the_next_request_over_the_same_connection() {
    let fixture = Fixture::start().await;
    // One idle connection, reused across the reload.
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(1)
        .build()
        .unwrap();
    let url = format!("{}/users/1", fixture.base);
    assert_eq!(
        get(&client, url.clone()).await,
        (200, r#"{"name":"Ada"}"#.into())
    );

    fixture.write("routes/users.yaml", &USERS.replace("Ada", "Grace"));
    let reloaded = fixture.reload().await.unwrap();

    assert_eq!(
        reloaded,
        Reloaded {
            routes: 1,
            collections: 2,
            warnings: vec![]
        }
    );
    assert_eq!(get(&client, url).await, (200, r#"{"name":"Grace"}"#.into()));
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
