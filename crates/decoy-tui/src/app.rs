//! The TUI's state, driven by [`App::handle_event`] so it can be tested without a terminal.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use decoy_runtime::events::{
    Event as RuntimeEvent, Level, LogEvent, SessionChange, SessionEvent, SessionSource,
};
use decoy_runtime::{RouteOverrideSnapshot, SelectionSnapshot};
use ratatui::layout::{Position, Rect};

use crate::log::{Entry, Log, RequestLine, Theme};

/// Something that happened: input from the terminal or news from the runtime.
#[derive(Debug, Clone)]
pub enum Event {
    Terminal(TermEvent),
    Runtime(RuntimeEvent),
    /// The runtime broadcast dropped this many events because the TUI fell behind.
    Lagged(u64),
}

/// Something [`App::handle_event`] asks the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Quit,
    /// Put the text on the clipboard (OSC 52).
    Copy(String),
}

/// What the TUI starts from.
pub struct Options {
    /// The address shown in the status bar.
    pub listen: String,
    /// The default Session's Selection at startup.
    pub selection: SelectionSnapshot,
    pub no_color: bool,
    /// Formats a log line's time; tests pass a fixed zone.
    pub clock: fn(SystemTime) -> String,
}

pub struct App {
    pub(crate) listen: String,
    /// The Session commands target.
    pub(crate) session: String,
    /// What a Session without its own Selection reads as.
    pub(crate) defaults: SelectionSnapshot,
    pub(crate) selections: BTreeMap<String, SelectionSnapshot>,
    pub(crate) no_color: bool,
    pub(crate) clock: fn(SystemTime) -> String,

    pub(crate) log: Log,
    pub(crate) input: String,
    /// A Ctrl+C on empty input armed quitting; the next one quits.
    ctrl_c_armed: bool,
    /// Replaces the status bar until the next key.
    pub(crate) hint: Option<String>,

    /// Log entries hidden below the bottom of the pane; 0 follows new lines.
    pub(crate) scroll: usize,
    /// Entries that arrived while scrolled back.
    pub(crate) unseen: usize,
    /// Selected log entries, inclusive, in drag order.
    pub(crate) selected: Option<(usize, usize)>,
    /// `Some(moved)` while the left button is down over the log.
    drag: Option<bool>,

    /// Where the last frame drew each visible entry, for mouse hit-testing.
    pub(crate) rows: Vec<(Rect, usize)>,
    pub(crate) log_area: Rect,
}

impl App {
    pub fn new(options: Options) -> Self {
        Self {
            listen: options.listen,
            session: decoy_runtime::DEFAULT_SESSION.to_owned(),
            defaults: options.selection,
            selections: BTreeMap::new(),
            no_color: options.no_color,
            clock: options.clock,
            log: Log::default(),
            input: String::new(),
            ctrl_c_armed: false,
            hint: None,
            scroll: 0,
            unseen: 0,
            selected: None,
            drag: None,
            rows: Vec::new(),
            log_area: Rect::default(),
        }
    }

    /// A dim system line, e.g. a startup message.
    pub fn info(&mut self, text: impl Into<String>) {
        self.push(Entry::Info(text.into()));
    }

