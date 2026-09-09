//! Interactive management app for the shared skills library — `suv skills`
//! with no subcommand launches this. Browse/filter with a live preview,
//! add/edit (body via $EDITOR), delete, sync, and review pending
//! agent-proposed skills, all from one screen.
//!
//! This is a presentation layer only: every mutation goes through the same
//! `Repository`/`skills_sync` functions the CLI subcommands in
//! `commands/skills.rs` already use.

use crate::models::Skill;

/// Fuzzy-rank `skills` against `query` (matched against name, description,
/// and triggers) and return their indices best-match-first. Returns every
/// index, in original order, when `query` is empty.
pub fn filtered_skill_indices(skills: &[Skill], query: &str) -> Vec<usize> {
    use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
    use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

    if query.trim().is_empty() {
        return (0..skills.len()).collect();
    }

    let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

    let mut scored: Vec<(usize, u32)> = skills
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let haystack = format!("{} {} {}", s.name, s.description, s.triggers.join(" "));
            let mut buf = Vec::new();
            let haystack = Utf32Str::new(&haystack, &mut buf);
            pattern
                .score(haystack, &mut matcher)
                .map(|score| (i, score))
        })
        .collect();

    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| skills[a.0].name.cmp(&skills[b.0].name))
    });
    scored.into_iter().map(|(i, _)| i).collect()
}

pub fn copy_feedback_message(name: &str) -> String {
    format!("Copied '{name}' to clipboard")
}

/// Write `initial` to a temp file, run $EDITOR/$VISUAL (falling back to
/// `vi`) on it, and read the result back. Returns `None` if the editor
/// exits non-zero (treated as a cancel), matching `git commit`'s convention.
pub fn edit_body(initial: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let tmp = tempfile::Builder::new().suffix(".md").tempfile()?;
    std::fs::write(tmp.path(), initial)?;
    let status = std::process::Command::new(&editor)
        .arg(tmp.path())
        .status()?;
    if !status.success() {
        return Ok(None);
    }
    Ok(Some(std::fs::read_to_string(tmp.path())?))
}

/// Suspend the TUI (leave raw mode + the alternate screen) for the duration
/// of `f`, then restore it and force a full repaint. Used around `edit_body`
/// since `$EDITOR` needs a normal terminal to draw into.
///
/// Rebuilds `terminal` from scratch afterward rather than calling
/// `Terminal::clear()` — `clear()` snapshots and restores the cursor
/// position via `crossterm::cursor::position()`, which hardcodes writing its
/// query to stdout and reading the reply from stdin regardless of which
/// stream the backend itself renders to. Since this app deliberately renders
/// to stderr (keeping stdout clean for piping, matching the rest of this
/// codebase's pickers), that query can go unanswered. A fresh `Terminal`
/// resets the same internal diff-buffer state without needing cursor
/// position at all — construction alone never queries it.
fn suspend_for_editor<T>(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    f: impl FnOnce() -> Result<T, Box<dyn std::error::Error>>,
) -> Result<T, Box<dyn std::error::Error>> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(io::stderr(), crossterm::terminal::LeaveAlternateScreen)?;
    let result = f();
    crossterm::execute!(io::stderr(), crossterm::terminal::EnterAlternateScreen)?;
    crossterm::terminal::enable_raw_mode()?;
    *terminal = Terminal::new(CrosstermBackend::new(io::stderr()))?;
    result
}

pub fn parse_triggers(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn format_sync_status(report: &crate::skills_sync::SyncReport) -> (StatusLevel, String) {
    if report.written == 0 {
        (
            StatusLevel::Info,
            "Nothing to sync — no active skills, or everything already up to date".to_string(),
        )
    } else {
        (
            StatusLevel::Info,
            format!("Synced: {} file(s) written", report.written),
        )
    }
}

/// After removing the item at `removed_index` from a list, what selection
/// index should follow it? `remaining_len` is the list's length *after*
/// removal. Returns `None` if the list is now empty.
pub fn reselect_after_removal(removed_index: usize, remaining_len: usize) -> Option<usize> {
    if remaining_len == 0 {
        None
    } else {
        Some(removed_index.min(remaining_len - 1))
    }
}

use crate::models::SKILL_STATUS_ACTIVE;
use crate::repository::Repository;
use crate::theme::theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};
use std::io;

