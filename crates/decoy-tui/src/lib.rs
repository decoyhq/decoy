//! Interactive terminal UI for bare `decoy` (design: `docs/design/tui.md`): the runtime's live
//! log, an input box and a status bar, fullscreen in the alternate screen.

mod app;
mod log;
mod ui;

use std::io::Write;

use base64::Engine;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, EventStream};
use crossterm::execute;
use decoy_runtime::{DEFAULT_SESSION, ServeError, Server};
use futures_util::StreamExt;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

pub use app::{App, Effect, Event, Options};

/// Runtime events handled per frame, so a burst of traffic draws once and keys stay responsive.
const BATCH: usize = 1024;

/// Runs the TUI over a started `server` until the user quits or the server stops, then restores
/// the terminal and waits for the server to drain. `quit` must resolve the shutdown future the
/// server was started with.
pub async fn run(server: Server, quit: tokio::sync::oneshot::Sender<()>) -> Result<(), ServeError> {
    let selection = server
        .controller
        .read()
        .await
        .selection_snapshot(DEFAULT_SESSION);
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
        no_color: std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()),
        clock: log::local_clock,
    });
    app.info(format!("decoy {}", env!("CARGO_PKG_VERSION")));
    for notice in &server.notices {
        app.info(notice.clone());
    }
    for listener in &server.listeners {
        match listener.name.as_str() {
            "port" => app.info(format!("listening on {}", listener.url())),
            name => app.info(format!("listening on {} ({name})", listener.url())),
        }
    }

    let mut events = server.subscribe();
    let mut served = tokio::spawn(server.wait());

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    // ratatui's hook leaves the alternate screen and raw mode; release the mouse before it.
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        restore(info);
    }));

    let result = event_loop(&mut terminal, &mut app, &mut events, &mut served).await;

    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    let stopped = match result {
        Ok(Some(stopped)) => stopped,
        Ok(None) => {
            let _ = quit.send(());
            served.await
        }
        Err(error) => {
            let _ = quit.send(());
            let _ = served.await;
            return Err(error.into());
        }
    };
    stopped.map_err(|error| Box::new(error) as ServeError)?
}

type Served = tokio::task::JoinHandle<Result<(), ServeError>>;

/// Returns the server's result when it stopped first, `None` when the user quit.
async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    events: &mut tokio::sync::broadcast::Receiver<decoy_runtime::events::Event>,
    served: &mut Served,
) -> std::io::Result<Option<Result<Result<(), ServeError>, tokio::task::JoinError>>> {
    let mut input = EventStream::new();
    loop {
        terminal.draw(|frame| app.draw(frame))?;
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
            stopped = &mut *served => return Ok(Some(stopped)),
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
                    Effect::Copy(text) => copy(&text)?,
                }
            }
        }
    }
}

/// Copies through the terminal (OSC 52), which also works over SSH and in WSL.
fn copy(text: &str) -> std::io::Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut stdout = std::io::stdout();
    write!(stdout, "\x1b]52;c;{encoded}\x07")?;
    stdout.flush()
}
