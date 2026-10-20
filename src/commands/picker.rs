//! Small interactive list pickers. Render to stderr (via `TerminalGuardStderr`)
//! so the chosen value can be printed to stdout for a shell wrapper (bookmarks).

use crate::models::Bookmark;
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

/// Cap on the in-memory bookmark search/form fields, matching the scope of
/// a filter field (not a document editor).
const MAX_FIELD_LEN: usize = 200;

/// Focus within the add/edit form.
#[derive(PartialEq, Eq)]
enum FormFocus {
    Command,
    Label,
}

enum Mode {
    Browse,
    /// Add when `editing` is `None`; Edit (pre-filled, tracking the
    /// original command so a rename can remove the old row) otherwise.
    Form {
        editing: Option<String>,
        command_input: String,
        label_input: String,
        focus: FormFocus,
        error: Option<String>,
    },
    ConfirmDelete {
        command: String,
    },
    /// Filling in a picked bookmark's `{{placeholders}}` before recalling it.
    Fill(FillState),
}

/// The placeholder popup: one value per distinct placeholder, in order,
/// each prefilled with its default.
struct FillState {
    template: crate::snippet::Template,
    values: Vec<String>,
    focus: usize,
}

impl FillState {
    fn new(command: &str) -> Option<Self> {
        let template = crate::snippet::Template::parse(command);
        if !template.has_placeholders() {
            return None;
        }
        let values = template
            .placeholders()
            .iter()
            .map(|p| p.default.clone().unwrap_or_default())
            .collect();
        Some(Self {
            template,
            values,
            focus: 0,
        })
    }

    fn filled(&self) -> String {
        self.template.fill_in_order(&self.values)
    }

    const fn is_last(&self) -> bool {
        self.focus + 1 >= self.values.len()
    }

    fn push_str_capped(&mut self, text: &str) {
        if let Some(field) = self.values.get_mut(self.focus) {
            for c in text.chars() {
                // A value is one shell word or phrase: a pasted line break
                // becomes a space rather than a second command.
                let c = if c == '\n' || c == '\r' { ' ' } else { c };
                if c.is_control() || field.len() + c.len_utf8() > MAX_FIELD_LEN {
                    continue;
                }
                field.push(c);
            }
        }
    }
}

struct PickerApp {
    bookmarks: Vec<Bookmark>,
    filtered: Vec<usize>,
    query: String,
    list_state: ListState,
    mode: Mode,
    status: Option<(String, Instant)>,
    /// The quit hint's label: "Home" when Suvadu Home opened this screen,
    /// since leaving returns there.
    leave_label: &'static str,
}

impl PickerApp {
    fn new(bookmarks: Vec<Bookmark>) -> Self {
        let mut app = Self {
            filtered: (0..bookmarks.len()).collect(),
            bookmarks,
            query: String::new(),
            list_state: ListState::default(),
            mode: Mode::Browse,
            status: None,
            leave_label: "Quit",
        };
        if !app.filtered.is_empty() {
            app.list_state.select(Some(0));
        }
        app
    }

