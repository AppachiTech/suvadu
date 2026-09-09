//! Interactive manager for shell aliases — `suv aliases` with no subcommand
//! launches this. Browse/filter, add, edit, and delete aliases, all backed
//! by the same `Repository` methods the CLI subcommands use. Every write
//! also regenerates the sourced `aliases.sh` file so changes take effect in
//! new shells without a separate `suv aliases apply`.

use crate::commands::alias::{validate_alias_name, write_aliases_file};
use crate::models::Alias;
use crate::repository::Repository;
use crate::theme::theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph},
    Terminal,
};
use std::io;
use std::time::Instant;

/// Cap on the search/form fields, matching the scope of a filter field.
const MAX_FIELD_LEN: usize = 200;

/// Focus within the add/edit form.
#[derive(PartialEq, Eq)]
enum FormFocus {
    Name,
    Command,
}

enum Mode {
    Browse,
    /// Add when `editing` is `None`; Edit (pre-filled, tracking the
    /// original name so a rename can remove the old row) otherwise.
    Form {
        editing: Option<String>,
        name_input: String,
        command_input: String,
        focus: FormFocus,
        error: Option<String>,
    },
    ConfirmDelete {
        name: String,
    },
}

struct AliasManagerApp {
    aliases: Vec<Alias>,
    filtered: Vec<usize>,
    query: String,
    list_state: ListState,
    mode: Mode,
    status: Option<(String, Instant)>,
}

impl AliasManagerApp {
    fn new(aliases: Vec<Alias>) -> Self {
        let mut app = Self {
            filtered: (0..aliases.len()).collect(),
            aliases,
            query: String::new(),
            list_state: ListState::default(),
            mode: Mode::Browse,
            status: None,
        };
        if !app.filtered.is_empty() {
            app.list_state.select(Some(0));
        }
        app
    }

    fn apply_filter(&mut self) {
        let needle = self.query.to_lowercase();
        self.filtered = if needle.trim().is_empty() {
            (0..self.aliases.len()).collect()
        } else {
            self.aliases
                .iter()
                .enumerate()
                .filter(|(_, a)| {
                    a.name.to_lowercase().contains(&needle)
                        || a.command.to_lowercase().contains(&needle)
                })
                .map(|(i, _)| i)
                .collect()
        };
        self.list_state.select(if self.filtered.is_empty() {
            None
        } else {
            Some(0)
        });
    }

    const fn move_up(&mut self) {
        if let Some(i) = self.list_state.selected() {
            self.list_state.select(Some(i.saturating_sub(1)));
        }
    }

    fn move_down(&mut self) {
        if let Some(i) = self.list_state.selected() {
            let max = self.filtered.len().saturating_sub(1);
            self.list_state.select(Some((i + 1).min(max)));
        }
    }

