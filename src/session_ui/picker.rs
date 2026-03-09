use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Terminal;

use crate::models::SessionSummary;
use crate::theme::theme;
use crate::util::format_duration_ms;

use chrono::{Local, TimeZone};

struct PickerApp {
    sessions: Vec<SessionSummary>,
    table_state: TableState,
}

impl PickerApp {
    fn new(sessions: Vec<SessionSummary>) -> Self {
        let mut table_state = TableState::default();
        if !sessions.is_empty() {
            table_state.select(Some(0));
        }
        Self {
            sessions,
            table_state,
        }
    }

    fn next(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let i = self
            .table_state
            .selected()
            .map_or(0, |i| (i + 1) % self.sessions.len());
        self.table_state.select(Some(i));
    }

    fn prev(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let i = self.table_state.selected().map_or(0, |i| {
            if i > 0 {
                i - 1
            } else {
                self.sessions.len() - 1
            }
        });
        self.table_state.select(Some(i));
    }
}

impl PickerApp {
    #[allow(clippy::cast_precision_loss)]
    fn build_session_row<'a>(s: &SessionSummary, t: &crate::theme::Theme) -> Row<'a> {
        let time = Local
            .timestamp_millis_opt(crate::util::normalize_display_ms(s.created_at))
            .single()
            .map_or_else(
                || "????-??-?? ??:??".into(),
                |dt| dt.format("%Y-%m-%d %H:%M").to_string(),
            );

        let tag_str = s
            .tag_name
            .as_deref()
            .map_or_else(|| "—".to_string(), std::string::ToString::to_string);

        let rate = if s.cmd_count > 0 {
            s.success_count as f64 / s.cmd_count as f64 * 100.0
        } else {
            0.0
        };
        let rate_style = if rate >= 90.0 {
            Style::default().fg(t.success)
        } else if rate >= 70.0 {
            Style::default().fg(t.warning)
        } else {
            Style::default().fg(t.error)
        };

        let duration = if s.last_cmd_at > s.first_cmd_at {
            format_duration_ms(s.last_cmd_at - s.first_cmd_at)
        } else {
            "—".into()
        };

        let id_short: String = s.id.chars().take(8).collect();

        Row::new(vec![
            Cell::from(time).style(Style::default().fg(t.text_muted)),
            Cell::from(id_short).style(Style::default().fg(t.info).add_modifier(Modifier::BOLD)),
            Cell::from(s.hostname.clone()).style(Style::default().fg(t.text)),
            Cell::from(tag_str).style(Style::default().fg(t.primary)),
            Cell::from(format!("{}", s.cmd_count)).style(Style::default().fg(t.text_secondary)),
            Cell::from(format!("{rate:.0}%")).style(rate_style),
            Cell::from(duration).style(Style::default().fg(t.text_muted)),
        ])
    }

    fn render_picker(&mut self, f: &mut ratatui::Frame) {
        let t = theme();
        let size = f.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(2)])
            .split(size);

        let header = Row::new(vec![
            Cell::from("Time"),
            Cell::from("ID"),
            Cell::from("Host"),
            Cell::from("Tag"),
            Cell::from("Cmds"),
            Cell::from("Rate"),
            Cell::from("Duration"),
        ])
        .style(
            Style::default()
                .fg(t.text_secondary)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

        let rows: Vec<Row> = self
            .sessions
            .iter()
            .map(|s| Self::build_session_row(s, t))
            .collect();

        let widths = [
            Constraint::Length(16),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Length(5),
            Constraint::Min(7),
        ];

        let table = Table::new(rows, widths)
            .header(header)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border_focus))
                    .title(Span::styled(
                        format!(" Sessions ({}) ", self.sessions.len()),
                        Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
                    )),
            )
            .row_highlight_style(
                Style::default()
                    .bg(t.selection_bg)
                    .fg(t.selection_fg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(" > ");

        f.render_stateful_widget(table, chunks[0], &mut self.table_state);

        let footer = Paragraph::new(Line::from(vec![
            Span::styled(" ↑↓", Style::default().fg(t.info)),
            Span::styled(" Navigate  ", Style::default().fg(t.text_muted)),
            Span::styled("Enter", Style::default().fg(t.success)),
            Span::styled(" Select  ", Style::default().fg(t.text_muted)),
            Span::styled("q", Style::default().fg(t.error)),
            Span::styled(" Quit", Style::default().fg(t.text_muted)),
        ]));
        f.render_widget(footer, chunks[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_summary(id: &str, cmd_count: i64) -> SessionSummary {
        SessionSummary {
            id: id.to_string(),
            hostname: "test-host".to_string(),
            created_at: 1_700_000_000_000,
            tag_name: None,
            cmd_count,
            success_count: cmd_count,
            first_cmd_at: 1_700_000_000_000,
            last_cmd_at: 1_700_000_060_000,
        }
    }

    #[test]
    fn new_empty_sessions_no_selection() {
        let app = PickerApp::new(vec![]);
        assert!(app.table_state.selected().is_none());
    }

    #[test]
    fn new_with_sessions_selects_first() {
        let app = PickerApp::new(vec![make_summary("s1", 5)]);
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn next_wraps_around() {
        let mut app = PickerApp::new(vec![make_summary("s1", 5), make_summary("s2", 3)]);
        assert_eq!(app.table_state.selected(), Some(0));
        app.next();
        assert_eq!(app.table_state.selected(), Some(1));
        app.next(); // wraps
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn prev_wraps_around() {
        let mut app = PickerApp::new(vec![make_summary("s1", 5), make_summary("s2", 3)]);
        assert_eq!(app.table_state.selected(), Some(0));
        app.prev(); // wraps to last
        assert_eq!(app.table_state.selected(), Some(1));
        app.prev();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn next_on_empty_does_nothing() {
        let mut app = PickerApp::new(vec![]);
        app.next(); // should not panic
        assert!(app.table_state.selected().is_none());
    }

    #[test]
    fn prev_on_empty_does_nothing() {
        let mut app = PickerApp::new(vec![]);
        app.prev(); // should not panic
        assert!(app.table_state.selected().is_none());
    }

    #[test]
    fn next_single_element_stays() {
        let mut app = PickerApp::new(vec![make_summary("s1", 5)]);
        app.next();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn prev_single_element_stays() {
        let mut app = PickerApp::new(vec![make_summary("s1", 5)]);
        app.prev();
        assert_eq!(app.table_state.selected(), Some(0));
    }
}

pub fn run_session_picker<B: Backend>(
    terminal: &mut Terminal<B>,
    sessions: Vec<SessionSummary>,
) -> io::Result<Option<String>>
where
    io::Error: From<B::Error>,
{
    let mut app = PickerApp::new(sessions);

    loop {
        terminal.draw(|f| app.render_picker(f))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                KeyCode::Enter => {
                    return Ok(app
                        .table_state
                        .selected()
                        .and_then(|i| app.sessions.get(i))
                        .map(|s| s.id.clone()));
                }
                KeyCode::Down | KeyCode::Char('j') => app.next(),
                KeyCode::Up | KeyCode::Char('k') => app.prev(),
                _ => {}
            }
        }
    }
}