pub enum StatusLevel {
    Info,
    Error,
}

/// Cap on the Add/Edit form's Name/Description/Triggers fields — plenty for
/// any realistic value, and guards against a stray multi-KB paste (e.g. an
/// entire skill body pasted into the wrong field) ballooning the string.
const MAX_FIELD_LEN: usize = 200;

// `Mode` lives in exactly one `SkillsApp` field (never in a collection), so
// the size difference between variants is a few hundred bytes on one struct,
// not a real cost — not worth the churn of boxing `FormState` everywhere
// it's constructed/matched.
#[allow(clippy::large_enum_variant)]
enum Mode {
    Browse,
    ConfirmDelete { name: String, scope: String },
    Form(FormState),
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Name,
    Description,
    Triggers,
    Scope,
}

pub struct FormState {
    editing: Option<Skill>, // Some = edit, None = add
    name: String,
    description: String,
    triggers: String,
    scope_is_global: bool,
    focus: FormField,
    /// The body most recently returned by `$EDITOR`, kept even if the save
    /// that followed it failed (e.g. a duplicate name) — without this, a
    /// failed save would discard the content the user just wrote and force
    /// a full rewrite on retry.
    pending_body: Option<String>,
}

impl FormState {
    const fn new_add() -> Self {
        Self {
            editing: None,
            name: String::new(),
            description: String::new(),
            triggers: String::new(),
            scope_is_global: true,
            focus: FormField::Name,
            pending_body: None,
        }
    }

    const fn next_field(&mut self) {
        self.focus = match self.focus {
            FormField::Name => FormField::Description,
            FormField::Description => FormField::Triggers,
            FormField::Triggers => FormField::Scope,
            FormField::Scope => FormField::Name,
        };
    }

    const fn focused_text_mut(&mut self) -> Option<&mut String> {
        match self.focus {
            FormField::Name if self.editing.is_none() => Some(&mut self.name),
            FormField::Description => Some(&mut self.description),
            FormField::Triggers => Some(&mut self.triggers),
            _ => None,
        }
    }
}

pub fn form_from_existing(skill: &Skill) -> FormState {
    FormState {
        editing: Some(skill.clone()),
        name: skill.name.clone(),
        description: skill.description.clone(),
        triggers: skill.triggers.join(", "),
        scope_is_global: skill.scope == crate::models::SKILL_SCOPE_GLOBAL,
        focus: FormField::Description,
        pending_body: None,
    }
}

pub struct SkillsApp {
    skills: Vec<Skill>,
    query: String,
    filtered: Vec<usize>,
    list_state: ListState,
    status_message: Option<(StatusLevel, String)>,
    mode: Mode,
    pending: Vec<Skill>,
    pending_state: ListState,
}

impl SkillsApp {
    fn load(repo: &Repository) -> Result<Self, Box<dyn std::error::Error>> {
        let skills = repo.list_skills(None, Some(SKILL_STATUS_ACTIVE))?;
        let filtered = filtered_skill_indices(&skills, "");
        let mut list_state = ListState::default();
        if !filtered.is_empty() {
            list_state.select(Some(0));
        }
        Ok(Self {
            skills,
            query: String::new(),
            filtered,
            list_state,
            status_message: None,
            mode: Mode::Browse,
            pending: Vec::new(),
            pending_state: ListState::default(),
        })
    }

    fn refresh_filter(&mut self) {
        self.filtered = filtered_skill_indices(&self.skills, &self.query);
        self.list_state
            .select((!self.filtered.is_empty()).then_some(0));
    }

    fn selected_skill(&self) -> Option<&Skill> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered.get(i))
            .and_then(|&idx| self.skills.get(idx))
    }
}

