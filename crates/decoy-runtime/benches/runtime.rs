//! Paths a user waits on: serving a request, starting up, Session control, reload.
//! Run: `cargo bench -p decoy-runtime --bench runtime`; results land in `target/criterion`.

use std::collections::BTreeMap;
use std::fs;
use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use decoy_runtime::{
    Catalog, CollectionsFile, ControlApiRequest, Controller, HttpExecutionOutcome,
    HttpExecutionRequest, HttpMethod, Route, RuntimeConfig, handle_control_request,
    load_catalog_from_files,
};
use tokio::runtime::Runtime;

/// `count` Routes `/r{i}` and a last one, `users`, that matches `/users/{id}`.
fn route_files(count: usize) -> Vec<(String, String)> {
    let mut files: Vec<_> = (0..count)
        .map(|i| {
            (
                format!("r{i}"),
                format!(
                    "id: r{i}\ntransport: http\nmatch: {{ method: GET, path: '/r{i}/{{id}}' }}\ncases:\n  any:\n    behaviors:\n      ok: {{ status: 200, body: {{ ok: true }} }}\n"
                ),
            )
        })
        .collect();
    files.push((
        "users".to_owned(),
        r#"id: users
transport: http
match: { method: POST, path: '/users/{id}' }
cases:
  vip:
    match: '${ body.tier == ''gold'' && headers."x-client" == ''web'' }'
    behaviors:
      ok:
        status: 200
        headers: { x-user: 'user-${ pathParams.id }' }
        body:
          id: '${ pathParams.id }'
          greeting: 'Hello, ${ body.name }!'
          items: '${ body.items }'
  plain:
    behaviors:
      ok: { status: 200, body: { id: '1', name: Ada Lovelace } }
"#
        .to_owned(),
    ));
    files
}

fn collections(count: usize, users_case: &str) -> String {
    let mut entries: Vec<_> = (0..count).map(|i| format!("r{i}:any:ok")).collect();
    entries.push(format!("users:{users_case}:ok"));
    format!("- id: default\n  routes: [{}]\n", entries.join(", "))
}

fn catalog(count: usize, users_case: &str) -> Catalog {
    let routes = route_files(count)
        .iter()
        .map(|(_, yaml)| Route::from_yaml(yaml).unwrap())
        .collect();
    Catalog::new(
        routes,
        CollectionsFile::from_yaml(&collections(count, users_case)).unwrap(),
        RuntimeConfig::default(),
    )
    .unwrap()
}

fn request(path: &str, body: &[u8]) -> HttpExecutionRequest {
    HttpExecutionRequest {
        method: HttpMethod::Post,
        path: path.into(),
        path_and_query: path.into(),
        headers: BTreeMap::from([
            ("content-type".into(), "application/json".into()),
            ("x-client".into(), "web".into()),
        ]),
        body: body.to_vec().into(),
        metadata: Default::default(),
    }
}

const BODY: &[u8] =
    br#"{"name":"Ada","tier":"gold","items":[{"sku":"a","qty":1},{"sku":"b","qty":2}]}"#;

fn assert_ok(outcome: HttpExecutionOutcome) {
    let HttpExecutionOutcome::Response { response, .. } = outcome else {
        panic!("expected a response")
    };
    assert_eq!(response.status, 200);
}

/// One request through the Session, as the server runs it: Route lookup, Case selection,
/// predicate, template render.
fn serve(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("serve");
    for routes in [1, 100, 1000] {
        for case in ["plain", "vip"] {
            let controller = Controller::new(catalog(routes, case), "default");
            let view = controller.session_view("default");
            let hit = request("/users/7", BODY);
            group.bench_with_input(
                BenchmarkId::new(format!("{case}/routes"), routes),
                &hit,
                |b, request| {
                    b.to_async(&rt)
                        .iter(|| async { assert_ok(view.execute_http(request.clone()).await) })
                },
            );
        }
    }

    // With the TUI (or any subscriber) attached, every request is also captured as an event.
    let controller = Controller::new(catalog(100, "vip"), "default");
    let _events = controller.subscribe();
    let view = controller.session_view("default");
    let hit = request("/users/7", BODY);
    group.bench_function("vip/routes/100/subscribed", |b| {
        b.to_async(&rt)
            .iter(|| async { assert_ok(view.execute_http(hit.clone()).await) })
    });

    // A miss explains every candidate, so it walks all of them.
    let controller = Controller::new(catalog(1000, "plain"), "default");
    let view = controller.session_view("default");
    let miss = request("/nowhere/7", BODY);
    group.bench_function("miss/routes/1000", |b| {
        b.to_async(&rt).iter(|| async {
            let outcome = view.execute_http(miss.clone()).await;
            let HttpExecutionOutcome::Response { response, .. } = outcome else {
                panic!("expected a response")
            };
            assert_eq!(response.status, 501);
        })
    });
    group.finish();
}

/// `decoy serve` start: discover, read, parse and validate Route files and the Collections file.
fn startup(c: &mut Criterion) {
    let mut group = c.benchmark_group("startup");
    for routes in [10, 200] {
        let dir = tempfile::tempdir().unwrap();
        let routes_dir = dir.path().join("routes");
        fs::create_dir(&routes_dir).unwrap();
        for (name, yaml) in route_files(routes) {
            fs::write(routes_dir.join(format!("{name}.yaml")), yaml).unwrap();
        }
        let collections_file = dir.path().join("collections.yaml");
        fs::write(&collections_file, collections(routes, "vip")).unwrap();
        group.bench_function(BenchmarkId::new("load/routes", routes), |b| {
            b.iter(|| {
                load_catalog_from_files(
                    &routes_dir,
                    &collections_file,
                    RuntimeConfig::default(),
                    None,
                )
                .unwrap()
            })
        });
    }
    group.finish();
}

/// What a runner does around each test: point its Session at a Behavior, then end the Session.
fn control(c: &mut Criterion) {
    let mut controller = Controller::new(catalog(200, "plain"), "default");
    c.bench_function("control/useRoute+endSession", |b| {
        b.iter(|| {
            let used = handle_control_request(
                &mut controller,
                ControlApiRequest::post(
                    "/__decoy__/control/useRoute",
                    r#"{"route":"users","case":"vip","behavior":"ok"}"#,
                )
                .with_session("worker-1"),
            );
            assert_eq!(used.status, 200);
            let ended = handle_control_request(
                &mut controller,
                ControlApiRequest::post("/__decoy__/control/endSession", "")
                    .with_session("worker-1"),
            );
            assert_eq!(ended.status, 200);
        })
    });
}

/// Swapping in an edited Catalog with live Sessions, as `--watch` does on every save.
fn reload(c: &mut Criterion) {
    let mut controller = Controller::new(catalog(200, "plain"), "default");
    for worker in 0..8 {
        let used = handle_control_request(
            &mut controller,
            ControlApiRequest::post(
                "/__decoy__/control/useRoute",
                r#"{"route":"users","case":"vip","behavior":"ok"}"#,
            )
            .with_session(format!("worker-{worker}")),
        );
        assert_eq!(used.status, 200);
    }
    c.bench_function("reload/routes/200", |b| {
        b.iter_batched(
            || catalog(200, "plain"),
            |catalog| black_box(controller.reload(catalog, "default")),
            criterion::BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, serve, startup, control, reload);
criterion_main!(benches);
