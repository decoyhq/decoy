//! What makes the TUI feel slow: frames with a full log, bursts of requests, a big body in the
//! detail panel. Run: `cargo bench -p decoy-tui --bench render`; results land in `target/criterion`.

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
};
use decoy_runtime::events::{
    CapturedBody, CapturedResponse, Event as RuntimeEvent, Match, Outcome, RequestEvent,
};
use decoy_runtime::{
    Activation, Catalog, CollectionsFile, Route, RuntimeConfig, SelectionSnapshot, Transport,
};
use decoy_tui::{App, Event, Options};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn app() -> App {
    let route = Route::from_yaml(
        "id: users\ntransport: http\nmatch: { method: GET, path: '/users/{id}' }\ncases:\n  any:\n    behaviors:\n      ok: { status: 200 }\n",
    )
    .unwrap();
    let catalog = Catalog::new(
        vec![route],
        CollectionsFile::from_yaml("- id: default\n  routes: [users:any:ok]\n").unwrap(),
        RuntimeConfig::default(),
    )
    .unwrap();
    App::new(Options {
        listen: "127.0.0.1:8080".into(),
        selection: SelectionSnapshot {
            collection: "default".into(),
            overrides: vec![],
        },
        catalog: Arc::new(catalog),
        no_color: false,
        clock: |_| "12:01:03".to_owned(),
    })
}

fn hit(id: u64, body: &[u8]) -> Event {
    let event = RequestEvent {
        id,
        at: UNIX_EPOCH,
        transport: Transport::Http,
        session: "default".into(),
        method: "GET".into(),
        url: format!("/users/{id}?include=orders&page=2"),
        headers: [("content-type".to_owned(), "application/json".to_owned())].into(),
        body: CapturedBody::raw(b""),
        outcome: Outcome::Hit(Match {
            activation: Activation::parse("users:any:ok").unwrap(),
            skipped: vec![],
            conditions: serde_json::Value::Null,
            path_params: Default::default(),
        }),
        response: CapturedResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: CapturedBody::raw(body),
        },
        duration: Duration::from_millis(4),
    };
    Event::Runtime(RuntimeEvent::Request(Arc::new(event)))
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Terminal(TermEvent::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }))
}

/// A pretty-printable JSON array, about `bytes` long.
fn json_body(bytes: usize) -> Vec<u8> {
    let item =
        r#"{"id":"0000","name":"Ada Lovelace","email":"ada@example.com","roles":["admin","dev"]}"#;
    let items = vec![item; bytes / item.len()];
    format!("[{}]", items.join(",")).into_bytes()
}

fn app_with(requests: u64, body: &[u8]) -> App {
    let mut app = app();
    for id in 1..=requests {
        app.handle_event(hit(id, body));
    }
    app
}

/// One frame of the log view; the log keeps at most 10 000 entries.
fn frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame");
    for (width, height) in [(80, 24), (300, 80)] {
        for requests in [100, 10_000] {
            let mut app = app_with(requests, b"{}");
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            group.bench_function(
                BenchmarkId::new(format!("log/{width}x{height}"), requests),
                |b| b.iter(|| terminal.draw(|frame| app.draw(frame)).unwrap().area),
            );
        }
    }
    group.finish();
}

/// A busy suite: 100 requests arrive, then one frame is drawn.
fn burst(c: &mut Criterion) {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let body = json_body(1024);
    c.bench_function("burst/100_requests+frame", |b| {
        b.iter_batched(
            || app_with(1_000, b"{}"),
            |mut app| {
                for id in 1_001..=1_100 {
                    app.handle_event(hit(id, &body));
                }
                terminal.draw(|frame| app.draw(frame)).unwrap().area
            },
            BatchSize::LargeInput,
        )
    });
}

/// Opening the newest request (Ctrl+O, Enter) with a body at the 64 KiB capture cap, then
/// scrolling frames of the open panel.
fn detail(c: &mut Criterion) {
    let body = json_body(64 * 1024);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    c.bench_function("detail/open/64KiB", |b| {
        b.iter_batched(
            || app_with(10, &body),
            |mut app| {
                app.handle_event(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
                app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
                terminal.draw(|frame| app.draw(frame)).unwrap().area
            },
            BatchSize::LargeInput,
        )
    });

    let mut app = app_with(10, &body);
    app.handle_event(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
    app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
    c.bench_function("detail/frame/64KiB", |b| {
        b.iter(|| {
            app.handle_event(key(KeyCode::PageDown, KeyModifiers::NONE));
            terminal.draw(|frame| app.draw(frame)).unwrap().area
        })
    });
}

criterion_group!(benches, frame, burst, detail);
criterion_main!(benches);