    fn apply_filter(&mut self) {
        let needle = self.query.to_lowercase();
        self.filtered = if needle.trim().is_empty() {
            (0..self.bookmarks.len()).collect()
        } else {
            self.bookmarks
                .iter()
                .enumerate()
                .filter(|(_, b)| {
                    b.command.to_lowercase().contains(&needle)
                        || b.label
                            .as_deref()
                            .is_some_and(|l| l.to_lowercase().contains(&needle))
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

    fn selected_bookmark(&self) -> Option<&Bookmark> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered.get(i))
            .and_then(|&idx| self.bookmarks.get(idx))
    }

    fn selected_command(&self) -> Option<String> {
        self.selected_bookmark().map(|b| b.command.clone())
    }

    /// Enter on a bookmark: recall it as-is, or open the placeholder popup
    /// first when it has `{{placeholders}}`.
    fn pick(&mut self) -> Step {
        let Some(command) = self.selected_command() else {
            return Step::Leave(None);
        };
        match FillState::new(&command) {
            Some(fill) => {
                self.mode = Mode::Fill(fill);
                Step::Stay
            }
            None => Step::Leave(Some(command)),
        }
    }

    fn start_add(&mut self) {
        self.mode = Mode::Form {
            editing: None,
            command_input: String::new(),
            label_input: String::new(),
            focus: FormFocus::Command,
            error: None,
        };
    }

    fn start_edit(&mut self) {
        if let Some(b) = self.selected_bookmark() {
            self.mode = Mode::Form {
                editing: Some(b.command.clone()),
                command_input: b.command.clone(),
                label_input: b.label.clone().unwrap_or_default(),
                focus: FormFocus::Command,
                error: None,
            };
        }
    }

    fn start_delete(&mut self) {
        if let Some(b) = self.selected_bookmark() {
            self.mode = Mode::ConfirmDelete {
                command: b.command.clone(),
            };
        }
    }

    /// Reload bookmarks from the database and re-apply the current filter —
    /// used after any write, so the in-memory list never drifts from what
    /// the database (and its ON CONFLICT upsert) actually did.
    fn reload(&mut self, repo: &Repository) -> Result<(), Box<dyn std::error::Error>> {
        self.bookmarks = repo.list_bookmarks()?;
        self.apply_filter();
        Ok(())
    }

    fn submit_form(&mut self, repo: &Repository) -> Result<(), Box<dyn std::error::Error>> {
        let Mode::Form {
            editing,
            command_input,
            label_input,
            error,
            ..
        } = &mut self.mode
        else {
            return Ok(());
        };
        let command = command_input.trim().to_string();
        if command.is_empty() {
            *error = Some("Command is required".to_string());
            return Ok(());
        }
        let label = label_input.trim();
        let label = if label.is_empty() {
            None
        } else {
            Some(label.to_string())
        };
        let editing = editing.clone();

        // Bookmarks are keyed by command text (UNIQUE), so renaming one
        // means removing the old row rather than letting add_bookmark's
        // upsert leave a stale duplicate behind under the old command.
        if let Some(original) = &editing {
            if original != &command {
                repo.remove_bookmark(original)?;
            }
        }
        repo.add_bookmark(&command, label.as_deref())?;

        let verb = if editing.is_some() {
            "Updated"
        } else {
            "Added"
        };
        self.mode = Mode::Browse;
        self.reload(repo)?;
        self.status = Some((format!("{verb}: {command}"), Instant::now()));
        Ok(())
    }

    fn confirm_delete(&mut self, repo: &Repository) -> Result<(), Box<dyn std::error::Error>> {
        let Mode::ConfirmDelete { command } = &self.mode else {
            return Ok(());
        };
        let command = command.clone();
        repo.remove_bookmark(&command)?;
        self.mode = Mode::Browse;
        self.reload(repo)?;
        self.status = Some((format!("Removed: {command}"), Instant::now()));
        Ok(())
    }
}

/// Show a picker over the repository's bookmarks and return the selected
/// command, or `None` if the user cancelled. Opens even with zero
/// bookmarks — it renders its own empty state, same as `suv search`.
pub fn pick_bookmark(repo: &Repository) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let _guard = crate::util::TerminalGuardStderr::new()?;
    let backend = CrosstermBackend::new(io::stderr());
    let mut terminal = Terminal::new(backend)?;

    let bookmarks = repo.list_bookmarks()?;
    let mut app = PickerApp::new(bookmarks);
    app.leave_label = crate::home::runner::leave_label();

    let result = loop {
        terminal.draw(|f| render(f, &mut app))?;

        if !event::poll(std::time::Duration::from_mins(1))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if let Step::Leave(picked) = handle_key(&mut app, repo, key)? {
                    break picked;
                }
            }
            Event::Paste(text) => handle_paste(&mut app, &text),
            _ => {}
        }
    };

    terminal.show_cursor()?;
    Ok(result)
}

/// What the picker does after a key.
enum Step {
    Stay,
    /// Close, recalling the command if one was picked.
    Leave(Option<String>),
}

