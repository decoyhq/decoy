//! `--watch` (#294): one reload signal per burst of config file changes.

use std::fs;
use std::path::Path;
use std::time::Duration;

use decoy_runtime::config::{ServeCliOptions, ServeConfig};
use decoy_runtime::watch::{ConfigWatcher, DEBOUNCE, WatchedPaths};

const ROUTE: &str = "id: users\ntransport: http\nmatch: { method: GET, path: /users }\ncases:\n  any:\n    behaviors:\n      ok: { status: 200 }\n";

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().unwrap();
    fs::create_dir(temp.path().join("routes")).unwrap();
    fs::write(temp.path().join("routes/users.yaml"), ROUTE).unwrap();
    fs::write(
        temp.path().join("collections.yaml"),
        "- id: default\n  routes: [users:any:ok]\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("decoy.yaml"),
        "routes: routes\ncollections: collections.yaml\nprotos: { descriptorSets: [api.binpb] }\n",
    )
    .unwrap();
    fs::write(temp.path().join("api.binpb"), b"").unwrap();
    temp
}

/// What the config in `dir` says to watch right now.
fn paths(dir: &Path) -> Option<WatchedPaths> {
    let config_file = dir.join("decoy.yaml");
    let config = ServeConfig::from_options(
        ServeCliOptions {
            config_file: Some(config_file.clone()),
            ..Default::default()
        },
        dir,
    )
    .ok()?;
    Some(WatchedPaths::of(&config, Some(&config_file)))
}

fn watcher(dir: &Path, poll: bool) -> ConfigWatcher {
    let owned = dir.to_path_buf();
    ConfigWatcher::new(paths(dir).unwrap(), Box::new(move || paths(&owned)), poll).unwrap()
}

/// Signals that arrive within `wait`.
async fn signals(watcher: &mut ConfigWatcher, wait: Duration) -> usize {
    let mut count = 0;
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Some(())) = tokio::time::timeout_at(deadline, watcher.changed()).await {
        count += 1;
    }
    count
}

/// Lets the watcher settle before the first write, so setup writes don't count.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(200)).await;
}

fn write(dir: &Path, file: &str, contents: &str) {
    fs::write(dir.join(file), contents).unwrap();
}

#[tokio::test]
async fn saving_a_routes_file_signals_once_within_a_second() {
    let temp = fixture();
    let mut changes = watcher(temp.path(), false);
    settle().await;

    write(
        temp.path(),
        "routes/users.yaml",
        &ROUTE.replace("200", "201"),
    );

    let started = std::time::Instant::now();
    assert_eq!(changes.changed().await, Some(()));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(signals(&mut changes, DEBOUNCE * 3).await, 0);
}

#[tokio::test]
async fn an_atomic_save_signals_exactly_once() {
    let temp = fixture();
    let mut changes = watcher(temp.path(), false);
    settle().await;

    // How editors save: write a temp file next to it, then rename it over the original.
    write(
        temp.path(),
        "collections.yaml.tmp",
        "- id: default\n  routes: []\n",
    );
    fs::rename(
        temp.path().join("collections.yaml.tmp"),
        temp.path().join("collections.yaml"),
    )
    .unwrap();

    assert_eq!(signals(&mut changes, Duration::from_secs(2)).await, 1);
}

#[tokio::test]
async fn a_burst_of_saves_signals_once_after_the_debounce() {
    let temp = fixture();
    let mut changes = watcher(temp.path(), false);
    settle().await;

    for status in 201..206 {
        write(
            temp.path(),
            "routes/users.yaml",
            &ROUTE.replace("200", &status.to_string()),
        );
        tokio::time::sleep(DEBOUNCE / 4).await;
    }

    assert_eq!(signals(&mut changes, Duration::from_secs(2)).await, 1);
}

#[tokio::test]
async fn unrelated_files_next_to_the_config_are_ignored() {
    let temp = fixture();
    let mut changes = watcher(temp.path(), false);
    settle().await;

    write(temp.path(), "notes.md", "not config");

    assert_eq!(signals(&mut changes, DEBOUNCE * 3).await, 0);
}

#[tokio::test]
async fn polling_picks_up_a_change_where_events_do_not_arrive() {
    let temp = fixture();
    let mut changes = watcher(temp.path(), true);
    assert_eq!(changes.describe(), "watching the config files (polling)");
    settle().await;

    write(
        temp.path(),
        "decoy.yaml",
        "routes: routes\ncollections: collections.yaml\nport: 4011\n",
    );

    assert_eq!(signals(&mut changes, Duration::from_secs(3)).await, 1);
}

#[tokio::test]
async fn a_rebuilt_descriptor_set_signals() {
    let temp = fixture();
    let mut changes = watcher(temp.path(), false);
    settle().await;

    fs::write(temp.path().join("api.binpb"), b"\x0a\x00").unwrap();

    assert_eq!(signals(&mut changes, Duration::from_secs(2)).await, 1);
}

#[tokio::test]
async fn editor_swap_files_in_the_routes_folder_are_ignored() {
    let temp = fixture();
    let mut changes = watcher(temp.path(), false);
    settle().await;

    write(temp.path(), "routes/.users.yaml.swp", "vim swap");
    write(temp.path(), "routes/users.yaml~", "backup");

    assert_eq!(signals(&mut changes, DEBOUNCE * 3).await, 0);
}