    fn selected_alias(&self) -> Option<&Alias> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered.get(i))
            .and_then(|&idx| self.aliases.get(idx))
    }

    fn start_add(&mut self) {
        self.mode = Mode::Form {
            editing: None,
            name_input: String::new(),
            command_input: String::new(),
            focus: FormFocus::Name,
            error: None,
        };
    }

    fn start_edit(&mut self) {
        if let Some(a) = self.selected_alias() {
            self.mode = Mode::Form {
                editing: Some(a.name.clone()),
                name_input: a.name.clone(),
                command_input: a.command.clone(),
                focus: FormFocus::Command,
                error: None,
            };
        }
    }

    fn start_delete(&mut self) {
        if let Some(a) = self.selected_alias() {
            self.mode = Mode::ConfirmDelete {
                name: a.name.clone(),
            };
        }
    }

    /// Reload aliases from the database and re-apply the current filter —
    /// used after any write, so the in-memory list never drifts from what
    /// the database (and its upsert-by-name) actually did.
    fn reload(&mut self, repo: &Repository) -> Result<(), Box<dyn std::error::Error>> {
        self.aliases = repo.list_aliases()?;
        self.apply_filter();
        Ok(())
    }

    fn submit_form(&mut self, repo: &Repository) -> Result<(), Box<dyn std::error::Error>> {
        let Mode::Form {
            editing,
            name_input,
            command_input,
            error,
            ..
        } = &mut self.mode
        else {
            return Ok(());
        };
        let name = name_input.trim().to_string();
        let command = command_input.trim().to_string();
        if let Err(msg) = validate_alias_name(&name) {
            *error = Some(msg);
            return Ok(());
        }
        if command.is_empty() {
            *error = Some("Command is required".to_string());
            return Ok(());
        }
        let editing = editing.clone();

        // Aliases are keyed by name (UNIQUE), so renaming one means removing
        // the old row rather than letting add_alias's upsert leave a stale
        // duplicate behind under the old name.
        if let Some(original) = &editing {
            if original != &name {
                repo.remove_alias(original)?;
            }
        }
        repo.add_alias(&name, &command)?;

        let verb = if editing.is_some() {
            "Updated"
        } else {
            "Added"
        };
        self.mode = Mode::Browse;
        self.reload(repo)?;
        self.status = Some((
            Self::sync_status(repo, &format!("{verb}: {name}")),
            Instant::now(),
        ));
        Ok(())
    }

    fn confirm_delete(&mut self, repo: &Repository) -> Result<(), Box<dyn std::error::Error>> {
        let Mode::ConfirmDelete { name } = &self.mode else {
            return Ok(());
        };
        let name = name.clone();
        repo.remove_alias(&name)?;
        self.mode = Mode::Browse;
        self.reload(repo)?;
        self.status = Some((
            Self::sync_status(repo, &format!("Removed: {name}")),
            Instant::now(),
        ));
        Ok(())
    }

    /// Regenerate `aliases.sh` after a write; on failure, still report the
    /// underlying DB change (already committed) but say the sync failed
    /// rather than silently leaving the shell file stale.
    fn sync_status(repo: &Repository, base_msg: &str) -> String {
        match write_aliases_file(repo) {
            Ok(_) => base_msg.to_string(),
            Err(e) => format!("{base_msg} (couldn't write aliases.sh: {e})"),
        }
    }
}

