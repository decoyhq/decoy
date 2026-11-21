//! Run with `cargo bench -p decoy-runtime --bench grpc`.
use std::{
    hint::black_box,
    path::Path,
    time::{Duration, Instant},
};

use decoy_runtime::{
    Catalog, CollectionsFile, Controller, Route, RuntimeConfig,
    config::ProtoSources,
    connections::Connections,
    grpc::{GrpcReply, GrpcRequest, StreamPart},
    protos::load_descriptor_pool,
};
use futures_util::StreamExt;
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage};

fn controller(
    pool: &DescriptorPool,
    method: &str,
    pipeline: &str,
    behavior: &str,
) -> impl Fn() -> Controller {
    let pool = pool.clone();
    let route = Route::from_yaml(&format!(
        "id: get\ntransport: grpc\nmatch: {{ service: approvals.v1.Approvals, method: {method} }}\n{pipeline}cases:\n  any:\n    behaviors:\n      ok: {behavior}\n"
    )).unwrap();
    let collection = CollectionsFile::from_yaml("- id: default\n  routes: [get:any:ok]\n").unwrap();
    move || {
        Controller::new(
            Catalog::new(
                vec![route.clone()],
                collection.clone(),
                RuntimeConfig {
                    protos: pool.clone(),
                    ..Default::default()
                },
            )
            .unwrap(),
            "default",
        )
    }
}

async fn bench(name: &str, rounds: u32, mut run: impl AsyncFnMut()) {
    let mut samples = [0.0; 5];
    for sample in &mut samples {
        let start = Instant::now();
        for _ in 0..rounds {
            run().await;
        }
        *sample = start.elapsed().as_secs_f64() * 1e6 / f64::from(rounds);
    }
    samples.sort_by(f64::total_cmp);
    println!("{name}: {:.3} µs/op (median of 5 × {rounds})", samples[2]);
}

async fn unary(
    name: &str,
    controller: &Controller,
    body: Vec<u8>,
    expected: &[u8],
    capture: bool,
    rounds: u32,
) {
    let view = controller.session_view("default");
    let connections = Connections::default();
    let mut events = capture.then(|| controller.subscribe());
    let request = GrpcRequest {
        service: "approvals.v1.Approvals".into(),
        method: "GetApproval".into(),
        metadata: Default::default(),
        body: body.into(),
    };
    let call = || view.execute_grpc(request.clone(), &connections);
    let GrpcReply::Unary(actual) = call().await else {
        panic!("expected unary")
    };
    assert_eq!(actual.status, 0, "{}", actual.status_message);
    assert_eq!(actual.body, expected);
    if let Some(events) = events.as_mut() {
        events.try_recv().unwrap();
    }
    bench(name, rounds, async || {
        black_box(call().await);
        if let Some(events) = events.as_mut() {
            black_box(events.try_recv().unwrap());
        }
    })
    .await;
}

async fn push(pool: &DescriptorPool, frame: Vec<u8>) {
    let controller = controller(pool, "WatchApprovals", "", "{ stream: [] }")();
    let view = controller.session_view("default");
    let connections = Connections::default();
    let mut streams = Vec::new();
    for _ in 0..16 {
        let request = GrpcRequest {
            service: "approvals.v1.Approvals".into(),
            method: "WatchApprovals".into(),
            metadata: Default::default(),
            body: vec![0; 5].into(),
        };
        let GrpcReply::Stream(stream) = view.execute_grpc(request, &connections).await else {
            panic!("expected stream")
        };
        streams.push(stream);
    }
    let mut deliver = async |verify_payload: bool| {
        let count = connections
            .send(
                "default",
                "get",
                "approvals.v1.Approval",
                frame.clone(),
                Duration::ZERO,
            )
            .await
            .unwrap();
        assert_eq!(count, streams.len());
        for stream in &mut streams {
            let Some(StreamPart::Message(bytes)) = stream.next().await else {
                panic!("expected message")
            };
            if verify_payload {
                assert_eq!(bytes, frame);
            } else {
                assert_eq!(black_box(bytes).len(), frame.len());
            }
        }
    };
    deliver(true).await;
    bench("push 64 KiB to 16 gRPC streams", 512, async || {
        deliver(false).await;
    })
    .await;
}

fn frame(bytes: &[u8]) -> Vec<u8> {
    let mut framed = vec![0];
    framed.extend_from_slice(&u32::try_from(bytes.len()).unwrap().to_be_bytes());
    framed.extend_from_slice(bytes);
    framed
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let pool = load_descriptor_pool(&ProtoSources {
        roots: vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/proto")],
        ..Default::default()
    })
    .unwrap();
    let raw = controller(
        &pool,
        "GetApproval",
        "pipeline: [grpc-frame]\n",
        "{ message: '${ message }' }",
    )();
    for size in [32usize, 1024, 65536] {
        let body = frame(&(0..size).map(|i| i as u8).collect::<Vec<_>>());
        unary(
            &format!("raw echo {size} B"),
            &raw,
            body.clone(),
            &body,
            false,
            if size < 65536 { 2000 } else { 200 },
        )
        .await;
    }
    let message =
        serde_json::json!({"id":"a-1","state":"PENDING","approvers":["one","two"],"priority":5});
    let fixture = format!("{{ message: {message} }}");
    let build = controller(&pool, "GetApproval", "", &fixture);
    bench("static unary catalog build", 2000, async || {
        black_box(build());
    })
    .await;
    let server = build();
    let descriptor = pool.get_message_by_name("approvals.v1.Approval").unwrap();
    let bytes = DynamicMessage::deserialize(descriptor.clone(), message)
        .unwrap()
        .encode_to_vec();
    let expected = frame(&bytes);
    for capture in [false, true] {
        unary(
            &format!("static unary capture={capture}"),
            &server,
            vec![0; 5],
            &expected,
            capture,
            5000,
        )
        .await;
    }
    let message = serde_json::json!({"id":"x".repeat(65536)});
    let bytes = DynamicMessage::deserialize(descriptor, message)
        .unwrap()
        .encode_to_vec();
    push(&pool, frame(&bytes)).await;
}