fn handle_key(
    app: &mut PickerApp,
    repo: &Repository,
    key: crossterm::event::KeyEvent,
) -> Result<Step, Box<dyn std::error::Error>> {
    // Cancels from anywhere, a form or the delete prompt included.
    if crate::util::is_interrupt(&key) {
        return Ok(Step::Leave(None));
    }
    match &app.mode {
        Mode::Form { .. } => handle_form_input(app, repo, key)?,
        Mode::ConfirmDelete { .. } => handle_confirm_delete_input(app, repo, key)?,
        Mode::Fill(_) => return Ok(handle_fill_input(app, key)),
        Mode::Browse => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match key.code {
                    KeyCode::Char('a') => app.start_add(),
                    KeyCode::Char('e') => app.start_edit(),
                    KeyCode::Char('d') => app.start_delete(),
                    _ => {}
                }
                return Ok(Step::Stay);
            }
            match key.code {
                // Always-on search, like suv search: Esc quits outright
                // rather than clearing the query first, since there's no
                // separate "typing mode" to fall back out of.
                KeyCode::Esc => return Ok(Step::Leave(None)),
                KeyCode::Up => app.move_up(),
                KeyCode::Down => app.move_down(),
                KeyCode::Enter => return Ok(app.pick()),
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
    Ok(Step::Stay)
}

fn handle_form_input(
    app: &mut PickerApp,
    repo: &Repository,
    key: crossterm::event::KeyEvent,
) -> Result<(), Box<dyn std::error::Error>> {
    let Mode::Form {
        command_input,
        label_input,
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
                FormFocus::Command => FormFocus::Label,
                FormFocus::Label => FormFocus::Command,
            };
        }
        KeyCode::Enter => {
            if *focus == FormFocus::Command {
                *focus = FormFocus::Label;
            } else {
                app.submit_form(repo)?;
            }
        }
        KeyCode::Backspace => {
            *error = None;
            match focus {
                FormFocus::Command => {
                    command_input.pop();
                }
                FormFocus::Label => {
                    label_input.pop();
                }
            }
        }
        KeyCode::Char(c) => {
            *error = None;
            let field = match focus {
                FormFocus::Command => &mut *command_input,
                FormFocus::Label => &mut *label_input,
            };
            if field.len() + c.len_utf8() <= MAX_FIELD_LEN {
                field.push(c);
            }
        }
        _ => {}
    }
    Ok(())
}

/// Keys in the placeholder popup. Tab/Enter move to the next value and
/// Enter on the last one recalls the filled command — never run, only put
/// on the prompt. Esc goes back to the list without recalling anything.
fn handle_fill_input(app: &mut PickerApp, key: crossterm::event::KeyEvent) -> Step {
    let Mode::Fill(fill) = &mut app.mode else {
        return Step::Stay;
    };
    match key.code {
        KeyCode::Esc => app.mode = Mode::Browse,
        KeyCode::Enter if fill.is_last() => return Step::Leave(Some(fill.filled())),
        KeyCode::Enter | KeyCode::Tab | KeyCode::Down => {
            if !fill.is_last() {
                fill.focus += 1;
            }
        }
        KeyCode::BackTab | KeyCode::Up => fill.focus = fill.focus.saturating_sub(1),
        KeyCode::Backspace => {
            if let Some(field) = fill.values.get_mut(fill.focus) {
                field.pop();
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            fill.push_str_capped(c.encode_utf8(&mut [0; 4]));
        }
        _ => {}
    }
    Step::Stay
}

/// A paste goes into the focused placeholder value; elsewhere in the
/// picker it is ignored, as before.
fn handle_paste(app: &mut PickerApp, text: &str) {
    if let Mode::Fill(fill) = &mut app.mode {
        fill.push_str_capped(text);
    }
}

fn handle_confirm_delete_input(
    app: &mut PickerApp,
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

fn render(f: &mut ratatui::Frame, app: &mut PickerApp) {
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
            command_input,
            label_input,
            focus,
            error,
        } => render_form_dialog(
            f,
            editing.is_some(),
            command_input,
            label_input,
            focus,
            error.as_deref(),
            t,
        ),
        Mode::ConfirmDelete { command } => render_confirm_delete_dialog(f, command, t),
        Mode::Fill(fill) => render_fill_dialog(f, fill, t),
        Mode::Browse => {}
    }
}