#[tokio::test]
async fn a_recreated_routes_folder_keeps_being_watched() {
    let temp = fixture();
    let mut changes = watcher(temp.path(), false);
    settle().await;

    fs::remove_dir_all(temp.path().join("routes")).unwrap();
    fs::create_dir(temp.path().join("routes")).unwrap();
    write(temp.path(), "routes/users.yaml", ROUTE);
    assert_eq!(signals(&mut changes, Duration::from_secs(1)).await, 1);

    write(
        temp.path(),
        "routes/users.yaml",
        &ROUTE.replace("200", "201"),
    );
    assert_eq!(signals(&mut changes, Duration::from_secs(2)).await, 1);
}

#[tokio::test]
async fn a_config_pointing_at_another_routes_folder_is_followed() {
    let temp = fixture();
    let mut changes = watcher(temp.path(), false);
    settle().await;

    fs::create_dir(temp.path().join("v2")).unwrap();
    write(temp.path(), "v2/users.yaml", ROUTE);
    write(
        temp.path(),
        "decoy.yaml",
        "routes: v2\ncollections: collections.yaml\nprotos: { descriptorSets: [api.binpb] }\n",
    );
    assert_eq!(signals(&mut changes, Duration::from_secs(2)).await, 1);

    write(temp.path(), "v2/users.yaml", &ROUTE.replace("200", "201"));
    assert_eq!(signals(&mut changes, Duration::from_secs(2)).await, 1);
}

#[tokio::test]
async fn polling_picks_up_an_atomic_save_of_the_config_file() {
    let temp = fixture();
    let mut changes = watcher(temp.path(), true);
    settle().await;

    write(
        temp.path(),
        "decoy.yaml.tmp",
        "routes: routes\ncollections: collections.yaml\nport: 4012\nprotos: { descriptorSets: [api.binpb] }\n",
    );
    fs::rename(
        temp.path().join("decoy.yaml.tmp"),
        temp.path().join("decoy.yaml"),
    )
    .unwrap();

    assert_eq!(signals(&mut changes, Duration::from_secs(3)).await, 1);
}

#[tokio::test]
async fn a_proto_root_deleted_and_recreated_keeps_being_watched() {
    a_deleted_and_recreated_proto_root_signals(false).await;
}

#[tokio::test]
async fn polling_keeps_watching_a_proto_root_deleted_and_recreated() {
    a_deleted_and_recreated_proto_root_signals(true).await;
}

async fn a_deleted_and_recreated_proto_root_signals(poll: bool) {
    let temp = fixture();
    fs::create_dir(temp.path().join("protos")).unwrap();
    write(temp.path(), "protos/api.proto", "syntax = \"proto3\";\n");
    write(
        temp.path(),
        "decoy.yaml",
        "routes: routes\ncollections: collections.yaml\nprotos: { roots: [protos] }\n",
    );
    let mut changes = watcher(temp.path(), poll);
    settle().await;

    // Gone for longer than the debounce, so the watches are set up again while it's missing.
    fs::remove_dir_all(temp.path().join("protos")).unwrap();
    assert_eq!(signals(&mut changes, Duration::from_secs(3)).await, 1);
    fs::create_dir(temp.path().join("protos")).unwrap();
    write(temp.path(), "protos/api.proto", "syntax = \"proto3\";\n");
    assert_eq!(signals(&mut changes, Duration::from_secs(3)).await, 1);

    write(
        temp.path(),
        "protos/api.proto",
        "syntax = \"proto3\";\npackage a;\n",
    );
    assert_eq!(signals(&mut changes, Duration::from_secs(3)).await, 1);
}

#[tokio::test]
async fn proto_files_watch_their_imports_but_not_the_rest_of_the_root() {
    let temp = fixture();
    for dir in ["protos/shared", "protos/services", "protos/legacy"] {
        fs::create_dir_all(temp.path().join(dir)).unwrap();
    }
    write(
        temp.path(),
        "protos/shared/money.proto",
        "syntax = \"proto3\";\nmessage Money {}\n",
    );
    write(
        temp.path(),
        "protos/services/api.proto",
        "syntax = \"proto3\";\nimport \"shared/money.proto\";\nmessage Api { Money price = 1; }\n",
    );
    write(
        temp.path(),
        "protos/legacy/old.proto",
        "syntax = \"proto3\";\n",
    );
    write(
        temp.path(),
        "decoy.yaml",
        "routes: routes\ncollections: collections.yaml\nprotos: { roots: [protos], files: [protos/services/*.proto] }\n",
    );
    let mut changes = watcher(temp.path(), false);
    settle().await;

    write(
        temp.path(),
        "protos/legacy/old.proto",
        "syntax = \"proto3\";\npackage old;\n",
    );
    assert_eq!(signals(&mut changes, DEBOUNCE * 3).await, 0);

    write(
        temp.path(),
        "protos/shared/money.proto",
        "syntax = \"proto3\";\nmessage Money { int64 units = 1; }\n",
    );
    assert_eq!(signals(&mut changes, Duration::from_secs(2)).await, 1);

    write(
        temp.path(),
        "protos/services/new.proto",
        "syntax = \"proto3\";\nmessage New {}\n",
    );
    assert_eq!(signals(&mut changes, Duration::from_secs(2)).await, 1);
}
