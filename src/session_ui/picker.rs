use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Terminal;

use crate::models::SessionSummary;
use crate::theme::theme;
use crate::util::format_duration_ms;

use chrono::{Local, TimeZone};

struct PickerApp {
    sessions: Vec<SessionSummary>,
    list_state: ListState,
}

impl PickerApp {
    fn new(sessions: Vec<SessionSummary>) -> Self {
        let mut list_state = ListState::default();
        if !sessions.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            sessions,
            list_state,
        }
    }

    fn next(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let i = self
            .list_state
            .selected()
            .map_or(0, |i| (i + 1) % self.sessions.len());
        self.list_state.select(Some(i));
    }

    fn prev(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        let i = self.list_state.selected().map_or(0, |i| {
            if i > 0 {
                i - 1
            } else {
                self.sessions.len() - 1
            }
        });
        self.list_state.select(Some(i));
    }
}

#[allow(clippy::too_many_lines)]
pub fn run_session_picker<B: Backend>(
    terminal: &mut Terminal<B>,
    sessions: Vec<SessionSummary>,
) -> io::Result<Option<String>>
where
    io::Error: From<B::Error>,
{
    let mut app = PickerApp::new(sessions);

    loop {
        terminal.draw(|f| {
            let t = theme();
            let size = f.area();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(5), Constraint::Length(2)])
                .split(size);

            let items: Vec<ListItem> = app
                .sessions
                .iter()
                .map(|s| {
                    let time = Local
                        .timestamp_millis_opt(s.created_at)
                        .single()
                        .map_or_else(
                            || "????-??-?? ??:??".into(),
                            |dt| dt.format("%Y-%m-%d %H:%M").to_string(),
                        );

                    let tag_str = s
                        .tag_name
                        .as_deref()
                        .map_or_else(|| "—".to_string(), std::string::ToString::to_string);

                    #[allow(clippy::cast_precision_loss)]
                    let rate = if s.cmd_count > 0 {
                        s.success_count as f64 / s.cmd_count as f64 * 100.0
                    } else {
                        0.0
                    };

                    let duration = if s.last_cmd_at > s.first_cmd_at {
                        format_duration_ms(s.last_cmd_at - s.first_cmd_at)
                    } else {
                        "—".into()
                    };

                    let id_short: String = s.id.chars().take(8).collect();

                    Line::from(vec![
                        Span::styled(format!("  {time}  "), Style::default().fg(t.text_muted)),
                        Span::styled(
                            format!("{id_short}  "),
                            Style::default().fg(t.info).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("{:<12}", s.hostname), Style::default().fg(t.text)),
                        Span::styled(format!("{tag_str:<10}"), Style::default().fg(t.primary)),
                        Span::styled(
                            format!("{:>4} cmds  ", s.cmd_count),
                            Style::default().fg(t.text_secondary),
                        ),
                        Span::styled(
                            format!("{rate:>3.0}%  "),
                            if rate >= 90.0 {
                                Style::default().fg(t.success)
                            } else if rate >= 70.0 {
                                Style::default().fg(t.warning)
                            } else {
                                Style::default().fg(t.error)
                            },
                        ),
                        Span::styled(format!("{duration:>7}"), Style::default().fg(t.text_muted)),
                    ])
                })
                .map(ListItem::new)
                .collect();

            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(t.border_focus))
                        .title(Span::styled(
                            format!(" Sessions ({}) ", app.sessions.len()),
                            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
                        )),
                )
                .highlight_style(
                    Style::default()
                        .bg(t.selection_bg)
                        .fg(t.selection_fg)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol(" > ");

            f.render_stateful_widget(list, chunks[0], &mut app.list_state);

            let footer = Paragraph::new(Line::from(vec![
                Span::styled(" ↑↓", Style::default().fg(t.info)),
                Span::styled(" Navigate  ", Style::default().fg(t.text_muted)),
                Span::styled("Enter", Style::default().fg(t.success)),
                Span::styled(" Select  ", Style::default().fg(t.text_muted)),
                Span::styled("q", Style::default().fg(t.error)),
                Span::styled(" Quit", Style::default().fg(t.text_muted)),
            ]));
            f.render_widget(footer, chunks[1]);
        })?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                KeyCode::Enter => {
                    return Ok(app
                        .list_state
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
