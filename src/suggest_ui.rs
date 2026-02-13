use crate::models::AliasSuggestion;
use crate::theme::theme;
use crossterm::event::{self, Event, KeyCode};
use ratatui::{
    backend::Backend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};
use std::io;

#[derive(PartialEq)]
enum InputMode {
    Normal,
    EditingName,
}

struct AppState {
    suggestions: Vec<AliasSuggestion>,
    selected_idx: usize,
    list_state: ListState,
    input_mode: InputMode,
    edit_buffer: String,
    skipped: Vec<String>,
}

impl AppState {
    fn new(suggestions: Vec<AliasSuggestion>, skipped: Vec<String>) -> Self {
        let mut list_state = ListState::default();
        if !suggestions.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            suggestions,
            selected_idx: 0,
            list_state,
            input_mode: InputMode::Normal,
            edit_buffer: String::new(),
            skipped,
        }
    }

    fn next(&mut self) {
        if self.suggestions.is_empty() {
            return;
        }
        self.selected_idx = (self.selected_idx + 1) % self.suggestions.len();
        self.list_state.select(Some(self.selected_idx));
    }

    fn prev(&mut self) {
        if self.suggestions.is_empty() {
            return;
        }
        if self.selected_idx > 0 {
            self.selected_idx -= 1;
        } else {
            self.selected_idx = self.suggestions.len() - 1;
        }
        self.list_state.select(Some(self.selected_idx));
    }

    fn toggle_selected(&mut self) {
        if let Some(s) = self.suggestions.get_mut(self.selected_idx) {
            s.selected = !s.selected;
        }
    }

    fn select_all(&mut self) {
        for s in &mut self.suggestions {
            s.selected = true;
        }
    }

    fn deselect_all(&mut self) {
        for s in &mut self.suggestions {
            s.selected = false;
        }
    }

    /// Returns Some(selected suggestions) on confirm, None on quit.
    fn handle_input(&mut self, key: event::KeyEvent) -> Option<bool> {
        match self.input_mode {
            InputMode::Normal => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Some(false),
                KeyCode::Enter => return Some(true),
                KeyCode::Down | KeyCode::Char('j') => self.next(),
                KeyCode::Up | KeyCode::Char('k') => self.prev(),
                KeyCode::Char(' ') => self.toggle_selected(),
                KeyCode::Char('a') => self.select_all(),
                KeyCode::Char('n') => self.deselect_all(),
                KeyCode::Char('e') => {
                    if let Some(s) = self.suggestions.get(self.selected_idx) {
                        self.edit_buffer = s.name.clone();
                        self.input_mode = InputMode::EditingName;
                    }
                }
                _ => {}
            },
            InputMode::EditingName => match key.code {
                KeyCode::Enter => {
                    if !self.edit_buffer.is_empty() {
                        if let Some(s) = self.suggestions.get_mut(self.selected_idx) {
                            s.name.clone_from(&self.edit_buffer);
                        }
                    }
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Backspace => {
                    self.edit_buffer.pop();
                }
                KeyCode::Char(c) => {
                    // Only allow valid alias name chars
                    if c.is_alphanumeric() || c == '_' || c == '-' {
                        self.edit_buffer.push(c);
                    }
                }
                _ => {}
            },
        }
        None
    }
}