/// Launch the interactive skills management app (bare `suv skills`).
pub fn run(repo: &Repository) -> Result<(), Box<dyn std::error::Error>> {
    let _guard = crate::util::TerminalGuardStderr::new()?;
    let backend = CrosstermBackend::new(io::stderr());
    let mut terminal = Terminal::new(backend)?;

    let mut app = SkillsApp::load(repo)?;

    loop {
        terminal.draw(|f| render(f, &mut app))?;

        if !event::poll(std::time::Duration::from_mins(1))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match std::mem::replace(&mut app.mode, Mode::Browse) {
            Mode::Browse => {
                app.mode = Mode::Browse;
                if handle_browse_key(&mut app, key, repo)? {
                    break;
                }
            }
            Mode::ConfirmDelete { name, scope } => {
                handle_confirm_delete_key(&mut app, key, repo, &name, &scope)?;
            }
            Mode::Form(form) => {
                app.mode = Mode::Form(form);
                handle_form_key(&mut app, key, repo, &mut terminal)?;
            }
            Mode::Review => {
                app.mode = Mode::Review;
                handle_review_key(&mut app, key, repo)?;
            }
        }
    }

    terminal.show_cursor()?;
    Ok(())
}

/// Returns `true` if the app should quit.
fn handle_browse_key(
    app: &mut SkillsApp,
    key: crossterm::event::KeyEvent,
    repo: &Repository,
) -> Result<bool, Box<dyn std::error::Error>> {
    match key.code {
        KeyCode::Esc => return Ok(true),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.mode = Mode::Form(FormState::new_add());
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(skill) = app.selected_skill() {
                app.mode = Mode::Form(form_from_existing(skill));
            }
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.pending = repo.list_skills(None, Some(crate::models::SKILL_STATUS_PENDING))?;
            app.pending_state = ListState::default();
            if !app.pending.is_empty() {
                app.pending_state.select(Some(0));
            }
            app.mode = Mode::Review;
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(skill) = app.selected_skill() {
                app.mode = Mode::ConfirmDelete {
                    name: skill.name.clone(),
                    scope: skill.scope.clone(),
                };
            }
        }
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let targets = [
                crate::cli::SyncTarget::ClaudeCode,
                crate::cli::SyncTarget::Cursor,
                crate::cli::SyncTarget::Codex,
            ];
            let cwd = std::env::current_dir()?;
            app.status_message = match crate::skills_sync::sync(repo, &targets, &cwd, false) {
                Ok(report) => Some(format_sync_status(&report)),
                Err(e) => Some((StatusLevel::Error, format!("Sync failed: {e}"))),
            };
        }
        KeyCode::Up => {
            let i = app.list_state.selected().unwrap_or(0);
            app.list_state.select(Some(i.saturating_sub(1)));
        }
        KeyCode::Down => {
            if !app.filtered.is_empty() {
                let i = app.list_state.selected().unwrap_or(0);
                app.list_state
                    .select(Some((i + 1).min(app.filtered.len() - 1)));
            }
        }
        KeyCode::Enter => {
            if let Some(skill) = app.selected_skill() {
                let name = skill.name.clone();
                let body = skill.body.clone();
                app.status_message =
                    match arboard::Clipboard::new().and_then(|mut c| c.set_text(body)) {
                        Ok(()) => Some((StatusLevel::Info, copy_feedback_message(&name))),
                        Err(e) => Some((
                            StatusLevel::Error,
                            format!("Could not copy to clipboard: {e}"),
                        )),
                    };
            }
        }
        KeyCode::Backspace => {
            app.query.pop();
            app.refresh_filter();
        }
        KeyCode::Char(c) => {
            app.query.push(c);
            app.refresh_filter();
        }
        _ => {}
    }
    Ok(false)
}

fn handle_confirm_delete_key(
    app: &mut SkillsApp,
    key: crossterm::event::KeyEvent,
    repo: &Repository,
    name: &str,
    scope: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match key.code {
        KeyCode::Char('y' | 'Y') => {
            match repo.delete_skill(name, scope) {
                Ok(removed) => {
                    let removed_index = app.list_state.selected().unwrap_or(0);
                    app.skills = repo.list_skills(None, Some(SKILL_STATUS_ACTIVE))?;
                    app.refresh_filter();
                    app.list_state
                        .select(reselect_after_removal(removed_index, app.filtered.len()));
                    app.status_message = Some(if removed {
                        (StatusLevel::Info, format!("Deleted '{name}'"))
                    } else {
                        (StatusLevel::Info, format!("'{name}' was already gone"))
                    });
                }
                Err(e) => {
                    app.status_message = Some((StatusLevel::Error, format!("Delete failed: {e}")));
                }
            }
            app.mode = Mode::Browse;
        }
        _ => app.mode = Mode::Browse,
    }
    Ok(())
}