/// Show an interactive manager over the repository's aliases. Opens even
/// with zero aliases — it renders its own empty state, same as `suv search`.
pub fn run_alias_manager(repo: &Repository) -> Result<(), Box<dyn std::error::Error>> {
    let _guard = crate::util::TerminalGuardStderr::new()?;
    let backend = CrosstermBackend::new(io::stderr());
    let mut terminal = Terminal::new(backend)?;

    let aliases = repo.list_aliases()?;
    let mut app = AliasManagerApp::new(aliases);

    loop {
        terminal.draw(|f| render(f, &mut app))?;

        if !event::poll(std::time::Duration::from_mins(1))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match &app.mode {
                Mode::Form { .. } => handle_form_input(&mut app, repo, key)?,
                Mode::ConfirmDelete { .. } => handle_confirm_delete_input(&mut app, repo, key)?,
                Mode::Browse => {
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        match key.code {
                            KeyCode::Char('a') => app.start_add(),
                            KeyCode::Char('e') => app.start_edit(),
                            KeyCode::Char('d') => app.start_delete(),
                            _ => {}
                        }
                        continue;
                    }
                    match key.code {
                        // Always-on search, like suv search: Esc quits
                        // outright rather than clearing the query first.
                        KeyCode::Esc => break,
                        KeyCode::Up => app.move_up(),
                        KeyCode::Down => app.move_down(),
                        KeyCode::Enter => app.start_edit(),
                        KeyCode::Backspace => {
                            app.query.pop();
                            app.apply_filter();
                        }
                        KeyCode::Char(c) if app.query.len() + c.len_utf8() <= MAX_FIELD_LEN => {
                            app.query.push(c);
                            app.apply_filter();
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    terminal.show_cursor()?;
    Ok(())
}

fn handle_form_input(
    app: &mut AliasManagerApp,
    repo: &Repository,
    key: crossterm::event::KeyEvent,
) -> Result<(), Box<dyn std::error::Error>> {
    let Mode::Form {
        name_input,
        command_input,
        focus,
        error,
        ..
    } = &mut app.mode
    else {
        return Ok(());
    };
    match key.code {
        KeyCode::Esc => app.mode = Mode::Browse,
        KeyCode::Tab | KeyCode::Down | KeyCode::Up => {
            *focus = match focus {
                FormFocus::Name => FormFocus::Command,
                FormFocus::Command => FormFocus::Name,
            };
        }
        KeyCode::Enter => {
            if *focus == FormFocus::Name {
                *focus = FormFocus::Command;
            } else {
                app.submit_form(repo)?;
            }
        }
        KeyCode::Backspace => {
            *error = None;
            match focus {
                FormFocus::Name => {
                    name_input.pop();
                }
                FormFocus::Command => {
                    command_input.pop();
                }
            }
        }
        KeyCode::Char(c) => {
            *error = None;
            let field = match focus {
                FormFocus::Name => &mut *name_input,
                FormFocus::Command => &mut *command_input,
            };
            if field.len() + c.len_utf8() <= MAX_FIELD_LEN {
                field.push(c);
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_confirm_delete_input(
    app: &mut AliasManagerApp,
    repo: &Repository,
    key: crossterm::event::KeyEvent,
) -> Result<(), Box<dyn std::error::Error>> {
    match key.code {
        KeyCode::Char('y' | 'Y') => app.confirm_delete(repo)?,
        KeyCode::Char('n' | 'N') | KeyCode::Esc => app.mode = Mode::Browse,
        _ => {}
    }
    Ok(())
}

fn render(f: &mut ratatui::Frame, app: &mut AliasManagerApp) {
    let t = theme();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(3), // search box (always-on, like suv search)
            Constraint::Min(1),    // list
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    render_header(f, chunks[0], t);
    render_search_box(f, app, chunks[1], t);
    render_list(f, app, chunks[2], t);
    render_footer(f, app, chunks[3], t);

    match &app.mode {
        Mode::Form {
            editing,
            name_input,
            command_input,
            focus,
            error,
        } => render_form_dialog(
            f,
            editing.is_some(),
            name_input,
            command_input,
            focus,
            error.as_deref(),
            t,
        ),
        Mode::ConfirmDelete { name } => render_confirm_delete_dialog(f, name, t),
        Mode::Browse => {}
    }
}

fn render_header(f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
    let header_line = Line::from(vec![Span::styled(
        "SUVADU ALIASES",
        Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
    )]);
    f.render_widget(
        Paragraph::new(header_line).alignment(Alignment::Center),
        area,
    );
}

fn render_search_box(
    f: &mut ratatui::Frame,
    app: &AliasManagerApp,
    area: Rect,
    t: &crate::theme::Theme,
) {
    let box_widget = Paragraph::new(app.query.as_str())
        .style(Style::default().fg(t.text))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border_focus))
                .title("Search (Typing)"),
        );
    f.render_widget(box_widget, area);
}

fn render_list(
    f: &mut ratatui::Frame,
    app: &mut AliasManagerApp,
    area: Rect,
    t: &crate::theme::Theme,
) {
    let items: Vec<ListItem> = app
        .filtered
        .iter()
        .filter_map(|&i| app.aliases.get(i))
        .map(|a| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<12}", a.name),
                    Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  →  ", Style::default().fg(t.text_muted)),
                Span::styled(a.command.clone(), Style::default().fg(t.text)),
            ]))
        })
        .collect();

    let title = format!(" Aliases ({}/{}) ", app.filtered.len(), app.aliases.len());
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(title),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD).fg(t.primary))
        .highlight_symbol(" > ");
    f.render_stateful_widget(list, area, &mut app.list_state);

    if app.aliases.is_empty() {
        let hint = Paragraph::new(Line::from(Span::styled(
            "  No aliases yet. Press Ctrl+A to add one.",
            Style::default().fg(t.text_muted),
        )));
        let hint_area = Rect {
            x: area.x + 1,
            y: area.y + 2,
            width: area.width.saturating_sub(2),
            height: 1,
        };
        f.render_widget(hint, hint_area);
    } else if app.filtered.is_empty() {
        let hint = Paragraph::new(Line::from(Span::styled(
            "  No aliases match your search.",
            Style::default().fg(t.text_muted),
        )));
        let hint_area = Rect {
            x: area.x + 1,
            y: area.y + 2,
            width: area.width.saturating_sub(2),
            height: 1,
        };
        f.render_widget(hint, hint_area);
    }
}

