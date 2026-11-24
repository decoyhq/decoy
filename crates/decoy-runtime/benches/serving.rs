//! Run with `cargo bench -p decoy-runtime --bench serving`.

use std::collections::BTreeMap;
use std::time::Instant;

use decoy_runtime::{
    Catalog, CollectionsFile, HttpExecutionOutcome, HttpExecutionRequest, HttpMethod, Route,
    RuntimeConfig,
};

const AHEAD: usize = 100;
const ROUNDS: u32 = 20_000;

fn http_fixture(winner_first: bool) -> (Catalog, HttpExecutionRequest) {
    // Ahead of the winner: AHEAD Cases that pass `pathParams` and fail on the JSON body, AHEAD
    // Routes whose path differs.
    let mut users = "id: users\ntransport: http\nmatch: { method: GET, path: '/users/{id}' }\ncases:\n  any:\n    behaviors:\n      ok: { status: 200, body: { ok: true } }\n".to_owned();
    let mut routes = Vec::new();
    let mut entries = vec!["users:any:ok".to_owned()];
    for i in 0..AHEAD {
        users.push_str(&format!(
            "  c{i}:\n    match: {{ pathParams: {{ id: x }}, body: {{ user: {{ name: 'n{i}' }} }} }}\n    behaviors:\n      ok: {{ status: 200 }}\n"
        ));
        routes.push(
            Route::from_yaml(&format!(
                "id: r{i}\ntransport: http\nmatch: {{ method: GET, path: /r{i} }}\ncases:\n  any:\n    behaviors:\n      ok: {{ status: 200 }}\n"
            ))
            .unwrap(),
        );
        entries.push(format!("r{i}:any:ok"));
        entries.push(format!("users:c{i}:ok"));
    }
    routes.push(Route::from_yaml(&users).unwrap());
    if winner_first {
        entries.rotate_left(1);
    }
    let catalog = Catalog::new(
        routes,
        CollectionsFile::from_yaml(&format!(
            "- id: default\n  routes: [{}]\n",
            entries.join(", ")
        ))
        .unwrap(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let request = HttpExecutionRequest {
        method: HttpMethod::Get,
        path: "/users/x".into(),
        path_and_query: "/users/x".into(),
        headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
        body: br#"{"user":{"name":"someone"}}"#.as_slice().into(),
        metadata: Default::default(),
    };

    (catalog, request)
}

fn http_same_path_fixture(ahead: usize) -> (Catalog, HttpExecutionRequest) {
    let mut route = r#"id: widgets
transport: http
match: { method: GET, path: '/api/widgets/{id}' }
cases:
  any:
    match: "${ pathParams.id == 'a/b' && query.mode == 'hit' }"
    behaviors:
      ok: { status: 200, body: { echo: '${ pathParams.id }' } }
"#
    .to_owned();
    let mut entries = vec!["widgets:any:ok".to_owned()];
    for i in 0..ahead {
        route.push_str(&format!(
            "  c{i}:\n    match: \"${{ pathParams.id == 'a/b' && query.mode == 'other-{i}' }}\"\n    behaviors:\n      ok: {{ status: 200 }}\n"
        ));
        entries.push(format!("widgets:c{i}:ok"));
    }
    let catalog = Catalog::new(
        vec![Route::from_yaml(&route).unwrap()],
        CollectionsFile::from_yaml(&format!(
            "- id: default\n  routes: [{}]\n",
            entries.join(", ")
        ))
        .unwrap(),
        RuntimeConfig::default(),
    )
    .unwrap();
    let request = HttpExecutionRequest {
        method: HttpMethod::Get,
        path: "/api/widgets/a%2Fb".into(),
        path_and_query: "/api/widgets/a%2Fb?mode=hit".into(),
        headers: BTreeMap::new(),
        body: Default::default(),
        metadata: Default::default(),
    };
    (catalog, request)
}

async fn http_hit(catalog: &Catalog, request: &HttpExecutionRequest, expected_body: &[u8]) {
    let outcome = catalog.execute_http("default", request.clone()).await;
    let HttpExecutionOutcome::Response {
        activation: Some(activation),
        response,
    } = outcome
    else {
        panic!("expected a hit");
    };
    assert_eq!(activation.case, "any");
    assert_eq!(response.status, 200);
    assert_eq!(response.body.as_ref(), expected_body);
}

fn measure(label: &str, rounds: u32, mut batch: impl FnMut()) {
    let mut samples = [0.0; 5];
    for sample in &mut samples {
        let started = Instant::now();
        batch();
        *sample = started.elapsed().as_secs_f64() * 1e6 / f64::from(rounds);
    }
    samples.sort_by(f64::total_cmp);
    println!("{label}: {:.3} µs/op (median of 5)", samples[2]);
}

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    for (first, label) in [(true, "HTTP first of 201"), (false, "HTTP last of 201")] {
        let (catalog, request) = http_fixture(first);
        measure(label, ROUNDS, || {
            runtime.block_on(async {
                for _ in 0..ROUNDS {
                    http_hit(&catalog, &request, br#"{"ok":true}"#).await;
                }
            })
        });
    }
    for cases in [1, 33, 129] {
        let (catalog, request) = http_same_path_fixture(cases - 1);
        measure(
            &format!("HTTP last of {cases} same-path Cases"),
            ROUNDS,
            || {
                runtime.block_on(async {
                    for _ in 0..ROUNDS {
                        http_hit(&catalog, &request, br#"{"echo":"a/b"}"#).await;
                    }
                })
            },
        );
    }
    for recipients in [1, 16] {
        let registry = decoy_runtime::connections::Connections::default();
        let mut connections: Vec<_> = (0..recipients)
            .map(|_| registry.register_shared("default", "live", "tick"))
            .collect();
        let payload = vec![42; 64 * 1024];
        const SENDS: u32 = 512;
        measure(&format!("queue 64 KiB to {recipients}"), SENDS, || {
            runtime.block_on(async {
                for _ in 0..SENDS {
                    let sent = registry
                        .send(
                            "default",
                            "live",
                            "tick",
                            payload.clone(),
                            std::time::Duration::ZERO,
                        )
                        .await
                        .unwrap();
                    assert_eq!(sent, recipients);
                    for (_, receiver) in &mut connections {
                        let bytes = receiver.try_recv().unwrap();
                        assert_eq!(std::hint::black_box(bytes).len(), payload.len());
                    }
                }
            })
        });
    }
}