fn handle_form_key(
    app: &mut SkillsApp,
    key: crossterm::event::KeyEvent,
    repo: &Repository,
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Mode::Form(mut form) = std::mem::replace(&mut app.mode, Mode::Browse) else {
        unreachable!()
    };
    match key.code {
        KeyCode::Esc => { /* discard `form`, stay in Browse */ }
        KeyCode::Char(' ') if form.focus == FormField::Scope => {
            form.scope_is_global = !form.scope_is_global;
            app.mode = Mode::Form(form);
        }
        KeyCode::Backspace => {
            if let Some(field) = form.focused_text_mut() {
                field.pop();
            }
            app.mode = Mode::Form(form);
        }
        // Excludes Ctrl+<letter> so Browse's shortcuts (Ctrl+A/E/D/S/P/C)
        // don't leak a literal character into whatever field has focus if
        // muscle memory triggers one of them while a form is open.
        KeyCode::Char(c)
            if form.focus != FormField::Scope && !key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            if let Some(field) = form.focused_text_mut() {
                if field.len() + c.len_utf8() <= MAX_FIELD_LEN {
                    field.push(c);
                }
            }
            app.mode = Mode::Form(form);
        }
        // Tab always advances; Enter advances too except on the last field,
        // where it submits (jumping to $EDITOR for the body). This also
        // protects a multi-line paste into Name/Description/Triggers — an
        // embedded newline just moves focus forward instead of launching
        // the editor mid-paste.
        KeyCode::Enter if form.focus == FormField::Scope => {
            submit_form(app, form, repo, terminal)?;
        }
        KeyCode::Tab | KeyCode::Enter => {
            form.next_field();
            app.mode = Mode::Form(form);
        }
        _ => app.mode = Mode::Form(form),
    }
    Ok(())
}

fn submit_form(
    app: &mut SkillsApp,
    form: FormState,
    repo: &Repository,
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Prefer a body from a previous failed save attempt over the
    // pre-existing/empty starting point, so a retry doesn't lose work.
    let initial = form.pending_body.clone().unwrap_or_else(|| {
        form.editing
            .as_ref()
            .map_or_else(String::new, |s| s.body.clone())
    });
    let edited = suspend_for_editor(terminal, || edit_body(&initial))?;
    match edited {
        None => {
            app.status_message = Some((StatusLevel::Error, "Editor cancelled".to_string()));
            app.mode = Mode::Form(form);
        }
        Some(body) if body.trim().is_empty() => {
            app.status_message = Some((
                StatusLevel::Error,
                "Skill body is empty — not saved".to_string(),
            ));
            app.mode = Mode::Form(form);
        }
        Some(body) => {
            let mut form = form;
            form.pending_body = Some(body.clone());
            save_form(app, form, repo, body)?;
        }
    }
    Ok(())
}

fn save_form(
    app: &mut SkillsApp,
    form: FormState,
    repo: &Repository,
    body: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let triggers = parse_triggers(&form.triggers);
    match &form.editing {
        None => {
            // Resolved at submit time, not when the form opened — equivalent in
            // practice since nothing in this app changes the process's cwd
            // during its lifetime.
            let scope = if form.scope_is_global {
                crate::models::SKILL_SCOPE_GLOBAL.to_string()
            } else {
                std::env::current_dir()?.to_string_lossy().to_string()
            };
            let new = crate::models::NewSkill {
                name: form.name.clone(),
                description: form.description.clone(),
                body,
                triggers,
                scope,
                source: crate::models::SKILL_SOURCE_HUMAN.to_string(),
                status: SKILL_STATUS_ACTIVE.to_string(),
            };
            match repo.create_skill(&new) {
                Ok(skill) => {
                    app.skills = repo.list_skills(None, Some(SKILL_STATUS_ACTIVE))?;
                    app.refresh_filter();
                    app.status_message =
                        Some((StatusLevel::Info, format!("Added '{}'", skill.name)));
                }
                Err(e) => {
                    app.status_message = Some((StatusLevel::Error, format!("Add failed: {e}")));
                    app.mode = Mode::Form(form);
                }
            }
        }
        Some(existing) => {
            match repo.update_skill(
                &existing.name,
                &existing.scope,
                Some(&form.description),
                Some(&body),
                Some(&triggers),
            ) {
                Ok(Some(updated)) => {
                    app.skills = repo.list_skills(None, Some(SKILL_STATUS_ACTIVE))?;
                    app.refresh_filter();
                    app.status_message = Some((
                        StatusLevel::Info,
                        format!("Updated '{}' (v{})", updated.name, updated.version),
                    ));
                }
                Ok(None) => {
                    app.status_message = Some((
                        StatusLevel::Error,
                        "Skill disappeared during edit".to_string(),
                    ));
                }
                Err(e) => {
                    app.status_message = Some((StatusLevel::Error, format!("Edit failed: {e}")));
                    app.mode = Mode::Form(form);
                }
            }
        }
    }
    Ok(())
}