    pub fn handle_event(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Terminal(TermEvent::Key(key)) if key.kind != KeyEventKind::Release => {
                self.on_key(key)
            }
            Event::Terminal(TermEvent::Mouse(mouse)) => self.on_mouse(mouse),
            Event::Terminal(TermEvent::Paste(text)) => {
                self.input.push_str(&text.replace(['\r', '\n'], " "));
                vec![]
            }
            Event::Terminal(_) => vec![],
            Event::Runtime(event) => {
                self.on_runtime(event);
                vec![]
            }
            Event::Lagged(dropped) => {
                self.push(Entry::Warn(format!(
                    "the log fell behind; {dropped} events dropped"
                )));
                vec![]
            }
        }
    }

    pub(crate) fn theme(&self) -> Theme<'_> {
        Theme {
            session: &self.session,
            no_color: self.no_color,
            clock: self.clock,
        }
    }

    pub(crate) fn selection(&self) -> &SelectionSnapshot {
        self.selections.get(&self.session).unwrap_or(&self.defaults)
    }

    fn push(&mut self, entry: Entry) {
        if self.log.push(entry) {
            self.selected = self
                .selected
                .and_then(|(a, b)| Some((a.checked_sub(1)?, b.checked_sub(1)?)));
        }
        if self.scroll > 0 {
            // Keep the view still while scrolled back.
            self.scroll = (self.scroll + 1).min(self.log.len() - 1);
            self.unseen += 1;
        }
    }

    fn on_runtime(&mut self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::Request(request) => {
                self.push(Entry::Request(RequestLine::from(&*request)));
            }
            RuntimeEvent::Session(event) => self.on_session(event),
            RuntimeEvent::Log(LogEvent { level, message }) => self.push(match level {
                Level::Warn => Entry::Warn(message),
                Level::Error => Entry::Error(message),
            }),
        }
    }

    fn on_session(&mut self, event: SessionEvent) {
        let SessionEvent {
            session,
            change,
            source,
            selection,
        } = event;
        let before = self.selections.get(&session).unwrap_or(&self.defaults);
        let what = match change {
            SessionChange::Created => return,
            SessionChange::UseCollection => format!("collection {}", selection.collection),
            SessionChange::UseRoute => {
                let added: Vec<String> = selection
                    .overrides
                    .iter()
                    .filter(|o| !before.overrides.contains(o))
                    .map(RouteOverrideSnapshot::address)
                    .collect();
                format!("route {}", added.join(", "))
            }
            SessionChange::Reset => "overrides cleared".to_owned(),
            SessionChange::Ended => "ended".to_owned(),
        };
        let by = match source {
            SessionSource::InProcess => "",
            SessionSource::ControlApi => " (control api)",
        };
        self.info(format!("session {session} {what}{by}"));
        if change == SessionChange::Ended {
            self.selections.remove(&session);
        } else {
            self.selections.insert(session, selection);
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let copy_chord = match key.code {
            KeyCode::Char('c' | 'C') => ctrl || key.modifiers.contains(KeyModifiers::SUPER),
            KeyCode::Insert => ctrl,
            _ => false,
        };
        if !(ctrl && key.code == KeyCode::Char('c')) {
            self.ctrl_c_armed = false;
        }
        self.hint = None;

        if self.selected.is_some() {
            match key.code {
                _ if copy_chord => {
                    self.ctrl_c_armed = false;
                    return self.copy_selection();
                }
                KeyCode::Enter => {
                    let effects = self.copy_selection();
                    self.follow();
                    return effects;
                }
                KeyCode::Esc => {
                    self.selected = None;
                    return vec![];
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('c') if ctrl => {
                if !self.input.is_empty() {
                    self.input.clear();
                    self.ctrl_c_armed = true;
                } else if self.ctrl_c_armed {
                    return vec![Effect::Quit];
                } else {
                    self.ctrl_c_armed = true;
                }
                self.hint = Some("Ctrl+C again to quit".to_owned());
            }
            KeyCode::Char('d') if ctrl && self.input.is_empty() => return vec![Effect::Quit],
            KeyCode::Char('u') if ctrl => self.input.clear(),
            KeyCode::Char(c) if !ctrl => self.input.push(c),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Esc => self.follow(),
            KeyCode::PageUp => self.scroll_log(true, self.page()),
            KeyCode::PageDown => self.scroll_log(false, self.page()),
            KeyCode::Home if ctrl || self.input.is_empty() => {
                self.scroll = self.log.len().saturating_sub(1);
            }
            KeyCode::End if ctrl || self.input.is_empty() => self.follow(),
            KeyCode::Enter => {
                let text = std::mem::take(&mut self.input);
                self.submit(text.trim());
            }
            _ => {}
        }
        vec![]
    }

    fn submit(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.push(Entry::Command(text.to_owned()));
        self.follow();
        match text.split_whitespace().next() {
            Some(command) if command.starts_with('/') => {
                self.push(Entry::Error(format!("unknown command {command}")));
            }
            _ => self.info("commands start with /"),
        }
    }

    fn page(&self) -> usize {
        usize::from(self.log_area.height.saturating_sub(1)).max(1)
    }

    pub(crate) fn follow(&mut self) {
        self.scroll = 0;
        self.unseen = 0;
    }

    fn scroll_log(&mut self, up: bool, by: usize) {
        if up {
            self.scroll = (self.scroll + by).min(self.log.len().saturating_sub(1));
        } else {
            self.scroll = self.scroll.saturating_sub(by);
            if self.scroll == 0 {
                self.unseen = 0;
            }
        }
    }

    fn copy_selection(&mut self) -> Vec<Effect> {
        let Some((a, b)) = self.selected.take() else {
            return vec![];
        };
        let (first, last) = (a.min(b), a.max(b));
        let theme = self.theme();
        let columns = self.log.columns(self.log_area.width);
        let text: Vec<String> = (first..=last)
            .filter_map(|index| self.log.get(index))
            .map(|entry| crate::log::plain(&theme.line(entry, columns)))
            .collect();
        let count = text.len();
        self.hint = Some(format!(
            "copied {count} line{}",
            if count == 1 { "" } else { "s" }
        ));
        vec![Effect::Copy(text.join("\n"))]
    }

    fn entry_at(&self, column: u16, row: u16) -> Option<usize> {
        let position = Position::new(column, row);
        self.rows
            .iter()
            .find(|(rect, _)| rect.contains(position))
            .map(|&(_, index)| index)
    }

    fn on_mouse(&mut self, mouse: MouseEvent) -> Vec<Effect> {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_log(true, 3),
            MouseEventKind::ScrollDown => self.scroll_log(false, 3),
            MouseEventKind::Down(MouseButton::Left) => {
                self.selected = self
                    .entry_at(mouse.column, mouse.row)
                    .map(|index| (index, index));
                self.drag = self.selected.map(|_| false);
            }
            MouseEventKind::Drag(MouseButton::Left) if self.drag.is_some() => {
                // Dragging past the pane's edge scrolls it.
                if mouse.row < self.log_area.y {
                    self.scroll_log(true, 1);
                } else if mouse.row >= self.log_area.bottom() {
                    self.scroll_log(false, 1);
                }
                let row = mouse
                    .row
                    .clamp(self.log_area.y, self.log_area.bottom().saturating_sub(1));
                if let (Some(index), Some((anchor, _))) =
                    (self.entry_at(mouse.column, row), self.selected)
                {
                    self.selected = Some((anchor, index));
                    self.drag = Some(true);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => match self.drag.take() {
                // Copy on select, as in Codex; the highlight stays until Esc or a copy key.
                Some(true) => {
                    let selected = self.selected;
                    let effects = self.copy_selection();
                    self.selected = selected;
                    return effects;
                }
                Some(false) => self.selected = None,
                None => {}
            },
            _ => {}
        }
        vec![]
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    use crossterm::event::KeyEventState;
    use decoy_runtime::events::{CapturedBody, CapturedResponse, Match, Outcome, RequestEvent};
    use decoy_runtime::schema::Transport;
    use decoy_runtime::{Activation, MissDiagnostic, RouteOverrideSnapshot};

    use super::*;

    pub fn utc_clock(at: SystemTime) -> String {
        let secs = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        format!(
            "{:02}:{:02}:{:02}",
            secs / 3600 % 24,
            secs / 60 % 60,
            secs % 60
        )
    }

    pub fn app() -> App {
        App::new(Options {
            listen: "http://127.0.0.1:4010".into(),
            selection: SelectionSnapshot {
                collection: "happy-path".into(),
                overrides: vec![],
            },
            no_color: false,
            clock: utc_clock,
        })
    }

    pub fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Terminal(TermEvent::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }))
    }

    pub fn ctrl(c: char) -> Event {
        key(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    pub fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_event(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    pub fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Terminal(TermEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }))
    }

    fn base(id: u64, outcome: Outcome) -> RequestEvent {
        RequestEvent {
            id,
            at: UNIX_EPOCH + Duration::from_secs(12 * 3600 + 63),
            transport: Transport::Http,
            session: "default".into(),
            method: "GET".into(),
            url: "/users/123".into(),
            headers: Default::default(),
            body: CapturedBody::raw(b""),
            outcome,
            response: CapturedResponse {
                status: 200,
                headers: vec![],
                body: CapturedBody::raw(b""),
            },
            duration: Duration::from_millis(4),
        }
    }

    fn matched(address: &str) -> Match {
        Match {
            activation: Activation::parse(address).unwrap(),
            skipped: vec![],
            conditions: serde_json::Value::Null,
            path_params: Default::default(),
        }
    }

    pub fn hit(id: u64) -> Event {
        let event = base(id, Outcome::Hit(matched("users-by-id:user-123:success")));
        Event::Runtime(RuntimeEvent::Request(Arc::new(event)))
    }

    pub fn miss(id: u64) -> Event {
        let mut event = base(
            id,
            Outcome::Miss(MissDiagnostic {
                reason: "no active route matched".into(),
                collection: "happy-path".into(),
                candidates: vec![],
            }),
        );
        event.method = "POST".into();
        event.url = "/orders?page=2&limit=50&sort=created_at&filter=open".into();
        event.response.status = 404;
        Event::Runtime(RuntimeEvent::Request(Arc::new(event)))
    }

    pub fn passthrough(id: u64) -> Event {
        let mut event = base(
            id,
            Outcome::Passthrough {
                matched: matched("avatars:any:real"),
                upstream_url: Some("http://cdn.local/avatars/ada.png".into()),
                upstream_status: Some(200),
            },
        );
        event.url = "/avatars/ada.png".into();
        event.duration = Duration::from_millis(31);
        Event::Runtime(RuntimeEvent::Request(Arc::new(event)))
    }

    pub fn from_session(id: u64, session: &str) -> Event {
        let mut event = base(id, Outcome::Hit(matched("users-by-id:user-123:success")));
        event.session = session.into();
        Event::Runtime(RuntimeEvent::Request(Arc::new(event)))
    }

    pub fn session_event(session: &str, change: SessionChange, overrides: &[&str]) -> Event {
        Event::Runtime(RuntimeEvent::Session(SessionEvent {
            session: session.into(),
            change,
            source: SessionSource::ControlApi,
            selection: SelectionSnapshot {
                collection: "errors".into(),
                overrides: overrides
                    .iter()
                    .map(|address| {
                        let a = Activation::parse(address).unwrap();
                        RouteOverrideSnapshot {
                            route: a.route,
                            case_id: a.case,
                            behavior: a.behavior,
                        }
                    })
                    .collect(),
            },
        }))
    }

    #[test]
    fn ctrl_c_clears_the_input_then_quits_on_the_second_press() {
        let mut app = app();
        typed(&mut app, "/sta");
        assert_eq!(app.handle_event(ctrl('c')), []);
        assert_eq!(app.input, "");
        assert_eq!(app.hint.as_deref(), Some("Ctrl+C again to quit"));
        assert_eq!(app.handle_event(ctrl('c')), [Effect::Quit]);
    }

    #[test]
    fn ctrl_c_on_empty_input_arms_and_any_other_key_disarms() {
        let mut app = app();
        assert_eq!(app.handle_event(ctrl('c')), []);
        typed(&mut app, "x");
        app.handle_event(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.handle_event(ctrl('c')), []);
        assert_eq!(app.handle_event(ctrl('c')), [Effect::Quit]);
    }

    #[test]
    fn ctrl_d_quits_only_on_empty_input() {
        let mut app = app();
        typed(&mut app, "/x");
        assert_eq!(app.handle_event(ctrl('d')), []);
        assert_eq!(app.input, "/x");
        app.handle_event(ctrl('u'));
        assert_eq!(app.handle_event(ctrl('d')), [Effect::Quit]);
    }

    #[test]
    fn enter_echoes_the_input_and_hints_at_commands() {
        let mut app = app();
        typed(&mut app, "hello");
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.log.get(0), Some(&Entry::Command("hello".into())));
        assert_eq!(
            app.log.get(1),
            Some(&Entry::Info("commands start with /".into()))
        );
        assert_eq!(app.input, "");
    }

    #[test]
    fn session_events_update_the_selection_and_say_what_changed() {
        let mut app = app();
        app.handle_event(session_event("default", SessionChange::Created, &[]));
        app.handle_event(session_event("default", SessionChange::UseCollection, &[]));
        app.handle_event(session_event(
            "default",
            SessionChange::UseRoute,
            &["users:u1:missing"],
        ));
        assert_eq!(app.selection().overrides.len(), 1);
        app.handle_event(session_event("default", SessionChange::Ended, &[]));
        assert_eq!(app.selection().collection, "happy-path");
        let lines: Vec<_> = (0..app.log.len())
            .map(|i| app.log.get(i).unwrap())
            .collect();
        assert_eq!(
            lines,
            [
                &Entry::Info("session default collection errors (control api)".into()),
                &Entry::Info("session default route users:u1:missing (control api)".into()),
                &Entry::Info("session default ended (control api)".into()),
            ]
        );
    }

    #[test]
    fn new_lines_keep_a_scrolled_back_view_still_and_count_as_unseen() {
        let mut app = app();
        for id in 1..=5 {
            app.handle_event(hit(id));
        }
        app.handle_event(mouse(MouseEventKind::ScrollUp, 0, 0));
        assert_eq!(app.scroll, 3);
        app.handle_event(hit(6));
        assert_eq!((app.scroll, app.unseen), (4, 1));
        app.handle_event(key(KeyCode::End, KeyModifiers::NONE));
        assert_eq!((app.scroll, app.unseen), (0, 0));
    }
}