fn render_footer(
    f: &mut ratatui::Frame,
    app: &AliasManagerApp,
    area: Rect,
    t: &crate::theme::Theme,
) {
    let badge_key = Style::default().bg(t.badge_bg).fg(t.text);
    let badge_label = Style::default().fg(t.text_secondary);

    let mut spans = match &app.mode {
        Mode::Form { .. } => vec![
            Span::styled(" Enter ", badge_key),
            Span::styled(" Next/Confirm  ", badge_label),
            Span::styled(" Tab ", badge_key),
            Span::styled(" Switch field  ", badge_label),
            Span::styled(" Esc ", badge_key),
            Span::styled(" Cancel  ", badge_label),
        ],
        Mode::ConfirmDelete { .. } => vec![
            Span::styled(" y ", badge_key),
            Span::styled(" Confirm  ", badge_label),
            Span::styled(" n/Esc ", badge_key),
            Span::styled(" Cancel  ", badge_label),
        ],
        Mode::Browse => vec![
            Span::styled(" Esc ", badge_key),
            Span::styled(" Quit  ", badge_label),
            Span::styled(" ↑↓ ", badge_key),
            Span::styled(" Navigate  ", badge_label),
            Span::styled(" Enter ", badge_key),
            Span::styled(" Edit  ", badge_label),
            Span::styled(" ^A ", badge_key),
            Span::styled(" Add  ", badge_label),
            Span::styled(" ^D ", badge_key),
            Span::styled(" Delete ", badge_label),
        ],
    };

    if let Some((msg, time)) = &app.status {
        if time.elapsed() < std::time::Duration::from_secs(2) {
            spans.push(Span::styled(
                format!(" {msg} "),
                Style::default().fg(t.success).add_modifier(Modifier::BOLD),
            ));
        }
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_form_dialog(
    f: &mut ratatui::Frame,
    editing: bool,
    name_input: &str,
    command_input: &str,
    focus: &FormFocus,
    error: Option<&str>,
    t: &crate::theme::Theme,
) {
    // 2 (outer border) + 3 (Name field) + 3 (Command field) + 1 (error line).
    let area = centered_rect(f.area(), 60, 9);
    f.render_widget(Clear, area);

    let title = if editing {
        " Edit Alias "
    } else {
        " Add Alias "
    };
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_focus))
        .title(Span::styled(
            title,
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        ));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(inner);

    render_field(f, rows[0], "Name", name_input, *focus == FormFocus::Name, t);
    render_field(
        f,
        rows[1],
        "Command",
        command_input,
        *focus == FormFocus::Command,
        t,
    );

    if let Some(msg) = error {
        let err = Paragraph::new(Line::from(Span::styled(
            msg,
            Style::default().fg(t.error).add_modifier(Modifier::BOLD),
        )));
        f.render_widget(err, rows[2]);
    }
}

fn render_field(
    f: &mut ratatui::Frame,
    area: Rect,
    label: &str,
    value: &str,
    focused: bool,
    t: &crate::theme::Theme,
) {
    let title = if focused {
        format!("{label} *")
    } else {
        label.to_string()
    };
    let border_color = if focused { t.border_focus } else { t.border };
    let field = Paragraph::new(value).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .title(title),
    );
    f.render_widget(field, area);
}

