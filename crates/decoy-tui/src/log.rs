//! The log pane's contents: one entry per finished request, runtime message or echoed command,
//! rendered into aligned lines at draw time so columns follow the widest id and the pane width.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use decoy_runtime::events::{Outcome, RequestEvent};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Log entries kept for scrolling back.
pub const CAP: usize = 10_000;
/// Columns kept for the outcome, status and duration after the path column.
const OUTCOME_ROOM: usize = 50;
const PATH_MIN: usize = 12;
const PATH_MAX: usize = 40;
/// `OPTIONS`, the longest method.
const METHOD_WIDTH: usize = 7;

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Request(RequestLine),
    /// A dim system line: startup, Session changes.
    Info(String),
    Warn(String),
    Error(String),
    /// A submitted input line, echoed with `›`.
    Command(String),
}

/// What a request line shows; the full record is not kept here, so 10k lines stay small.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestLine {
    pub id: u64,
    pub at: SystemTime,
    pub method: String,
    pub url: String,
    pub outcome: LineOutcome,
    pub status: u16,
    pub duration: Duration,
    pub session: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LineOutcome {
    Hit(String),
    Miss(String),
    Passthrough {
        upstream: Option<String>,
        failed: bool,
    },
}

impl From<&RequestEvent> for RequestLine {
    fn from(event: &RequestEvent) -> Self {
        Self {
            id: event.id,
            at: event.at,
            method: event.method.clone(),
            url: event.url.clone(),
            outcome: match &event.outcome {
                Outcome::Hit(hit) => LineOutcome::Hit(hit.activation.address()),
                Outcome::Miss(miss) => LineOutcome::Miss(miss.reason.clone()),
                Outcome::Passthrough {
                    upstream_url,
                    upstream_status,
                    ..
                } => LineOutcome::Passthrough {
                    upstream: upstream_url.clone(),
                    failed: upstream_status.is_none(),
                },
            },
            status: event.response.status,
            duration: event.duration,
            session: event.session.clone(),
        }
    }
}

/// The entries, oldest first, capped at [`CAP`].
#[derive(Debug, Default)]
pub struct Log {
    entries: VecDeque<Entry>,
    /// The widest request id so far, so ids stay right-aligned as they grow.
    max_id: u64,
}

impl Log {
    /// Appends `entry`; true when the oldest entry was dropped to make room.
    pub fn push(&mut self, entry: Entry) -> bool {
        if let Entry::Request(request) = &entry {
            self.max_id = self.max_id.max(request.id);
        }
        let evicted = self.entries.len() == CAP;
        if evicted {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
        evicted
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    /// Lays out lines for a pane `width` columns wide.
    pub fn columns(&self, width: u16) -> Columns {
        let id = format!("#{}", self.max_id).len();
        let fixed = id + 1 + "12:01:03".len() + 1 + METHOD_WIDTH + 1;
        Columns {
            id,
            path: usize::from(width)
                .saturating_sub(fixed + OUTCOME_ROOM)
                .clamp(PATH_MIN, PATH_MAX),
        }
    }
}

/// Column widths for request lines, from [`Log::columns`].
#[derive(Debug, Clone, Copy)]
pub struct Columns {
    id: usize,
    path: usize,
}

/// How lines look: the active Session (other Sessions get a tag), colors, the clock.
pub struct Theme<'a> {
    pub session: &'a str,
    pub no_color: bool,
    pub clock: fn(SystemTime) -> String,
}

impl Theme<'_> {
    /// `style`, minus colors under `NO_COLOR`; modifiers stay so the UI still reads.
    pub fn style(&self, style: Style) -> Style {
        if self.no_color {
            Style::default().add_modifier(style.add_modifier)
        } else {
            style
        }
    }

    pub fn fg(&self, color: Color) -> Style {
        self.style(Style::default().fg(color))
    }

    pub fn dim(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    pub fn line(&self, entry: &Entry, columns: Columns) -> Line<'static> {
        match entry {
            Entry::Request(request) => self.request(request, columns),
            Entry::Info(text) => Line::styled(text.clone(), self.dim()),
            Entry::Warn(text) => Line::styled(format!("warning: {text}"), self.fg(Color::Yellow)),
            Entry::Error(text) => Line::styled(format!("error: {text}"), self.fg(Color::Red)),
            Entry::Command(text) => Line::from(vec![
                Span::styled("› ", self.fg(Color::Cyan)),
                Span::raw(text.clone()),
            ]),
        }
    }

    fn request(&self, request: &RequestLine, columns: Columns) -> Line<'static> {
        let id = format!("#{}", request.id);
        let mut spans = vec![
            Span::styled(format!("{id:>width$} ", width = columns.id), self.dim()),
            Span::styled(format!("{} ", (self.clock)(request.at)), self.dim()),
            Span::raw(format!("{:<METHOD_WIDTH$} ", request.method)),
            Span::raw(format!(
                "{:<width$} ",
                truncate(&request.url, columns.path),
                width = columns.path
            )),
        ];
        match &request.outcome {
            LineOutcome::Hit(address) => {
                spans.push(Span::styled("→ ", self.dim()));
                spans.push(Span::styled(address.clone(), self.fg(Color::Green)));
            }
            LineOutcome::Miss(reason) => {
                spans.push(Span::styled(
                    "MISS",
                    self.style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                ));
                spans.push(Span::styled(format!(" {reason}"), self.fg(Color::Red)));
            }
            LineOutcome::Passthrough { upstream, failed } => {
                let upstream = upstream.as_deref().unwrap_or("<no target>");
                let failed = if *failed { " (failed)" } else { "" };
                spans.push(Span::styled(
                    format!("→ upstream {upstream}{failed}"),
                    self.fg(Color::Yellow),
                ));
            }
        }
        spans.push(Span::raw(format!(" {}", request.status)));
        spans.push(Span::styled(
            format!(" {}ms", request.duration.as_millis()),
            self.dim(),
        ));
        if request.session != self.session {
            spans.push(Span::styled(
                format!(" [sess:{}]", request.session),
                self.fg(Color::Magenta),
            ));
        }
        Line::from(spans)
    }
}

/// `text` cut to `width` characters, ending in `…` when cut.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The line's text without styles, trailing padding trimmed: what a copy puts on the clipboard.
pub fn plain(line: &Line) -> String {
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    text.trim_end().to_owned()
}

/// `HH:MM:SS` in the system time zone.
pub fn local_clock(at: SystemTime) -> String {
    jiff::Timestamp::try_from(at)
        .map(|at| {
            at.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| "--:--:--".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_paths_end_in_an_ellipsis() {
        assert_eq!(truncate("/users/123", 10), "/users/123");
        assert_eq!(truncate("/users/1234", 10), "/users/12…");
    }

    #[test]
    fn the_path_column_grows_with_the_pane_within_bounds() {
        let mut log = Log::default();
        assert_eq!(log.columns(40).path, PATH_MIN);
        assert_eq!(log.columns(200).path, PATH_MAX);
        log.push(Entry::Info("x".into()));
        assert_eq!(log.columns(100).id, 2);
    }

    #[test]
    fn the_local_clock_shows_hours_minutes_and_seconds() {
        let time = local_clock(SystemTime::now());
        let digits: Vec<&str> = time.split(':').collect();
        assert_eq!(digits.len(), 3, "{time}");
        assert!(
            digits
                .iter()
                .all(|part| part.len() == 2 && part.parse::<u8>().is_ok())
        );
    }
}