fn handle_review_key(
    app: &mut SkillsApp,
    key: crossterm::event::KeyEvent,
    repo: &Repository,
) -> Result<(), Box<dyn std::error::Error>> {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.mode = Mode::Browse,
        KeyCode::Char('a' | 'r') => apply_review_key(app, key, repo)?,
        KeyCode::Char('s') | KeyCode::Down => {
            if !app.pending.is_empty() {
                let i = app.pending_state.selected().unwrap_or(0);
                app.pending_state
                    .select(Some((i + 1).min(app.pending.len() - 1)));
            }
        }
        KeyCode::Up => {
            let i = app.pending_state.selected().unwrap_or(0);
            app.pending_state.select(Some(i.saturating_sub(1)));
        }
        _ => {}
    }
    Ok(())
}

fn apply_review_key(
    app: &mut SkillsApp,
    key: crossterm::event::KeyEvent,
    repo: &Repository,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(&i) = app.pending_state.selected().as_ref() else {
        return Ok(());
    };
    let Some(skill) = app.pending.get(i).cloned() else {
        return Ok(());
    };
    let approve = key.code == KeyCode::Char('a');
    let decision = if approve {
        crate::commands::skills::ReviewDecision::Approve
    } else {
        crate::commands::skills::ReviewDecision::Reject
    };
    match crate::commands::skills::apply_review_decision(repo, &skill, &decision) {
        Ok(()) => {
            app.pending.remove(i);
            app.pending_state
                .select(reselect_after_removal(i, app.pending.len()));
            let verb = if approve { "Approved" } else { "Rejected" };
            app.status_message = Some((StatusLevel::Info, format!("{verb} '{}'", skill.name)));
            if approve {
                app.skills = repo.list_skills(None, Some(SKILL_STATUS_ACTIVE))?;
                app.refresh_filter();
            }
        }
        Err(e) => {
            app.status_message = Some((StatusLevel::Error, format!("Review failed: {e}")));
        }
    }
    Ok(())
}

fn scope_label(scope: &str) -> String {
    if scope == crate::models::SKILL_SCOPE_GLOBAL {
        crate::models::SKILL_SCOPE_GLOBAL.to_string()
    } else {
        crate::util::truncate_str(scope, 24, "…")
    }
}

fn render(f: &mut ratatui::Frame, app: &mut SkillsApp) {
    let t = theme();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(3), // search box
            Constraint::Min(1),    // body
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    render_header(f, rows[0], t);

    if matches!(app.mode, Mode::Review) {
        render_review(f, app, rows[2]);
        render_review_footer(f, rows[3], t);
        return;
    }

    render_browse(f, app, t, &rows[1..]);
    render_delete_dialog(f, app, t);
    render_form_dialog(f, app, t);
}

fn render_header(f: &mut ratatui::Frame, area: ratatui::layout::Rect, t: &crate::theme::Theme) {
    let header_line = Line::from(vec![Span::styled(
        "SUVADU SKILLS",
        Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
    )]);
    f.render_widget(
        Paragraph::new(header_line).alignment(Alignment::Center),
        area,
    );
}

