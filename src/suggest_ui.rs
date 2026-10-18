use crate::models::AliasSuggestion;
use crate::theme::theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    backend::Backend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};
use std::collections::HashSet;
use std::io;

/// Longest an alias name being edited may grow, in bytes: a guard against a
/// stray huge paste.
const MAX_FIELD_LEN: usize = 200;

#[derive(Debug, PartialEq)]
enum InputMode {
    Normal,
    EditingName,
    /// "Add N aliases? y/n" — shown before anything is written.
    ConfirmAdd,
}

/// How the suggest screen is being used.
pub struct SuggestOptions {
    /// Alias names already defined — managed by suvadu or in the shell. A
    /// selected suggestion may not take one.
    pub existing: HashSet<String>,
    /// Confirming writes the aliases (`suv aliases add-suggested`), so Enter
    /// asks first.
    pub confirm_add: bool,
}

struct AppState {
    suggestions: Vec<AliasSuggestion>,
    selected_idx: usize,
    list_state: ListState,
    input_mode: InputMode,
    edit_buffer: String,
    skipped: Vec<String>,
    existing: HashSet<String>,
    confirm_add: bool,
    /// Why the last Enter was refused.
    status: Option<String>,
    /// The quit hint's label: "Home" when Suvadu Home opened this screen,
    /// since leaving returns there.
    leave_label: &'static str,
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
            existing: HashSet::new(),
            confirm_add: false,
            status: None,
            leave_label: "Quit",
        }
    }

    const fn next(&mut self) {
        if self.suggestions.is_empty() {
            return;
        }
        self.selected_idx = (self.selected_idx + 1) % self.suggestions.len();
        self.list_state.select(Some(self.selected_idx));
    }

    const fn prev(&mut self) {
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

    fn selected_count(&self) -> usize {
        self.suggestions.iter().filter(|s| s.selected).count()
    }

    /// Why suggestion `idx` cannot be taken as it is named, if it cannot:
    /// the name is already an alias, or another selected suggestion has it.
    fn clash(&self, idx: usize) -> Option<&'static str> {
        let s = self.suggestions.get(idx)?;
        if self.existing.contains(&s.name) {
            Some("exists")
        } else if self
            .suggestions
            .iter()
            .enumerate()
            .any(|(j, other)| j != idx && other.selected && other.name == s.name)
        {
            Some("duplicate")
        } else {
            None
        }
    }

    /// Names of selected suggestions that clash, each once.
    fn selected_clashes(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for (i, s) in self.suggestions.iter().enumerate() {
            if s.selected && self.clash(i).is_some() && !names.contains(&s.name) {
                names.push(s.name.clone());
            }
        }
        names
    }

    /// Enter on the list: refuse while a selected name clashes, ask first
    /// when confirming writes, otherwise confirm.
    fn confirm(&mut self) -> Option<bool> {
        let clashes = self.selected_clashes();
        if !clashes.is_empty() {
            self.status = Some(format!(
                "Already taken: {} — rename (^E) or deselect (Space)",
                clashes.join(", ")
            ));
            return None;
        }
        if self.confirm_add && self.selected_count() > 0 {
            self.input_mode = InputMode::ConfirmAdd;
            return None;
        }
        Some(true)
    }

    /// Returns `Some(true)` on confirm, `Some(false)` on quit.
    fn handle_input(&mut self, key: event::KeyEvent) -> Option<bool> {
        if crate::util::is_interrupt(&key) {
            return Some(false);
        }
        self.status = None;
        match self.input_mode {
            InputMode::ConfirmAdd => match key.code {
                KeyCode::Char('y' | 'Y') => return Some(true),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => self.input_mode = InputMode::Normal,
                _ => {}
            },
            InputMode::Normal if key.modifiers.contains(KeyModifiers::CONTROL) => match key.code {
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
            InputMode::Normal => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Some(false),
                KeyCode::Enter => return self.confirm(),
                KeyCode::Down | KeyCode::Char('j') => self.next(),
                KeyCode::Up | KeyCode::Char('k') => self.prev(),
                KeyCode::Char(' ') => self.toggle_selected(),
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
                KeyCode::Char(c)
                    // Only allow valid alias name chars
                    if (c.is_alphanumeric() || c == '_' || c == '-')
                        && self.edit_buffer.len() + c.len_utf8() <= MAX_FIELD_LEN => {
                        self.edit_buffer.push(c);
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
    options: SuggestOptions,
) -> io::Result<Option<Vec<AliasSuggestion>>>
where
    io::Error: From<B::Error>,
{
    let mut app = AppState::new(suggestions, skipped);
    app.existing = options.existing;
    app.confirm_add = options.confirm_add;
    app.leave_label = crate::home::runner::leave_label();

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
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

fn ui(f: &mut ratatui::Frame, app: &mut AppState) {
    let t = theme();
    let size = f.area();

    // Layout: suggestions list, skipped section, footer
    let has_skipped = !app.skipped.is_empty();
    let skipped_height = if has_skipped { 3 } else { 0 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),              // header
            Constraint::Min(5),                 // suggestions list
            Constraint::Length(skipped_height), // skipped section
            Constraint::Length(2),              // footer
        ])
        .split(size);

    render_header(f, chunks[0], t);
    render_suggestion_list(f, app, chunks[1], size.width, t);

    if has_skipped {
        render_skipped_section(f, app, chunks[2], t);
    }

    render_suggest_footer(f, app, chunks[3], t);
}

fn render_header(f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
    let header_line = Line::from(vec![Span::styled(
        "SUVADU ALIAS SUGGEST",
        Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
    )]);
    f.render_widget(
        Paragraph::new(header_line).alignment(Alignment::Center),
        area,
    );
}

fn render_suggestion_list(
    f: &mut ratatui::Frame,
    app: &mut AppState,
    area: Rect,
    total_width: u16,
    t: &crate::theme::Theme,
) {
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
            let clash = app.clash(i).map(|why| {
                Span::styled(
                    format!(" {why}"),
                    Style::default().fg(t.error).add_modifier(Modifier::BOLD),
                )
            });

            // Command text — truncate and highlight like search
            let max_cmd_len = total_width.saturating_sub(40) as usize;
            let cmd_display = crate::util::truncate_str(&s.command, max_cmd_len, "...");
            let cmd_highlighted = crate::util::highlight_command(&cmd_display, 0);
            let cmd_spans: Vec<Span> = cmd_highlighted
                .lines
                .into_iter()
                .next()
                .map_or_else(Vec::new, |line| line.spans);

            // Count + dir diversity
            let count_str = format!("  {} uses", s.count);
            let count_span = Span::styled(count_str, Style::default().fg(t.text_muted));

            let mut spans = vec![Span::raw("  "), checkbox, name_span];
            spans.extend(clash);
            spans.push(Span::raw("  "));
            spans.extend(cmd_spans);
            spans.push(count_span);

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

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_skipped_section(
    f: &mut ratatui::Frame,
    app: &AppState,
    area: Rect,
    t: &crate::theme::Theme,
) {
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
    f.render_widget(skipped_para, area);
}

fn render_suggest_footer(
    f: &mut ratatui::Frame,
    app: &AppState,
    area: Rect,
    t: &crate::theme::Theme,
) {
    let badge_key = Style::default().bg(t.badge_bg).fg(t.text);
    let badge_label = Style::default().fg(t.text_secondary);

    let mut footer_spans = match app.input_mode {
        InputMode::EditingName => vec![
            Span::styled(" Enter ", badge_key),
            Span::styled(" Save  ", badge_label),
            Span::styled(" Esc ", badge_key),
            Span::styled(" Cancel  ", badge_label),
        ],
        InputMode::ConfirmAdd => {
            let n = app.selected_count();
            vec![
                Span::styled(
                    format!(" Add {n} alias{}? ", if n == 1 { "" } else { "es" }),
                    Style::default().fg(t.warning).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" y ", badge_key),
                Span::styled(" Add  ", badge_label),
                Span::styled(" n/Esc ", badge_key),
                Span::styled(" Back  ", badge_label),
            ]
        }
        InputMode::Normal => vec![
            Span::styled(" q/Esc ", badge_key),
            Span::styled(format!(" {}  ", app.leave_label), badge_label),
            Span::styled(" \u{2191}\u{2193} ", badge_key),
            Span::styled(" Navigate  ", badge_label),
            Span::styled(" Space ", badge_key),
            Span::styled(" Toggle  ", badge_label),
            Span::styled(" ^E ", badge_key),
            Span::styled(" Edit name  ", badge_label),
            Span::styled(" ^A ", badge_key),
            Span::styled(" All  ", badge_label),
            Span::styled(" ^N ", badge_key),
            Span::styled(" None  ", badge_label),
            Span::styled(" Enter ", badge_key),
            Span::styled(" Confirm ", badge_label),
        ],
    };
    if let Some(status) = &app.status {
        footer_spans.push(Span::styled(
            format!(" {status} "),
            Style::default().fg(t.error).add_modifier(Modifier::BOLD),
        ));
    }

    let footer = Paragraph::new(Line::from(footer_spans)).wrap(Wrap { trim: false });
    f.render_widget(footer, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AliasSuggestion;
    use crossterm::event::KeyEvent;

    fn make_suggestions(n: usize) -> Vec<AliasSuggestion> {
        (0..n)
            .map(|i| AliasSuggestion {
                name: format!("alias{i}"),
                command: format!("command{i}"),
                count: i64::try_from(i + 1).unwrap(),
                dir_count: 1,
                selected: false,
            })
            .collect()
    }

    #[test]
    fn new_state_selects_first() {
        let app = AppState::new(make_suggestions(3), vec![]);
        assert_eq!(app.selected_idx, 0);
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn new_state_empty_suggestions() {
        let app = AppState::new(vec![], vec![]);
        assert_eq!(app.selected_idx, 0);
        assert_eq!(app.list_state.selected(), None);
    }

    #[test]
    fn next_wraps_around() {
        let mut app = AppState::new(make_suggestions(3), vec![]);
        app.next();
        assert_eq!(app.selected_idx, 1);
        app.next();
        assert_eq!(app.selected_idx, 2);
        app.next();
        assert_eq!(app.selected_idx, 0); // wraps
    }

    #[test]
    fn prev_wraps_around() {
        let mut app = AppState::new(make_suggestions(3), vec![]);
        app.prev();
        assert_eq!(app.selected_idx, 2); // wraps to last
        app.prev();
        assert_eq!(app.selected_idx, 1);
    }

    #[test]
    fn next_on_empty_is_noop() {
        let mut app = AppState::new(vec![], vec![]);
        app.next();
        assert_eq!(app.selected_idx, 0);
    }

    #[test]
    fn prev_on_empty_is_noop() {
        let mut app = AppState::new(vec![], vec![]);
        app.prev();
        assert_eq!(app.selected_idx, 0);
    }

    #[test]
    fn toggle_selected() {
        let mut app = AppState::new(make_suggestions(2), vec![]);
        assert!(!app.suggestions[0].selected);
        app.toggle_selected();
        assert!(app.suggestions[0].selected);
        app.toggle_selected();
        assert!(!app.suggestions[0].selected);
    }

    #[test]
    fn select_all() {
        let mut app = AppState::new(make_suggestions(3), vec![]);
        app.select_all();
        assert!(app.suggestions.iter().all(|s| s.selected));
    }

    #[test]
    fn deselect_all() {
        let mut app = AppState::new(make_suggestions(3), vec![]);
        app.select_all();
        app.deselect_all();
        assert!(app.suggestions.iter().all(|s| !s.selected));
    }

    #[test]
    fn handle_input_quit_q() {
        let mut app = AppState::new(make_suggestions(1), vec![]);
        let result = app.handle_input(KeyEvent::from(KeyCode::Char('q')));
        assert_eq!(result, Some(false));
    }

    #[test]
    fn handle_input_quit_esc() {
        let mut app = AppState::new(make_suggestions(1), vec![]);
        let result = app.handle_input(KeyEvent::from(KeyCode::Esc));
        assert_eq!(result, Some(false));
    }

    /// The state `suv aliases add-suggested` runs with: every suggestion
    /// selected, writes confirmed first, `existing` already taken.
    fn adding(n: usize, existing: &[&str]) -> AppState {
        let mut suggestions = make_suggestions(n);
        for s in &mut suggestions {
            s.selected = true;
        }
        let mut app = AppState::new(suggestions, vec![]);
        app.existing = existing.iter().map(ToString::to_string).collect();
        app.confirm_add = true;
        app
    }

    #[test]
    fn adding_asks_before_writing_anything() {
        let mut app = adding(2, &[]);
        assert_eq!(app.handle_input(KeyEvent::from(KeyCode::Enter)), None);
        assert_eq!(app.input_mode, InputMode::ConfirmAdd);

        // n and Esc go back to the list rather than adding or quitting.
        assert_eq!(app.handle_input(KeyEvent::from(KeyCode::Char('n'))), None);
        assert_eq!(app.input_mode, InputMode::Normal);
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.handle_input(KeyEvent::from(KeyCode::Esc)), None);
        assert_eq!(app.input_mode, InputMode::Normal);

        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            app.handle_input(KeyEvent::from(KeyCode::Char('y'))),
            Some(true)
        );
    }

    #[test]
    fn the_confirmation_says_how_many_aliases_it_adds() {
        let mut app = adding(3, &[]);
        app.toggle_selected(); // deselect the first
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        let screen = crate::test_utils::render_to_text(160, 20, |f| ui(f, &mut app));
        assert!(screen.contains("Add 2 aliases?"), "{screen}");
    }

    #[test]
    fn a_name_that_already_exists_is_flagged_and_cannot_be_added() {
        let mut app = adding(2, &["alias0"]);
        let screen = crate::test_utils::render_to_text(160, 20, |f| ui(f, &mut app));
        assert!(screen.contains("exists"), "{screen}");

        assert_eq!(app.handle_input(KeyEvent::from(KeyCode::Enter)), None);
        assert_eq!(app.input_mode, InputMode::Normal, "no confirmation offered");
        let status = app.status.clone().unwrap_or_default();
        assert!(status.contains("alias0"), "{status}");

        // Deselecting it clears the way.
        app.toggle_selected();
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::ConfirmAdd);
    }

    #[test]
    fn renaming_onto_an_existing_or_a_duplicate_name_is_refused() {
        let mut app = adding(2, &["gst"]);
        app.handle_input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        app.edit_buffer = "gst".to_string();
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.suggestions[0].name, "gst");
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::Normal);

        // Two suggestions renamed to the same new name collide too.
        let mut app = adding(2, &[]);
        app.suggestions[0].name = "same".to_string();
        app.suggestions[1].name = "same".to_string();
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.status.clone().unwrap_or_default().contains("same"));
    }

    #[test]
    fn opened_from_home_the_quit_hint_says_home() {
        let mut app = AppState::new(make_suggestions(2), vec![]);
        let screen = crate::test_utils::render_to_text(160, 20, |f| ui(f, &mut app));
        assert!(screen.contains("q/Esc  Quit"), "{screen}");

        app.leave_label = "Home";
        let screen = crate::test_utils::render_to_text(160, 20, |f| ui(f, &mut app));
        assert!(screen.contains("q/Esc  Home"), "{screen}");
        assert!(!screen.contains("Quit"), "{screen}");
    }

    #[test]
    fn ctrl_c_quits_suggest_without_confirming() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut app = AppState::new(make_suggestions(1), vec![]);
        assert_eq!(app.handle_input(ctrl_c), Some(false));

        // While a name is being edited: quit, and no literal 'c' typed.
        let mut app = AppState::new(make_suggestions(1), vec![]);
        app.handle_input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(app.input_mode, InputMode::EditingName);
        assert_eq!(app.handle_input(ctrl_c), Some(false));
        assert_eq!(app.suggestions[0].name, "alias0");
        assert_eq!(app.edit_buffer, "alias0");
    }

    #[test]
    fn handle_input_confirm_enter() {
        let mut app = AppState::new(make_suggestions(1), vec![]);
        let result = app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(true));
    }

    #[test]
    fn handle_input_navigation() {
        let mut app = AppState::new(make_suggestions(3), vec![]);
        app.handle_input(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.selected_idx, 1);
        app.handle_input(KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(app.selected_idx, 0);
        app.handle_input(KeyEvent::from(KeyCode::Down));
        assert_eq!(app.selected_idx, 1);
        app.handle_input(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.selected_idx, 0);
    }

    #[test]
    fn handle_input_space_toggles() {
        let mut app = AppState::new(make_suggestions(1), vec![]);
        app.handle_input(KeyEvent::from(KeyCode::Char(' ')));
        assert!(app.suggestions[0].selected);
    }

    #[test]
    fn handle_input_a_selects_all() {
        let mut app = AppState::new(make_suggestions(3), vec![]);
        app.handle_input(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert!(app.suggestions.iter().all(|s| s.selected));
    }

    #[test]
    fn handle_input_n_deselects_all() {
        let mut app = AppState::new(make_suggestions(3), vec![]);
        app.select_all();
        app.handle_input(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert!(app.suggestions.iter().all(|s| !s.selected));
    }

    #[test]
    fn handle_input_e_enters_edit_mode() {
        let mut app = AppState::new(make_suggestions(1), vec![]);
        app.handle_input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(app.input_mode, InputMode::EditingName);
        assert_eq!(app.edit_buffer, "alias0");
    }

    #[test]
    fn edit_mode_typing_and_confirm() {
        let mut app = AppState::new(make_suggestions(1), vec![]);
        app.handle_input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        // Clear and type new name
        app.edit_buffer.clear();
        app.handle_input(KeyEvent::from(KeyCode::Char('g')));
        app.handle_input(KeyEvent::from(KeyCode::Char('s')));
        app.handle_input(KeyEvent::from(KeyCode::Char('t')));
        assert_eq!(app.edit_buffer, "gst");
        // Confirm
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.suggestions[0].name, "gst");
    }

    #[test]
    fn edit_mode_caps_the_name() {
        let mut app = AppState::new(make_suggestions(1), vec![]);
        app.handle_input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        app.edit_buffer.clear();
        for _ in 0..MAX_FIELD_LEN + 10 {
            app.handle_input(KeyEvent::from(KeyCode::Char('g')));
        }
        assert_eq!(app.edit_buffer.len(), MAX_FIELD_LEN);
        // In bytes: a two-byte letter does not fit in the last byte.
        app.edit_buffer.pop();
        app.handle_input(KeyEvent::from(KeyCode::Char('é')));
        assert_eq!(app.edit_buffer.len(), MAX_FIELD_LEN - 1);
    }

    #[test]
    fn edit_mode_backspace() {
        let mut app = AppState::new(make_suggestions(1), vec![]);
        app.handle_input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        app.handle_input(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.edit_buffer, "alias"); // removed trailing '0'
    }

    #[test]
    fn edit_mode_esc_cancels() {
        let mut app = AppState::new(make_suggestions(1), vec![]);
        app.handle_input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        app.edit_buffer = "changed".to_string();
        app.handle_input(KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.input_mode, InputMode::Normal);
        // Name should NOT be changed on cancel
        assert_eq!(app.suggestions[0].name, "alias0");
    }

    #[test]
    fn edit_mode_rejects_invalid_chars() {
        let mut app = AppState::new(make_suggestions(1), vec![]);
        app.handle_input(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        let before = app.edit_buffer.clone();
        app.handle_input(KeyEvent::from(KeyCode::Char('!')));
        app.handle_input(KeyEvent::from(KeyCode::Char(' ')));
        app.handle_input(KeyEvent::from(KeyCode::Char('@')));
        assert_eq!(app.edit_buffer, before); // unchanged
    }
}
