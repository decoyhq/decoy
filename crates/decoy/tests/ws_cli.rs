//! `decoy serve` answering WebSocket clients on the HTTP port: Routes by path and query,
//! `on: connect` Cases, message Cases, timed `messages`, `close`, Sessions and Control API `send`
//! and `wait`.

use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const FRIEND: &str = r#"
id: friend-socket
transport: ws
match:
  path: /subscribe
  query: { service: friend }
cases:
  hello:
    on: connect
    behaviors:
      ping: { message: ping }
      silent: { messages: [] }
      unauthorized:
        message: { type: error, reason: unauthorized }
        close: { code: 4001, reason: unauthorized }
  status:
    match: { message: { type: get-status } }
    behaviors:
      online: { message: { id: '${ message.id }', status: online } }
      offline: { message: { id: '${ message.id }', status: offline } }
  subscribe:
    match: { message: { type: subscribe } }
    behaviors:
      updates:
        messages:
          - { payload: { type: subscribed, topic: '${ message.topic }' } }
          - { payload: { type: update, topic: '${ message.topic }', count: 1 }, delay: 200ms }
"#;

const BEST_FRIEND: &str = r#"
id: best-friend-socket
transport: ws
match:
  path: /subscribe
  query: { service: best-friend }
cases:
  hello:
    on: connect
    behaviors:
      ping: { message: 'ping ${ query.client }' }
"#;

const COLLECTIONS: &str = r#"
- id: happy-path
  routes:
    - friend-socket:hello:ping
    - friend-socket:status:online
    - friend-socket:subscribe:updates
    - best-friend-socket:hello:ping
- id: manual-start
  from: happy-path
  routes:
    - friend-socket:hello:silent
"#;

const QUIET: Duration = Duration::from_millis(300);

#[tokio::test]
async fn answers_the_open_and_each_message_and_skips_unmatched_ones() {
    let server = Server::start().await;
    let mut friend = server
        .connect("/subscribe?client=web&service=friend", None)
        .await;
    assert_eq!(friend.text().await, "ping");

    friend.send(json!({ "id": 7, "type": "get-status" })).await;
    assert_eq!(friend.json().await, json!({ "id": 7, "status": "online" }));

    // No Case: nothing comes back and the socket stays open.
    friend.send(json!({ "type": "nope" })).await;
    friend.silent().await;

    friend
        .send(json!({ "type": "subscribe", "topic": "news" }))
        .await;
    assert_eq!(
        friend.json().await,
        json!({ "type": "subscribed", "topic": "news" })
    );
    assert_eq!(
        friend.json().await,
        json!({ "type": "update", "topic": "news", "count": 1 })
    );

    let mut best = server
        .connect("/subscribe?service=best-friend&client=web", None)
        .await;
    assert_eq!(best.text().await, "ping web");

    let rejected = server.try_connect("/subscribe?service=unknown", None).await;
    let Err(WsError::Http(response)) = rejected else {
        panic!("expected the HTTP miss, got {rejected:?}")
    };
    assert_eq!(response.status(), 501);
}

