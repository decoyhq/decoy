//! `--watch` (#294): one reload signal per burst of config file changes.

use std::fs;
use std::path::Path;
use std::time::Duration;

use decoy_runtime::config::{ServeCliOptions, ServeConfig};
use decoy_runtime::watch::{DEBOUNCE, WatchedPaths, watch};
use tokio::sync::mpsc::Receiver;

const ROUTE: &str = "id: users\ntransport: http\nmatch: { method: GET, path: /users }\ncases:\n  any:\n    behaviors:\n      ok: { status: 200 }\n";

fn fixture() -> (tempfile::TempDir, WatchedPaths) {
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
        "routes: routes\ncollections: collections.yaml\n",
    )
    .unwrap();
    let config_file = temp.path().join("decoy.yaml");
    let config = ServeConfig::from_options(
        ServeCliOptions {
            config_file: Some(config_file.clone()),
            ..Default::default()
        },
        temp.path(),
    )
    .unwrap();
    let paths = WatchedPaths::of(&config, Some(&config_file));
    (temp, paths)
}

/// Signals that arrive within `wait`.
async fn signals(changes: &mut Receiver<()>, wait: Duration) -> usize {
    let mut count = 0;
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Some(())) = tokio::time::timeout_at(deadline, changes.recv()).await {
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
    let (temp, paths) = fixture();
    let (_watcher, mut changes) = watch(paths, false).unwrap();
    settle().await;

    write(
        temp.path(),
        "routes/users.yaml",
        &ROUTE.replace("200", "201"),
    );

    let started = std::time::Instant::now();
    assert_eq!(changes.recv().await, Some(()));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(signals(&mut changes, DEBOUNCE * 3).await, 0);
}

#[tokio::test]
async fn an_atomic_save_signals_exactly_once() {
    let (temp, paths) = fixture();
    let (_watcher, mut changes) = watch(paths, false).unwrap();
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
    let (temp, paths) = fixture();
    let (_watcher, mut changes) = watch(paths, false).unwrap();
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
    let (temp, paths) = fixture();
    let (_watcher, mut changes) = watch(paths, false).unwrap();
    settle().await;

    write(temp.path(), "notes.md", "not config");

    assert_eq!(signals(&mut changes, DEBOUNCE * 3).await, 0);
}

#[tokio::test]
async fn polling_picks_up_a_change_where_events_do_not_arrive() {
    let (temp, paths) = fixture();
    let (watcher, mut changes) = watch(paths, true).unwrap();
    assert!(watcher.polling);
    settle().await;

    write(
        temp.path(),
        "decoy.yaml",
        "routes: routes\ncollections: collections.yaml\nport: 4011\n",
    );

    assert_eq!(signals(&mut changes, Duration::from_secs(3)).await, 1);
}