fn render_review_footer(
    f: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    t: &crate::theme::Theme,
) {
    let badge_key = Style::default().bg(t.badge_bg).fg(t.text);
    let badge_label = Style::default().fg(t.text_secondary);
    let spans = vec![
        Span::styled(" Esc ", badge_key),
        Span::styled(" Back  ", badge_label),
        Span::styled(" ↑↓ ", badge_key),
        Span::styled(" Navigate  ", badge_label),
        Span::styled(" a ", badge_key),
        Span::styled(" Approve  ", badge_label),
        Span::styled(" r ", badge_key),
        Span::styled(" Reject ", badge_label),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_browse(
    f: &mut ratatui::Frame,
    app: &mut SkillsApp,
    t: &crate::theme::Theme,
    rows: &[ratatui::layout::Rect],
) {
    // Dims and drops the "(Typing)" suffix while a dialog has input focus,
    // matching suv search's Search / "Search (Typing)" convention.
    let in_dialog = !matches!(app.mode, Mode::Browse);
    let (search_title, search_border) = if in_dialog {
        ("Search", t.border)
    } else {
        ("Search (Typing)", t.border_focus)
    };
    let input = Paragraph::new(Line::from(app.query.as_str())).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(search_border))
            .title(search_title),
    );
    f.render_widget(input, rows[0]);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(rows[1]);

    let items: Vec<ListItem> = app
        .filtered
        .iter()
        .filter_map(|&i| app.skills.get(i))
        .map(|s| {
            ListItem::new(Line::from(format!(
                "{}  ({})",
                s.name,
                scope_label(&s.scope)
            )))
        })
        .collect();
    let empty = items.is_empty();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(format!(" Skills ({}) ", app.filtered.len())),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD).fg(t.primary))
        .highlight_symbol(" > ");
    f.render_stateful_widget(list, panes[0], &mut app.list_state);

    let preview_text = if empty {
        "No matching skills.".to_string()
    } else {
        app.selected_skill()
            .map(|s| {
                use std::fmt::Write;
                let mut out = format!("{}\n", s.name);
                let _ = writeln!(out, "scope:   {}", s.scope);
                let _ = writeln!(out, "version: {}", s.version);
                if !s.triggers.is_empty() {
                    let _ = writeln!(out, "triggers: {}", s.triggers.join(", "));
                }
                if !s.description.is_empty() {
                    let _ = write!(out, "\n{}\n", s.description);
                }
                let _ = write!(out, "\n{}", s.body);
                out
            })
            .unwrap_or_default()
    };
    let preview = Paragraph::new(preview_text)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(" Preview "),
        );
    f.render_widget(preview, panes[1]);

    render_browse_footer(f, app, rows[2], t);
}

fn render_browse_footer(
    f: &mut ratatui::Frame,
    app: &SkillsApp,
    area: ratatui::layout::Rect,
    t: &crate::theme::Theme,
) {
    let badge_key = Style::default().bg(t.badge_bg).fg(t.text);
    let badge_label = Style::default().fg(t.text_secondary);
    let mut spans = vec![
        Span::styled(" Esc ", badge_key),
        Span::styled(" Quit  ", badge_label),
        Span::styled(" ↑↓ ", badge_key),
        Span::styled(" Navigate  ", badge_label),
        Span::styled(" Enter ", badge_key),
        Span::styled(" Copy  ", badge_label),
        Span::styled(" ^A ", badge_key),
        Span::styled(" Add  ", badge_label),
        Span::styled(" ^E ", badge_key),
        Span::styled(" Edit  ", badge_label),
        Span::styled(" ^D ", badge_key),
        Span::styled(" Delete  ", badge_label),
        Span::styled(" ^S ", badge_key),
        Span::styled(" Sync  ", badge_label),
        Span::styled(" ^P ", badge_key),
        Span::styled(" Review ", badge_label),
    ];

    match &app.status_message {
        Some((StatusLevel::Error, msg)) => {
            spans.push(Span::styled(
                format!(" {msg} "),
                Style::default().fg(t.error).add_modifier(Modifier::BOLD),
            ));
        }
        Some((StatusLevel::Info, msg)) => {
            spans.push(Span::styled(
                format!(" {msg} "),
                Style::default().fg(t.success).add_modifier(Modifier::BOLD),
            ));
        }
        None => {}
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_delete_dialog(f: &mut ratatui::Frame, app: &SkillsApp, t: &crate::theme::Theme) {
    let Mode::ConfirmDelete { name, .. } = &app.mode else {
        return;
    };
    let area = centered_rect(50, 3, f.area());
    f.render_widget(ratatui::widgets::Clear, area);
    let dialog = Paragraph::new(Line::from(format!("Delete skill '{name}'? [y/N]"))).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.error))
            .title(" Confirm delete "),
    );
    f.render_widget(dialog, area);
}

