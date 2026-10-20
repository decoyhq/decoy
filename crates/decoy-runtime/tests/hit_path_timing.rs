//! Hit-path timing with many active route entries ahead of the winner (#287: explanations must
//! not slow serving). Run: `cargo test --release -p decoy-runtime --test hit_path_timing -- --ignored --nocapture`.

use std::collections::BTreeMap;
use std::time::Instant;

use decoy_runtime::{
    Catalog, CollectionsFile, HttpExecutionOutcome, HttpExecutionRequest, HttpMethod, Route,
    RuntimeConfig,
};

const AHEAD: usize = 100;
const ROUNDS: u32 = 20_000;

#[tokio::test]
#[ignore = "timing, run by hand"]
async fn hit_path_with_many_entries_ahead_of_the_winner() {
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

    let mut best = f64::MAX;
    for _ in 0..5 {
        let started = Instant::now();
        for _ in 0..ROUNDS {
            let outcome = catalog.execute_http("default", request.clone()).await;
            let HttpExecutionOutcome::Response { response, .. } = outcome else {
                panic!("expected a response")
            };
            assert_eq!(response.status, 200);
        }
        best = best.min(started.elapsed().as_secs_f64() * 1e6 / f64::from(ROUNDS));
    }
    println!(
        "hit path, {} entries ahead: {best:.2} µs/request (best of 5)",
        2 * AHEAD
    );
}
