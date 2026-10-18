//! Interactive terminal UI for bare `decoy` (design: `docs/design/tui.md`): the runtime's live
//! log, an input box and a status bar, fullscreen in the alternate screen.

mod app;
mod command;
mod complete;
mod detail;
mod log;
mod ui;

use std::io::Write;

use base64::Engine;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, EventStream, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::supports_keyboard_enhancement;
use decoy_runtime::{ControlError, Controller, DEFAULT_SESSION, ServeError, Server};
use futures_util::{Stream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::RwLock;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

pub use app::{App, Effect, Event, Options, Verb};

/// Runtime events handled per frame, so a burst of traffic draws once and keys stay responsive.
const BATCH: usize = 1024;

/// Runs the TUI over a started `server` until the user quits or the server stops, then restores
/// the terminal and waits for the server to drain. `quit` must resolve the shutdown future the
/// server was started with.
pub async fn run(server: Server, quit: tokio::sync::oneshot::Sender<()>) -> Result<(), ServeError> {
    let controller = server.controller.clone();
    let (selection, catalog) = {
        let controller = controller.read().await;
        (
            controller.selection_snapshot(DEFAULT_SESSION),
            controller.catalog(),
        )
    };
    let listen = server
        .listeners
        .iter()
        .find(|listener| listener.addr.is_ipv4())
        .or(server.listeners.first())
        .map(|listener| listener.url())
        .unwrap_or_default();
    let mut app = App::new(Options {
        listen,
        selection,
        catalog,
        no_color: std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()),
        clock: log::local_clock,
    });
    app.info(format!("decoy {}", env!("CARGO_PKG_VERSION")));
    for notice in &server.notices {
        app.info(notice.clone());
    }
    for listener in &server.listeners {
        app.info(format!("listening on {}", listener.label()));
    }

    let mut events = server.subscribe();
    let mut served = tokio::spawn(server.wait());

    let mut terminal = ratatui::init();
    // Keyboard enhancement reports Ctrl+Shift+C and Cmd+C, where the terminal supports it.
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        let _ = execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    // Without these the TUI still works (no wheel, no Cmd+C), and an early return here would
    // leave the terminal in raw mode on the alternate screen.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    // ratatui's hook leaves the alternate screen and raw mode; release the rest before it.
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        release(enhanced);
        restore(info);
    }));

    let result = event_loop(
        &mut terminal,
        &mut EventStream::new(),
        &mut std::io::stdout(),
        &mut app,
        &controller,
        &mut events,
        &mut served,
    )
    .await;

    release(enhanced);
    ratatui::restore();
    // Nobody reads the log any more, so drain warnings go to stderr.
    drop(events);

    match result {
        Ok(Some(stopped)) => stopped,
        Ok(None) => {
            let _ = quit.send(());
            joined(served.await)
        }
        Err(error) => {
            let _ = quit.send(());
            let _ = served.await;
            Err(error.into())
        }
    }
}

fn release(enhanced: bool) {
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    if enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
}

type Served = tokio::task::JoinHandle<Result<(), ServeError>>;

fn joined(
    result: Result<Result<(), ServeError>, tokio::task::JoinError>,
) -> Result<(), ServeError> {
    result.map_err(|error| Box::new(error) as ServeError)?
}

/// Returns the server's result when it stopped first, `None` when the user quit. Copies are
/// written to `out`, the terminal's output.
async fn event_loop(
    terminal: &mut Terminal<impl Backend>,
    input: &mut (impl Stream<Item = std::io::Result<crossterm::event::Event>> + Unpin),
    out: &mut impl Write,
    app: &mut App,
    controller: &RwLock<Controller>,
    events: &mut tokio::sync::broadcast::Receiver<decoy_runtime::events::Event>,
    served: &mut Served,
) -> std::io::Result<Option<Result<(), ServeError>>> {
    loop {
        terminal
            .draw(|frame| app.draw(frame))
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let first = tokio::select! {
            event = input.next() => match event {
                Some(event) => Event::Terminal(event?),
                None => return Ok(None),
            },
            event = events.recv() => match event {
                Ok(event) => Event::Runtime(event),
                Err(RecvError::Lagged(dropped)) => Event::Lagged(dropped),
                Err(RecvError::Closed) => return Ok(None),
            },
            stopped = &mut *served => return Ok(Some(joined(stopped))),
        };
        let mut batch = vec![first];
        while batch.len() < BATCH {
            match events.try_recv() {
                Ok(event) => batch.push(Event::Runtime(event)),
                Err(TryRecvError::Lagged(dropped)) => batch.push(Event::Lagged(dropped)),
                Err(_) => break,
            }
        }
        for event in batch {
            for effect in app.handle_event(event) {
                match effect {
                    Effect::Quit => return Ok(None),
                    Effect::Copy(text) => copy(out, &text)?,
                    Effect::Control { session, verb } => {
                        if let Err(error) = control(&mut *controller.write().await, &session, verb)
                        {
                            app.handle_event(Event::ControlFailed(error.to_string()));
                        }
                    }
                }
            }
        }
    }
}

