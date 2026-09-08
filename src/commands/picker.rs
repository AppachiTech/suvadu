//! Small interactive list pickers. Render to stderr (via `TerminalGuardStderr`)
//! so the chosen value can be printed to stdout for a shell wrapper (bookmarks)
//! or for piping (skills) to consume.

use crate::models::{Bookmark, Skill};
use crate::theme::theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph},
    Terminal,
};
use std::io;

/// Show a picker over `bookmarks` and return the selected command, or `None`
/// if the user cancelled.
pub fn pick_bookmark(bookmarks: &[Bookmark]) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let _guard = crate::util::TerminalGuardStderr::new()?;
    let backend = CrosstermBackend::new(io::stderr());
    let mut terminal = Terminal::new(backend)?;

    let mut state = ListState::default();
    state.select(Some(0));

    let result = loop {
        terminal.draw(|f| render(f, bookmarks, &mut state))?;

        if !event::poll(std::time::Duration::from_mins(1))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => break None,
                KeyCode::Up | KeyCode::Char('k') => {
                    let i = state.selected().unwrap_or(0);
                    state.select(Some(i.saturating_sub(1)));
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let i = state.selected().unwrap_or(0);
                    state.select(Some((i + 1).min(bookmarks.len().saturating_sub(1))));
                }
                KeyCode::Enter => {
                    let cmd = state
                        .selected()
                        .and_then(|i| bookmarks.get(i))
                        .map(|b| b.command.clone());
                    break cmd;
                }
                _ => {}
            }
        }
    };

    terminal.show_cursor()?;
    Ok(result)
}

fn render(f: &mut ratatui::Frame, bookmarks: &[Bookmark], state: &mut ListState) {
    let t = theme();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

    let items: Vec<ListItem> = bookmarks
        .iter()
        .map(|b| {
            let label = b
                .label
                .as_deref()
                .map(|l| format!("  ({l})"))
                .unwrap_or_default();
            ListItem::new(Line::from(format!("{}{label}", b.command)))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(" Bookmarks "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD).fg(t.primary))
        .highlight_symbol(" > ");
    f.render_stateful_widget(list, chunks[0], state);

    let help = Paragraph::new(Line::from(" ↑/↓ or j/k move · Enter recall · Esc cancel "))
        .style(Style::default().fg(t.text_muted));
    f.render_widget(help, chunks[1]);
}

/// Show a picker over `skills` and return the selected skill's body, or
/// `None` if the user cancelled.
pub fn pick_skill(skills: &[Skill]) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let _guard = crate::util::TerminalGuardStderr::new()?;
    let backend = CrosstermBackend::new(io::stderr());
    let mut terminal = Terminal::new(backend)?;

    let mut state = ListState::default();
    state.select(Some(0));

    let result = loop {
        terminal.draw(|f| render_skills(f, skills, &mut state))?;

        if !event::poll(std::time::Duration::from_mins(1))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => break None,
                KeyCode::Up | KeyCode::Char('k') => {
                    let i = state.selected().unwrap_or(0);
                    state.select(Some(i.saturating_sub(1)));
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let i = state.selected().unwrap_or(0);
                    state.select(Some((i + 1).min(skills.len().saturating_sub(1))));
                }
                KeyCode::Enter => {
                    let body = state
                        .selected()
                        .and_then(|i| skills.get(i))
                        .map(|s| s.body.clone());
                    break body;
                }
                _ => {}
            }
        }
    };

    terminal.show_cursor()?;
    Ok(result)
}

fn render_skills(f: &mut ratatui::Frame, skills: &[Skill], state: &mut ListState) {
    let t = theme();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

    let items: Vec<ListItem> = skills
        .iter()
        .map(|s| {
            ListItem::new(Line::from(format!(
                "{}  ({})  — {}",
                s.name, s.scope, s.description
            )))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(" Skills "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD).fg(t.primary))
        .highlight_symbol(" > ");
    f.render_stateful_widget(list, chunks[0], state);

    let help = Paragraph::new(Line::from(
        " ↑/↓ or j/k move · Enter print body · Esc cancel ",
    ))
    .style(Style::default().fg(t.text_muted));
    f.render_widget(help, chunks[1]);
}
