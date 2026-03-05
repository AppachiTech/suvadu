use crate::models::{Entry, Tag};
use crate::repository::Repository;
use crate::risk;
use crate::theme::theme;
use crate::util;
use arboard::Clipboard;
use chrono::{Local, TimeZone};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row,
        Scrollbar, ScrollbarOrientation, ScrollbarState, Table, TableState,
    },
    Terminal,
};
use std::io;

#[derive(Clone)]
pub enum SearchAction {
    Continue,
    Select(String),
    Exit,
    Reload,
    Copy(String),
    Delete(i64),
    SetPage(usize),
    AssociateSession(i64),
    ToggleBookmark(String),
    SaveNote(i64, String),
    DeleteNote(i64),
}

#[allow(clippy::struct_excessive_bools)]
pub struct SearchApp {
    query: String,
    entries: Vec<Entry>,
    table_state: TableState,

    // Pagination State
    pub page: usize, // 1-based index
    pub total_items: usize,
    pub page_size: usize,

    // Filter Mode State
    filter_mode: bool,
    start_date_input: String,
    end_date_input: String,
    tag_filter_input: String,      // Tag name to filter by
    exit_code_input: String,       // Exit code to filter by
    executor_filter_input: String, // Executor type/name to filter by
    focus_index: usize,            // 0=start, 1=end, 2=tag, 3=exit, 4=executor

    // Active Filters
    pub filter_after: Option<i64>,
    pub filter_before: Option<i64>,
    pub filter_tag_id: Option<i64>,
    pub filter_exit_code: Option<i32>,
    pub filter_executor_type: Option<String>,

    // Unique Mode
    pub unique_mode: bool,
    pub unique_counts: std::collections::HashMap<i64, i64>, // entry_id -> count

    // Delete Dialog State
    delete_dialog_open: bool,
    pending_delete_id: Option<i64>,

    // Go To Page Dialog
    goto_dialog_open: bool,
    goto_input: String,

    // Tag Association Dialog
    tag_dialog_open: bool,
    tags: Vec<Tag>,
    tag_list_state: ListState,

    // Notes
    noted_entry_ids: std::collections::HashSet<i64>,
    note_dialog_open: bool,
    note_input: String,
    note_entry_id: Option<i64>,

    // Directory filter
    pub filter_cwd: Option<String>,

    // Context-aware boost
    context_boost: bool,
    current_cwd: Option<String>,

    // Detail preview pane
    detail_pane_open: bool,
    show_risk_in_search: bool,

    // Bookmarks
    bookmarked_commands: std::collections::HashSet<String>,

    // Fuzzy search: cached scored results for pagination
    fuzzy_results: Vec<Entry>,

    // UI Feedback
    status_message: Option<(String, std::time::Instant)>,
}

