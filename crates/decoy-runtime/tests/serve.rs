use std::fs;

use decoy_runtime::config::{ServeCliOptions, ServeConfig};

#[tokio::test]
async fn serve_hands_back_listeners_and_controller_and_stops_on_the_shutdown_signal() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::create_dir(temp.path().join("routes")).unwrap();
    fs::write(
        temp.path().join("collections.yaml"),
        "- id: default\n  routes: []\n",
    )
    .unwrap();
    let config = ServeConfig::from_options(
        ServeCliOptions {
            routes: Some("routes".into()),
            collections: Some("collections.yaml".into()),
            host: Some("127.0.0.1".into()),
            port: Some(0),
            ..Default::default()
        },
        temp.path(),
    )
    .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();

    let server = decoy_runtime::serve(config, async {
        let _ = stopped.await;
    })
    .await
    .unwrap();

    let [listener] = server.listeners.as_slice() else {
        panic!("{:?}", server.listeners);
    };
    assert_eq!((listener.name.as_str(), listener.tls), ("port", false));
    assert_ne!(listener.addr.port(), 0);
    // The handed-back Controller holds the Catalog loaded from the config.
    let mut controller = server.controller.write().await;
    assert!(controller.try_use_collection("global", "default").is_ok());
    assert!(controller.try_use_collection("global", "missing").is_err());
    drop(controller);
    let addr = listener.addr;
    tokio::net::TcpStream::connect(addr).await.unwrap();

    stop.send(()).unwrap();
    server.wait().await.unwrap();
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
}