/// Runs one Controller verb, with the Control API's validation and errors.
fn control(controller: &mut Controller, session: &str, verb: Verb) -> Result<(), ControlError> {
    match verb {
        Verb::UseCollection(id) => controller.try_use_collection(session, id).map(drop),
        Verb::UseRoute(address) => controller
            .try_use_route(session, address.route, address.case, address.behavior)
            .map(drop),
        Verb::Reset => {
            controller.reset(session);
            Ok(())
        }
    }
}

/// Copies through the terminal (OSC 52), which also works over SSH and in WSL.
fn copy(out: &mut impl Write, text: &str) -> std::io::Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    write!(out, "\x1b]52;c;{encoded}\x07")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};
    use decoy_runtime::Activation;
    use decoy_runtime::events::{Event as RuntimeEvent, Level, LogEvent};
    use ratatui::backend::TestBackend;
    use tokio::sync::{broadcast, mpsc, oneshot};

    use super::*;

    type Input = std::io::Result<TermEvent>;

    /// The loop over a test terminal, with its keys, runtime events and server end in the test's
    /// hands.
    struct Harness {
        terminal: Terminal<TestBackend>,
        app: App,
        controller: RwLock<Controller>,
        out: Vec<u8>,
        keys: mpsc::UnboundedSender<Input>,
        input: std::pin::Pin<Box<dyn Stream<Item = Input>>>,
        runtime: broadcast::Sender<RuntimeEvent>,
        events: broadcast::Receiver<RuntimeEvent>,
        stop: Option<oneshot::Sender<Result<(), ServeError>>>,
        served: Served,
    }

    impl Harness {
        fn new(capacity: usize) -> Self {
            let (keys, mut rx) = mpsc::unbounded_channel();
            let (runtime, events) = broadcast::channel(capacity);
            let (stop, stopped) = oneshot::channel();
            Self {
                terminal: Terminal::new(TestBackend::new(80, 8)).unwrap(),
                app: app::tests::app(),
                controller: RwLock::new(Controller::new(complete::tests::catalog(), "happy-path")),
                out: Vec::new(),
                keys,
                input: Box::pin(futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))),
                runtime,
                events,
                stop: Some(stop),
                served: tokio::spawn(async move { stopped.await.unwrap_or(Ok(())) }),
            }
        }

        fn log(&self, message: &str) {
            let event = RuntimeEvent::Log(LogEvent {
                level: Level::Warn,
                message: message.into(),
            });
            self.runtime.send(event).unwrap();
        }

        /// Sends `keys` once the loop has taken every runtime event, so they come last.
        fn then_keys(&self, keys: Vec<Input>) {
            let runtime = self.runtime.clone();
            let sender = self.keys.clone();
            tokio::spawn(async move {
                while !runtime.is_empty() {
                    tokio::task::yield_now().await;
                }
                for key in keys {
                    sender.send(key).unwrap();
                }
            });
        }

        async fn run(&mut self) -> std::io::Result<Option<Result<(), ServeError>>> {
            event_loop(
                &mut self.terminal,
                &mut self.input,
                &mut self.out,
                &mut self.app,
                &self.controller,
                &mut self.events,
                &mut self.served,
            )
            .await
        }

        fn screen(&self) -> String {
            let buffer = self.terminal.backend().buffer();
            (0..buffer.area.height)
                .map(|y| {
                    let row: String = (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect();
                    row.trim_end().to_owned() + "\n"
                })
                .collect()
        }
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Input {
        Ok(TermEvent::Key(KeyEvent::new(code, modifiers)))
    }

    fn ctrl_d() -> Input {
        key(KeyCode::Char('d'), KeyModifiers::CONTROL)
    }

    #[tokio::test]
    async fn runtime_events_are_drawn_until_the_user_quits() {
        let mut harness = Harness::new(16);
        harness.log("first");
        harness.log("second");
        harness.then_keys(vec![ctrl_d()]);

        assert!(harness.run().await.unwrap().is_none());
        let screen = harness.screen();
        assert!(screen.contains("warning: first"), "{screen}");
        assert!(screen.contains("warning: second"), "{screen}");
    }

    #[tokio::test]
    async fn a_lagging_log_says_how_many_events_it_missed() {
        let mut harness = Harness::new(2);
        for n in 1..=4 {
            harness.log(&format!("line {n}"));
        }
        harness.then_keys(vec![ctrl_d()]);

        assert!(harness.run().await.unwrap().is_none());
        let screen = harness.screen();
        assert!(
            screen.contains("the log fell behind; 2 events dropped"),
            "{screen}"
        );
        assert!(screen.contains("warning: line 4"), "{screen}");
    }

    #[tokio::test]
    async fn a_copy_goes_to_the_terminal_as_osc_52() {
        let mut harness = Harness::new(16);
        harness.app.info("hello");
        harness.app.selected = Some((0, 0));
        harness.then_keys(vec![key(KeyCode::Enter, KeyModifiers::NONE), ctrl_d()]);

        assert!(harness.run().await.unwrap().is_none());
        let out = String::from_utf8(harness.out).unwrap();
        let encoded = out
            .strip_prefix("\x1b]52;c;")
            .and_then(|rest| rest.strip_suffix('\x07'))
            .unwrap_or_else(|| panic!("{out:?}"));
        let copied = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert!(String::from_utf8(copied).unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn a_command_changes_the_controllers_selection() {
        let mut harness = Harness::new(16);
        let mut keys: Vec<Input> = "/collection errors"
            .chars()
            .map(|c| key(KeyCode::Char(c), KeyModifiers::NONE))
            .collect();
        keys.extend([key(KeyCode::Enter, KeyModifiers::NONE), ctrl_d()]);
        harness.then_keys(keys);

        assert!(harness.run().await.unwrap().is_none());
        let controller = harness.controller.read().await;
        assert_eq!(
            controller.selection_snapshot("default").collection,
            "errors"
        );
    }

    #[tokio::test]
    async fn a_server_that_stops_ends_the_loop_with_its_result() {
        let mut harness = Harness::new(16);
        let stop = harness.stop.take().unwrap();
        stop.send(Err("port gone".into())).unwrap();

        let stopped = harness.run().await.unwrap().unwrap();
        assert_eq!(stopped.unwrap_err().to_string(), "port gone");
    }

    #[tokio::test]
    async fn a_closed_runtime_or_a_broken_terminal_ends_the_loop() {
        let mut harness = Harness::new(16);
        harness.runtime = broadcast::channel(1).0;
        assert!(harness.run().await.unwrap().is_none());

        let mut harness = Harness::new(16);
        harness
            .keys
            .send(Err(std::io::Error::other("tty gone")))
            .unwrap();
        assert_eq!(harness.run().await.unwrap_err().to_string(), "tty gone");
    }

    #[test]
    fn verbs_fail_with_the_control_api_errors() {
        let mut controller = Controller::new(complete::tests::catalog(), "happy-path");
        let mut error = |verb| {
            control(&mut controller, "default", verb)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            error(Verb::UseCollection("nope".into())),
            "unknown collection `nope`"
        );
        assert_eq!(
            error(Verb::UseRoute(Activation::parse("nope:any:ok").unwrap())),
            "unknown route `nope`"
        );
        assert_eq!(
            error(Verb::UseRoute(Activation::parse("orders:nope:ok").unwrap())),
            "unknown case `orders:nope`"
        );
        assert_eq!(
            error(Verb::UseRoute(
                Activation::parse("orders:any:nope").unwrap()
            )),
            "unknown behavior `orders:any:nope`"
        );
    }

    #[test]
    fn verbs_change_the_sessions_selection() {
        let mut controller = Controller::new(complete::tests::catalog(), "happy-path");
        control(
            &mut controller,
            "e2e-1",
            Verb::UseCollection("errors".into()),
        )
        .unwrap();
        let address = Activation::parse("orders:any:ok").unwrap();
        control(&mut controller, "e2e-1", Verb::UseRoute(address)).unwrap();
        let selection = controller.selection_snapshot("e2e-1");
        assert_eq!(selection.collection, "errors");
        assert_eq!(selection.overrides[0].address(), "orders:any:ok");
        control(&mut controller, "e2e-1", Verb::Reset).unwrap();
        assert!(controller.selection_snapshot("e2e-1").overrides.is_empty());
        assert_eq!(
            controller.selection_snapshot("default").collection,
            "happy-path"
        );
    }
}