fn render_confirm_delete_dialog(f: &mut ratatui::Frame, name: &str, t: &crate::theme::Theme) {
    let area = centered_rect(f.area(), 50, 3);
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.warning))
        .title(Span::styled(
            " Confirm delete ",
            Style::default().fg(t.warning).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let text = Paragraph::new(Line::from(vec![
        Span::styled("Delete alias ", Style::default().fg(t.text)),
        Span::styled(
            format!("'{name}'"),
            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled("? [y/N]", Style::default().fg(t.text)),
    ]));
    f.render_widget(text, inner);
}

/// A fixed-size rect centered within `area`.
fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_alias(id: i64, name: &str, command: &str) -> Alias {
        Alias {
            id,
            name: name.to_string(),
            command: command.to_string(),
            created_at: 1000,
        }
    }

    #[test]
    fn new_app_selects_first_when_nonempty() {
        let app = AliasManagerApp::new(vec![make_alias(1, "gst", "git status")]);
        assert_eq!(app.list_state.selected(), Some(0));
        assert_eq!(app.filtered, vec![0]);
    }

    #[test]
    fn new_app_selects_none_when_empty() {
        let app = AliasManagerApp::new(vec![]);
        assert_eq!(app.list_state.selected(), None);
        assert!(app.filtered.is_empty());
    }

    #[test]
    fn filter_matches_name_case_insensitively() {
        let mut app = AliasManagerApp::new(vec![
            make_alias(1, "gst", "git status"),
            make_alias(2, "dps", "docker ps"),
        ]);
        app.query = "GST".to_string();
        app.apply_filter();
        assert_eq!(app.filtered, vec![0]);
    }

    #[test]
    fn filter_matches_command_text() {
        let mut app = AliasManagerApp::new(vec![
            make_alias(1, "gst", "git status"),
            make_alias(2, "dps", "docker ps"),
        ]);
        app.query = "docker".to_string();
        app.apply_filter();
        assert_eq!(app.filtered, vec![1]);
    }

    #[test]
    fn filter_with_no_matches_clears_selection() {
        let mut app = AliasManagerApp::new(vec![make_alias(1, "gst", "git status")]);
        app.query = "nonexistent".to_string();
        app.apply_filter();
        assert!(app.filtered.is_empty());
        assert_eq!(app.list_state.selected(), None);
    }

    #[test]
    fn empty_query_shows_all_aliases() {
        let mut app = AliasManagerApp::new(vec![
            make_alias(1, "gst", "git status"),
            make_alias(2, "dps", "docker ps"),
        ]);
        app.query = "docker".to_string();
        app.apply_filter();
        assert_eq!(app.filtered.len(), 1);
        app.query.clear();
        app.apply_filter();
        assert_eq!(app.filtered.len(), 2);
    }

    #[test]
    fn move_down_clamps_at_last_filtered_item() {
        let mut app = AliasManagerApp::new(vec![
            make_alias(1, "a", "cmd-a"),
            make_alias(2, "b", "cmd-b"),
        ]);
        app.move_down();
        assert_eq!(app.list_state.selected(), Some(1));
        app.move_down();
        assert_eq!(app.list_state.selected(), Some(1));
    }

    #[test]
    fn move_up_clamps_at_zero() {
        let mut app = AliasManagerApp::new(vec![make_alias(1, "a", "cmd-a")]);
        app.move_up();
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn submit_form_rejects_invalid_name() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = AliasManagerApp::new(vec![]);
        app.mode = Mode::Form {
            editing: None,
            name_input: "has space".to_string(),
            command_input: "git status".to_string(),
            focus: FormFocus::Command,
            error: None,
        };
        app.submit_form(&repo).unwrap();
        assert!(matches!(app.mode, Mode::Form { .. }));
        assert!(app.aliases.is_empty());
    }

    #[test]
    fn submit_form_rejects_empty_command() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = AliasManagerApp::new(vec![]);
        app.mode = Mode::Form {
            editing: None,
            name_input: "gst".to_string(),
            command_input: "   ".to_string(),
            focus: FormFocus::Command,
            error: None,
        };
        app.submit_form(&repo).unwrap();
        assert!(matches!(app.mode, Mode::Form { .. }));
        assert!(app.aliases.is_empty());
    }

    #[test]
    fn submit_form_adds_alias_and_returns_to_browse() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = AliasManagerApp::new(vec![]);
        app.mode = Mode::Form {
            editing: None,
            name_input: "gst".to_string(),
            command_input: "git status".to_string(),
            focus: FormFocus::Command,
            error: None,
        };
        app.submit_form(&repo).unwrap();
        assert!(matches!(app.mode, Mode::Browse));
        assert_eq!(app.aliases.len(), 1);
        assert_eq!(app.aliases[0].name, "gst");
        assert_eq!(app.aliases[0].command, "git status");
        assert_eq!(app.filtered.len(), 1);
        assert_eq!(
            app.status.as_ref().map(|(m, _)| m.as_str()),
            Some("Added: gst")
        );
    }

    #[test]
    fn submit_form_trims_whitespace() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = AliasManagerApp::new(vec![]);
        app.mode = Mode::Form {
            editing: None,
            name_input: "  gst  ".to_string(),
            command_input: "  git status  ".to_string(),
            focus: FormFocus::Command,
            error: None,
        };
        app.submit_form(&repo).unwrap();
        assert_eq!(app.aliases[0].name, "gst");
        assert_eq!(app.aliases[0].command, "git status");
    }

    #[test]
    fn start_edit_prefills_form_from_selected_alias() {
        let mut app = AliasManagerApp::new(vec![make_alias(1, "gst", "git status")]);
        app.start_edit();
        match app.mode {
            Mode::Form {
                editing,
                name_input,
                command_input,
                ..
            } => {
                assert_eq!(editing.as_deref(), Some("gst"));
                assert_eq!(name_input, "gst");
                assert_eq!(command_input, "git status");
            }
            _ => panic!("expected Form mode"),
        }
    }

    #[test]
    fn start_edit_on_empty_list_is_noop() {
        let mut app = AliasManagerApp::new(vec![]);
        app.start_edit();
        assert!(matches!(app.mode, Mode::Browse));
    }

    #[test]
    fn submit_form_editing_same_name_updates_command_in_place() {
        let (_dir, repo) = crate::test_utils::test_repo();
        repo.add_alias("gst", "git status").unwrap();
        let mut app = AliasManagerApp::new(repo.list_aliases().unwrap());
        app.mode = Mode::Form {
            editing: Some("gst".to_string()),
            name_input: "gst".to_string(),
            command_input: "git status --short".to_string(),
            focus: FormFocus::Command,
            error: None,
        };
        app.submit_form(&repo).unwrap();
        assert_eq!(app.aliases.len(), 1);
        assert_eq!(app.aliases[0].command, "git status --short");
        assert_eq!(
            app.status.as_ref().map(|(m, _)| m.as_str()),
            Some("Updated: gst")
        );
    }

    #[test]
    fn submit_form_editing_renamed_alias_removes_old_row() {
        let (_dir, repo) = crate::test_utils::test_repo();
        repo.add_alias("gst", "git status").unwrap();
        let mut app = AliasManagerApp::new(repo.list_aliases().unwrap());
        app.mode = Mode::Form {
            editing: Some("gst".to_string()),
            name_input: "gs".to_string(),
            command_input: "git status".to_string(),
            focus: FormFocus::Command,
            error: None,
        };
        app.submit_form(&repo).unwrap();
        // Only the renamed alias remains — no leftover "gst" row.
        assert_eq!(app.aliases.len(), 1);
        assert_eq!(app.aliases[0].name, "gs");
    }

    #[test]
    fn start_delete_opens_confirm_dialog_with_selected_name() {
        let mut app = AliasManagerApp::new(vec![make_alias(1, "gst", "git status")]);
        app.start_delete();
        match app.mode {
            Mode::ConfirmDelete { name } => assert_eq!(name, "gst"),
            _ => panic!("expected ConfirmDelete mode"),
        }
    }

    #[test]
    fn start_delete_on_empty_list_is_noop() {
        let mut app = AliasManagerApp::new(vec![]);
        app.start_delete();
        assert!(matches!(app.mode, Mode::Browse));
    }

    #[test]
    fn confirm_delete_removes_alias_and_returns_to_browse() {
        let (_dir, repo) = crate::test_utils::test_repo();
        repo.add_alias("gst", "git status").unwrap();
        let mut app = AliasManagerApp::new(repo.list_aliases().unwrap());
        app.mode = Mode::ConfirmDelete {
            name: "gst".to_string(),
        };
        app.confirm_delete(&repo).unwrap();
        assert!(matches!(app.mode, Mode::Browse));
        assert!(app.aliases.is_empty());
        assert_eq!(
            app.status.as_ref().map(|(m, _)| m.as_str()),
            Some("Removed: gst")
        );
    }
}
