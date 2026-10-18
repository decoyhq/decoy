//! The TUI's state, driven by [`App::handle_event`] so it can be tested without a terminal.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::SystemTime;

use crossterm::event::{
    Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use decoy_runtime::events::{
    Event as RuntimeEvent, Level, LogEvent, RequestEvent, SessionChange, SessionEvent,
    SessionSource,
};
use decoy_runtime::{Activation, Catalog, RouteOverrideSnapshot, SelectionSnapshot};
use ratatui::layout::{Position, Rect};

use crate::command::{COMMANDS, Command};
use crate::complete::{Completion, Sources, complete};
use crate::log::{Entry, Log, RequestLine, Theme};

/// Full request records kept for the detail panel; older ids print `#N expired`.
pub const RETAIN: usize = 500;

const KEYS: &[&str] = &[
    "keys: ↑↓ pick · Tab/→ complete · Enter run · Esc close · ↑↓ history (no popup)",
    "      Ctrl+O recent requests · click a request: details (Esc closes, PgUp/PgDn scroll it)",
    "      PgUp/PgDn/Home/End or wheel scroll · drag select + copy · Shift+drag terminal selection",
    "      Ctrl+U clear line · Ctrl+C clear, twice quits · Ctrl+D quit on empty input",
];

/// Something that happened: input from the terminal or news from the runtime.
#[derive(Debug, Clone)]
pub enum Event {
    Terminal(TermEvent),
    Runtime(RuntimeEvent),
    /// The runtime broadcast dropped this many events because the TUI fell behind.
    Lagged(u64),
    /// An [`Effect::Control`] failed; the Controller's error, as the Control API words it.
    ControlFailed(String),
}

/// Something [`App::handle_event`] asks the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Quit,
    /// Put the text on the clipboard (OSC 52).
    Copy(String),
    /// Call a Controller verb on `session`; report a failure back as [`Event::ControlFailed`].
    Control {
        session: String,
        verb: Verb,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    UseCollection(String),
    UseRoute(Activation),
    Reset,
}

/// What the TUI starts from.
pub struct Options {
    /// The address shown in the status bar.
    pub listen: String,
    /// The default Session's Selection at startup.
    pub selection: SelectionSnapshot,
    /// The Routes and Collections, for completion.
    pub catalog: Arc<Catalog>,
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
    /// Every Session seen in a request or a Selection change, for `/session` completion.
    seen: BTreeSet<String>,
    catalog: Arc<Catalog>,
    pub(crate) no_color: bool,
    pub(crate) clock: fn(SystemTime) -> String,

    pub(crate) log: Log,
    pub(crate) input: String,
    /// Submitted lines, oldest first, for ↑/↓.
    history: Vec<String>,
    /// The history entry ↑/↓ put in the input.
    history_at: Option<usize>,
    /// The highlighted completion.
    pub(crate) popup: usize,
    /// Esc closed the popup; the next edit opens it again.
    popup_closed: bool,
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

    /// The last [`RETAIN`] requests, oldest first.
    pub(crate) records: VecDeque<Arc<RequestEvent>>,
    /// The open detail panel.
    pub(crate) detail: Option<Detail>,
    /// The open request picker's highlighted row, 0 being the newest request.
    pub(crate) picker: Option<usize>,

    /// Where the last frame drew each visible entry, for mouse hit-testing.
    pub(crate) rows: Vec<(Rect, usize)>,
    pub(crate) log_area: Rect,
    pub(crate) detail_area: Option<Rect>,
}

impl App {
    pub fn new(options: Options) -> Self {
        Self {
            listen: options.listen,
            session: decoy_runtime::DEFAULT_SESSION.to_owned(),
            defaults: options.selection,
            selections: BTreeMap::new(),
            seen: BTreeSet::from([decoy_runtime::DEFAULT_SESSION.to_owned()]),
            catalog: options.catalog,
            no_color: options.no_color,
            clock: options.clock,
            log: Log::default(),
            input: String::new(),
            history: Vec::new(),
            history_at: None,
            popup: 0,
            popup_closed: false,
            ctrl_c_armed: false,
            hint: None,
            scroll: 0,
            unseen: 0,
            selected: None,
            drag: None,
            records: VecDeque::new(),
            detail: None,
            picker: None,
            rows: Vec::new(),
            log_area: Rect::default(),
            detail_area: None,
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
                self.edit(|input| input.push_str(&text.replace(['\r', '\n'], " ")));
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
            Event::ControlFailed(error) => {
                self.push(Entry::Error(error));
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

    pub(crate) fn record(&self, id: u64) -> Option<&Arc<RequestEvent>> {
        self.records.iter().find(|record| record.id == id)
    }

    /// What the popup offers for the current input; empty when it is closed.
    pub(crate) fn completions(&self) -> Vec<Completion> {
        if self.popup_closed || self.picker.is_some() {
            return Vec::new();
        }
        let sources = Sources {
            catalog: &self.catalog,
            sessions: self.seen.iter().map(String::as_str).collect(),
            session: &self.session,
            collection: &self.selection().collection,
        };
        complete(&self.input, &sources)
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
                self.seen.insert(request.session.clone());
                self.push(Entry::Request(RequestLine::from(&*request)));
                if self.records.len() == RETAIN {
                    self.records.pop_front();
                }
                self.records.push_back(request);
                if let Some(row) = self.picker.as_mut() {
                    // Keep the same request highlighted as newer ones arrive on top.
                    *row = (*row + 1).min(self.records.len() - 1);
                }
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
        self.seen.insert(session.clone());
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

        if let Some(row) = self.picker {
            match key.code {
                KeyCode::Esc => self.picker = None,
                KeyCode::Char('c') if ctrl => self.picker = None,
                KeyCode::Up => self.picker = Some((row + 1).min(self.records.len() - 1)),
                KeyCode::Down => self.picker = Some(row.saturating_sub(1)),
                KeyCode::Enter => {
                    self.picker = None;
                    if let Some(id) = self.records.iter().rev().nth(row).map(|record| record.id) {
                        self.open(id);
                    }
                }
                _ => {}
            }
            return vec![];
        }

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

        let items = self.completions();
        match key.code {
            KeyCode::Char('o') if ctrl => {
                if self.records.is_empty() {
                    self.hint = Some("no requests yet".to_owned());
                } else {
                    self.picker = Some(0);
                }
            }
            KeyCode::Char('c') if ctrl => {
                if !self.input.is_empty() {
                    self.edit(String::clear);
                    self.ctrl_c_armed = true;
                } else if self.ctrl_c_armed {
                    return vec![Effect::Quit];
                } else {
                    self.ctrl_c_armed = true;
                }
                self.hint = Some("Ctrl+C again to quit".to_owned());
            }
            KeyCode::Char('d') if ctrl && self.input.is_empty() => return vec![Effect::Quit],
            KeyCode::Char('u') if ctrl => self.edit(String::clear),
            KeyCode::Char(c) if !ctrl => self.edit(|input| input.push(c)),
            KeyCode::Backspace => self.edit(|input| {
                input.pop();
            }),
            KeyCode::Esc if !items.is_empty() => self.popup_closed = true,
            KeyCode::Esc if self.detail.is_some() => self.detail = None,
            KeyCode::Esc => self.follow(),
            KeyCode::Down if !items.is_empty() => self.popup = (self.popup + 1) % items.len(),
            KeyCode::Up | KeyCode::BackTab if !items.is_empty() => {
                self.popup = (self.popup + items.len() - 1) % items.len();
            }
            KeyCode::Tab | KeyCode::Right if !items.is_empty() => {
                self.accept(&items);
            }
            KeyCode::Enter if !items.is_empty() => {
                if self.accept(&items) {
                    return self.submit();
                }
            }
            KeyCode::Up if !self.history.is_empty() => {
                let at = self
                    .history_at
                    .map_or(self.history.len() - 1, |at| at.saturating_sub(1));
                self.recall(Some(at));
            }
            KeyCode::Down if self.history_at.is_some() => {
                let next = self.history_at.map(|at| at + 1);
                self.recall(next.filter(|&at| at < self.history.len()));
            }
            // With the detail open, PgUp/PgDn scroll it; Shift+PgUp/PgDn still scroll the log.
            KeyCode::PageUp | KeyCode::PageDown
                if self.detail.is_some() && !key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                let page = self
                    .detail_area
                    .map_or(10, |area| area.height.saturating_sub(3).max(1));
                self.scroll_detail(key.code == KeyCode::PageUp, page);
            }
            KeyCode::PageUp => self.scroll_log(true, self.page()),
            KeyCode::PageDown => self.scroll_log(false, self.page()),
            KeyCode::Home if ctrl || self.input.is_empty() => {
                self.scroll = self.log.len().saturating_sub(1);
            }
            KeyCode::End if ctrl || self.input.is_empty() => self.follow(),
            KeyCode::Enter => return self.submit(),
            _ => {}
        }
        vec![]
    }

    /// Changes the input by hand, which reopens the popup and leaves the history.
    fn edit(&mut self, change: impl FnOnce(&mut String)) {
        change(&mut self.input);
        self.popup = 0;
        self.popup_closed = false;
        self.history_at = None;
    }

    /// Puts a history entry (or, past the newest, an empty line) in the input, popup closed.
    fn recall(&mut self, at: Option<usize>) {
        self.input = at.map_or_else(String::new, |at| self.history[at].clone());
        self.history_at = at;
        self.popup_closed = true;
    }

    /// Applies the highlighted completion; true when that leaves a whole command.
    fn accept(&mut self, items: &[Completion]) -> bool {
        let item = &items[self.popup.min(items.len() - 1)];
        self.input.truncate(item.from);
        self.input.push_str(&item.insert);
        self.popup = 0;
        self.popup_closed = false;
        item.done
    }

    /// Runs the input line: echoes it, then the command's result or error.
    fn submit(&mut self) -> Vec<Effect> {
        let text = std::mem::take(&mut self.input).trim().to_owned();
        self.edit(|_| {});
        if text.is_empty() {
            return vec![];
        }
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        self.push(Entry::Command(text.clone()));
        self.follow();
        let command = match crate::command::parse(&text) {
            Ok(command) => command,
            Err(error) if text.starts_with('/') => {
                self.push(Entry::Error(error));
                return vec![];
            }
            Err(hint) => {
                self.info(hint);
                return vec![];
            }
        };
        let control = |verb| {
            vec![Effect::Control {
                session: self.session.clone(),
                verb,
            }]
        };
        match command {
            Command::Collection(id) => return control(Verb::UseCollection(id)),
            Command::Route(address) => return control(Verb::UseRoute(address)),
            Command::Reset => return control(Verb::Reset),
            Command::Quit => return vec![Effect::Quit],
            Command::Session(id) => {
                if self.seen.contains(&id) {
                    self.info(format!("session {id} active"));
                } else {
                    self.info(format!(
                        "new session {id} active (defaults until its first change)"
                    ));
                    self.seen.insert(id.clone());
                }
                self.session = id;
            }
            Command::Show(id) => self.open(id),
            Command::Sessions => self.sessions(),
            Command::Status => self.status(),
            Command::Help => {
                for spec in COMMANDS {
                    let usage = format!("{} {}", spec.name, spec.args);
                    self.info(format!("  {usage:<32} {}", spec.about));
                }
                for line in KEYS {
                    self.info(format!("  {line}"));
                }
            }
        }
        vec![]
    }

    fn sessions(&mut self) {
        let mut lines: Vec<String> = self
            .selections
            .iter()
            .map(|(id, selection)| {
                let mark = if *id == self.session { "*" } else { " " };
                format!(
                    "{mark} {id:<16} collection {} · {}",
                    selection.collection,
                    overrides(selection.overrides.len())
                )
            })
            .collect();
        if !self.selections.contains_key(&self.session) {
            lines.push(format!("* {:<16} (defaults)", self.session));
        }
        for line in lines {
            self.info(line);
        }
    }

    fn status(&mut self) {
        let selection = self.selection().clone();
        self.info(format!(
            "session {} · collection {} · {}",
            self.session,
            selection.collection,
            overrides(selection.overrides.len())
        ));
        for one in &selection.overrides {
            self.info(format!("  {}", one.address()));
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
        self.hint = Some(format!("copied {}", plural(text.len(), "line")));
        vec![Effect::Copy(text.join("\n"))]
    }

    fn entry_at(&self, column: u16, row: u16) -> Option<usize> {
        let position = Position::new(column, row);
        self.rows
            .iter()
            .find(|(rect, _)| rect.contains(position))
            .map(|&(_, index)| index)
    }

    /// Opens request `id` in the detail panel, or says it is gone or never was.
    fn open(&mut self, id: u64) {
        let Some(record) = self.record(id).cloned() else {
            let newest = self.records.back().map_or(0, |record| record.id);
            self.info(if id > newest {
                format!("#{id}: no such request")
            } else {
                format!("#{id} expired")
            });
            return;
        };
        self.detail = Some(Detail {
            title: format!(" #{id} {} {} ", record.method, record.url),
            lines: crate::detail::lines(&record, &self.theme()),
            scroll: 0,
        });
    }

    fn scroll_detail(&mut self, up: bool, by: u16) {
        if let Some(detail) = self.detail.as_mut() {
            detail.scroll = if up {
                detail.scroll.saturating_sub(by)
            } else {
                // The next frame clamps it to the panel's last page.
                detail.scroll.saturating_add(by)
            };
        }
    }

    fn on_mouse(&mut self, mouse: MouseEvent) -> Vec<Effect> {
        // The picker covers the log; a click there must not open the line beneath it.
        if self.picker.is_some() {
            return vec![];
        }
        let over_detail = self
            .detail_area
            .is_some_and(|area| area.contains(Position::new(mouse.column, mouse.row)));
        match mouse.kind {
            MouseEventKind::ScrollUp if over_detail => self.scroll_detail(true, 3),
            MouseEventKind::ScrollDown if over_detail => self.scroll_detail(false, 3),
            MouseEventKind::ScrollUp => self.scroll_log(true, 3),
            MouseEventKind::ScrollDown => self.scroll_log(false, 3),
            MouseEventKind::Down(MouseButton::Left) if over_detail => {}
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
                // A click: open the request, if the line is one.
                Some(false) => {
                    let clicked = self
                        .selected
                        .take()
                        .and_then(|(index, _)| self.log.get(index));
                    if let Some(Entry::Request(request)) = clicked {
                        let id = request.id;
                        self.open(id);
                    }
                }
                None => {}
            },
            _ => {}
        }
        vec![]
    }
}

/// The open request detail, rendered once: it stays readable after the record is evicted, and
/// frames don't re-render its bodies.
pub(crate) struct Detail {
    pub title: String,
    pub lines: Vec<ratatui::text::Line<'static>>,
    /// Rows scrolled off the top.
    pub scroll: u16,
}

/// `1 override`, `2 overrides`.
pub(crate) fn overrides(count: usize) -> String {
    plural(count, "override")
}

fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
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
            catalog: Arc::new(crate::complete::tests::catalog()),
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

    pub fn base(id: u64, outcome: Outcome) -> RequestEvent {
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

    pub fn matched(address: &str) -> Match {
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
            Some(&Entry::Info(
                "commands start with /; free text is reserved (try /help)".into()
            ))
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

    #[test]
    fn keys_and_the_wheel_scroll_between_the_oldest_line_and_following() {
        let mut app = app();
        app.log_area = Rect::new(0, 0, 80, 4);
        for id in 1..=10 {
            app.handle_event(hit(id));
        }
        app.handle_event(key(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.scroll, 9);
        app.handle_event(hit(11));
        app.handle_event(key(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!((app.scroll, app.unseen), (7, 1));
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 0));
        assert_eq!((app.scroll, app.unseen), (4, 1));
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 0));
        app.handle_event(mouse(MouseEventKind::ScrollDown, 0, 0));
        assert_eq!((app.scroll, app.unseen), (0, 0));
        app.handle_event(key(KeyCode::PageUp, KeyModifiers::NONE));
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn dragging_past_the_pane_scrolls_it() {
        let mut app = app();
        app.log_area = Rect::new(0, 1, 80, 2);
        for id in 1..=5 {
            app.handle_event(hit(id));
        }
        app.rows = vec![(Rect::new(0, 1, 80, 1), 3), (Rect::new(0, 2, 80, 1), 4)];
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 2));
        // Above the pane: scrolls back, and the selection reaches the top row.
        app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 0, 0));
        assert_eq!((app.scroll, app.selected), (1, Some((4, 3))));
        // Below it: scrolls forward again.
        app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 0, 9));
        assert_eq!((app.scroll, app.selected), (0, Some((4, 4))));
    }

    #[test]
    fn a_full_log_drops_its_oldest_line_and_the_selection_moves_with_the_rest() {
        let mut app = app();
        for _ in 0..crate::log::CAP {
            app.info("line");
        }
        app.selected = Some((1, 2));
        app.info("one more");
        assert_eq!(app.log.len(), crate::log::CAP);
        assert_eq!(app.selected, Some((0, 1)));
        app.info("and another");
        assert_eq!(app.selected, None);
    }

    #[test]
    fn paste_lands_in_the_input_on_one_line() {
        let mut app = app();
        app.handle_event(Event::Terminal(TermEvent::Paste("/session\r\ne2e".into())));
        assert_eq!(app.input, "/session  e2e");
        app.handle_event(Event::Terminal(TermEvent::FocusGained));
        assert_eq!(app.input, "/session  e2e");
    }

    #[test]
    fn ctrl_insert_copies_a_selection_like_ctrl_c() {
        let mut app = app();
        app.info("hello");
        app.selected = Some((0, 0));
        let effects = app.handle_event(key(KeyCode::Insert, KeyModifiers::CONTROL));
        assert!(matches!(&effects[..], [Effect::Copy(text)] if text.contains("hello")));
        assert_eq!(app.hint.as_deref(), Some("copied 1 line"));
    }

    #[test]
    fn runtime_errors_lags_and_in_process_resets_get_their_own_lines() {
        let mut app = app();
        app.handle_event(Event::Runtime(RuntimeEvent::Log(LogEvent {
            level: Level::Error,
            message: "accept failed".into(),
        })));
        app.handle_event(Event::Lagged(3));
        let Event::Runtime(RuntimeEvent::Session(mut reset)) =
            session_event("e2e-1", SessionChange::Reset, &[])
        else {
            unreachable!()
        };
        reset.source = SessionSource::InProcess;
        app.handle_event(Event::Runtime(RuntimeEvent::Session(reset)));
        let entries: Vec<_> = (0..app.log.len())
            .map(|i| app.log.get(i).unwrap())
            .collect();
        assert!(matches!(entries[0], Entry::Error(text) if text == "accept failed"));
        assert!(
            matches!(entries[1], Entry::Warn(text) if text == "the log fell behind; 3 events dropped")
        );
        assert!(
            matches!(entries[2], Entry::Info(text) if text == "session e2e-1 overrides cleared")
        );
    }
    fn enter() -> Event {
        key(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn tab() -> Event {
        key(KeyCode::Tab, KeyModifiers::NONE)
    }

    fn infos(app: &App) -> Vec<String> {
        (0..app.log.len())
            .filter_map(|i| match app.log.get(i) {
                Some(Entry::Info(text) | Entry::Error(text)) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn route_completes_segment_by_segment_and_runs_on_enter() {
        let mut app = app();
        typed(&mut app, "/route users");
        app.handle_event(tab());
        assert_eq!(app.input, "/route users-by-id:");
        typed(&mut app, "user-1:");
        app.handle_event(tab());
        assert_eq!(app.input, "/route users-by-id:user-1:ada");
        assert_eq!(
            app.handle_event(enter()),
            [Effect::Control {
                session: "default".into(),
                verb: Verb::UseRoute(Activation::parse("users-by-id:user-1:ada").unwrap()),
            }]
        );
        assert_eq!(
            app.log.get(0),
            Some(&Entry::Command("/route users-by-id:user-1:ada".into()))
        );
    }

    #[test]
    fn enter_on_a_partial_argument_accepts_the_pick_and_runs() {
        let mut app = app();
        typed(&mut app, "/collection er");
        assert_eq!(
            app.handle_event(enter()),
            [Effect::Control {
                session: "default".into(),
                verb: Verb::UseCollection("errors".into()),
            }]
        );
    }

    #[test]
    fn enter_on_a_command_that_takes_an_argument_only_completes_it() {
        let mut app = app();
        typed(&mut app, "/coll");
        assert_eq!(app.handle_event(enter()), []);
        assert_eq!(app.input, "/collection ");
    }

    #[test]
    fn arrows_pick_and_esc_closes_the_popup_until_the_next_edit() {
        let mut app = app();
        typed(&mut app, "/s");
        app.handle_event(key(KeyCode::Down, KeyModifiers::NONE));
        app.handle_event(key(KeyCode::Down, KeyModifiers::NONE));
        app.handle_event(key(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.input, "/status");
        app.handle_event(key(KeyCode::Backspace, KeyModifiers::NONE));
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.completions().is_empty());
        typed(&mut app, "s");
        assert_eq!(app.completions().len(), 1);
    }

    #[test]
    fn reset_and_quit_map_to_their_effects_on_the_active_session() {
        let mut app = app();
        typed(&mut app, "/session e2e-1");
        app.handle_event(enter());
        typed(&mut app, "/reset");
        assert_eq!(
            app.handle_event(enter()),
            [Effect::Control {
                session: "e2e-1".into(),
                verb: Verb::Reset,
            }]
        );
        typed(&mut app, "/quit");
        assert_eq!(app.handle_event(enter()), [Effect::Quit]);
    }

    #[test]
    fn enter_on_a_whole_id_runs_it_even_when_a_longer_id_shares_its_prefix() {
        let mut app = app();
        app.handle_event(from_session(1, "e2e-1"));
        app.handle_event(from_session(2, "e2e"));
        typed(&mut app, "/session e2e");
        app.handle_event(enter());
        assert_eq!(app.session, "e2e");
    }

    #[test]
    fn switching_to_an_unknown_session_says_it_is_new() {
        let mut app = app();
        app.handle_event(from_session(1, "e2e-1"));
        typed(&mut app, "/session e2e-1");
        app.handle_event(enter());
        typed(&mut app, "/session fresh");
        app.handle_event(enter());
        assert_eq!(
            infos(&app),
            [
                "session e2e-1 active",
                "new session fresh active (defaults until its first change)"
            ]
        );
        assert_eq!(app.session, "fresh");
    }

    #[test]
    fn control_failures_and_bad_input_print_errors() {
        let mut app = app();
        app.handle_event(Event::ControlFailed("unknown collection `nope`".into()));
        typed(&mut app, "/route nope");
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_event(enter());
        typed(&mut app, "hello");
        app.handle_event(enter());
        assert_eq!(
            infos(&app),
            [
                "unknown collection `nope`",
                "usage: /route <route:case:behavior>",
                "commands start with /; free text is reserved (try /help)",
            ]
        );
    }

    #[test]
    fn up_and_down_walk_the_history_when_the_popup_is_closed() {
        let mut app = app();
        for line in ["/status", "/sessions", "/sessions"] {
            typed(&mut app, line);
            app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
            app.handle_event(enter());
        }
        let up = || key(KeyCode::Up, KeyModifiers::NONE);
        let down = || key(KeyCode::Down, KeyModifiers::NONE);
        app.handle_event(up());
        assert_eq!(app.input, "/sessions");
        app.handle_event(up());
        assert_eq!(app.input, "/status");
        app.handle_event(up());
        assert_eq!(app.input, "/status");
        app.handle_event(down());
        assert_eq!(app.input, "/sessions");
        app.handle_event(down());
        assert_eq!(app.input, "");
    }
}
