//! Drawing: the log pane, the input box and the status bar, top to bottom.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use crate::app::{App, overrides};

/// Completion rows shown at once; the popup scrolls past that.
const POPUP_ROWS: usize = 8;

impl App {
    pub fn draw(&mut self, frame: &mut Frame) {
        let [log_area, input_area, status_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        self.log_area = log_area;
        self.draw_log(frame, log_area);
        self.draw_input(frame, input_area);
        self.draw_status(frame, status_area);
        self.draw_popup(frame, log_area);
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
        let text = if self.input.is_empty() {
            Span::styled("type / for commands", theme.dim())
        } else {
            Span::raw(self.input.clone())
        };
        frame.render_widget(Line::from(vec![prompt, text]), inner);
        if inner.width > 0 {
            let x = inner.x + 2 + self.input.chars().count() as u16;
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
}
