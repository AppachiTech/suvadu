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
pub(crate) fn filtered_skill_indices(skills: &[Skill], query: &str) -> Vec<usize> {
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

pub(crate) fn copy_feedback_message(name: &str) -> String {
    format!("Copied '{name}' to clipboard")
}

/// Write `initial` to a temp file, run $EDITOR/$VISUAL (falling back to
/// `vi`) on it, and read the result back. Returns `None` if the editor
/// exits non-zero (treated as a cancel), matching `git commit`'s convention.
pub(crate) fn edit_body(initial: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let tmp = tempfile::Builder::new().suffix(".md").tempfile()?;
    std::fs::write(tmp.path(), initial)?;
    let status = std::process::Command::new(&editor).arg(tmp.path()).status()?;
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

pub(crate) fn parse_triggers(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn format_sync_status(report: &crate::skills_sync::SyncReport) -> (StatusLevel, String) {
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
pub(crate) fn reselect_after_removal(removed_index: usize, remaining_len: usize) -> Option<usize> {
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
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};
use std::io;

pub(crate) enum StatusLevel {
    Info,
    Error,
}

enum Mode {
    Browse,
    ConfirmDelete { name: String, scope: String },
    Form(FormState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormField {
    Name,
    Description,
    Triggers,
    Scope,
}

pub(crate) struct FormState {
    editing: Option<Skill>, // Some = edit, None = add
    name: String,
    description: String,
    triggers: String,
    scope_is_global: bool,
    focus: FormField,
}

impl FormState {
    fn new_add() -> Self {
        Self {
            editing: None,
            name: String::new(),
            description: String::new(),
            triggers: String::new(),
            scope_is_global: true,
            focus: FormField::Name,
        }
    }

    fn next_field(&mut self) {
        self.focus = match self.focus {
            FormField::Name => FormField::Description,
            FormField::Description => FormField::Triggers,
            FormField::Triggers => FormField::Scope,
            FormField::Scope => FormField::Name,
        };
    }

    fn focused_text_mut(&mut self) -> Option<&mut String> {
        match self.focus {
            FormField::Name if self.editing.is_none() => Some(&mut self.name),
            FormField::Description => Some(&mut self.description),
            FormField::Triggers => Some(&mut self.triggers),
            _ => None,
        }
    }
}

pub(crate) fn form_from_existing(skill: &Skill) -> FormState {
    FormState {
        editing: Some(skill.clone()),
        name: skill.name.clone(),
        description: skill.description.clone(),
        triggers: skill.triggers.join(", "),
        scope_is_global: skill.scope == crate::models::SKILL_SCOPE_GLOBAL,
        focus: FormField::Description,
    }
}

pub(crate) struct SkillsApp {
    skills: Vec<Skill>,
    query: String,
    filtered: Vec<usize>,
    list_state: ListState,
    status_message: Option<(StatusLevel, String)>,
    mode: Mode,
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

        match &app.mode {
            Mode::Browse => match key.code {
                KeyCode::Esc => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.mode = Mode::Form(FormState::new_add());
                }
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(skill) = app.selected_skill() {
                        app.mode = Mode::Form(form_from_existing(skill));
                    }
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
                    app.status_message = match crate::skills_sync::sync(repo, &targets, &cwd, false)
                    {
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
                        app.status_message = match arboard::Clipboard::new()
                            .and_then(|mut c| c.set_text(body))
                        {
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
            },
            Mode::ConfirmDelete { name, scope } => {
                let (name, scope) = (name.clone(), scope.clone());
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        match repo.delete_skill(&name, &scope) {
                            Ok(_) => {
                                let removed_index = app.list_state.selected().unwrap_or(0);
                                app.skills = repo.list_skills(None, Some(SKILL_STATUS_ACTIVE))?;
                                app.refresh_filter();
                                app.list_state.select(reselect_after_removal(
                                    removed_index,
                                    app.filtered.len(),
                                ));
                                app.status_message =
                                    Some((StatusLevel::Info, format!("Deleted '{name}'")));
                            }
                            Err(e) => {
                                app.status_message =
                                    Some((StatusLevel::Error, format!("Delete failed: {e}")));
                            }
                        }
                        app.mode = Mode::Browse;
                    }
                    _ => app.mode = Mode::Browse,
                }
            }
            Mode::Form(_) => {
                let Mode::Form(mut form) = std::mem::replace(&mut app.mode, Mode::Browse) else {
                    unreachable!()
                };
                match key.code {
                    KeyCode::Esc => { /* discard `form`, stay in Browse */ }
                    KeyCode::Tab => {
                        form.next_field();
                        app.mode = Mode::Form(form);
                    }
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
                    KeyCode::Char(c) if form.focus != FormField::Scope => {
                        if let Some(field) = form.focused_text_mut() {
                            field.push(c);
                        }
                        app.mode = Mode::Form(form);
                    }
                    KeyCode::Enter => {
                        let initial = form
                            .editing
                            .as_ref()
                            .map_or_else(String::new, |s| s.body.clone());
                        let edited = suspend_for_editor(&mut terminal, || edit_body(&initial))?;
                        match edited {
                            None => {
                                app.status_message =
                                    Some((StatusLevel::Error, "Editor cancelled".to_string()));
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
                                let triggers = parse_triggers(&form.triggers);
                                match &form.editing {
                                    None => {
                                        // Resolved at submit time, not when the form opened —
                                        // equivalent in practice since nothing in this app changes
                                        // the process's cwd during its lifetime.
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
                                                app.skills =
                                                    repo.list_skills(None, Some(SKILL_STATUS_ACTIVE))?;
                                                app.refresh_filter();
                                                app.status_message = Some((
                                                    StatusLevel::Info,
                                                    format!("Added '{}'", skill.name),
                                                ));
                                            }
                                            Err(e) => {
                                                app.status_message = Some((
                                                    StatusLevel::Error,
                                                    format!("Add failed: {e}"),
                                                ));
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
                                                app.skills =
                                                    repo.list_skills(None, Some(SKILL_STATUS_ACTIVE))?;
                                                app.refresh_filter();
                                                app.status_message = Some((
                                                    StatusLevel::Info,
                                                    format!(
                                                        "Updated '{}' (v{})",
                                                        updated.name, updated.version
                                                    ),
                                                ));
                                            }
                                            Ok(None) => {
                                                app.status_message = Some((
                                                    StatusLevel::Error,
                                                    "Skill disappeared during edit".to_string(),
                                                ));
                                            }
                                            Err(e) => {
                                                app.status_message = Some((
                                                    StatusLevel::Error,
                                                    format!("Edit failed: {e}"),
                                                ));
                                                app.mode = Mode::Form(form);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => app.mode = Mode::Form(form),
                }
            }
        }
    }

    terminal.show_cursor()?;
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
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    let input = Paragraph::new(Line::from(app.query.as_str())).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(" Search skills "),
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
        .map(|s| ListItem::new(Line::from(format!("{}  ({})", s.name, scope_label(&s.scope)))))
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

    let status_line = match &app.status_message {
        Some((StatusLevel::Error, msg)) => {
            Line::from(msg.as_str()).style(Style::default().fg(t.error))
        }
        Some((StatusLevel::Info, msg)) => {
            Line::from(msg.as_str()).style(Style::default().fg(t.success))
        }
        None => Line::from(
            " type to filter · ↑/↓ move · Ctrl+A add · Ctrl+E edit · Ctrl+D delete · Ctrl+S sync · Ctrl+P review · Enter copy · Esc quit ",
        )
        .style(Style::default().fg(t.text_muted)),
    };
    f.render_widget(Paragraph::new(status_line), rows[2]);

    if let Mode::ConfirmDelete { name, .. } = &app.mode {
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

    if let Mode::Form(form) = &app.mode {
        let area = centered_rect(60, 9, f.area());
        f.render_widget(ratatui::widgets::Clear, area);
        let title = if form.editing.is_some() {
            " Edit skill "
        } else {
            " Add skill "
        };
        let scope_text = if form.scope_is_global { "Global" } else { "Here" };
        let lines = vec![
            Line::from(format!(
                "{} Name:        {}",
                if form.focus == FormField::Name { ">" } else { " " },
                form.editing
                    .as_ref()
                    .map_or(form.name.as_str(), |s| s.name.as_str())
            )),
            Line::from(format!(
                "{} Description: {}",
                if form.focus == FormField::Description { ">" } else { " " },
                form.description
            )),
            Line::from(format!(
                "{} Triggers:    {}",
                if form.focus == FormField::Triggers { ">" } else { " " },
                form.triggers
            )),
            Line::from(format!(
                "{} Scope:       {} (space to toggle)",
                if form.focus == FormField::Scope { ">" } else { " " },
                scope_text
            )),
            Line::from(""),
            Line::from("Tab: next field · Enter: edit body in $EDITOR & save · Esc: cancel"),
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
            triggers: triggers.iter().map(|s| s.to_string()).collect(),
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
