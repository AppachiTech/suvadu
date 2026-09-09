//! Small interactive list pickers. Render to stderr (via `TerminalGuardStderr`)
//! so the chosen value can be printed to stdout for a shell wrapper (bookmarks).

use crate::models::Bookmark;
use crate::theme::theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph},
    Terminal,
};
use std::io;

/// Cap on the in-memory bookmark search box, matching the scope of a filter
/// field (not a document editor).
const MAX_SEARCH_LEN: usize = 200;

struct PickerApp<'a> {
    bookmarks: &'a [Bookmark],
    filtered: Vec<usize>,
    query: String,
    list_state: ListState,
}

impl<'a> PickerApp<'a> {
    fn new(bookmarks: &'a [Bookmark]) -> Self {
        let mut app = Self {
            bookmarks,
            filtered: (0..bookmarks.len()).collect(),
            query: String::new(),
            list_state: ListState::default(),
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

    fn selected_command(&self) -> Option<String> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered.get(i))
            .and_then(|&idx| self.bookmarks.get(idx))
            .map(|b| b.command.clone())
    }
}

/// Show a picker over `bookmarks` and return the selected command, or `None`
/// if the user cancelled. Opens even when `bookmarks` is empty — it renders
/// its own empty state, same as `suv search`.
pub fn pick_bookmark(bookmarks: &[Bookmark]) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let _guard = crate::util::TerminalGuardStderr::new()?;
    let backend = CrosstermBackend::new(io::stderr());
    let mut terminal = Terminal::new(backend)?;

    let mut app = PickerApp::new(bookmarks);

    let result = loop {
        terminal.draw(|f| render(f, &mut app))?;

        if !event::poll(std::time::Duration::from_mins(1))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                // Always-on search, like suv search: Esc quits outright
                // rather than clearing the query first, since there's no
                // separate "typing mode" to fall back out of.
                KeyCode::Esc => break None,
                KeyCode::Up => app.move_up(),
                KeyCode::Down => app.move_down(),
                KeyCode::Enter => break app.selected_command(),
                KeyCode::Backspace => {
                    app.query.pop();
                    app.apply_filter();
                }
                KeyCode::Char(c) if app.query.len() + c.len_utf8() <= MAX_SEARCH_LEN => {
                    app.query.push(c);
                    app.apply_filter();
                }
                _ => {}
            }
        }
    };

    terminal.show_cursor()?;
    Ok(result)
}

fn render(f: &mut ratatui::Frame, app: &mut PickerApp) {
    let t = theme();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // search box (always-on, like suv search)
            Constraint::Min(1),    // list
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    render_search_box(f, app, chunks[0], t);
    render_list(f, app, chunks[1], t);
    render_footer(f, chunks[2], t);
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
            "  No bookmarks yet. Use `suv bookmark add <command>` to save one.",
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

fn render_footer(f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
    let badge_key = Style::default().bg(t.badge_bg).fg(t.text);
    let badge_label = Style::default().fg(t.text_secondary);
    let help = Paragraph::new(Line::from(vec![
        Span::styled(" ↑↓ ", badge_key),
        Span::styled(" Navigate  ", badge_label),
        Span::styled(" Enter ", badge_key),
        Span::styled(" Recall  ", badge_label),
        Span::styled(" Esc ", badge_key),
        Span::styled(" Quit  ", badge_label),
    ]));
    f.render_widget(help, area);
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
        let bookmarks = vec![make_bookmark(1, "git status", None)];
        let app = PickerApp::new(&bookmarks);
        assert_eq!(app.list_state.selected(), Some(0));
        assert_eq!(app.filtered, vec![0]);
    }

    #[test]
    fn new_app_selects_none_when_empty() {
        let bookmarks: Vec<Bookmark> = vec![];
        let app = PickerApp::new(&bookmarks);
        assert_eq!(app.list_state.selected(), None);
        assert!(app.filtered.is_empty());
    }

    #[test]
    fn filter_matches_command_text_case_insensitively() {
        let bookmarks = vec![
            make_bookmark(1, "git status", None),
            make_bookmark(2, "cargo build", None),
        ];
        let mut app = PickerApp::new(&bookmarks);
        app.query = "GIT".to_string();
        app.apply_filter();
        assert_eq!(app.filtered, vec![0]);
    }

    #[test]
    fn filter_matches_label_text() {
        let bookmarks = vec![
            make_bookmark(1, "git status", Some("check repo")),
            make_bookmark(2, "cargo build", None),
        ];
        let mut app = PickerApp::new(&bookmarks);
        app.query = "check".to_string();
        app.apply_filter();
        assert_eq!(app.filtered, vec![0]);
    }

    #[test]
    fn filter_with_no_matches_clears_selection() {
        let bookmarks = vec![make_bookmark(1, "git status", None)];
        let mut app = PickerApp::new(&bookmarks);
        app.query = "nonexistent".to_string();
        app.apply_filter();
        assert!(app.filtered.is_empty());
        assert_eq!(app.list_state.selected(), None);
    }

    #[test]
    fn empty_query_shows_all_bookmarks() {
        let bookmarks = vec![
            make_bookmark(1, "git status", None),
            make_bookmark(2, "cargo build", None),
        ];
        let mut app = PickerApp::new(&bookmarks);
        app.query = "cargo".to_string();
        app.apply_filter();
        assert_eq!(app.filtered.len(), 1);
        app.query.clear();
        app.apply_filter();
        assert_eq!(app.filtered.len(), 2);
    }

    #[test]
    fn move_down_clamps_at_last_filtered_item() {
        let bookmarks = vec![make_bookmark(1, "a", None), make_bookmark(2, "b", None)];
        let mut app = PickerApp::new(&bookmarks);
        app.move_down();
        assert_eq!(app.list_state.selected(), Some(1));
        app.move_down();
        assert_eq!(app.list_state.selected(), Some(1));
    }

    #[test]
    fn move_up_clamps_at_zero() {
        let bookmarks = vec![make_bookmark(1, "a", None)];
        let mut app = PickerApp::new(&bookmarks);
        app.move_up();
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn selected_command_returns_filtered_entry() {
        let bookmarks = vec![
            make_bookmark(1, "git status", None),
            make_bookmark(2, "cargo build", None),
        ];
        let mut app = PickerApp::new(&bookmarks);
        app.query = "cargo".to_string();
        app.apply_filter();
        assert_eq!(app.selected_command(), Some("cargo build".to_string()));
    }

    #[test]
    fn selected_command_none_when_list_empty() {
        let bookmarks: Vec<Bookmark> = vec![];
        let app = PickerApp::new(&bookmarks);
        assert_eq!(app.selected_command(), None);
    }
}