#[tokio::test]
async fn sessions_pick_behaviors_and_send_reaches_only_their_sockets() {
    let server = Server::start().await;
    server
        .control("useCollection", r#"{"collection":"manual-start"}"#, "s1")
        .await;
    let mut quiet = server
        .connect("/subscribe?service=friend", Some("s1"))
        .await;
    let mut default = server.connect("/subscribe?service=friend", None).await;
    assert_eq!(default.text().await, "ping");
    quiet.silent().await;

    let (status, body) = server
        .send(r#"{"route":"friend-socket","message":"ping"}"#, "s1")
        .await;
    assert_eq!((status, body), (200, json!({ "delivered": 1 })));
    assert_eq!(quiet.text().await, "ping");
    default.silent().await;

    // A Selection change applies to the open socket's next message.
    server
        .control(
            "useRoute",
            r#"{"route":"friend-socket","case":"status","behavior":"offline"}"#,
            "s1",
        )
        .await;
    quiet.send(json!({ "id": 1, "type": "get-status" })).await;
    assert_eq!(quiet.json().await, json!({ "id": 1, "status": "offline" }));
    default.send(json!({ "id": 1, "type": "get-status" })).await;
    assert_eq!(default.json().await, json!({ "id": 1, "status": "online" }));
}

#[tokio::test]
async fn a_behavior_closes_the_socket_after_its_messages() {
    let server = Server::start().await;
    server
        .control(
            "useRoute",
            r#"{"route":"friend-socket","case":"hello","behavior":"unauthorized"}"#,
            "s2",
        )
        .await;
    let mut socket = server
        .connect("/subscribe?service=friend", Some("s2"))
        .await;
    assert_eq!(
        socket.json().await,
        json!({ "type": "error", "reason": "unauthorized" })
    );
    let Some(Ok(Message::Close(Some(frame)))) = socket.0.next().await else {
        panic!("expected a close frame")
    };
    assert_eq!(frame.code, CloseCode::from(4001));
    assert_eq!(frame.reason.as_str(), "unauthorized");
}

#[tokio::test]
async fn send_to_a_socket_route_waits_for_a_connection_then_times_out() {
    let server = Server::start().await;
    let (status, body) = server
        .send(
            r#"{"route":"friend-socket","message":{"a":1},"timeout":100}"#,
            "nobody",
        )
        .await;
    assert_eq!(status, 408, "{body}");
}

#[tokio::test]
async fn wait_resolves_once_that_sessions_socket_opens_else_times_out() {
    let server = Server::start().await;
    let _elsewhere = server
        .connect("/subscribe?service=friend", Some("other"))
        .await;
    let (status, body) = server
        .wait(
            r#"{"route":"friend-socket","connected":true,"timeout":100}"#,
            "w1",
        )
        .await;
    assert_eq!(status, 408, "{body}");

    let waiting = server.wait(r#"{"route":"friend-socket","connected":true}"#, "w1");
    let connecting = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        server
            .connect("/subscribe?service=friend", Some("w1"))
            .await
    };
    let ((status, body), _socket) = tokio::join!(waiting, connecting);
    assert_eq!((status, body), (200, json!({ "connections": 1 })));
}

#[tokio::test]
async fn back_to_back_messages_are_answered_in_order() {
    let server = Server::start().await;
    // Several sockets bursting at once, so replies run on many Decoy threads.
    let bursts = (0..20).map(|_| async {
        let mut friend = server.connect("/subscribe?service=friend", None).await;
        assert_eq!(friend.text().await, "ping");
        for id in 0..50 {
            let frame = json!({ "id": id, "type": "get-status" }).to_string();
            friend.0.feed(Message::Text(frame.into())).await.unwrap();
        }
        friend.0.flush().await.unwrap();
        let mut ids = Vec::new();
        for _ in 0..50 {
            ids.push(friend.json().await["id"].as_u64().unwrap());
        }
        assert_eq!(ids, (0..50).collect::<Vec<_>>());
    });
    futures_util::future::join_all(bursts).await;
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_cancels_an_active_transform() {
    let route = FRIEND.replace("transport: ws", "transport: ws\npipeline: slow");
    let temp = Server::mocks(&[("friend.yaml", &route), ("best-friend.yaml", BEST_FRIEND)]);
    let marker = temp.path().join("started");
    fs::write(temp.path().join("decoy.yaml"), "routes: routes\ncollections: collections.yaml\nport: 0\ntransformers:\n  slow: {output: [sh, -c, 'touch started && exec sleep 30']}\npipelines:\n  slow: {output: [slow]}\n").unwrap();
    let server = Server::start_from(temp);
    let mut socket = server.connect("/subscribe?service=friend", None).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        Command::new("kill")
            .args(["-INT", &server.child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), socket.close_code())
            .await
            .unwrap(),
        CloseCode::Away
    );
}

// Linux only: 400 sockets pass macOS's default limit of 256 open files.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn shutdown_closes_every_open_socket_with_going_away() {
    let mut server = Server::start().await;
    let mut sockets = Vec::new();
    // Enough sockets that some are still closing when Decoy would otherwise exit.
    for _ in 0..400 {
        let mut socket = server.connect("/subscribe?service=friend", None).await;
        assert_eq!(socket.text().await, "ping");
        sockets.push(socket);
    }

    let status = Command::new("kill")
        .args(["-INT", &server.child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    for socket in &mut sockets {
        assert_eq!(socket.close_code().await, CloseCode::Away);
    }
    let _ = server.child.wait();
}

#[test]
fn ws_routes_that_could_match_one_upgrade_fail_startup() {
    let twin = FRIEND.replace("id: friend-socket", "id: friend-twin");
    let stderr = Server::fails_to_start(&[("friend.yaml", FRIEND), ("twin.yaml", &twin)]);
    assert!(
        stderr.contains(
            "WebSocket Routes `friend-socket` and `friend-twin` can both match the same upgrade"
        ),
        "{stderr}"
    );
}

#[derive(Debug)]
struct Socket(WebSocketStream<MaybeTlsStream<TcpStream>>);

impl Socket {
    async fn send(&mut self, message: Value) {
        self.0
            .send(Message::Text(message.to_string().into()))
            .await
            .unwrap();
    }

    async fn text(&mut self) -> String {
        let next = tokio::time::timeout(Duration::from_secs(5), self.0.next()).await;
        match next.expect("no message within 5s") {
            Some(Ok(Message::Text(text))) => text.as_str().to_owned(),
            other => panic!("expected a text frame, got {other:?}"),
        }
    }

    async fn json(&mut self) -> Value {
        serde_json::from_str(&self.text().await).unwrap()
    }

    /// The code of the close frame that ends the socket, skipping frames before it.
    async fn close_code(&mut self) -> CloseCode {
        let closed = async {
            while let Some(next) = self.0.next().await {
                match next {
                    Ok(Message::Close(Some(frame))) => return frame.code,
                    Ok(_) => {}
                    Err(error) => panic!("socket ended without a close frame: {error}"),
                }
            }
            panic!("socket ended without a close frame")
        };
        tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .expect("no close frame within 5s")
    }

    /// Nothing arrives for a while, and the socket is still open.
    async fn silent(&mut self) {
        if let Ok(next) = tokio::time::timeout(QUIET, self.0.next()).await {
            panic!("expected silence, got {next:?}");
        }
    }
}

struct Server {
    _temp: TempDir,
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn mocks(routes: &[(&str, &str)]) -> TempDir {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("routes")).unwrap();
        for (file, route) in routes {
            fs::write(temp.path().join("routes").join(file), route).unwrap();
        }
        fs::write(temp.path().join("collections.yaml"), COLLECTIONS).unwrap();
        fs::write(
            temp.path().join("decoy.yaml"),
            "routes: routes\ncollections: collections.yaml\nport: 0\n",
        )
        .unwrap();
        temp
    }

    fn serve(temp: &TempDir) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_decoy"));
        command
            .args(["serve", "--config", "decoy.yaml"])
            .current_dir(temp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        command
    }

    /// Runs `decoy serve` on `routes`, expecting it to fail at startup; returns its stderr.
    fn fails_to_start(routes: &[(&str, &str)]) -> String {
        let temp = Self::mocks(routes);
        let output = Self::serve(&temp).output().unwrap();
        assert!(!output.status.success(), "decoy started");
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    async fn start() -> Self {
        let temp = Self::mocks(&[("friend.yaml", FRIEND), ("best-friend.yaml", BEST_FRIEND)]);
        Self::start_from(temp)
    }

    fn start_from(temp: TempDir) -> Self {
        let mut child = Self::serve(&temp).spawn().unwrap();
        // Port 0 lets decoy pick the port; read it from `listening on`, then keep echoing stderr
        // so the pipe never fills.
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
        Self {
            _temp: temp,
            child,
            port,
        }
    }

    async fn try_connect(
        &self,
        path_and_query: &str,
        session: Option<&str>,
    ) -> Result<Socket, WsError> {
        let mut request = format!("ws://127.0.0.1:{}{path_and_query}", self.port)
            .into_client_request()
            .unwrap();
        if let Some(session) = session {
            request
                .headers_mut()
                .insert("x-mock-session", session.parse().unwrap());
        }
        let (socket, _) = tokio_tungstenite::connect_async(request).await?;
        Ok(Socket(socket))
    }

    async fn connect(&self, path_and_query: &str, session: Option<&str>) -> Socket {
        self.try_connect(path_and_query, session).await.unwrap()
    }

    async fn post(&self, verb: &str, body: &str, session: &str) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!(
                "http://127.0.0.1:{}/__decoy__/control/{verb}",
                self.port
            ))
            .header("content-type", "application/json")
            .header("x-mock-session", session)
            .body(body.to_owned())
            .send()
            .await
            .unwrap()
    }

    async fn control(&self, verb: &str, body: &str, session: &str) {
        let response = self.post(verb, body, session).await;
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    }

    async fn send(&self, body: &str, session: &str) -> (u16, Value) {
        let response = self.post("send", body, session).await;
        (response.status().as_u16(), response.json().await.unwrap())
    }

    async fn wait(&self, body: &str, session: &str) -> (u16, Value) {
        let response = self.post("wait", body, session).await;
        (response.status().as_u16(), response.json().await.unwrap())
    }
}
