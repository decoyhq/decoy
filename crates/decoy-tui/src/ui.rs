//! Drawing: the log pane, the input box and the status bar, top to bottom.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;

use crate::app::{App, overrides};
use crate::log::{Entry, RequestLine};

/// Completion rows shown at once; the popup scrolls past that.
const POPUP_ROWS: usize = 12;
/// Request rows the picker shows at once.
const PICKER_ROWS: usize = 12;
/// The narrowest terminal that fits the detail panel beside the log.
const DETAIL_BESIDE: u16 = 120;

impl App {
    pub fn draw(&mut self, frame: &mut Frame) {
        let [log_area, input_area, status_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let main = log_area;
        // The detail panel sits beside the log on wide terminals and over it on narrow ones.
        let (log_area, detail_area) = match self.detail {
            Some(_) if main.width >= DETAIL_BESIDE => {
                let [log, detail] =
                    Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                        .areas(main);
                (log, Some(detail))
            }
            Some(_) => (main, Some(main)),
            None => (main, None),
        };
        self.log_area = log_area;
        self.detail_area = detail_area;
        self.draw_log(frame, log_area);
        if let Some(area) = detail_area {
            self.draw_detail(frame, area);
        }
        self.draw_input(frame, input_area);
        self.draw_status(frame, status_area);
        match self.picker {
            Some(row) => self.draw_picker(frame, main, row),
            None => self.draw_popup(frame, main),
        }
    }

    fn draw_detail(&mut self, frame: &mut Frame, area: Rect) {
        let theme = self.theme();
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme.dim())
            .title(Line::styled(
                detail.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Line::styled(
                " Esc close · PgUp/PgDn or wheel scroll ",
                theme.dim(),
            ));
        let detail = self.detail.as_mut().expect("checked above");
        let width = area.width.saturating_sub(2);
        let rows = usize::from(area.height.saturating_sub(2));
        let wrap = Wrap { trim: false };
        let heights = match &mut detail.heights {
            Some((wrapped_at, heights)) if *wrapped_at == width => heights,
            slot => {
                let heights = detail
                    .lines
                    .iter()
                    .map(|line| Paragraph::new(line.clone()).wrap(wrap).line_count(width))
                    .collect();
                &mut slot.insert((width, heights)).1
            }
        };
        let total: usize = heights.iter().sum();
        let last_page = u16::try_from(total.saturating_sub(rows)).unwrap_or(u16::MAX);
        let scroll = detail.scroll.min(last_page);
        detail.scroll = scroll;
        // Wrap only the lines in view: skip whole lines above it, then the first one's top rows.
        let mut skip = usize::from(scroll);
        let first = heights
            .iter()
            .position(|&height| {
                let shown = height > skip;
                if !shown {
                    skip -= height;
                }
                shown
            })
            .unwrap_or(heights.len());
        let mut filled = 0;
        let visible: Vec<Line> = detail.lines[first..]
            .iter()
            .zip(&heights[first..])
            .take_while(|(_, height)| {
                let needed = filled < skip + rows;
                filled += **height;
                needed
            })
            .map(|(line, _)| line.clone())
            .collect();
        let paragraph = Paragraph::new(visible).wrap(wrap).block(block);
        frame.render_widget(Clear, area);
        // `skip` is under `scroll`, so it fits.
        frame.render_widget(paragraph.scroll((skip as u16, 0)), area);
    }

    /// Recent requests over the bottom of the log, the newest last, just above the input.
    fn draw_picker(&self, frame: &mut Frame, area: Rect, row: usize) {
        let theme = self.theme();
        let height = (self.records.len().min(PICKER_ROWS) as u16 + 2).min(area.height);
        if height < 3 {
            return;
        }
        let rect = Rect::new(area.x, area.bottom() - height, area.width, height);
        let rows = usize::from(height - 2);
        // Rows counted from the newest; show a window that contains the highlighted one.
        let first = row.saturating_sub(rows - 1);
        let columns = self.log.columns(rect.width.saturating_sub(2));
        let mut lines: Vec<Line> = self
            .records
            .iter()
            .rev()
            .enumerate()
            .skip(first)
            .take(rows)
            .map(|(index, record)| {
                let line = theme.line(&Entry::Request(RequestLine::from(&**record)), columns);
                if index == row {
                    line.patch_style(Style::default().add_modifier(Modifier::REVERSED))
                } else {
                    line
                }
            })
            .collect();
        lines.reverse();
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(theme.fg(Color::Cyan))
                    .title(format!(" recent requests ({}) ", self.records.len()))
                    .title_bottom(Line::styled(
                        " ↑↓ move · Enter details · Esc close ",
                        theme.dim(),
                    )),
            ),
            rect,
        );
    }

    /// The completion popup, over the bottom of the log just above the input.
    fn draw_popup(&self, frame: &mut Frame, area: Rect) {
        let items = self.completions();
        if items.is_empty() || area.height < 3 {
            return;
        }
        let theme = self.theme();
        let label_width = items
            .iter()
            .map(|item| item.label.chars().count())
            .max()
            .unwrap_or(0);
        let about_width = items
            .iter()
            .map(|item| item.about.chars().count())
            .max()
            .unwrap_or(0);
        let height = (items.len().min(POPUP_ROWS) as u16 + 2).min(area.height);
        let width = ((label_width + about_width + 6) as u16).min(area.width);
        let rect = Rect::new(area.x, area.bottom() - height, width, height);
        let rows = usize::from(height - 2);
        let selected = self.popup.min(items.len() - 1);
        let first = selected.saturating_sub(rows - 1);
        let lines: Vec<Line> = items
            .iter()
            .enumerate()
            .skip(first)
            .take(rows)
            .map(|(index, item)| {
                let style = if index == selected {
                    theme.style(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::REVERSED),
                    )
                } else {
                    Style::default()
                };
                Line::from(vec![
                    Span::styled(format!(" {:<label_width$} ", item.label), style),
                    Span::styled(format!(" {}", item.about), theme.dim()),
                ])
            })
            .collect();
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(theme.dim()),
            ),
            rect,
        );
    }

    fn draw_log(&mut self, frame: &mut Frame, area: Rect) {
        self.rows.clear();
        if area.height == 0 || self.log.is_empty() {
            return;
        }
        let theme = self.theme();
        let columns = self.log.columns(area.width);
        let paragraph = |index: usize| {
            let entry = self.log.get(index).expect("index is in the log");
            Paragraph::new(theme.line(entry, columns)).wrap(Wrap { trim: false })
        };
        let height = |paragraph: &Paragraph| paragraph.line_count(area.width).max(1) as u16;

        // Never scroll back further than a pane that starts at the oldest entry.
        let mut filled: u16 = 0;
        let mut max_scroll = 0;
        for index in 0..self.log.len() {
            filled = filled.saturating_add(height(&paragraph(index)));
            if filled >= area.height {
                max_scroll = self.log.len() - 1 - index;
                break;
            }
        }
        let scroll = self.scroll.min(max_scroll);

        let selected = self.selected.map(|(a, b)| (a.min(b), a.max(b)));
        let end = self.log.len() - scroll;
        // Walk up from the newest visible entry until the pane is full.
        let mut picked = Vec::new();
        let mut used: u16 = 0;
        for index in (0..end).rev() {
            if used >= area.height {
                break;
            }
            let mut paragraph = paragraph(index);
            if selected.is_some_and(|(first, last)| (first..=last).contains(&index)) {
                paragraph = paragraph.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            let h = height(&paragraph);
            used = used.saturating_add(h);
            picked.push((index, paragraph, h));
        }
        // A short log sits at the top, like a fresh transcript; a full pane is bottom-aligned.
        let mut y = area.y + used.min(area.height);
        let mut rows = Vec::new();
        for (index, paragraph, h) in picked {
            let top = y.saturating_sub(h).max(area.y);
            let rect = Rect::new(area.x, top, area.width, y - top);
            // An entry cut by the top edge shows its last rows.
            frame.render_widget(paragraph.scroll((h - rect.height, 0)), rect);
            rows.push((rect, index));
            y = top;
        }
        self.rows = rows;
        if scroll != self.scroll {
            self.scroll = scroll;
            if scroll == 0 {
                self.unseen = 0;
            }
        }

        if self.scroll > 0 {
            let text = if self.unseen > 0 {
                format!(" ↓ {} new · End to follow ", self.unseen)
            } else {
                " scrolled back · End to follow ".to_owned()
            };
            let width = (text.chars().count() as u16).min(area.width);
            let badge = Rect::new(
                area.right().saturating_sub(width + 1).max(area.x),
                area.bottom() - 1,
                width,
                1,
            );
            frame.render_widget(Clear, badge);
            frame.render_widget(
                Line::styled(
                    text,
                    self.theme()
                        .style(Style::default().fg(Color::Black).bg(Color::Yellow))
                        .add_modifier(Modifier::REVERSED),
                ),
                badge,
            );
        }
    }

    fn draw_input(&self, frame: &mut Frame, area: Rect) {
        let theme = self.theme();
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme.fg(Color::Cyan));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let prompt = Span::styled(
            "› ",
            theme.style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        );
        // Show the tail that fits beside the prompt, keeping a cell for the cursor.
        let room = usize::from(inner.width).saturating_sub(prompt.width() + 1);
        // Whole graphemes, so `👍🏽` isn't split into a lone modifier.
        let mut start = self.input.len();
        let mut used = 0;
        for (index, grapheme) in self.input.grapheme_indices(true).rev() {
            used += Span::raw(grapheme).width();
            if used > room {
                break;
            }
            start = index;
        }
        let tail = Span::raw(&self.input[start..]);
        let cursor = Line::from(vec![prompt.clone(), tail.clone()]).width();
        let text = if self.input.is_empty() {
            Span::styled("type / for commands · Ctrl+O recent requests", theme.dim())
        } else {
            tail
        };
        frame.render_widget(Line::from(vec![prompt, text]), inner);
        if inner.width > 0 {
            let x = inner
                .x
                .saturating_add(u16::try_from(cursor).unwrap_or(u16::MAX));
            frame.set_cursor_position(Position::new(x.min(inner.right() - 1), inner.y));
        }
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let theme = self.theme();
        let line = match &self.hint {
            Some(hint) => Line::styled(format!(" {hint}"), theme.fg(Color::Yellow)),
            None => {
                let selection = self.selection();
                Line::styled(
                    format!(
                        " {} · session {} · collection {} · {}",
                        self.listen,
                        self.session,
                        selection.collection,
                        overrides(selection.overrides.len()),
                    ),
                    theme.dim(),
                )
            }
        };
        frame.render_widget(line, area);
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
    use decoy_runtime::events::{Event as RuntimeEvent, Level, LogEvent, SessionChange};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use crate::app::tests::*;
    use crate::app::{App, Effect, Event};

    fn render(app: &mut App, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The screen as text; runs of non-default style are wrapped as `[fg bg modifiers]text[/]`.
    fn styled(buffer: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buffer.area.height {
            let mut current = String::new();
            let mut row = String::new();
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                let mut tags = Vec::new();
                if cell.fg != ratatui::style::Color::Reset {
                    tags.push(format!("{:?}", cell.fg));
                }
                if cell.bg != ratatui::style::Color::Reset {
                    tags.push(format!("bg:{:?}", cell.bg));
                }
                if !cell.modifier.is_empty() {
                    tags.push(format!("{:?}", cell.modifier));
                }
                let tag = tags.join(" ");
                if tag != current {
                    if !current.is_empty() {
                        row.push_str("[/]");
                    }
                    if !tag.is_empty() {
                        row.push_str(&format!("[{tag}]"));
                    }
                    current = tag;
                }
                row.push_str(cell.symbol());
            }
            if !current.is_empty() {
                row.push_str("[/]");
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        out
    }

    fn plain(buffer: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buffer.area.height {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            out.push_str(row.trim_end());
            out.push('\n');
        }
        out
    }

    fn mixed(app: &mut App) {
        app.info("decoy 0.2.0 · listening on http://127.0.0.1:4010");
        app.handle_event(hit(7));
        app.handle_event(miss(8));
        app.handle_event(passthrough(9));
        app.handle_event(from_session(10, "e2e-worker-1"));
        app.handle_event(session_event(
            "e2e-worker-1",
            SessionChange::UseCollection,
            &[],
        ));
        app.handle_event(Event::Runtime(RuntimeEvent::Log(LogEvent {
            level: Level::Warn,
            message: "predicate `${ abs(path) }` failed, treated as false (logged once)".into(),
        })));
        typed(app, "hello");
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
    }

    #[test]
    fn empty_state() {
        insta::assert_snapshot!(plain(&render(&mut app(), 80, 8)));
    }

    #[test]
    fn hit_miss_passthrough_session_and_system_lines() {
        let mut app = app();
        mixed(&mut app);
        insta::assert_snapshot!(styled(&render(&mut app, 120, 16)));
    }

    #[test]
    fn no_color_keeps_only_modifiers() {
        let mut app = app();
        app.no_color = true;
        mixed(&mut app);
        insta::assert_snapshot!(styled(&render(&mut app, 120, 16)));
    }

    #[test]
    fn narrow_width_wraps_and_keeps_the_path_column_at_its_minimum() {
        let mut app = app();
        mixed(&mut app);
        insta::assert_snapshot!(plain(&render(&mut app, 60, 20)));
    }

    #[test]
    fn ids_stay_right_aligned_as_they_grow() {
        let mut app = app();
        app.handle_event(hit(9));
        app.handle_event(hit(10));
        app.handle_event(hit(1000));
        insta::assert_snapshot!(plain(&render(&mut app, 100, 7)));
    }

    #[test]
    fn scrolled_back_shows_the_new_lines_badge() {
        let mut app = app();
        for id in 1..=20 {
            app.handle_event(hit(id));
        }
        render(&mut app, 100, 10);
        app.handle_event(key(KeyCode::PageUp, KeyModifiers::NONE));
        app.handle_event(hit(21));
        app.handle_event(hit(22));
        insta::assert_snapshot!(plain(&render(&mut app, 100, 10)));
    }

    #[test]
    fn scrolling_stops_at_the_oldest_line() {
        let mut app = app();
        for id in 1..=8 {
            app.handle_event(hit(id));
        }
        render(&mut app, 100, 10);
        for _ in 0..5 {
            app.handle_event(key(KeyCode::PageUp, KeyModifiers::NONE));
        }
        let screen = plain(&render(&mut app, 100, 10));
        assert!(screen.starts_with("#1 "), "{screen}");
        assert_eq!(app.scroll, 2);
    }

    #[test]
    fn dragging_selects_lines_and_copies_them_on_release() {
        let mut app = app();
        for id in 1..=3 {
            app.handle_event(hit(id));
        }
        render(&mut app, 100, 10);
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 5, 0));
        app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 5, 1));
        let effects = app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 5, 1));
        let [Effect::Copy(text)] = effects.as_slice() else {
            panic!("{effects:?}")
        };
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[0].starts_with("#1 12:01:03 GET     /users/123 "),
            "{text}"
        );
        assert!(lines[1].starts_with("#2 "), "{text}");
        assert!(
            lines[1].ends_with("users-by-id:user-123:success 200 4ms"),
            "{text}"
        );
        assert_eq!(app.selected, Some((0, 1)));
        assert_eq!(app.hint.as_deref(), Some("copied 2 lines"));

        // With a selection, Ctrl+C copies instead of arming quit, and Esc clears it.
        assert!(matches!(
            app.handle_event(ctrl('c')).as_slice(),
            [Effect::Copy(_)]
        ));
        assert_eq!(app.selected, None);
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 5, 2));
        app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 5, 2));
        app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 5, 2));
        assert_eq!(app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE)), []);
        assert_eq!(app.selected, None);
        assert_eq!(app.handle_event(ctrl('c')), []);
    }

    #[test]
    fn a_ctrl_c_that_copies_disarms_quitting() {
        let mut app = app();
        app.handle_event(hit(1));
        render(&mut app, 100, 10);
        app.handle_event(ctrl('c'));
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 5, 0));
        app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 5, 0));
        app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 5, 0));
        assert!(matches!(
            app.handle_event(ctrl('c')).as_slice(),
            [Effect::Copy(_)]
        ));
        assert_eq!(app.handle_event(ctrl('c')), []);
    }

    #[test]
    fn enter_with_a_selection_copies_and_follows() {
        let mut app = app();
        for id in 1..=20 {
            app.handle_event(hit(id));
        }
        render(&mut app, 100, 10);
        app.handle_event(key(KeyCode::PageUp, KeyModifiers::NONE));
        render(&mut app, 100, 10);
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 5, 0));
        app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 5, 0));
        app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 5, 0));
        let effects = app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(effects.as_slice(), [Effect::Copy(text)] if text.starts_with("#10 ")),
            "{effects:?}"
        );
        assert_eq!((app.scroll, app.selected), (0, None));
    }

    #[test]
    fn a_click_without_a_drag_copies_nothing() {
        let mut app = app();
        app.handle_event(hit(1));
        render(&mut app, 100, 10);
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 5, 0));
        let effects = app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 5, 0));
        assert_eq!((effects, app.selected), (vec![], None));
    }

    fn popup(input: &str) -> String {
        let mut app = app();
        typed(&mut app, input);
        plain(&render(&mut app, 80, 14))
    }

    #[test]
    fn popup_lists_commands() {
        insta::assert_snapshot!(popup("/"));
    }

    #[test]
    fn popup_at_the_route_segment() {
        insta::assert_snapshot!(popup("/route "));
    }

    #[test]
    fn popup_at_the_case_segment() {
        insta::assert_snapshot!(popup("/route users-by-id:"));
    }

    #[test]
    fn popup_at_the_behavior_segment() {
        let mut app = app();
        typed(&mut app, "/route users-by-id:user-1:");
        app.handle_event(key(KeyCode::Down, KeyModifiers::NONE));
        insta::assert_snapshot!(styled(&render(&mut app, 80, 10)));
    }

    fn after(commands: &[&str], setup: impl FnOnce(&mut App)) -> String {
        let mut app = app();
        setup(&mut app);
        for command in commands {
            typed(&mut app, command);
            app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
            app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        }
        plain(&render(&mut app, 100, 20))
    }

    #[test]
    fn status_lists_the_collection_and_every_override() {
        insta::assert_snapshot!(after(&["/status"], |app| {
            app.handle_event(session_event(
                "default",
                SessionChange::UseRoute,
                &["users-by-id:user-1:missing", "orders:any:ok"],
            ));
        }));
    }

    #[test]
    fn sessions_lists_who_holds_a_selection() {
        insta::assert_snapshot!(after(&["/session fresh", "/sessions"], |app| {
            app.handle_event(session_event("default", SessionChange::UseCollection, &[]));
            app.handle_event(session_event(
                "e2e-1",
                SessionChange::UseRoute,
                &["orders:any:ok"],
            ));
        }));
    }

    #[test]
    fn help_lists_commands_and_keys() {
        insta::assert_snapshot!(after(&["/help"], |_| {}));
    }

    /// The id of the request the detail panel shows.
    fn open_id(app: &App) -> Option<u64> {
        let title = &app.detail.as_ref()?.title;
        title
            .trim_start()
            .strip_prefix('#')?
            .split(' ')
            .next()?
            .parse()
            .ok()
    }

    fn three_requests() -> App {
        let mut app = app();
        app.handle_event(hit(1));
        app.handle_event(miss(2));
        app.handle_event(passthrough(3));
        app
    }

    #[test]
    fn show_a_click_and_the_picker_open_the_same_detail() {
        let mut app = three_requests();
        typed(&mut app, "/show 2");
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(open_id(&app), Some(2));
        let shown = plain(&render(&mut app, 100, 30));
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.detail.is_none());

        render(&mut app, 100, 30);
        // Row 0 shows `› /show 2`; the requests are rows 0-2 before it was echoed.
        let row = app.rows.iter().find(|(_, index)| *index == 1).unwrap().0.y;
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 5, row));
        app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 5, row));
        assert_eq!(open_id(&app), Some(2));
        assert_eq!(plain(&render(&mut app, 100, 30)), shown);
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));

        app.handle_event(ctrl('o'));
        app.handle_event(key(KeyCode::Up, KeyModifiers::NONE));
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!((app.picker, open_id(&app)), (None, Some(2)));
        assert_eq!(plain(&render(&mut app, 100, 30)), shown);
        insta::assert_snapshot!(shown);
    }

    #[test]
    fn the_picker_lists_recent_requests_newest_last() {
        let mut app = three_requests();
        app.handle_event(ctrl('o'));
        app.handle_event(key(KeyCode::Up, KeyModifiers::NONE));
        let screen = styled(&render(&mut app, 100, 12));
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.picker.is_none() && app.detail.is_none());
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn the_picker_says_when_there_is_nothing_to_pick() {
        let mut app = app();
        app.handle_event(ctrl('o'));
        assert_eq!(app.picker, None);
        assert_eq!(app.hint.as_deref(), Some("no requests yet"));
    }

    #[test]
    fn the_detail_sits_beside_the_log_when_wide() {
        let mut app = three_requests();
        typed(&mut app, "/show 1");
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        insta::assert_snapshot!(plain(&render(&mut app, 140, 24)));
    }

    #[test]
    fn page_down_scrolls_the_detail_and_stops_at_its_end() {
        let mut app = three_requests();
        typed(&mut app, "/show 1");
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        render(&mut app, 80, 16);
        for _ in 0..10 {
            app.handle_event(key(KeyCode::PageDown, KeyModifiers::NONE));
            render(&mut app, 80, 16);
        }
        let screen = plain(&render(&mut app, 80, 16));
        assert!(screen.contains("duration"), "{screen}");
        assert!(app.detail.as_ref().unwrap().scroll > 0);
        app.handle_event(mouse(MouseEventKind::ScrollUp, 10, 3));
        assert!(app.detail.as_ref().unwrap().scroll > 0);
        assert_eq!(
            app.scroll, 0,
            "the wheel over the panel leaves the log alone"
        );
    }

    #[test]
    fn the_detail_shows_the_same_rows_as_wrapping_the_whole_body() {
        use ratatui::text::Line;
        use ratatui::widgets::{Paragraph, Wrap};

        let lines: Vec<Line<'static>> = (0..40)
            .map(|i| match i % 4 {
                0 => Line::default(),
                1 => Line::raw(format!("line {i} ").repeat(i)),
                2 => Line::raw(format!("short {i}")),
                _ => Line::raw("x".repeat(i * 7)),
            })
            .collect();
        let mut app = app();
        app.detail = Some(crate::app::Detail {
            title: String::new(),
            lines: lines.clone(),
            scroll: 0,
            heights: None,
        });
        // Narrow, then wide, then narrow again: the panel re-wraps when its width changes.
        for (width, height) in [(60, 20), (140, 24), (60, 20)] {
            for scroll in [0, 1, 7, 33, 100, u16::MAX] {
                app.detail.as_mut().unwrap().scroll = scroll;
                let screen = render(&mut app, width, height);
                let area = app.detail_area.unwrap();
                let inner = ratatui::layout::Rect::new(0, 0, area.width - 2, area.height - 2);
                let whole = Paragraph::new(lines.clone()).wrap(Wrap { trim: false });
                let last_page = (whole.line_count(inner.width) as u16).saturating_sub(inner.height);
                let expected_scroll = scroll.min(last_page);
                assert_eq!(app.detail.as_ref().unwrap().scroll, expected_scroll);
                let mut expected = Buffer::empty(inner);
                ratatui::widgets::Widget::render(
                    whole.scroll((expected_scroll, 0)),
                    inner,
                    &mut expected,
                );
                for y in 0..inner.height {
                    for x in 0..inner.width {
                        assert_eq!(
                            screen[(area.x + 1 + x, area.y + 1 + y)].symbol(),
                            expected[(x, y)].symbol(),
                            "width {width}, scroll {scroll}, at ({x}, {y})"
                        );
                    }
                }
            }
        }
    }

    /// The input row's text and the cursor's column.
    fn input_row(input: &str, width: u16) -> (String, u16) {
        let mut app = app();
        app.input = input.to_owned();
        let mut terminal = Terminal::new(TestBackend::new(width, 6)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        assert_eq!(cursor.y, 3);
        let screen = plain(terminal.backend().buffer());
        (screen.lines().nth(3).unwrap().to_owned(), cursor.x)
    }

    #[test]
    fn long_input_shows_its_tail_with_the_cursor_after_it() {
        assert_eq!(
            input_row("/route users-by-id:user-1:missing", 20),
            ("│› :user-1:missing │".to_owned(), 18)
        );
    }

    #[test]
    fn the_cursor_counts_wide_characters_as_two_columns() {
        assert_eq!(input_row("日本語", 20).1, 9);
        // Room for 7 columns after the prompt: three wide characters fit, a fourth does not.
        assert_eq!(input_row("日本語日本語日本語", 12).1, 9);
    }

    #[test]
    fn the_tail_keeps_an_emoji_with_its_modifier() {
        // Room for 15 columns: `👍🏽` (2) and 13 `a` fit; the row must not start with a lone `🏽`. The
        // buffer prints a wide grapheme's second cell as a space.
        let (row, cursor) = input_row(&format!("x👍🏽{}", "a".repeat(13)), 20);
        assert_eq!(
            (row.as_str(), cursor),
            (format!("│› 👍🏽 {} │", "a".repeat(13)).as_str(), 18)
        );
    }

    #[test]
    fn very_long_input_does_not_overflow_the_cursor() {
        // Past u16::MAX columns once the box's border and prompt are added.
        let (row, cursor) = input_row(&"a".repeat(65_534), 20);
        assert_eq!((row.as_str(), cursor), ("│› aaaaaaaaaaaaaaa │", 18));
    }

    #[test]
    fn an_open_detail_outlives_its_record() {
        let mut app = three_requests();
        typed(&mut app, "/show 1");
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        for id in 4..=crate::app::RETAIN as u64 + 4 {
            app.handle_event(hit(id));
        }
        assert!(app.record(1).is_none());
        let screen = plain(&render(&mut app, 100, 20));
        assert!(screen.contains("╭ #1 GET /users/123"), "{screen}");
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.detail.is_none());
    }

    #[test]
    fn the_picker_keeps_its_keys_while_log_lines_are_selected() {
        let mut app = three_requests();
        render(&mut app, 100, 12);
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 5, 0));
        app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), 5, 1));
        app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 5, 1));
        assert!(app.selected.is_some());
        app.handle_event(ctrl('o'));
        assert_eq!(
            app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            []
        );
        assert_eq!(open_id(&app), Some(3));
        assert!(app.picker.is_none());
    }

    #[test]
    fn clicks_do_nothing_while_the_picker_is_open() {
        let mut app = three_requests();
        render(&mut app, 100, 12);
        app.handle_event(ctrl('o'));
        render(&mut app, 100, 12);
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), 5, 1));
        app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), 5, 1));
        assert!(app.detail.is_none());
        assert_eq!(app.picker, Some(0));
    }

    #[test]
    fn show_tells_an_unissued_id_from_an_expired_one() {
        let mut app = three_requests();
        typed(&mut app, "/show 99");
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        let screen = plain(&render(&mut app, 100, 12));
        assert!(screen.contains("#99: no such request"), "{screen}");
    }

    #[test]
    fn an_evicted_request_reads_as_expired() {
        let mut app = app();
        for id in 1..=crate::app::RETAIN as u64 + 1 {
            app.handle_event(hit(id));
        }
        typed(&mut app, "/show #1");
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.detail.is_none());
        let screen = plain(&render(&mut app, 100, 8));
        assert!(screen.contains("#1 expired"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn reload_prints_its_result_and_follows_the_new_default() {
        use decoy_runtime::Reloaded;

        let mut app = app();
        typed(&mut app, "/reload");
        app.handle_event(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            app.handle_event(key(KeyCode::Enter, KeyModifiers::NONE)),
            [Effect::Reload]
        );
        app.handle_event(session_event("e2e-1", SessionChange::Reload, &[]));
        app.handle_event(Event::Reloaded(Ok(Reloaded {
            catalog: std::sync::Arc::new(crate::complete::tests::catalog()),
            default_collection: "calm".into(),
            warnings: vec![
                "listener changes need restart".into(),
                "session e2e-1: override `users-by-id:user-1:missing` dropped, it no longer exists"
                    .into(),
            ],
        })));
        app.handle_event(Event::Reloaded(Err(
            "routes/users.yaml:2: expected a mapping".into(),
        )));
        insta::assert_snapshot!(styled(&render(&mut app, 120, 10)));
    }
}