fn render_form_dialog(f: &mut ratatui::Frame, app: &SkillsApp, t: &crate::theme::Theme) {
    let Mode::Form(form) = &app.mode else {
        return;
    };
    // 80% of the terminal (capped at 90 cols) rather than a fixed 60 —
    // roomier for pasted names/descriptions/trigger lists.
    let width = (f.area().width * 80 / 100).clamp(60, 90);
    let area = centered_rect(width, 13, f.area());
    f.render_widget(ratatui::widgets::Clear, area);
    let title = if form.editing.is_some() {
        " Edit skill "
    } else {
        " Add skill "
    };
    let scope_text = if form.scope_is_global {
        "Global"
    } else {
        "Here"
    };
    let marker = |field: FormField| if form.focus == field { ">" } else { " " };
    let lines = vec![
        Line::from(""),
        Line::from(format!(
            "{} Name:        {}",
            marker(FormField::Name),
            form.editing
                .as_ref()
                .map_or(form.name.as_str(), |s| s.name.as_str())
        )),
        Line::from(format!(
            "{} Description: {}",
            marker(FormField::Description),
            form.description
        )),
        Line::from(format!(
            "{} Triggers:    {}",
            marker(FormField::Triggers),
            form.triggers
        )),
        Line::from(format!(
            "{} Scope:       {} (space to toggle)",
            marker(FormField::Scope),
            scope_text
        )),
        Line::from(""),
        Line::from("  Tab / Enter   Next field"),
        Line::from("  Enter (Scope) Edit body in $EDITOR & save — paste works there too"),
        Line::from("  Esc           Cancel"),
    ];
    let dialog = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(title),
    );
    f.render_widget(dialog, area);
}

fn render_review(f: &mut ratatui::Frame, app: &mut SkillsApp, area: ratatui::layout::Rect) {
    let t = theme();
    if app.pending.is_empty() {
        let msg = Paragraph::new(Line::from("No skills awaiting review.")).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(" Review queue "),
        );
        f.render_widget(msg, area);
        return;
    }

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    let items: Vec<ListItem> = app
        .pending
        .iter()
        .map(|s| {
            ListItem::new(Line::from(format!(
                "{}  ({})",
                s.name,
                scope_label(&s.scope)
            )))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(format!(" Review queue ({}) ", app.pending.len())),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD).fg(t.primary))
        .highlight_symbol(" > ");
    f.render_stateful_widget(list, panes[0], &mut app.pending_state);

    let preview_text = app
        .pending_state
        .selected()
        .and_then(|i| app.pending.get(i))
        .map(|s| {
            use std::fmt::Write;
            let mut out = format!("{}\n", s.name);
            let _ = writeln!(out, "scope:   {}", s.scope);
            let _ = writeln!(out, "source:  {}", s.source);
            if !s.description.is_empty() {
                let _ = write!(out, "\n{}\n", s.description);
            }
            let _ = write!(out, "\n{}", s.body);
            out
        })
        .unwrap_or_default();
    let preview = Paragraph::new(preview_text)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(" Preview "),
        );
    f.render_widget(preview, panes[1]);
}