fn render_header(f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
    let header_line = Line::from(vec![Span::styled(
        "SUVADU BOOKMARKS",
        Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
    )]);
    f.render_widget(
        Paragraph::new(header_line).alignment(Alignment::Center),
        area,
    );
}

fn render_search_box(f: &mut ratatui::Frame, app: &PickerApp, area: Rect, t: &crate::theme::Theme) {
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

fn render_list(f: &mut ratatui::Frame, app: &mut PickerApp, area: Rect, t: &crate::theme::Theme) {
    let items: Vec<ListItem> = app
        .filtered
        .iter()
        .filter_map(|&i| app.bookmarks.get(i))
        .map(|b| {
            let label = b
                .label
                .as_deref()
                .map(|l| format!("  ({l})"))
                .unwrap_or_default();
            ListItem::new(Line::from(format!("{}{label}", b.command)))
        })
        .collect();

    let title = format!(
        " Bookmarks ({}/{}) ",
        app.filtered.len(),
        app.bookmarks.len()
    );
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

    if app.bookmarks.is_empty() {
        let hint = Paragraph::new(Line::from(Span::styled(
            "  No bookmarks yet. Press Ctrl+A to add one.",
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
            "  No bookmarks match your search.",
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

fn render_footer(f: &mut ratatui::Frame, app: &PickerApp, area: Rect, t: &crate::theme::Theme) {
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
        Mode::Fill(fill) => vec![
            Span::styled(" Esc ", badge_key),
            Span::styled(" Back  ", badge_label),
            Span::styled(" Tab ", badge_key),
            Span::styled(" Next field  ", badge_label),
            Span::styled(" Enter ", badge_key),
            Span::styled(
                if fill.is_last() {
                    " Recall  "
                } else {
                    " Next  "
                },
                badge_label,
            ),
        ],
        Mode::ConfirmDelete { .. } => vec![
            Span::styled(" y ", badge_key),
            Span::styled(" Confirm  ", badge_label),
            Span::styled(" n/Esc ", badge_key),
            Span::styled(" Cancel  ", badge_label),
        ],
        Mode::Browse => vec![
            Span::styled(" Esc ", badge_key),
            Span::styled(format!(" {}  ", app.leave_label), badge_label),
            Span::styled(" ↑↓ ", badge_key),
            Span::styled(" Navigate  ", badge_label),
            Span::styled(" Enter ", badge_key),
            Span::styled(" Recall  ", badge_label),
            Span::styled(" ^A ", badge_key),
            Span::styled(" Add  ", badge_label),
            Span::styled(" ^E ", badge_key),
            Span::styled(" Edit  ", badge_label),
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
    command_input: &str,
    label_input: &str,
    focus: &FormFocus,
    error: Option<&str>,
    t: &crate::theme::Theme,
) {
    // 2 (outer border) + 3 (Command field) + 3 (Label field) + 1 (error line).
    let area = centered_rect(f.area(), 60, 9);
    f.render_widget(Clear, area);

    let title = if editing {
        " Edit Bookmark "
    } else {
        " Add Bookmark "
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

    render_field(
        f,
        rows[0],
        "Command",
        command_input,
        *focus == FormFocus::Command,
        t,
    );
    render_field(
        f,
        rows[1],
        "Label (optional)",
        label_input,
        *focus == FormFocus::Label,
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

fn render_fill_dialog(f: &mut ratatui::Frame, fill: &FillState, t: &crate::theme::Theme) {
    let placeholders = fill.template.placeholders();
    // Rows that fit: the popup keeps 2 border rows, a blank line and two
    // preview lines for itself and scrolls the fields past that.
    let max_rows = usize::from(f.area().height.saturating_sub(7)).max(1);
    let shown = placeholders.len().min(max_rows);
    let first = (fill.focus + 1).saturating_sub(shown);
    let height = u16::try_from(shown + 5).unwrap_or(u16::MAX);
    let width = (f.area().width * 80 / 100).clamp(40, 90);
    let area = centered_rect(f.area(), width, height);
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_focus))
        .title(Span::styled(
            " Fill Placeholders ",
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let name_width = placeholders
        .iter()
        .map(|p| p.name.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines: Vec<Line> = placeholders
        .iter()
        .zip(&fill.values)
        .enumerate()
        .skip(first)
        .take(shown)
        .map(|(i, (p, value))| {
            let focused = i == fill.focus;
            let marker = if focused { "> " } else { "  " };
            let name_style = if focused {
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.text_secondary)
            };
            let cursor = if focused { "▏" } else { "" };
            Line::from(vec![
                Span::styled(marker, name_style),
                Span::styled(format!("{:<name_width$}  ", p.name), name_style),
                Span::styled(format!("{value}{cursor}"), Style::default().fg(t.text)),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  → ", Style::default().fg(t.text_muted)),
        Span::styled(fill.filled(), Style::default().fg(t.text)),
    ]));
    f.render_widget(
        Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
        inner,
    );
}

fn render_confirm_delete_dialog(f: &mut ratatui::Frame, command: &str, t: &crate::theme::Theme) {
    let area = centered_rect(f.area(), 60, 3);
    f.render_widget(Clear, area);

    let display = crate::util::truncate_str(command, 48, "…");
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
        Span::styled("Delete bookmark ", Style::default().fg(t.text)),
        Span::styled(
            format!("'{display}'"),
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

    fn make_bookmark(id: i64, command: &str, label: Option<&str>) -> Bookmark {
        Bookmark {
            id,
            command: command.to_string(),
            label: label.map(String::from),
            created_at: 1000,
        }
    }

    #[test]
    fn new_app_selects_first_when_nonempty() {
        let app = PickerApp::new(vec![make_bookmark(1, "git status", None)]);
        assert_eq!(app.list_state.selected(), Some(0));
        assert_eq!(app.filtered, vec![0]);
    }

    fn ctrl_c() -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    #[test]
    fn opened_from_home_the_quit_hint_says_home() {
        let mut app = PickerApp::new(vec![make_bookmark(1, "ls -la", None)]);
        let screen = crate::test_utils::render_to_text(120, 20, |f| render(f, &mut app));
        assert!(screen.contains("Esc  Quit"), "{screen}");

        app.leave_label = "Home";
        let screen = crate::test_utils::render_to_text(120, 20, |f| render(f, &mut app));
        assert!(screen.contains("Esc  Home"), "{screen}");
        assert!(!screen.contains("Quit"), "{screen}");
    }

    #[test]
    fn ctrl_c_cancels_the_bookmark_picker_from_every_mode() {
        let (_dir, repo) = crate::test_utils::test_repo();
        repo.add_bookmark("git status", None).unwrap();

        let mut app = PickerApp::new(repo.list_bookmarks().unwrap());
        assert!(matches!(
            handle_key(&mut app, &repo, ctrl_c()).unwrap(),
            Step::Leave(None)
        ));
        assert!(app.query.is_empty());

        let mut app = PickerApp::new(repo.list_bookmarks().unwrap());
        app.start_add();
        assert!(matches!(
            handle_key(&mut app, &repo, ctrl_c()).unwrap(),
            Step::Leave(None)
        ));

        let mut app = PickerApp::new(repo.list_bookmarks().unwrap());
        app.start_delete();
        assert!(matches!(
            handle_key(&mut app, &repo, ctrl_c()).unwrap(),
            Step::Leave(None)
        ));
        assert_eq!(repo.list_bookmarks().unwrap().len(), 1, "nothing deleted");
    }

    fn key(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(app: &mut PickerApp, repo: &Repository, text: &str) {
        for c in text.chars() {
            assert!(matches!(
                handle_key(app, repo, key(KeyCode::Char(c))).unwrap(),
                Step::Stay
            ));
        }
    }

    fn leave(step: Step) -> Option<String> {
        match step {
            Step::Leave(picked) => picked,
            Step::Stay => panic!("expected the picker to close"),
        }
    }

    #[test]
    fn picking_a_plain_bookmark_recalls_it_without_a_popup() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = PickerApp::new(vec![make_bookmark(1, "git status", None)]);
        let picked = leave(handle_key(&mut app, &repo, key(KeyCode::Enter)).unwrap());
        assert_eq!(picked.as_deref(), Some("git status"));
    }

    #[test]
    fn picking_a_parameterised_bookmark_asks_for_each_value_then_recalls_it_filled() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = PickerApp::new(vec![make_bookmark(
            1,
            "ssh {{user:root}}@{{host}} -p {{port:22}} # {{host}}",
            None,
        )]);
        assert!(matches!(
            handle_key(&mut app, &repo, key(KeyCode::Enter)).unwrap(),
            Step::Stay
        ));
        let Mode::Fill(fill) = &app.mode else {
            panic!("expected the placeholder popup");
        };
        // One field per distinct placeholder, prefilled with its default.
        assert_eq!(fill.values, vec!["root", "", "22"]);

        // Enter on a field that is not the last moves on, like Tab.
        handle_key(&mut app, &repo, key(KeyCode::Enter)).unwrap();
        type_text(&mut app, &repo, "db1");
        handle_key(&mut app, &repo, key(KeyCode::Tab)).unwrap();
        handle_key(&mut app, &repo, key(KeyCode::Backspace)).unwrap();
        type_text(&mut app, &repo, "3");

        let screen = crate::test_utils::render_to_text(100, 20, |f| render(f, &mut app));
        assert!(screen.contains("Fill Placeholders"), "{screen}");
        assert!(screen.contains("ssh root@db1 -p 23 # db1"), "{screen}");
        assert!(screen.contains("Esc  Back"), "{screen}");

        let picked = leave(handle_key(&mut app, &repo, key(KeyCode::Enter)).unwrap());
        assert_eq!(picked.as_deref(), Some("ssh root@db1 -p 23 # db1"));
    }

    #[test]
    fn esc_in_the_popup_cancels_the_pick_and_ctrl_c_quits() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = PickerApp::new(vec![make_bookmark(1, "echo {{msg}}", None)]);
        handle_key(&mut app, &repo, key(KeyCode::Enter)).unwrap();
        assert!(matches!(app.mode, Mode::Fill(_)));
        assert!(matches!(
            handle_key(&mut app, &repo, key(KeyCode::Esc)).unwrap(),
            Step::Stay
        ));
        assert!(matches!(app.mode, Mode::Browse), "back to the list");

        handle_key(&mut app, &repo, key(KeyCode::Enter)).unwrap();
        assert!(leave(handle_key(&mut app, &repo, ctrl_c()).unwrap()).is_none());
    }

    #[test]
    fn popup_values_are_capped_and_a_pasted_line_break_becomes_a_space() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = PickerApp::new(vec![make_bookmark(1, "echo {{msg}}", None)]);
        handle_key(&mut app, &repo, key(KeyCode::Enter)).unwrap();
        handle_paste(&mut app, "a\nb");
        handle_paste(&mut app, &"x".repeat(MAX_FIELD_LEN * 2));
        let Mode::Fill(fill) = &app.mode else {
            panic!("expected the placeholder popup");
        };
        assert!(fill.values[0].starts_with("a b"));
        assert_eq!(fill.values[0].len(), MAX_FIELD_LEN);
        // Control-modified letters are shortcuts, never text.
        handle_key(
            &mut app,
            &repo,
            crossterm::event::KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        )
        .unwrap();
        let Mode::Fill(fill) = &app.mode else {
            panic!("expected the placeholder popup");
        };
        assert_eq!(fill.values[0].len(), MAX_FIELD_LEN);
    }

    #[test]
    fn shift_tab_moves_back_a_field() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = PickerApp::new(vec![make_bookmark(1, "{{a}} {{b}}", None)]);
        handle_key(&mut app, &repo, key(KeyCode::Enter)).unwrap();
        handle_key(&mut app, &repo, key(KeyCode::Tab)).unwrap();
        handle_key(&mut app, &repo, key(KeyCode::BackTab)).unwrap();
        type_text(&mut app, &repo, "1");
        handle_key(&mut app, &repo, key(KeyCode::Tab)).unwrap();
        type_text(&mut app, &repo, "2");
        let picked = leave(handle_key(&mut app, &repo, key(KeyCode::Enter)).unwrap());
        assert_eq!(picked.as_deref(), Some("1 2"));
    }

    #[test]
    fn new_app_selects_none_when_empty() {
        let app = PickerApp::new(vec![]);
        assert_eq!(app.list_state.selected(), None);
        assert!(app.filtered.is_empty());
    }

    #[test]
    fn filter_matches_command_text_case_insensitively() {
        let mut app = PickerApp::new(vec![
            make_bookmark(1, "git status", None),
            make_bookmark(2, "cargo build", None),
        ]);
        app.query = "GIT".to_string();
        app.apply_filter();
        assert_eq!(app.filtered, vec![0]);
    }

    #[test]
    fn filter_matches_label_text() {
        let mut app = PickerApp::new(vec![
            make_bookmark(1, "git status", Some("check repo")),
            make_bookmark(2, "cargo build", None),
        ]);
        app.query = "check".to_string();
        app.apply_filter();
        assert_eq!(app.filtered, vec![0]);
    }

    #[test]
    fn filter_with_no_matches_clears_selection() {
        let mut app = PickerApp::new(vec![make_bookmark(1, "git status", None)]);
        app.query = "nonexistent".to_string();
        app.apply_filter();
        assert!(app.filtered.is_empty());
        assert_eq!(app.list_state.selected(), None);
    }

    #[test]
    fn empty_query_shows_all_bookmarks() {
        let mut app = PickerApp::new(vec![
            make_bookmark(1, "git status", None),
            make_bookmark(2, "cargo build", None),
        ]);
        app.query = "cargo".to_string();
        app.apply_filter();
        assert_eq!(app.filtered.len(), 1);
        app.query.clear();
        app.apply_filter();
        assert_eq!(app.filtered.len(), 2);
    }

    #[test]
    fn move_down_clamps_at_last_filtered_item() {
        let mut app = PickerApp::new(vec![
            make_bookmark(1, "a", None),
            make_bookmark(2, "b", None),
        ]);
        app.move_down();
        assert_eq!(app.list_state.selected(), Some(1));
        app.move_down();
        assert_eq!(app.list_state.selected(), Some(1));
    }

    #[test]
    fn move_up_clamps_at_zero() {
        let mut app = PickerApp::new(vec![make_bookmark(1, "a", None)]);
        app.move_up();
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn selected_command_returns_filtered_entry() {
        let mut app = PickerApp::new(vec![
            make_bookmark(1, "git status", None),
            make_bookmark(2, "cargo build", None),
        ]);
        app.query = "cargo".to_string();
        app.apply_filter();
        assert_eq!(app.selected_command(), Some("cargo build".to_string()));
    }

    #[test]
    fn selected_command_none_when_list_empty() {
        let app = PickerApp::new(vec![]);
        assert_eq!(app.selected_command(), None);
    }

    #[test]
    fn submit_form_rejects_empty_command() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = PickerApp::new(vec![]);
        app.mode = Mode::Form {
            editing: None,
            command_input: "   ".to_string(),
            label_input: String::new(),
            focus: FormFocus::Label,
            error: None,
        };
        app.submit_form(&repo).unwrap();
        // Still in Form mode with an error, nothing written.
        assert!(matches!(app.mode, Mode::Form { .. }));
        assert!(app.bookmarks.is_empty());
    }

    #[test]
    fn submit_form_adds_bookmark_and_returns_to_browse() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = PickerApp::new(vec![]);
        app.mode = Mode::Form {
            editing: None,
            command_input: "git status".to_string(),
            label_input: "check repo".to_string(),
            focus: FormFocus::Label,
            error: None,
        };
        app.submit_form(&repo).unwrap();
        assert!(matches!(app.mode, Mode::Browse));
        assert_eq!(app.bookmarks.len(), 1);
        assert_eq!(app.bookmarks[0].command, "git status");
        assert_eq!(app.bookmarks[0].label.as_deref(), Some("check repo"));
        assert_eq!(app.filtered.len(), 1);
        assert_eq!(
            app.status.as_ref().map(|(m, _)| m.as_str()),
            Some("Added: git status")
        );
    }

    #[test]
    fn submit_form_trims_whitespace_and_empty_label_becomes_none() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = PickerApp::new(vec![]);
        app.mode = Mode::Form {
            editing: None,
            command_input: "  cargo test  ".to_string(),
            label_input: "   ".to_string(),
            focus: FormFocus::Label,
            error: None,
        };
        app.submit_form(&repo).unwrap();
        assert_eq!(app.bookmarks[0].command, "cargo test");
        assert_eq!(app.bookmarks[0].label, None);
    }

    #[test]
    fn start_edit_prefills_form_from_selected_bookmark() {
        let mut app = PickerApp::new(vec![make_bookmark(1, "git status", Some("check repo"))]);
        app.start_edit();
        match app.mode {
            Mode::Form {
                editing,
                command_input,
                label_input,
                ..
            } => {
                assert_eq!(editing.as_deref(), Some("git status"));
                assert_eq!(command_input, "git status");
                assert_eq!(label_input, "check repo");
            }
            _ => panic!("expected Form mode"),
        }
    }

    #[test]
    fn start_edit_on_empty_list_is_noop() {
        let mut app = PickerApp::new(vec![]);
        app.start_edit();
        assert!(matches!(app.mode, Mode::Browse));
    }

    #[test]
    fn submit_form_editing_same_command_updates_label_in_place() {
        let (_dir, repo) = crate::test_utils::test_repo();
        repo.add_bookmark("git status", Some("old label")).unwrap();
        let mut app = PickerApp::new(repo.list_bookmarks().unwrap());
        app.mode = Mode::Form {
            editing: Some("git status".to_string()),
            command_input: "git status".to_string(),
            label_input: "new label".to_string(),
            focus: FormFocus::Label,
            error: None,
        };
        app.submit_form(&repo).unwrap();
        assert_eq!(app.bookmarks.len(), 1);
        assert_eq!(app.bookmarks[0].label.as_deref(), Some("new label"));
        assert_eq!(
            app.status.as_ref().map(|(m, _)| m.as_str()),
            Some("Updated: git status")
        );
    }

    #[test]
    fn submit_form_editing_renamed_command_removes_old_row() {
        let (_dir, repo) = crate::test_utils::test_repo();
        repo.add_bookmark("git status", None).unwrap();
        let mut app = PickerApp::new(repo.list_bookmarks().unwrap());
        app.mode = Mode::Form {
            editing: Some("git status".to_string()),
            command_input: "git status -sb".to_string(),
            label_input: String::new(),
            focus: FormFocus::Label,
            error: None,
        };
        app.submit_form(&repo).unwrap();
        // Only the renamed bookmark remains — no leftover "git status" row.
        assert_eq!(app.bookmarks.len(), 1);
        assert_eq!(app.bookmarks[0].command, "git status -sb");
    }

    #[test]
    fn start_delete_opens_confirm_dialog_with_selected_command() {
        let mut app = PickerApp::new(vec![make_bookmark(1, "git status", None)]);
        app.start_delete();
        match app.mode {
            Mode::ConfirmDelete { command } => assert_eq!(command, "git status"),
            _ => panic!("expected ConfirmDelete mode"),
        }
    }

    #[test]
    fn start_delete_on_empty_list_is_noop() {
        let mut app = PickerApp::new(vec![]);
        app.start_delete();
        assert!(matches!(app.mode, Mode::Browse));
    }

    #[test]
    fn confirm_delete_removes_bookmark_and_returns_to_browse() {
        let (_dir, repo) = crate::test_utils::test_repo();
        repo.add_bookmark("git status", None).unwrap();
        let mut app = PickerApp::new(repo.list_bookmarks().unwrap());
        app.mode = Mode::ConfirmDelete {
            command: "git status".to_string(),
        };
        app.confirm_delete(&repo).unwrap();
        assert!(matches!(app.mode, Mode::Browse));
        assert!(app.bookmarks.is_empty());
        assert_eq!(
            app.status.as_ref().map(|(m, _)| m.as_str()),
            Some("Removed: git status")
        );
    }
}