impl SearchApp {
    #[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
    pub fn new(
        entries: Vec<Entry>,
        initial_query: Option<String>,
        total_items: usize,
        page: usize,
        page_size: usize,
        tags: Vec<Tag>,
        unique_mode: bool,
        unique_counts: std::collections::HashMap<i64, i64>,
        filter_after: Option<i64>,
        filter_before: Option<i64>,
        filter_tag_id: Option<i64>,
        filter_exit_code: Option<i32>,
        filter_executor_type: Option<String>,
        start_date_input: Option<String>,
        end_date_input: Option<String>,
        tag_filter_input: Option<String>,
        exit_code_input: Option<String>,
        executor_filter_input: Option<String>,
        bookmarked_commands: std::collections::HashSet<String>,
        filter_cwd: Option<String>,
        noted_entry_ids: std::collections::HashSet<i64>,
        context_boost: bool,
        show_detail_pane: bool,
        show_risk_in_search: bool,
    ) -> Self {
        let query = initial_query.unwrap_or_default();

        let now = Local::now();
        let five_days_ago = now - chrono::Duration::days(5);
        let start_default =
            start_date_input.unwrap_or_else(|| five_days_ago.format("%Y-%m-%d").to_string());
        let end_default = end_date_input.unwrap_or_else(|| "today".to_string());

        let mut app = Self {
            query: query.clone(),
            entries,
            table_state: TableState::default(),

            page,
            total_items,
            page_size,

            filter_mode: false,
            start_date_input: start_default,
            end_date_input: end_default,
            tag_filter_input: tag_filter_input.unwrap_or_default(),
            exit_code_input: exit_code_input.unwrap_or_default(),
            executor_filter_input: executor_filter_input.unwrap_or_default(),
            focus_index: 0,

            filter_after,
            filter_before,
            filter_tag_id,
            filter_exit_code,
            filter_executor_type,

            unique_mode,
            unique_counts,

            delete_dialog_open: false,
            pending_delete_id: None,

            goto_dialog_open: false,
            goto_input: String::new(),

            tag_dialog_open: false,
            tags,
            tag_list_state: ListState::default(),

            noted_entry_ids,
            note_dialog_open: false,
            note_input: String::new(),
            note_entry_id: None,

            filter_cwd,

            detail_pane_open: show_detail_pane,
            show_risk_in_search,

            context_boost,
            current_cwd: std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string()),

            bookmarked_commands,

            fuzzy_results: Vec::new(),

            status_message: None,
        };
        app.table_state.select(if app.entries.is_empty() {
            None
        } else {
            Some(0)
        });
        app
    }

    #[allow(clippy::too_many_lines)]
    fn handle_input(&mut self, key: KeyEvent) -> SearchAction {
        if self.delete_dialog_open {
            return self.handle_delete_dialog_input(key);
        }

        if self.goto_dialog_open {
            return self.handle_goto_dialog_input(key);
        }

        if self.tag_dialog_open {
            return self.handle_tag_dialog_input(key);
        }

        if self.note_dialog_open {
            return self.handle_note_dialog_input(key);
        }

        if self.filter_mode {
            return self.handle_filter_input(key);
        }

        match key.code {
            // Pagination Controls
            KeyCode::Left => {
                if self.page > 1 {
                    return SearchAction::SetPage(self.page - 1);
                }
            }
            KeyCode::Right => {
                let total_pages = self.total_items.div_ceil(self.page_size);
                if self.page < total_pages {
                    return SearchAction::SetPage(self.page + 1);
                }
            }
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.goto_dialog_open = true;
                self.goto_input.clear();
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.tag_dialog_open = true;
                if !self.tags.is_empty() {
                    self.tag_list_state.select(Some(0));
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.unique_mode = !self.unique_mode;
                self.page = 1;
                return SearchAction::Reload;
            }

            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter_mode = true;
                self.focus_index = 0;
            }
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(cmd) = self.get_selected_command() {
                    return SearchAction::Copy(cmd);
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(entry) = self.get_selected_entry() {
                    if let Some(id) = entry.id {
                        self.pending_delete_id = Some(id);
                        self.delete_dialog_open = true;
                    }
                }
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(cmd) = self.get_selected_command() {
                    return SearchAction::ToggleBookmark(cmd);
                }
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(entry) = self.get_selected_entry() {
                    if let Some(id) = entry.id {
                        self.note_entry_id = Some(id);
                        // Pre-populate with existing note text if any
                        // (we'll load from repo in the action handler instead)
                        self.note_input.clear();
                        self.note_dialog_open = true;
                    }
                }
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.context_boost = !self.context_boost;
                self.status_message = Some((
                    if self.context_boost {
                        "Smart mode ON".into()
                    } else {
                        "Smart mode OFF".into()
                    },
                    std::time::Instant::now(),
                ));
                return SearchAction::Reload;
            }
            KeyCode::Tab => {
                self.detail_pane_open = !self.detail_pane_open;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.filter_cwd.is_some() {
                    self.filter_cwd = None;
                } else if let Ok(cwd) = std::env::current_dir() {
                    self.filter_cwd = Some(cwd.to_string_lossy().to_string());
                }
                self.page = 1;
                return SearchAction::Reload;
            }
            KeyCode::Char(c) => {
                self.query.push(c);
                return SearchAction::Reload;
            }
            KeyCode::Backspace => {
                self.query.pop();
                return SearchAction::Reload;
            }
            KeyCode::Up => {
                if let Some(selected) = self.table_state.selected() {
                    if selected > 0 {
                        self.table_state.select(Some(selected - 1));
                    }
                }
            }
            KeyCode::Down => {
                if let Some(selected) = self.table_state.selected() {
                    if selected + 1 < self.entries.len() {
                        self.table_state.select(Some(selected + 1));
                    }
                } else if !self.entries.is_empty() {
                    self.table_state.select(Some(0));
                }
            }
            KeyCode::Enter => {
                if let Some(cmd) = self.get_selected_command() {
                    return SearchAction::Select(cmd);
                }
            }
            KeyCode::Esc => {
                return SearchAction::Exit;
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_tag_dialog_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Esc => self.tag_dialog_open = false,
            KeyCode::Up => {
                if let Some(selected) = self.tag_list_state.selected() {
                    if selected > 0 {
                        self.tag_list_state.select(Some(selected - 1));
                    }
                }
            }
            KeyCode::Down => {
                if let Some(selected) = self.tag_list_state.selected() {
                    if selected + 1 < self.tags.len() {
                        self.tag_list_state.select(Some(selected + 1));
                    }
                } else if !self.tags.is_empty() {
                    self.tag_list_state.select(Some(0));
                }
            }
            KeyCode::Enter => {
                if let Some(selected) = self.tag_list_state.selected() {
                    if let Some(tag) = self.tags.get(selected) {
                        self.tag_dialog_open = false;
                        return SearchAction::AssociateSession(tag.id);
                    }
                }
                self.tag_dialog_open = false;
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_note_dialog_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Esc => {
                self.note_dialog_open = false;
                self.note_input.clear();
                self.note_entry_id = None;
            }
            KeyCode::Enter => {
                if let Some(entry_id) = self.note_entry_id {
                    self.note_dialog_open = false;
                    let text = self.note_input.clone();
                    self.note_input.clear();
                    self.note_entry_id = None;
                    if text.is_empty() {
                        return SearchAction::DeleteNote(entry_id);
                    }
                    return SearchAction::SaveNote(entry_id, text);
                }
                self.note_dialog_open = false;
            }
            KeyCode::Backspace => {
                self.note_input.pop();
            }
            KeyCode::Char(c) => {
                self.note_input.push(c);
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_delete_dialog_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(id) = self.pending_delete_id {
                    self.delete_dialog_open = false;
                    self.pending_delete_id = None;
                    return SearchAction::Delete(id);
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.delete_dialog_open = false;
                self.pending_delete_id = None;
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_goto_dialog_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Enter => {
                if let Ok(page_num) = self.goto_input.parse::<usize>() {
                    let total_pages = self.total_items.div_ceil(self.page_size);
                    let page_num = page_num.max(1).min(total_pages); // Clamp
                    self.goto_dialog_open = false;
                    return SearchAction::SetPage(page_num);
                }
                self.goto_dialog_open = false;
            }
            KeyCode::Esc => {
                self.goto_dialog_open = false;
            }
            KeyCode::Backspace => {
                self.goto_input.pop();
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                self.goto_input.push(c);
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_filter_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Esc => {
                self.filter_mode = false;
            }
            KeyCode::Tab => {
                self.focus_index = (self.focus_index + 1) % 5;
            }
            KeyCode::BackTab => {
                self.focus_index = if self.focus_index == 0 {
                    4
                } else {
                    self.focus_index - 1
                };
            }
            KeyCode::Enter => {
                // Apply filters
                self.filter_after = if self.start_date_input.is_empty() {
                    None
                } else {
                    util::parse_date_input(&self.start_date_input, false)
                };

                self.filter_before = if self.end_date_input.is_empty() {
                    None
                } else {
                    util::parse_date_input(&self.end_date_input, true)
                };

                // Resolve tag name to ID
                self.filter_tag_id = if self.tag_filter_input.is_empty() {
                    None
                } else {
                    let input_lower = self.tag_filter_input.to_lowercase();
                    self.tags
                        .iter()
                        .find(|t| t.name == input_lower)
                        .map(|t| t.id)
                };

                // Parse exit code
                self.filter_exit_code = if self.exit_code_input.is_empty() {
                    None
                } else {
                    self.exit_code_input.trim().parse::<i32>().ok()
                };

                // Parse executor filter
                self.filter_executor_type = if self.executor_filter_input.is_empty() {
                    None
                } else {
                    Some(self.executor_filter_input.trim().to_lowercase())
                };

                self.filter_mode = false;
                // Reset to page 1 on new filter
                self.page = 1;
                return SearchAction::Reload;
            }
            KeyCode::Backspace => match self.focus_index {
                0 => {
                    self.start_date_input.pop();
                }
                1 => {
                    self.end_date_input.pop();
                }
                2 => {
                    self.tag_filter_input.pop();
                }
                3 => {
                    self.exit_code_input.pop();
                }
                4 => {
                    self.executor_filter_input.pop();
                }
                _ => {}
            },
            KeyCode::Char(c) => match self.focus_index {
                0 => {
                    self.start_date_input.push(c);
                }
                1 => {
                    self.end_date_input.push(c);
                }
                2 => {
                    self.tag_filter_input.push(c);
                }
                3 => {
                    self.exit_code_input.push(c);
                }
                4 => {
                    self.executor_filter_input.push(c);
                }
                _ => {}
            },
            _ => {}
        }
        SearchAction::Continue
    }

    fn get_selected_entry(&self) -> Option<&Entry> {
        self.table_state
            .selected()
            .and_then(|idx| self.entries.get(idx))
    }

    fn get_selected_command(&self) -> Option<String> {
        self.get_selected_entry().map(|entry| entry.command.clone())
    }

    /// Count active filters for badge display
    fn active_filter_count(&self) -> usize {
        let mut count = 0;
        if self.filter_after.is_some() {
            count += 1;
        }
        if self.filter_before.is_some() {
            count += 1;
        }
        if self.filter_tag_id.is_some() {
            count += 1;
        }
        if self.filter_exit_code.is_some() {
            count += 1;
        }
        if self.filter_executor_type.is_some() {
            count += 1;
        }
        count
    }

    #[allow(clippy::too_many_lines)]
    fn render(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> io::Result<()> {
        terminal.draw(|f| {
            let t = theme();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // Branding
                    Constraint::Length(3), // Query input
                    Constraint::Min(0),    // Results list
                    Constraint::Length(2), // Help text
                ])
                .split(f.area());

            // Minimalist Header
            let branding = Line::from(vec![Span::styled(
                "Suvadu",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            )]);
            f.render_widget(
                Paragraph::new(branding).alignment(Alignment::Center),
                chunks[0],
            );

            // Dynamic Search Bar
            let active_filters = self.active_filter_count();
            let filter_badge = if active_filters > 0 {
                format!(
                    " [{active_filters} filter{}]",
                    if active_filters > 1 { "s" } else { "" }
                )
            } else {
                String::new()
            };
            let unique_badge = if self.unique_mode { " [unique]" } else { "" };

            let search_border_color = if self.filter_mode {
                t.border
            } else {
                t.border_focus
            };
            let search_title = if self.filter_mode {
                "Search"
            } else {
                "Search (Typing)"
            };
            let query_display = format!("{}{filter_badge}{unique_badge}", self.query);
            let query = Paragraph::new(query_display)
                .style(Style::default().fg(t.text))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(search_border_color))
                        .title(search_title),
                );
            f.render_widget(query, chunks[1]);

            // Results Table + Optional Detail Pane
            if self.detail_pane_open {
                let result_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Percentage(70), // Results
                        Constraint::Percentage(30), // Detail
                    ])
                    .split(chunks[2]);
                self.render_results_table(f, result_chunks[0]);
                self.render_detail_pane(f, result_chunks[1]);
            } else {
                self.render_results_table(f, chunks[2]);
            }

            // Footer
            self.render_footer(f, chunks[3]);

            // Render Overlays
            if self.filter_mode {
                self.render_filter_popup(f, f.area());
            } else if self.goto_dialog_open {
                self.render_goto_dialog(f, f.area());
            } else if self.delete_dialog_open {
                self.render_delete_dialog(f, f.area());
            } else if self.tag_dialog_open {
                self.render_tag_dialog(f, f.area());
            } else if self.note_dialog_open {
                self.render_note_dialog(f, f.area());
            }
        })?;

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn render_footer(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let total_pages = self.total_items.div_ceil(self.page_size).max(1);
        let progress_pct = if total_pages > 0 {
            (self.page * 100) / total_pages
        } else {
            0
        };

        let status_text = if let Some((msg, time)) = &self.status_message {
            if time.elapsed() < std::time::Duration::from_secs(2) {
                Some(msg.clone())
            } else {
                None
            }
        } else {
            None
        };

        let badge_key_style = Style::default().bg(t.badge_bg).fg(t.text);
        let badge_label_style = Style::default().fg(t.text_secondary);

        let mut help_badges = vec![
            Span::styled(" Esc ", badge_key_style),
            Span::styled(" Quit  ", badge_label_style),
            Span::styled(" ^F ", badge_key_style),
            Span::styled(" Filter  ", badge_label_style),
            Span::styled(" ^D ", badge_key_style),
            Span::styled(" Delete  ", badge_label_style),
            Span::styled(" ^T ", badge_key_style),
            Span::styled(" Tag  ", badge_label_style),
            Span::styled(" ^G ", badge_key_style),
            Span::styled(" Goto  ", badge_label_style),
            Span::styled(" ^U ", badge_key_style),
            Span::styled(
                if self.unique_mode {
                    " All  "
                } else {
                    " Unique  "
                },
                badge_label_style,
            ),
            Span::styled(" ^Y ", badge_key_style),
            Span::styled(" Copy  ", badge_label_style),
            Span::styled(" ^B ", badge_key_style),
            Span::styled(" Bookmark  ", badge_label_style),
            Span::styled(" ^N ", badge_key_style),
            Span::styled(" Note  ", badge_label_style),
            Span::styled(" ^L ", badge_key_style),
            Span::styled(
                if self.filter_cwd.is_some() {
                    " All Dirs  "
                } else {
                    " Here  "
                },
                badge_label_style,
            ),
            Span::styled(" ^S ", badge_key_style),
            Span::styled(
                if self.context_boost {
                    " Recent  "
                } else {
                    " Smart  "
                },
                badge_label_style,
            ),
            Span::styled(" Tab ", badge_key_style),
            Span::styled(
                if self.detail_pane_open {
                    " Hide  "
                } else {
                    " Detail  "
                },
                badge_label_style,
            ),
        ];

        // Active filter badges
        if self.filter_after.is_some() || self.filter_before.is_some() {
            help_badges.push(Span::styled(
                " date ",
                Style::default().bg(t.info).fg(Color::Black),
            ));
            help_badges.push(Span::raw(" "));
        }
        if self.filter_tag_id.is_some() {
            help_badges.push(Span::styled(
                " tag ",
                Style::default().bg(t.warning).fg(Color::Black),
            ));
            help_badges.push(Span::raw(" "));
        }
        if self.filter_exit_code.is_some() {
            help_badges.push(Span::styled(
                " exit ",
                Style::default().bg(t.error).fg(Color::White),
            ));
            help_badges.push(Span::raw(" "));
        }
        if self.filter_executor_type.is_some() {
            help_badges.push(Span::styled(
                " exec ",
                Style::default()
                    .bg(Color::Rgb(147, 51, 234))
                    .fg(Color::White),
            ));
            help_badges.push(Span::raw(" "));
        }
        if self.filter_cwd.is_some() {
            help_badges.push(Span::styled(
                " dir ",
                Style::default()
                    .bg(Color::Rgb(6, 182, 212))
                    .fg(Color::Black),
            ));
            help_badges.push(Span::raw(" "));
        }
        if self.context_boost {
            help_badges.push(Span::styled(
                " smart ",
                Style::default().bg(t.success).fg(Color::Black),
            ));
            help_badges.push(Span::raw(" "));
        }

        // Page progress
        let page_info = format!(" {}/{} ({progress_pct}%) ", self.page, total_pages);
        help_badges.push(Span::styled(page_info, Style::default().fg(t.text_muted)));

        if let Some(msg) = status_text {
            help_badges.push(Span::styled(
                format!(" {msg} "),
                Style::default().fg(t.success).add_modifier(Modifier::BOLD),
            ));
        }

        let help_line = Line::from(help_badges);
        let help_paragraph = Paragraph::new(help_line).block(
            Block::default()
                .borders(Borders::TOP)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border)),
        );
        f.render_widget(help_paragraph, area);
    }

    #[allow(clippy::cast_precision_loss)]
    #[allow(clippy::too_many_lines)]
    fn render_results_table(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        // Reserve 1 column for scrollbar
        let table_area = Rect {
            width: area.width.saturating_sub(1),
            ..area
        };
        let scrollbar_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            width: 1,
            ..area
        };

        // Time (15) + Session/Tag (25) + Executor (12) + Path (20) + Status (6) + Duration (8) = 86
        let fixed_width: u16 = 15 + 25 + 12 + 20 + 6 + 8;
        let command_col_width = table_area.width.saturating_sub(fixed_width + 6);

        let rows: Vec<Row> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let is_selected = self.table_state.selected() == Some(i);

                #[allow(clippy::cast_precision_loss)]
                let duration_secs = entry.duration_ms as f64 / 1000.0;

                // Normalize: if timestamp looks like microseconds (16+ digits), convert to ms
                let ts_ms = if entry.started_at > 9_999_999_999_999 {
                    entry.started_at / 1000
                } else {
                    entry.started_at
                };
                let start_time = Local
                    .timestamp_millis_opt(ts_ms)
                    .single()
                    .unwrap_or_else(|| {
                        chrono::DateTime::from_timestamp_millis(0)
                            .unwrap()
                            .with_timezone(&Local)
                    });
                let time_str = start_time.format("%m-%d %H:%M").to_string();
                let duration_str = format!("{duration_secs:.1}s");

                // Combine session and tag
                let session_short = &entry.session_id[..8];

                let session_tag_display = if let Some(tag) = &entry.tag_name {
                    format!("{session_short} ({tag})")
                } else {
                    session_short.to_string()
                };

                // Wrap session/tag if selected
                let st_display = if is_selected {
                    fill_text(&session_tag_display, 25)
                } else {
                    session_tag_display
                };

                // Shorten path - replace home directory with ~
                let path_full = if let Ok(home) = std::env::var("HOME") {
                    entry.cwd.replace(&home, "~")
                } else {
                    entry.cwd.clone()
                };

                // For selected items, show full path; for others, truncate
                let path_display = if is_selected {
                    path_full.clone()
                } else if path_full.len() > 18 {
                    format!("...{}", &path_full[path_full.len() - 15..])
                } else {
                    path_full
                };

                // Format executor with icon
                let executor_icon = match entry.executor_type.as_deref() {
                    Some("human") => "👤",
                    Some("bot" | "agent") => "🤖",
                    Some("ide") => "💻",
                    Some("ci") => "⚙️",
                    Some("programmatic") => "⚡",
                    _ => "❓",
                };
                let executor_display = if let Some(exec_name) = &entry.executor {
                    format!("{executor_icon} {exec_name}")
                } else {
                    executor_icon.to_string()
                };

                // Get occurrence count if in unique mode
                let count_display = if self.unique_mode {
                    format!(
                        "({}) ",
                        self.unique_counts.get(&entry.id.unwrap_or(0)).unwrap_or(&1)
                    )
                } else {
                    String::new()
                };

                // Bookmark and note indicators
                let bookmark_prefix = if self.bookmarked_commands.contains(&entry.command) {
                    "★ "
                } else {
                    ""
                };
                let note_prefix = if entry
                    .id
                    .is_some_and(|id| self.noted_entry_ids.contains(&id))
                {
                    "📝"
                } else {
                    ""
                };

                // Command text
                let cmd_text = format!(
                    "{}{}{}{}",
                    note_prefix, bookmark_prefix, count_display, entry.command
                );

                // Syntax highlighting for ALL rows
                let command_display = if is_selected {
                    Self::highlight_command(&cmd_text, true, command_col_width as usize)
                } else {
                    Self::highlight_command(&cmd_text, false, 0)
                };

                let cmd_height = u16::try_from(command_display.lines.len())
                    .unwrap_or(1)
                    .max(1);
                let st_height = u16::try_from(st_display.lines().count())
                    .unwrap_or(1)
                    .max(1);

                let height = cmd_height.max(st_height);

                // Check if this entry is from the current directory (for context boost highlight)
                let is_local = self.context_boost
                    && self
                        .current_cwd
                        .as_deref()
                        .is_some_and(|cwd| entry.cwd == cwd);

                // Styles: prominent selection with blue background
                let (
                    bg_style,
                    time_style,
                    session_style,
                    executor_style,
                    path_style,
                    duration_style,
                ) = if is_selected {
                    let sel = Style::default().bg(t.selection_bg);
                    (
                        sel,
                        sel.fg(t.selection_fg).add_modifier(Modifier::BOLD),
                        sel.fg(t.info).add_modifier(Modifier::BOLD),
                        sel.fg(t.warning).add_modifier(Modifier::BOLD),
                        if is_local {
                            sel.fg(t.primary).add_modifier(Modifier::BOLD)
                        } else {
                            sel.fg(t.selection_fg)
                        },
                        sel.fg(t.text_secondary),
                    )
                } else {
                    let base = Style::default();
                    (
                        base,
                        base.fg(t.text_muted),
                        base.fg(t.info),
                        base.fg(t.warning),
                        if is_local {
                            base.fg(t.primary)
                        } else {
                            base.fg(t.text_secondary)
                        },
                        base.fg(t.text_muted),
                    )
                };

                let exit_display = match entry.exit_code {
                    Some(0) => "✔".to_string(),
                    Some(code) => format!("✘ {code}"),
                    None => "○".to_string(),
                };
                let exit_style_item = match entry.exit_code {
                    Some(0) => bg_style.fg(t.success),
                    Some(_) => bg_style.fg(t.error),
                    None => bg_style.fg(t.text_muted),
                };

                if self.unique_mode {
                    Row::new(vec![Cell::from(command_display)])
                        .height(height)
                        .style(bg_style)
                } else {
                    Row::new(vec![
                        Cell::from(time_str).style(time_style),
                        Cell::from(st_display).style(session_style),
                        Cell::from(executor_display).style(executor_style),
                        Cell::from(path_display).style(path_style),
                        Cell::from(command_display),
                        Cell::from(exit_display).style(exit_style_item),
                        Cell::from(duration_str).style(duration_style),
                    ])
                    .height(height)
                    .style(bg_style)
                }
            })
            .collect();

        let widths = if self.unique_mode {
            vec![Constraint::Percentage(100)]
        } else {
            vec![
                Constraint::Length(15), // Time
                Constraint::Length(25), // Session/Tag
                Constraint::Length(12), // Executor
                Constraint::Length(20), // Path
                Constraint::Min(10),    // Command
                Constraint::Length(6),  // Status
                Constraint::Length(8),  // Duration
            ]
        };

        let header_row = if self.unique_mode {
            Row::new(vec!["Command".to_string()])
        } else {
            Row::new(vec![
                "Time".to_string(),
                "Session/Tag".to_string(),
                "Executor".to_string(),
                "Path".to_string(),
                "Command".to_string(),
                "Status".to_string(),
                "Duration".to_string(),
            ])
        };

        let table = Table::new(rows, widths)
            .header(
                header_row
                    .style(
                        Style::default()
                            .fg(t.text_secondary)
                            .add_modifier(Modifier::BOLD),
                    )
                    .bottom_margin(1),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border))
                    .title({
                        if self.total_items == 0 {
                            "History (0/0)".to_string()
                        } else {
                            let start_index = (self.page - 1) * self.page_size + 1;
                            let end_index = start_index + self.entries.len() - 1;
                            format!(
                                "History ({}-{} / {})",
                                start_index, end_index, self.total_items
                            )
                        }
                    }),
            )
            .highlight_symbol(" > ");

        f.render_stateful_widget(table, table_area, &mut self.table_state);

        // Scrollbar
        let total_pages = self.total_items.div_ceil(self.page_size).max(1);
        let mut scrollbar_state =
            ScrollbarState::new(total_pages).position(self.page.saturating_sub(1));
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(Style::default().fg(t.primary_dim))
            .track_style(Style::default().fg(t.border));
        f.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }

    #[allow(clippy::too_many_lines)]
    fn render_detail_pane(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let entry = self.get_selected_entry();

        let block = Block::default()
            .title(" Detail ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border));

        if let Some(entry) = entry {
            let label_style = Style::default()
                .fg(t.text_secondary)
                .add_modifier(Modifier::BOLD);
            let value_style = Style::default().fg(t.text);

            // Timestamp
            let ts_ms = if entry.started_at > 9_999_999_999_999 {
                entry.started_at / 1000
            } else {
                entry.started_at
            };
            let start_time = Local
                .timestamp_millis_opt(ts_ms)
                .single()
                .unwrap_or_else(|| {
                    chrono::DateTime::from_timestamp_millis(0)
                        .unwrap()
                        .with_timezone(&Local)
                });
            let time_str = start_time.format("%Y-%m-%d %H:%M:%S").to_string();

            #[allow(clippy::cast_precision_loss)]
            let duration_secs = entry.duration_ms as f64 / 1000.0;

            let exit_str = match entry.exit_code {
                Some(0) => "✔ 0 (success)".to_string(),
                Some(code) => format!("✘ {code} (failed)"),
                None => "○ (unknown)".to_string(),
            };

            let executor_str = match (&entry.executor_type, &entry.executor) {
                (Some(t), Some(n)) => format!("{t}: {n}"),
                (Some(t), None) => t.clone(),
                _ => "unknown".to_string(),
            };

            let session_str = &entry.session_id[..8.min(entry.session_id.len())];
            let tag_str = entry.tag_name.as_deref().unwrap_or("none");

            let is_bookmarked = self.bookmarked_commands.contains(&entry.command);
            let has_note = entry
                .id
                .is_some_and(|id| self.noted_entry_ids.contains(&id));

            let mut lines = vec![
                Line::from(vec![Span::styled("Command  ", label_style)]),
                Line::from(vec![Span::styled(
                    entry.command.clone(),
                    Style::default().fg(t.primary),
                )]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Path     ", label_style),
                    Span::styled(entry.cwd.clone(), value_style),
                ]),
                Line::from(vec![
                    Span::styled("Time     ", label_style),
                    Span::styled(time_str, value_style),
                ]),
                Line::from(vec![
                    Span::styled("Duration ", label_style),
                    Span::styled(format!("{duration_secs:.2}s"), value_style),
                ]),
                Line::from(vec![
                    Span::styled("Exit     ", label_style),
                    Span::styled(exit_str, value_style),
                ]),
                Line::from(vec![
                    Span::styled("Session  ", label_style),
                    Span::styled(session_str.to_string(), value_style),
                ]),
                Line::from(vec![
                    Span::styled("Tag      ", label_style),
                    Span::styled(tag_str.to_string(), value_style),
                ]),
                Line::from(vec![
                    Span::styled("Executor ", label_style),
                    Span::styled(executor_str, value_style),
                ]),
            ];

            // Risk assessment
            if self.show_risk_in_search {
                let assessment = risk::assess_risk(&entry.command);
                let risk_level = assessment
                    .as_ref()
                    .map_or(risk::RiskLevel::None, |a| a.level);
                if risk_level > risk::RiskLevel::None {
                    let risk_color = match risk_level {
                        risk::RiskLevel::Critical => t.risk_critical,
                        risk::RiskLevel::High => t.risk_high,
                        risk::RiskLevel::Medium => t.risk_medium,
                        risk::RiskLevel::Low | risk::RiskLevel::None => t.risk_low,
                    };
                    let risk_text = format!(
                        "{} {}{}",
                        risk_level.icon(),
                        risk_level.label(),
                        assessment
                            .as_ref()
                            .map_or(String::new(), |a| format!(" ({})", a.category))
                    );
                    lines.push(Line::from(vec![
                        Span::styled("Risk     ", label_style),
                        Span::styled(risk_text, Style::default().fg(risk_color)),
                    ]));
                }
            }

            if is_bookmarked || has_note {
                lines.push(Line::from(""));
                if is_bookmarked {
                    lines.push(Line::from(vec![
                        Span::styled("★ ", Style::default().fg(t.warning)),
                        Span::styled("Bookmarked", value_style),
                    ]));
                }
                if has_note {
                    lines.push(Line::from(vec![
                        Span::styled("📝 ", Style::default()),
                        Span::styled("Has note", value_style),
                    ]));
                }
            }

            let paragraph = Paragraph::new(lines)
                .block(block)
                .wrap(ratatui::widgets::Wrap { trim: false });
            f.render_widget(paragraph, area);
        } else {
            let empty = Paragraph::new("No entry selected")
                .block(block)
                .style(Style::default().fg(t.text_muted))
                .alignment(Alignment::Center);
            f.render_widget(empty, area);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn render_filter_popup(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let block = Block::default()
            .title(" Filters ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.primary).add_modifier(Modifier::BOLD))
            .style(Style::default().bg(t.bg_elevated));

        let popup_area = centered_rect(60, 50, area);
        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints(
                [
                    Constraint::Length(1), // Progress indicator
                    Constraint::Length(3), // Start Date
                    Constraint::Length(3), // End Date
                    Constraint::Length(3), // Tag
                    Constraint::Length(3), // Exit Code
                    Constraint::Length(3), // Executor
                    Constraint::Min(0),    // Help
                ]
                .as_ref(),
            )
            .split(popup_area);

        // Field progress indicator
        let progress_blocks: Vec<Span> = (0..5)
            .map(|i| {
                if i == self.focus_index {
                    Span::styled(" ■ ", Style::default().fg(t.primary))
                } else {
                    Span::styled(" □ ", Style::default().fg(t.text_muted))
                }
            })
            .collect();
        let field_names = ["Start Date", "End Date", "Tag", "Exit Code", "Executor"];
        let mut progress_line = progress_blocks;
        progress_line.push(Span::styled(
            format!(
                "  Field {} of 5: {}",
                self.focus_index + 1,
                field_names[self.focus_index]
            ),
            Style::default().fg(t.text_secondary),
        ));
        f.render_widget(
            Paragraph::new(Line::from(progress_line)).alignment(Alignment::Center),
            chunks[0],
        );

        let fields: Vec<(&str, &str, &str)> = vec![
            (
                "Start Date (After)",
                &self.start_date_input,
                "e.g. today, yesterday, 2024-01-15",
            ),
            (
                "End Date (Before)",
                &self.end_date_input,
                "e.g. today, 3 days ago, 2024-12-31",
            ),
            ("Tag Name", &self.tag_filter_input, "e.g. work, personal"),
            (
                "Exit Code",
                &self.exit_code_input,
                "e.g. 0 (success), 1 (failure)",
            ),
            (
                "Executor",
                &self.executor_filter_input,
                "e.g. human, agent, ide, ci, vscode",
            ),
        ];

        for (i, (title, value, hint)) in fields.iter().enumerate() {
            let is_focused = self.focus_index == i;
            let border_color = if is_focused { t.border_focus } else { t.border };
            let text_color = if is_focused { t.text } else { t.text_secondary };

            let display_text = if value.is_empty() && !is_focused {
                hint.to_string()
            } else {
                value.to_string()
            };

            let text_style = if value.is_empty() && !is_focused {
                Style::default().fg(t.text_muted)
            } else {
                Style::default().fg(text_color)
            };

            let input = Paragraph::new(display_text)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(border_color))
                        .title(format!("{title}{}", if is_focused { " *" } else { "" })),
                )
                .style(text_style);
            f.render_widget(input, chunks[i + 1]);
        }

        let help_text = Paragraph::new("Tab/S-Tab: switch fields  |  Enter: apply  |  Esc: cancel")
            .alignment(Alignment::Center)
            .style(Style::default().fg(t.text_muted));
        f.render_widget(help_text, chunks[6]);
    }

    fn render_tag_dialog(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let block = Block::default()
            .title(" Associate Session with Tag ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.info))
            .style(Style::default().bg(t.bg_elevated));

        let popup_area = centered_rect(50, 40, area);
        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([Constraint::Min(0), Constraint::Length(1)].as_ref())
            .split(popup_area);

        let items: Vec<ListItem> = self
            .tags
            .iter()
            .map(|tag| {
                ListItem::new(format!(
                    " {} : {}",
                    tag.name,
                    tag.description.clone().unwrap_or_default()
                ))
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::NONE))
            .highlight_style(
                Style::default()
                    .bg(t.selection_bg)
                    .fg(t.selection_fg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(" > ");

        f.render_stateful_widget(list, chunks[0], &mut self.tag_list_state);

        let help = Paragraph::new("Enter: Select  |  Esc: Cancel")
            .alignment(Alignment::Center)
            .style(Style::default().fg(t.text_muted));
        f.render_widget(help, chunks[1]);
    }

    #[allow(clippy::unused_self)]
    fn render_delete_dialog(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let popup_area = centered_rect(50, 25, area);
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Delete Entry ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.error))
            .style(Style::default().bg(Color::Rgb(40, 10, 10)));

        // Show command preview
        let cmd_preview = self
            .get_selected_entry()
            .map(|e| {
                if e.command.len() > 50 {
                    format!("{}...", &e.command[..47])
                } else {
                    e.command.clone()
                }
            })
            .unwrap_or_default();

        let content = vec![
            Line::from(""),
            Line::from(Span::styled(
                cmd_preview,
                Style::default().fg(t.text).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("Delete this entry?"),
            Line::from(""),
            Line::from(vec![
                Span::styled(" [Y] ", Style::default().bg(t.error).fg(Color::White)),
                Span::raw(" Yes   "),
                Span::styled(" [N] ", Style::default().bg(t.badge_bg).fg(t.text)),
                Span::raw(" No"),
            ]),
        ];

        let confirm_text = Paragraph::new(content)
            .block(block)
            .alignment(Alignment::Center);

        f.render_widget(confirm_text, popup_area);
    }

    fn render_goto_dialog(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let total_pages = self.total_items.div_ceil(self.page_size).max(1);

        let block = Block::default()
            .title(" Go To Page ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.primary))
            .style(Style::default().bg(t.bg_elevated));

        let popup_area = centered_rect(30, 20, area);
        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);

        let inner_layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([Constraint::Length(3), Constraint::Length(1)].as_ref())
            .split(popup_area);

        let input = Paragraph::new(self.goto_input.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border_focus))
                    .title(format!("Page (1-{total_pages})")),
            )
            .style(Style::default().fg(t.text));

        f.render_widget(input, inner_layout[0]);

        let hint = Paragraph::new("Enter: go  |  Esc: cancel")
            .alignment(Alignment::Center)
            .style(Style::default().fg(t.text_muted));
        f.render_widget(hint, inner_layout[1]);
    }

    fn render_note_dialog(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let block = Block::default()
            .title(" Add Note (Enter: save, Esc: cancel, empty: delete) ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.warning))
            .style(Style::default().bg(t.bg_elevated));

        let popup_area = centered_rect(50, 20, area);
        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);

        let inner_layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([Constraint::Length(3), Constraint::Length(1)].as_ref())
            .split(popup_area);

        let input = Paragraph::new(self.note_input.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border_focus))
                    .title("Note"),
            )
            .style(Style::default().fg(t.text));

        f.render_widget(input, inner_layout[0]);

        let hint = Paragraph::new("Enter: save  |  Esc: cancel  |  Empty = delete note")
            .alignment(Alignment::Center)
            .style(Style::default().fg(t.text_muted));
        f.render_widget(hint, inner_layout[1]);
    }

    pub fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
        repo: &Repository,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        loop {
            self.render(terminal)?;

            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match self.handle_input(key) {
                        SearchAction::Select(cmd) => return Ok(Some(cmd)),
                        SearchAction::Exit => return Ok(None),
                        SearchAction::Reload => {
                            self.reload_entries(repo)?;
                        }
                        SearchAction::SetPage(page) => {
                            self.set_page(repo, page)?;
                        }
                        SearchAction::Copy(cmd) => {
                            let mut clipboard = Clipboard::new()?;
                            if clipboard.set_text(cmd.clone()).is_ok() {
                                self.status_message =
                                    Some(("Copied!".to_string(), std::time::Instant::now()));
                            }
                        }
                        SearchAction::Delete(id) => {
                            if repo.delete_entry(id).is_ok() {
                                self.reload_entries(repo)?;
                                self.status_message =
                                    Some(("Deleted!".to_string(), std::time::Instant::now()));
                            }
                        }
                        SearchAction::SaveNote(entry_id, text) => {
                            if repo.upsert_note(entry_id, &text).is_ok() {
                                self.noted_entry_ids.insert(entry_id);
                                self.status_message =
                                    Some(("Note saved!".to_string(), std::time::Instant::now()));
                            }
                        }
                        SearchAction::DeleteNote(entry_id) => {
                            if repo.delete_note(entry_id).is_ok() {
                                self.noted_entry_ids.remove(&entry_id);
                                self.status_message =
                                    Some(("Note deleted".to_string(), std::time::Instant::now()));
                            }
                        }
                        SearchAction::ToggleBookmark(cmd) => {
                            if self.bookmarked_commands.contains(&cmd) {
                                if repo.remove_bookmark(&cmd).is_ok() {
                                    self.bookmarked_commands.remove(&cmd);
                                    self.status_message = Some((
                                        "Bookmark removed".to_string(),
                                        std::time::Instant::now(),
                                    ));
                                }
                            } else if repo.add_bookmark(&cmd, None).is_ok() {
                                self.bookmarked_commands.insert(cmd);
                                self.status_message =
                                    Some(("Bookmarked!".to_string(), std::time::Instant::now()));
                            }
                        }
                        SearchAction::AssociateSession(tag_id) => {
                            let sid = std::env::var("SUVADU_SESSION_ID").unwrap_or_default();
                            if sid.is_empty() {
                                self.status_message = Some((
                                    "No session ID found".to_string(),
                                    std::time::Instant::now(),
                                ));
                            } else if let Err(e) = repo.tag_session(&sid, Some(tag_id)) {
                                self.status_message =
                                    Some((format!("Error: {e}"), std::time::Instant::now()));
                            } else {
                                let tag_name = self
                                    .tags
                                    .iter()
                                    .find(|t| t.id == tag_id)
                                    .map(|t| t.name.clone())
                                    .unwrap_or_default();
                                self.status_message = Some((
                                    format!("Session tagged: {tag_name}"),
                                    std::time::Instant::now(),
                                ));
                            }
                        }
                        SearchAction::Continue => {}
                    }
                }
            }
        }
    }

    /// Score entries against a query using nucleo fuzzy matching.
    /// Returns entries sorted by score (highest first).
    /// If `boost_cwd` is Some, entries whose cwd matches get a 1.5× score boost.
    fn fuzzy_score(entries: Vec<Entry>, query: &str, boost_cwd: Option<&str>) -> Vec<Entry> {
        use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
        use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

        let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

        let mut scored: Vec<(Entry, u32)> = Vec::new();
        let mut buf = Vec::new();

        for entry in entries {
            buf.clear();
            let haystack = Utf32Str::new(&entry.command, &mut buf);
            if let Some(score) = pattern.score(haystack, &mut matcher) {
                let final_score = if boost_cwd.is_some_and(|cwd| entry.cwd == cwd) {
                    score.saturating_add(score / 2)
                } else {
                    score
                };
                scored.push((entry, final_score));
            }
        }

        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored.into_iter().map(|(e, _)| e).collect()
    }

    /// Stable re-sort: float same-CWD entries to top, preserving recency within each group.
    fn apply_context_sort(entries: &mut [Entry], current_cwd: &str) {
        entries.sort_by(|a, b| {
            let a_local = a.cwd == current_cwd;
            let b_local = b.cwd == current_cwd;
            b_local.cmp(&a_local) // true > false → locals first
        });
    }

    #[allow(clippy::too_many_lines)]
    fn reload_entries(&mut self, repo: &Repository) -> Result<(), Box<dyn std::error::Error>> {
        let use_fuzzy = self.query.len() >= 2;

        if use_fuzzy {
            // Fuzzy path: fetch broad candidates from DB, then score + rank
            const MAX_FUZZY_CANDIDATES: usize = 10_000;

            if self.unique_mode {
                let unique_res = repo.get_unique_entries(
                    MAX_FUZZY_CANDIDATES,
                    0,
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    None, // No SQL query filter — nucleo handles matching
                    false,
                    false, // Recency sort (will be re-sorted by score)
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?;
                let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();

                // Build count map before fuzzy filtering
                let mut count_map = std::collections::HashMap::new();
                for (entry, count) in entries.iter().zip(counts.iter()) {
                    if let Some(id) = entry.id {
                        count_map.insert(id, *count);
                    }
                }

                let boost_cwd = if self.context_boost {
                    self.current_cwd.as_deref()
                } else {
                    None
                };
                let scored = Self::fuzzy_score(entries, &self.query, boost_cwd);
                self.unique_counts = count_map;
                self.fuzzy_results = scored;
            } else {
                let entries = repo.get_entries(
                    MAX_FUZZY_CANDIDATES,
                    0,
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    None, // No SQL query filter
                    false,
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?;

                let boost_cwd = if self.context_boost {
                    self.current_cwd.as_deref()
                } else {
                    None
                };
                self.fuzzy_results = Self::fuzzy_score(entries, &self.query, boost_cwd);
            }

            self.total_items = self.fuzzy_results.len();
            self.page = 1;
            let end = self.page_size.min(self.fuzzy_results.len());
            self.entries = self.fuzzy_results[..end].to_vec();
        } else {
            // Non-fuzzy path: use DB-level LIKE filtering + pagination
            self.fuzzy_results.clear();
            let query_param = if self.query.is_empty() {
                None
            } else {
                Some(self.query.as_str())
            };

            if self.unique_mode {
                let new_count = usize::try_from(repo.count_unique_entries(
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    query_param,
                    false,
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?)?;
                self.total_items = new_count;
                self.page = 1;

                let unique_res = repo.get_unique_entries(
                    self.page_size,
                    0,
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    query_param,
                    false,
                    true, // Alphabetical for unique
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?;
                let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();
                self.unique_counts.clear();
                for (entry, count) in entries.iter().zip(counts.iter()) {
                    if let Some(id) = entry.id {
                        self.unique_counts.insert(id, *count);
                    }
                }
                self.entries = entries;
            } else {
                let new_count = usize::try_from(repo.count_filtered_entries(
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    query_param,
                    false,
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?)?;
                self.total_items = new_count;
                self.page = 1;

                let new_entries = repo.get_entries(
                    self.page_size,
                    0,
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    query_param,
                    false,
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?;
                self.entries = new_entries;
            }

            // Apply context sort for non-fuzzy results
            if self.context_boost {
                if let Some(ref cwd) = self.current_cwd {
                    Self::apply_context_sort(&mut self.entries, cwd);
                }
            }
        }

        self.table_state.select(if self.entries.is_empty() {
            None
        } else {
            Some(0)
        });
        Ok(())
    }

    fn set_page(
        &mut self,
        repo: &Repository,
        page: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.page = page;
        let offset = (self.page - 1) * self.page_size;

        if self.fuzzy_results.is_empty() {
            // Standard DB-level pagination
            let query_param = if self.query.is_empty() {
                None
            } else {
                Some(self.query.as_str())
            };

            if self.unique_mode {
                let unique_res = repo.get_unique_entries(
                    self.page_size,
                    offset,
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    query_param,
                    false,
                    true, // Alphabetical for unique
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?;
                let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();
                self.unique_counts.clear();
                for (entry, count) in entries.iter().zip(counts.iter()) {
                    if let Some(id) = entry.id {
                        self.unique_counts.insert(id, *count);
                    }
                }
                self.entries = entries;
            } else {
                let new_entries = repo.get_entries(
                    self.page_size,
                    offset,
                    self.filter_after,
                    self.filter_before,
                    self.filter_tag_id,
                    self.filter_exit_code,
                    query_param,
                    false,
                    self.filter_executor_type.as_deref(),
                    self.filter_cwd.as_deref(),
                )?;
                self.entries = new_entries;
            }

            // Apply context sort for non-fuzzy pages
            if self.context_boost {
                if let Some(ref cwd) = self.current_cwd {
                    Self::apply_context_sort(&mut self.entries, cwd);
                }
            }
        } else {
            // Fuzzy mode: paginate from in-memory scored results
            let end = (offset + self.page_size).min(self.fuzzy_results.len());
            self.entries = if offset < self.fuzzy_results.len() {
                self.fuzzy_results[offset..end].to_vec()
            } else {
                Vec::new()
            };
        }

        self.table_state.select(if self.entries.is_empty() {
            None
        } else {
            Some(0)
        });
        Ok(())
    }

    fn highlight_command(
        command: &str,
        _is_selected: bool,
        width: usize,
    ) -> ratatui::text::Text<'static> {
        let t = theme();
        let mut lines = Vec::new();
        let mut current_line_spans = Vec::new();
        let mut current_line_width = 0;

        let parts: Vec<&str> = command.split_whitespace().collect();
        for (idx, part) in parts.iter().enumerate() {
            // Heuristic syntax highlighting
            let (color, modifier) = if idx == 0 {
                // First word = command (green, bold)
                (t.primary, Modifier::BOLD)
            } else if part.starts_with('-') {
                // Flags (yellow)
                (t.warning, Modifier::empty())
            } else if (part.starts_with('"') && part.ends_with('"'))
                || (part.starts_with('\'') && part.ends_with('\''))
            {
                // Strings (cyan)
                (Color::Cyan, Modifier::empty())
            } else if part.starts_with('$') {
                // Variables (magenta)
                (Color::Magenta, Modifier::empty())
            } else if part.contains('/') || part.starts_with('.') || part.starts_with('~') {
                // Paths
                (t.text_secondary, Modifier::empty())
            } else if *part == "|"
                || *part == "&&"
                || *part == "||"
                || *part == ";"
                || *part == ">"
                || *part == ">>"
                || *part == "<"
            {
                // Operators / pipes
                (t.info, Modifier::BOLD)
            } else {
                // Default (light text)
                (t.text, Modifier::empty())
            };

            let style = Style::default().fg(color).add_modifier(modifier);
            let part_len = part.chars().count();

            // Check wrap
            if width > 0
                && current_line_width + part_len + 1 > width
                && !current_line_spans.is_empty()
            {
                lines.push(Line::from(current_line_spans.clone()));
                current_line_spans.clear();
                current_line_width = 0;
            }

            current_line_spans.push(Span::styled(part.to_string(), style));
            current_line_spans.push(Span::raw(" "));
            current_line_width += part_len + 1;
        }

        if !current_line_spans.is_empty() {
            lines.push(Line::from(current_line_spans));
        }

        if lines.is_empty() {
            return ratatui::text::Text::from(command.to_string());
        }

        ratatui::text::Text::from(lines)
    }
}

// Simple text wrapping helper
fn fill_text(text: &str, width: usize) -> String {
    if width == 0 {
        return text.to_string();
    }
    let mut result = String::new();
    let mut current_line_len = 0;

    for word in text.split_inclusive(' ') {
        let word_len = word.chars().count();
        if current_line_len + word_len > width {
            if !result.is_empty() {
                result.push('\n');
            }
            current_line_len = 0;
        }
        result.push_str(word);
        current_line_len += word_len;
    }
    result
}

// Helper function to create a centered rect using up certain percentage of the available rect
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(popup_layout[1])[1]
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn run_search(
    repo: &Repository,
    initial_query: Option<&str>,
    unique_mode: bool,
    after: Option<&str>,
    before: Option<&str>,
    tag: Option<&str>,
    exit_code: Option<i32>,
    executor: Option<&str>,
    prefix_match: bool,
    cwd: Option<&str>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    // Load config
    let config = crate::config::load_config().unwrap_or_default();
    let page_size = config.search.page_limit;

    let effective_unique = unique_mode || config.search.show_unique_by_default;

    // Load Tags
    let tags = repo.get_tags().unwrap_or_default();

    let tag_id = tag.and_then(|tname| {
        let tname_lower = tname.to_lowercase();
        tags.iter().find(|t| t.name == tname_lower).map(|t| t.id)
    });

    let filter_after = after.and_then(|s| util::parse_date_input(s, false));
    let filter_before = before.and_then(|s| util::parse_date_input(s, true));

    let (entries, total_count, unique_counts) = if effective_unique {
        let count = usize::try_from(repo.count_unique_entries(
            filter_after,
            filter_before,
            tag_id,
            exit_code,
            initial_query,
            prefix_match,
            executor,
            cwd,
        )?)?;
        let unique_res = repo.get_unique_entries(
            page_size,
            0,
            filter_after,
            filter_before,
            tag_id,
            exit_code,
            initial_query,
            prefix_match,
            true,
            executor,
            cwd,
        )?;
        let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();
        let mut count_map = std::collections::HashMap::new();
        for (entry, cnt) in entries.iter().zip(counts.iter()) {
            if let Some(id) = entry.id {
                count_map.insert(id, *cnt);
            }
        }
        (entries, count, count_map)
    } else {
        let count = usize::try_from(repo.count_filtered_entries(
            filter_after,
            filter_before,
            tag_id,
            exit_code,
            initial_query,
            prefix_match,
            executor,
            cwd,
        )?)?;
        let entries = repo.get_entries(
            page_size,
            0,
            filter_after,
            filter_before,
            tag_id,
            exit_code,
            initial_query,
            prefix_match,
            executor,
            cwd,
        )?;
        (entries, count, std::collections::HashMap::new())
    };

    if entries.is_empty() && total_count == 0 {
        eprintln!("No history entries found matching filters.");
        return Ok(None);
    }

    // Load bookmarks and notes
    let bookmarked_commands = repo.get_bookmarked_commands().unwrap_or_default();
    let noted_entry_ids = repo.get_noted_entry_ids().unwrap_or_default();

    // Setup terminal
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;

    let context_boost = config.search.context_boost;
    let show_detail_pane = config.search.show_detail_pane;
    let show_risk_in_search = config.agent.show_risk_in_search;

    let mut app = SearchApp::new(
        entries,
        initial_query.map(String::from),
        total_count,
        1,
        page_size,
        tags,
        effective_unique,
        unique_counts,
        filter_after,
        filter_before,
        tag_id,
        exit_code,
        executor.map(String::from),
        after.map(String::from),
        before.map(String::from),
        tag.map(String::from),
        exit_code.map(|ec| ec.to_string()),
        executor.map(String::from),
        bookmarked_commands,
        cwd.map(String::from),
        noted_entry_ids,
        context_boost,
        show_detail_pane,
        show_risk_in_search,
    );

    let result = app.run(&mut terminal, repo);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Entry;

    fn create_test_entry(cmd: &str) -> Entry {
        Entry {
            id: None,
            session_id: "session123".to_string(),
            command: cmd.to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1000,
            ended_at: 2000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("terminal".to_string()),
        }
    }

    #[test]
    fn test_search_app_initialization() {
        let entries = vec![
            create_test_entry("cargo build"),
            create_test_entry("git status"),
        ];
        let app = SearchApp::new(
            entries.clone(),
            None,
            2,
            1,
            50,
            vec![],
            false,
            std::collections::HashMap::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::collections::HashSet::new(),
            None,
            std::collections::HashSet::new(),
            true,
            true,
            false,
        );

        assert_eq!(app.entries.len(), 2);
        assert_eq!(app.page, 1);
        assert_eq!(app.total_items, 2);
    }

    #[test]
    fn test_pagination_logic() {
        let entries = vec![create_test_entry("cmd")];
        // Pretend we have 1500 items, page size 50. So 30 pages.
        let mut app = SearchApp::new(
            entries,
            None,
            1500,
            1,
            50,
            vec![],
            false,
            std::collections::HashMap::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            std::collections::HashSet::new(),
            None,
            std::collections::HashSet::new(),
            true,
            true,
            false,
        );

        // Next page
        let key = KeyEvent::from(KeyCode::Right);
        let action = app.handle_input(key);
        match action {
            SearchAction::SetPage(p) => assert_eq!(p, 2),
            _ => panic!("Expected SetPage(2)"),
        }

        // Prev page (from page 2)
        app.page = 2;
        let key = KeyEvent::from(KeyCode::Left);
        let action = app.handle_input(key);
        match action {
            SearchAction::SetPage(p) => assert_eq!(p, 1),
            _ => panic!("Expected SetPage(1)"),
        }
    }

    #[test]
    fn test_fuzzy_score_ranking() {
        let entries = vec![
            create_test_entry("git checkout main"),
            create_test_entry("echo hello world"),
            create_test_entry("git commit -m 'fix'"),
            create_test_entry("cargo build"),
        ];

        // "gco" should match git commands but not "echo" or "cargo build"
        let scored = SearchApp::fuzzy_score(entries, "gco", None);
        assert!(!scored.is_empty());
        // Both git commands should match, non-git commands should not
        let cmds: Vec<&str> = scored.iter().map(|e| e.command.as_str()).collect();
        assert!(cmds.contains(&"git checkout main"));
        assert!(cmds.contains(&"git commit -m 'fix'"));
        assert!(!cmds.contains(&"cargo build"));
    }

    #[test]
    fn test_fuzzy_score_no_match() {
        let entries = vec![create_test_entry("ls -la"), create_test_entry("pwd")];

        let scored = SearchApp::fuzzy_score(entries, "zzzzz", None);
        assert!(scored.is_empty());
    }

    #[test]
    fn test_fuzzy_score_filters_irrelevant() {
        let entries = vec![
            create_test_entry("cargo test --release"),
            create_test_entry("cargo build"),
            create_test_entry("npm install"),
            create_test_entry("cargo test"),
        ];

        let scored = SearchApp::fuzzy_score(entries, "cargo test", None);
        assert!(!scored.is_empty());
        // Both "cargo test" entries should match, "npm install" should not
        let cmds: Vec<&str> = scored.iter().map(|e| e.command.as_str()).collect();
        assert!(cmds.contains(&"cargo test"));
        assert!(cmds.contains(&"cargo test --release"));
        assert!(!cmds.contains(&"npm install"));
    }
}