pub fn run_suggest_ui<B: Backend>(
    terminal: &mut Terminal<B>,
    suggestions: Vec<AliasSuggestion>,
    skipped: Vec<String>,
) -> io::Result<Option<Vec<AliasSuggestion>>> {
    let mut app = AppState::new(suggestions, skipped);

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if let Event::Key(key) = event::read()? {
            if let Some(confirmed) = app.handle_input(key) {
                if confirmed {
                    let selected: Vec<AliasSuggestion> =
                        app.suggestions.into_iter().filter(|s| s.selected).collect();
                    return Ok(Some(selected));
                }
                return Ok(None);
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn ui(f: &mut ratatui::Frame, app: &mut AppState) {
    let t = theme();
    let size = f.area();

    // Layout: suggestions list, skipped section, footer
    let has_skipped = !app.skipped.is_empty();
    let skipped_height = if has_skipped { 3 } else { 0 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),                 // suggestions list
            Constraint::Length(skipped_height), // skipped section
            Constraint::Length(2),              // footer
        ])
        .split(size);

    // ── Suggestions list ──
    let items: Vec<ListItem> = app
        .suggestions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let is_current = i == app.selected_idx;
            let is_editing = is_current && app.input_mode == InputMode::EditingName;

            let checkbox = if s.selected {
                Span::styled("[x] ", Style::default().fg(t.success))
            } else {
                Span::styled("[ ] ", Style::default().fg(t.text_muted))
            };

            let name_text = if is_editing {
                format!("{}_", app.edit_buffer)
            } else {
                s.name.clone()
            };

            let name_style = if is_editing {
                Style::default()
                    .fg(Color::Black)
                    .bg(t.warning)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.info).add_modifier(Modifier::BOLD)
            };

            // Pad name to 8 chars for alignment
            let name_padded = format!("{name_text:<8}");
            let name_span = Span::styled(name_padded, name_style);

            // Command text — truncate if needed
            let max_cmd_len = size.width.saturating_sub(40) as usize;
            let cmd_display = if s.command.len() > max_cmd_len {
                format!("{}...", &s.command[..max_cmd_len.saturating_sub(3)])
            } else {
                s.command.clone()
            };
            let cmd_span = Span::styled(cmd_display, Style::default().fg(t.text));

            // Count + dir diversity
            let count_str = format!("  {} uses", s.count);
            let count_span = Span::styled(count_str, Style::default().fg(t.text_muted));

            let mut spans = vec![
                Span::raw("  "),
                checkbox,
                name_span,
                Span::raw("  "),
                cmd_span,
                count_span,
            ];

            if s.dir_count > 1 {
                let dir_color = if s.dir_count >= 5 {
                    t.success
                } else {
                    t.text_secondary
                };
                spans.push(Span::styled(
                    format!("  {} dirs", s.dir_count),
                    Style::default().fg(dir_color),
                ));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let selected_count = app.suggestions.iter().filter(|s| s.selected).count();
    let title = format!(
        " Alias Suggestions ({}/{} selected) ",
        selected_count,
        app.suggestions.len()
    );

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border_focus))
                .title(Span::styled(
                    title,
                    Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
                )),
        )
        .highlight_style(Style::default().bg(t.selection_bg).fg(t.selection_fg));

    f.render_stateful_widget(list, chunks[0], &mut app.list_state);

    // ── Skipped section ──
    if has_skipped {
        let skipped_text = app
            .skipped
            .iter()
            .map(|s| format!(" {s} "))
            .collect::<Vec<_>>()
            .join("  ");
        let skipped_para = Paragraph::new(Line::from(vec![
            Span::styled("  Already aliased: ", Style::default().fg(t.text_muted)),
            Span::styled(skipped_text, Style::default().fg(t.text_secondary)),
        ]))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(t.border)),
        );
        f.render_widget(skipped_para, chunks[1]);
    }

    // ── Footer ──
    let footer_spans = if app.input_mode == InputMode::EditingName {
        vec![
            Span::styled(" Type ", Style::default().fg(t.text_muted)),
            Span::styled("alias name", Style::default().fg(t.info)),
            Span::styled("  Enter ", Style::default().fg(t.text_muted)),
            Span::styled("Save", Style::default().fg(t.text)),
            Span::styled("  Esc ", Style::default().fg(t.text_muted)),
            Span::styled("Cancel", Style::default().fg(t.text)),
        ]
    } else {
        vec![
            Span::styled(" \u{2191}\u{2193}", Style::default().fg(t.info)),
            Span::styled(" Navigate  ", Style::default().fg(t.text_muted)),
            Span::styled("Space", Style::default().fg(t.info)),
            Span::styled(" Toggle  ", Style::default().fg(t.text_muted)),
            Span::styled("e", Style::default().fg(t.info)),
            Span::styled(" Edit name  ", Style::default().fg(t.text_muted)),
            Span::styled("a", Style::default().fg(t.info)),
            Span::styled("/", Style::default().fg(t.text_muted)),
            Span::styled("n", Style::default().fg(t.info)),
            Span::styled(" All/None  ", Style::default().fg(t.text_muted)),
            Span::styled("Enter", Style::default().fg(t.success)),
            Span::styled(" Confirm  ", Style::default().fg(t.text_muted)),
            Span::styled("q", Style::default().fg(t.error)),
            Span::styled(" Quit", Style::default().fg(t.text_muted)),
        ]
    };

    let footer = Paragraph::new(Line::from(footer_spans)).wrap(Wrap { trim: false });
    f.render_widget(footer, chunks[2]);
}