fn centered_rect(width: u16, height: u16, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    ratatui::layout::Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SKILL_SCOPE_GLOBAL;

    #[test]
    fn copy_feedback_names_the_skill() {
        assert_eq!(
            copy_feedback_message("release"),
            "Copied 'release' to clipboard"
        );
    }

    #[test]
    fn form_from_existing_prefills_editable_fields() {
        let s = skill("release", "cut a release", &["release", "changelog"]);
        let form = form_from_existing(&s);
        assert!(form.editing.is_some());
        assert_eq!(form.description, "cut a release");
        assert_eq!(form.triggers, "release, changelog");
        assert_eq!(form.focus, FormField::Description);
    }

    #[test]
    fn edit_body_roundtrips_through_a_fake_editor() {
        let dir = tempfile::TempDir::new().unwrap();
        let fake_editor = dir.path().join("fake_editor.sh");
        std::fs::write(
            &fake_editor,
            "#!/bin/sh\necho 'edited by fake editor' >> \"$1\"\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&fake_editor).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&fake_editor, perms).unwrap();

        let prev = std::env::var("EDITOR").ok();
        std::env::set_var("EDITOR", &fake_editor);
        let result = edit_body("original content\n").unwrap();
        match prev {
            Some(v) => std::env::set_var("EDITOR", v),
            None => std::env::remove_var("EDITOR"),
        }

        let content = result.unwrap();
        assert!(content.contains("original content"));
        assert!(content.contains("edited by fake editor"));
    }

    #[test]
    fn edit_body_returns_none_when_editor_exits_nonzero() {
        let prev = std::env::var("EDITOR").ok();
        std::env::set_var("EDITOR", "false");
        let result = edit_body("content").unwrap();
        match prev {
            Some(v) => std::env::set_var("EDITOR", v),
            None => std::env::remove_var("EDITOR"),
        }
        assert!(result.is_none());
    }

    #[test]
    fn parse_triggers_splits_trims_and_drops_empties() {
        assert_eq!(
            parse_triggers("release, changelog ,, ship it "),
            vec!["release", "changelog", "ship it"]
        );
    }

    #[test]
    fn parse_triggers_empty_string_yields_empty_vec() {
        assert!(parse_triggers("").is_empty());
        assert!(parse_triggers("   ").is_empty());
    }

    #[test]
    fn format_sync_status_reports_written_count() {
        let report = crate::skills_sync::SyncReport {
            written: 3,
            lines: vec!["wrote a".into(), "wrote b".into()],
        };
        let (level, msg) = format_sync_status(&report);
        assert!(matches!(level, StatusLevel::Info));
        assert_eq!(msg, "Synced: 3 file(s) written");
    }

    #[test]
    fn format_sync_status_reports_nothing_to_sync() {
        let report = crate::skills_sync::SyncReport {
            written: 0,
            lines: vec![],
        };
        let (level, msg) = format_sync_status(&report);
        assert!(matches!(level, StatusLevel::Info));
        assert_eq!(
            msg,
            "Nothing to sync — no active skills, or everything already up to date"
        );
    }

    #[test]
    fn reselect_after_removal_keeps_same_index_when_items_remain_after_it() {
        assert_eq!(reselect_after_removal(0, 2), Some(0));
    }

    #[test]
    fn reselect_after_removal_clamps_to_new_last_index_when_it_was_last() {
        assert_eq!(reselect_after_removal(2, 2), Some(1));
    }

    #[test]
    fn reselect_after_removal_returns_none_when_list_becomes_empty() {
        assert_eq!(reselect_after_removal(0, 0), None);
    }

    fn skill(name: &str, description: &str, triggers: &[&str]) -> Skill {
        Skill {
            id: name.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            body: format!("body of {name}"),
            triggers: triggers.iter().map(ToString::to_string).collect(),
            scope: SKILL_SCOPE_GLOBAL.to_string(),
            source: "human".to_string(),
            status: "active".to_string(),
            version: 1,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn empty_query_returns_all_in_original_order() {
        let skills = vec![skill("release", "", &[]), skill("commit-style", "", &[])];
        assert_eq!(filtered_skill_indices(&skills, ""), vec![0, 1]);
    }

    #[test]
    fn matches_by_name() {
        let skills = vec![
            skill("release", "cut a release", &[]),
            skill("commit-style", "how to write commits", &[]),
        ];
        assert_eq!(filtered_skill_indices(&skills, "rel"), vec![0]);
    }

    #[test]
    fn matches_by_description_and_triggers_not_just_name() {
        let skills = vec![
            skill("deploy-checklist", "steps before a deploy", &["ship it"]),
            skill("unrelated", "something else entirely", &[]),
        ];
        assert_eq!(filtered_skill_indices(&skills, "ship"), vec![0]);
        assert_eq!(filtered_skill_indices(&skills, "deploy"), vec![0]);
    }

    #[test]
    fn no_match_returns_empty() {
        let skills = vec![skill("release", "cut a release", &[])];
        assert!(filtered_skill_indices(&skills, "xyzzy-nonsense").is_empty());
    }

    #[test]
    fn ranks_better_match_first() {
        let skills = vec![
            skill("release-notes-formatter", "", &[]),
            skill("release", "", &[]),
        ];
        assert_eq!(filtered_skill_indices(&skills, "release")[0], 1);
    }
}
