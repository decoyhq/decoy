//! The in-process event broadcast a `Server` hands its subscribers (#288).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use decoy_runtime::config::{ProtoSources, ServeCliOptions, ServeConfig};
use decoy_runtime::connections::Connections;
use decoy_runtime::events::{
    CAPACITY, Event, Level, Outcome, RequestEvent, SessionChange, SessionEvent, SessionSource,
};
use decoy_runtime::grpc::GrpcReply;
use decoy_runtime::{Candidate, HttpExecutionRequest, HttpMethod, Rejection, Server};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

const USERS: &str = r#"
id: get-user
transport: http
match: { method: GET, path: '/users/{id}' }
cases:
  user-123:
    match: { pathParams: { id: "123" } }
    behaviors:
      success:
        status: 200
        headers: { x-mock: yes }
        body: { id: "123" }
"#;

struct Running {
    server: Server,
    base: String,
    _temp: tempfile::TempDir,
}

async fn start(routes: &[&str], collections: &str) -> Running {
    let temp = tempfile::TempDir::new().unwrap();
    fs::create_dir(temp.path().join("routes")).unwrap();
    for (index, route) in routes.iter().enumerate() {
        fs::write(temp.path().join(format!("routes/{index}.yaml")), route).unwrap();
    }
    fs::write(temp.path().join("collections.yaml"), collections).unwrap();
    let mut config = ServeConfig::from_options(
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
    config.protos = ProtoSources {
        roots: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proto")],
        descriptor_sets: vec![],
    };
    let server = decoy_runtime::serve(config, std::future::pending())
        .await
        .unwrap();
    let base = format!("http://{}", server.listeners[0].addr);
    Running {
        server,
        base,
        _temp: temp,
    }
}

async fn next_request(events: &mut Receiver<Event>) -> Arc<RequestEvent> {
    loop {
        if let Event::Request(event) = events.recv().await.unwrap() {
            return event;
        }
    }
}

#[tokio::test]
async fn a_hit_emits_the_request_the_winner_and_the_response() {
    let other = "id: other\ntransport: http\nmatch: { method: GET, path: /other }\ncases:\n  any:\n    behaviors:\n      ok: { status: 200 }\n";
    let running = start(
        &[USERS, other],
        "- id: default\n  routes: [get-user:user-123:success, other:any:ok]\n",
    )
    .await;
    let mut events = running.server.subscribe();

    let response = reqwest::Client::new()
        .get(format!("{}/users/123?expand=1", running.base))
        .header("x-mock-session", "s1")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let event = next_request(&mut events).await;
    assert_eq!(event.session, "s1");
    assert_eq!(event.method, "GET");
    assert_eq!(event.url, "/users/123?expand=1");
    assert_eq!(event.headers["x-mock-session"], "s1");
    assert_eq!(event.body.size, 0);
    let Outcome::Hit(hit) = &event.outcome else {
        panic!("{:?}", event.outcome)
    };
    assert_eq!(hit.activation.address(), "get-user:user-123:success");
    assert_eq!(
        hit.skipped,
        [Candidate {
            address: "other:any:ok".into(),
            rejection: Rejection::Path {
                expected: "/other".into()
            },
        }]
    );
    assert_eq!(
        hit.path_params,
        BTreeMap::from([("id".into(), "123".into())])
    );
    assert_eq!(
        hit.conditions,
        serde_json::json!({ "pathParams": { "id": "123" } })
    );
    let response = &event.response;
    assert_eq!(response.status, 200);
    assert!(response.headers.contains(&("x-mock".into(), "yes".into())));
    assert_eq!(response.body.bytes, br#"{"id":"123"}"#);
    assert!(!response.body.decoded);
    assert_eq!(response.body.size, 12);
}

#[tokio::test]
async fn a_miss_emits_every_rejected_candidate() {
    let running = start(
        &[USERS],
        "- id: default\n  routes: [get-user:user-123:success]\n",
    )
    .await;
    let mut events = running.server.subscribe();

    reqwest::get(format!("{}/users/456", running.base))
        .await
        .unwrap();

    let event = next_request(&mut events).await;
    assert_eq!(event.session, "default");
    let Outcome::Miss(miss) = &event.outcome else {
        panic!("{:?}", event.outcome)
    };
    assert_eq!(miss.candidates.len(), 1);
    assert!(matches!(
        miss.candidates[0].rejection,
        Rejection::Condition(_)
    ));
    assert_eq!(event.response.status, 501);
}

#[tokio::test]
async fn a_passthrough_emits_the_upstream_url_and_status() {
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    let app = axum::Router::new().fallback(|| async { (axum::http::StatusCode::ACCEPTED, "real") });
    tokio::spawn(async move { axum::serve(upstream, app).await });
    let route = format!(
        "id: real\ntransport: http\nmatch: {{ method: GET, path: /real }}\ncases:\n  any:\n    behaviors:\n      upstream: {{ kind: passthrough, target: {{ baseUrl: 'http://{upstream_addr}' }} }}\n"
    );
    let running = start(&[&route], "- id: default\n  routes: [real:any:upstream]\n").await;
    let mut events = running.server.subscribe();

    reqwest::get(format!("{}/real?x=1", running.base))
        .await
        .unwrap();

    let event = next_request(&mut events).await;
    let Outcome::Passthrough {
        matched,
        upstream_url,
        upstream_status,
    } = &event.outcome
    else {
        panic!("{:?}", event.outcome)
    };
    assert_eq!(matched.activation.address(), "real:any:upstream");
    assert_eq!(
        upstream_url.as_deref(),
        Some(format!("http://{upstream_addr}/real?x=1").as_str())
    );
    assert_eq!(*upstream_status, Some(202));
    assert_eq!(event.response.status, 202);
    assert_eq!(event.response.body.bytes, b"real");
}

/// SSE, long-poll and downloads need the upstream body streamed, not buffered (#267).
#[tokio::test]
async fn a_passthrough_streams_the_upstream_body_and_emits_it_once_it_ends() {
    use futures_util::StreamExt as _;

    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let released = Arc::new(std::sync::Mutex::new(Some(released)));
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    let app = axum::Router::new().fallback(move || {
        let released = released.lock().unwrap().take().unwrap();
        async move {
            let first = futures_util::stream::once(async { Ok::<_, std::io::Error>("first") });
            let rest = futures_util::stream::once(async move {
                let _ = released.await;
                Ok("rest")
            });
            axum::body::Body::from_stream(first.chain(rest))
        }
    });
    tokio::spawn(async move { axum::serve(upstream, app).await });
    let route = format!(
        "id: real\ntransport: http\nmatch: {{ method: GET, path: /real }}\ncases:\n  any:\n    behaviors:\n      upstream: {{ kind: passthrough, target: {{ baseUrl: 'http://{upstream_addr}' }} }}\n"
    );
    let running = start(&[&route], "- id: default\n  routes: [real:any:upstream]\n").await;
    let mut events = running.server.subscribe();

    let within = std::time::Duration::from_secs(5);
    let mut response = tokio::time::timeout(within, reqwest::get(format!("{}/real", running.base)))
        .await
        .expect("headers arrive before the upstream body ends")
        .unwrap();
    let first = tokio::time::timeout(within, response.chunk())
        .await
        .expect("first chunk streams");
    assert_eq!(first.unwrap().as_deref(), Some(&b"first"[..]));

    release.send(()).unwrap();
    let mut rest = Vec::new();
    while let Some(chunk) = response.chunk().await.unwrap() {
        rest.extend_from_slice(&chunk);
    }
    assert_eq!(rest, b"rest");

    let event = tokio::time::timeout(within, next_request(&mut events))
        .await
        .unwrap();
    assert_eq!(event.response.status, 200);
    assert_eq!(event.response.body.bytes, b"firstrest");
    assert_eq!(event.response.body.size, 9);
}

#[tokio::test]
async fn a_failed_passthrough_emits_the_error_response_decoy_answered() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_addr = closed.local_addr().unwrap();
    drop(closed);
    let route = format!(
        "id: real\ntransport: http\nmatch: {{ method: GET, path: /real }}\ncases:\n  any:\n    behaviors:\n      upstream: {{ kind: passthrough, target: {{ baseUrl: 'http://{upstream_addr}' }} }}\n"
    );
    let running = start(&[&route], "- id: default\n  routes: [real:any:upstream]\n").await;
    let mut events = running.server.subscribe();

    let response = reqwest::get(format!("{}/real", running.base))
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    let answered = response.bytes().await.unwrap();

    let event = next_request(&mut events).await;
    let Outcome::Passthrough {
        upstream_url,
        upstream_status,
        ..
    } = &event.outcome
    else {
        panic!("{:?}", event.outcome)
    };
    assert_eq!(
        upstream_url.as_deref(),
        Some(format!("http://{upstream_addr}/real").as_str())
    );
    assert_eq!(*upstream_status, None);
    assert_eq!(event.response.status, 502);
    assert_eq!(event.response.body.bytes, answered);
}

const APPROVALS: &str = r#"
id: approvals-get
transport: grpc
match: { service: approvals.v1.Approvals, method: GetApproval }
cases:
  any:
    behaviors:
      ok:
        message: { id: '${ message.id }', state: PENDING }
"#;

#[tokio::test]
async fn a_grpc_call_emits_its_decoded_messages_as_json() {
    use prost::Message as _;

    let running = start(
        &[APPROVALS],
        "- id: default\n  routes: [approvals-get:any:ok]\n",
    )
    .await;
    let mut events = running.server.subscribe();
    let pool = decoy_runtime::protos::load_descriptor_pool(&ProtoSources {
        roots: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proto")],
        descriptor_sets: vec![],
    })
    .unwrap();
    let input = pool
        .get_message_by_name("approvals.v1.GetApprovalRequest")
        .unwrap();
    let message =
        prost_reflect::DynamicMessage::deserialize(input, serde_json::json!({ "id": "a-1" }))
            .unwrap()
            .encode_to_vec();
    let mut body = vec![0];
    body.extend_from_slice(&u32::try_from(message.len()).unwrap().to_be_bytes());
    body.extend_from_slice(&message);

    let view = running.server.controller.read().await.session_view("s1");
    let GrpcReply::Unary(response) = view
        .execute_grpc(
            decoy_runtime::grpc::GrpcRequest {
                service: "approvals.v1.Approvals".into(),
                method: "GetApproval".into(),
                metadata: BTreeMap::from([("x-user".into(), "alice".into())]),
                body,
            },
            &Connections::default(),
        )
        .await
    else {
        panic!("expected a unary reply")
    };
    assert_eq!(response.status, 0);

    let event = next_request(&mut events).await;
    assert_eq!(event.transport, decoy_runtime::Transport::Grpc);
    assert_eq!(
        (
            event.method.as_str(),
            event.url.as_str(),
            event.session.as_str()
        ),
        ("POST", "/approvals.v1.Approvals/GetApproval", "s1")
    );
    assert_eq!(event.headers["x-user"], "alice");
    assert!(event.body.decoded);
    assert_eq!(event.body.bytes, br#"{"id":"a-1"}"#);
    assert!(matches!(event.outcome, Outcome::Hit(_)));
    assert_eq!(event.response.status, 0);
    assert!(event.response.body.decoded);
    let message: serde_json::Value = serde_json::from_slice(&event.response.body.bytes).unwrap();
    assert_eq!(
        (&message["id"], &message["state"]),
        (&"a-1".into(), &"PENDING".into())
    );
}

#[tokio::test]
async fn a_server_streaming_call_emits_one_hit_as_it_opens() {
    let running = start(
        &["id: watch\ntransport: grpc\nmatch: { service: approvals.v1.Approvals, method: WatchApprovals }\ncases:\n  any:\n    behaviors:\n      open: {}\n"],
        "- id: default\n  routes: [watch:any:open]\n",
    )
    .await;
    let mut events = running.server.subscribe();
    let view = running.server.controller.read().await.session_view("s1");
    let connections = Connections::default();
    let reply = view
        .execute_grpc(
            decoy_runtime::grpc::GrpcRequest {
                service: "approvals.v1.Approvals".into(),
                method: "WatchApprovals".into(),
                metadata: BTreeMap::new(),
                // An empty WatchRequest, framed.
                body: vec![0, 0, 0, 0, 0],
            },
            &connections,
        )
        .await;
    assert!(matches!(reply, GrpcReply::Stream(_)));

    let event = next_request(&mut events).await;
    assert!(matches!(event.outcome, Outcome::Hit(_)));
    assert_eq!(event.response.status, 0);
    assert_eq!(event.body.bytes, br#"{"ids":[]}"#);
    assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
}

#[tokio::test]
async fn a_server_streaming_call_records_its_end_status_when_known_at_open() {
    let running = start(
        &["id: watch\ntransport: grpc\nmatch: { service: approvals.v1.Approvals, method: WatchApprovals }\ncases:\n  any:\n    behaviors:\n      aborted: { stream: [], status: ABORTED, message: gave up }\n"],
        "- id: default\n  routes: [watch:any:aborted]\n",
    )
    .await;
    let mut events = running.server.subscribe();
    let view = running.server.controller.read().await.session_view("s1");
    let reply = view
        .execute_grpc(
            decoy_runtime::grpc::GrpcRequest {
                service: "approvals.v1.Approvals".into(),
                method: "WatchApprovals".into(),
                metadata: BTreeMap::new(),
                body: vec![0, 0, 0, 0, 0],
            },
            &Connections::default(),
        )
        .await;
    assert!(matches!(reply, GrpcReply::Stream(_)));

    let event = next_request(&mut events).await;
    assert_eq!(event.response.status, 10);
    assert!(
        event
            .response
            .headers
            .contains(&("grpc-message".into(), "gave up".into()))
    );
}

#[tokio::test]
async fn a_grpc_miss_emits_its_status_message_as_a_header() {
    let running = start(
        &[APPROVALS],
        "- id: default\n  routes: [approvals-get:any:ok]\n",
    )
    .await;
    let view = running.server.controller.read().await.session_view("s1");
    let connections = Connections::default();
    let call = || async {
        let reply = view
            .execute_grpc(
                decoy_runtime::grpc::GrpcRequest {
                    service: "approvals.v1.Approvals".into(),
                    method: "ListApprovals".into(),
                    metadata: BTreeMap::new(),
                    body: Vec::new(),
                },
                &connections,
            )
            .await;
        let GrpcReply::Unary(response) = reply else {
            panic!("expected a unary reply")
        };
        response.status
    };
    // Nobody subscribed yet: answered without capturing.
    assert_eq!(call().await, 12);

    let mut events = running.server.subscribe();
    assert_eq!(call().await, 12);

    let event = next_request(&mut events).await;
    assert!(matches!(event.outcome, Outcome::Miss(_)));
    assert_eq!(event.response.status, 12);
    assert!(event.response.headers.contains(&(
        "grpc-message".into(),
        "no active route case matched request".into()
    )));
}

async fn control(running: &Running, verb: &str, body: &str, session: &str) {
    let response = reqwest::Client::new()
        .post(format!("{}/__decoy__/control/{verb}", running.base))
        .header("content-type", "application/json")
        .header("x-mock-session", session)
        .body(body.to_owned())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{verb}");
}

async fn next_session(events: &mut Receiver<Event>) -> SessionEvent {
    loop {
        if let Event::Session(event) = events.recv().await.unwrap() {
            return event;
        }
    }
}

#[tokio::test]
async fn session_changes_emit_their_kind_source_and_selection() {
    let running = start(
        &[USERS],
        "- id: default\n  routes: [get-user:user-123:success]\n- id: other\n  routes: []\n",
    )
    .await;
    let mut events = running.server.subscribe();

    control(
        &running,
        "useCollection",
        r#"{"collection":"other"}"#,
        "e2e",
    )
    .await;
    let created = next_session(&mut events).await;
    assert_eq!(
        (created.session.as_str(), created.change, created.source),
        ("e2e", SessionChange::Created, SessionSource::ControlApi)
    );
    let changed = next_session(&mut events).await;
    assert_eq!(changed.change, SessionChange::UseCollection);
    assert_eq!(changed.selection.collection, "other");

    running
        .server
        .controller
        .write()
        .await
        .try_use_route("e2e", "get-user", "user-123", "success")
        .unwrap();
    let routed = next_session(&mut events).await;
    assert_eq!(
        (routed.change, routed.source),
        (SessionChange::UseRoute, SessionSource::InProcess)
    );
    assert_eq!(routed.selection.overrides.len(), 1);

    control(&running, "endSession", "", "e2e").await;
    let ended = next_session(&mut events).await;
    assert_eq!(
        (ended.session.as_str(), ended.change, ended.source),
        ("e2e", SessionChange::Ended, SessionSource::ControlApi)
    );
    assert_eq!(ended.selection.collection, "default");
}

#[tokio::test]
async fn a_lagging_subscriber_loses_the_oldest_events_without_blocking_requests() {
    let running = start(
        &[USERS],
        "- id: default\n  routes: [get-user:user-123:success]\n",
    )
    .await;
    let mut events = running.server.subscribe();
    let view = running
        .server
        .controller
        .read()
        .await
        .session_view("default");

    for _ in 0..CAPACITY + 5 {
        view.execute_http(HttpExecutionRequest {
            method: HttpMethod::Get,
            path: "/users/123".into(),
            path_and_query: "/users/123".into(),
            headers: BTreeMap::new(),
            body: Vec::new(),
            metadata: Default::default(),
        })
        .await;
    }

    assert_eq!(events.recv().await.unwrap_err(), RecvError::Lagged(5));
    let Event::Request(oldest_kept) = events.recv().await.unwrap() else {
        panic!("expected a request event")
    };
    let Event::Request(next) = events.recv().await.unwrap() else {
        panic!("expected a request event")
    };
    assert!(next.id > oldest_kept.id);
}

#[tokio::test]
async fn runtime_warnings_reach_only_the_subscribers_of_their_server() {
    // `abs` of a string fails at request time; the Case reads as not matching and the runtime warns.
    // The predicate text is unique to this test: each one is logged once per process.
    let route = "id: bad-predicate\ntransport: http\nmatch: { method: GET, path: /bad-predicate }\ncases:\n  any:\n    match: '${ abs(path) }'\n    behaviors:\n      ok: { status: 200 }\n";
    let collections = "- id: default\n  routes: [bad-predicate:any:ok]\n";
    let served = start(&[route], collections).await;
    let other = start(&[route], collections).await;
    let mut events = served.server.subscribe();
    let mut other_events = other.server.subscribe();

    reqwest::get(format!("{}/bad-predicate", served.base))
        .await
        .unwrap();

    let log = loop {
        if let Event::Log(log) = events.recv().await.unwrap() {
            break log;
        }
    };
    assert_eq!(log.level, Level::Warn);
    assert!(
        log.message.starts_with("predicate `${ abs(path) }` failed"),
        "{}",
        log.message
    );
    assert_eq!(other_events.try_recv().unwrap_err(), TryRecvError::Empty);
}
