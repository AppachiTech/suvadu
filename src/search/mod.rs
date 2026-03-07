mod data;
mod input;
mod render;

#[cfg(test)]
mod tests;

use crate::models::{Entry, Tag};
use crate::repository::{QueryFilter, Repository};
use crate::util;
use arboard::Clipboard;
use chrono::Local;
use crossterm::{
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    widgets::{ListState, TableState},
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

/// Configuration bundle for constructing a `SearchApp`, reducing constructor parameter count.
#[allow(clippy::struct_excessive_bools)]
pub struct SearchConfig {
    pub entries: Vec<Entry>,
    pub initial_query: Option<String>,
    pub total_items: usize,
    pub page: usize,
    pub page_size: usize,
    pub tags: Vec<Tag>,
    pub unique_mode: bool,
    pub unique_counts: std::collections::HashMap<i64, i64>,
    pub filter_after: Option<i64>,
    pub filter_before: Option<i64>,
    pub filter_tag_id: Option<i64>,
    pub filter_exit_code: Option<i32>,
    pub filter_executor_type: Option<String>,
    pub start_date_input: Option<String>,
    pub end_date_input: Option<String>,
    pub tag_filter_input: Option<String>,
    pub exit_code_input: Option<String>,
    pub executor_filter_input: Option<String>,
    pub bookmarked_commands: std::collections::HashSet<String>,
    pub filter_cwd: Option<String>,
    pub noted_entry_ids: std::collections::HashSet<i64>,
    pub context_boost: bool,
    pub show_detail_pane: bool,
    pub show_risk_in_search: bool,
    pub search_field: String,
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

    // Field-specific search (command, cwd, session, executor)
    pub search_field: String,

    // UI Feedback
    status_message: Option<(String, std::time::Instant)>,
}

impl SearchApp {
    pub fn new(cfg: SearchConfig) -> Self {
        let query = cfg.initial_query.unwrap_or_default();

        let now = Local::now();
        let five_days_ago = now - chrono::Duration::days(5);
        let start_default = cfg
            .start_date_input
            .unwrap_or_else(|| five_days_ago.format("%Y-%m-%d").to_string());
        let end_default = cfg.end_date_input.unwrap_or_else(|| "today".to_string());

        let mut app = Self {
            query,
            entries: cfg.entries,
            table_state: TableState::default(),

            page: cfg.page,
            total_items: cfg.total_items,
            page_size: cfg.page_size.max(1),

            filter_mode: false,
            start_date_input: start_default,
            end_date_input: end_default,
            tag_filter_input: cfg.tag_filter_input.unwrap_or_default(),
            exit_code_input: cfg.exit_code_input.unwrap_or_default(),
            executor_filter_input: cfg.executor_filter_input.unwrap_or_default(),
            focus_index: 0,

            filter_after: cfg.filter_after,
            filter_before: cfg.filter_before,
            filter_tag_id: cfg.filter_tag_id,
            filter_exit_code: cfg.filter_exit_code,
            filter_executor_type: cfg.filter_executor_type,

            unique_mode: cfg.unique_mode,
            unique_counts: cfg.unique_counts,

            delete_dialog_open: false,
            pending_delete_id: None,

            goto_dialog_open: false,
            goto_input: String::new(),

            tag_dialog_open: false,
            tags: cfg.tags,
            tag_list_state: ListState::default(),

            noted_entry_ids: cfg.noted_entry_ids,
            note_dialog_open: false,
            note_input: String::new(),
            note_entry_id: None,

            filter_cwd: cfg.filter_cwd,

            detail_pane_open: cfg.show_detail_pane,
            show_risk_in_search: cfg.show_risk_in_search,

            context_boost: cfg.context_boost,
            current_cwd: std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string()),

            bookmarked_commands: cfg.bookmarked_commands,

            fuzzy_results: Vec::new(),

            search_field: cfg.search_field,

            status_message: None,
        };
        app.table_state.select(if app.entries.is_empty() {
            None
        } else {
            Some(0)
        });
        app
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
                        SearchAction::Copy(cmd) => match Clipboard::new() {
                            Ok(mut clipboard) => {
                                if clipboard.set_text(cmd.clone()).is_ok() {
                                    self.status_message =
                                        Some(("Copied!".to_string(), std::time::Instant::now()));
                                } else {
                                    self.status_message = Some((
                                        "Copy failed".to_string(),
                                        std::time::Instant::now(),
                                    ));
                                }
                            }
                            Err(_) => {
                                self.status_message = Some((
                                    "Clipboard unavailable".to_string(),
                                    std::time::Instant::now(),
                                ));
                            }
                        },
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

use crate::util::centered_rect;

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
    field: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    // Load config
    let config = crate::config::load_config().unwrap_or_default();
    let page_size = config.search.page_limit;

    let effective_unique = unique_mode || config.search.show_unique_by_default;

    // Load Tags
    let tags = repo.get_tags().unwrap_or_default();

    let tag_id = tag
        .map(|t| repo.get_tag_id_by_name(t))
        .transpose()
        .unwrap_or(None)
        .flatten();

    let filter_after = after.and_then(|s| util::parse_date_input(s, false));
    let filter_before = before.and_then(|s| util::parse_date_input(s, true));

    let qf = QueryFilter {
        after: filter_after,
        before: filter_before,
        tag_id,
        exit_code,
        query: initial_query,
        prefix_match,
        executor,
        cwd,
        field,
    };

    let (entries, total_count, unique_counts) = if effective_unique {
        let count = usize::try_from(repo.count_unique_filtered(&qf)?)?;
        let unique_res = repo.get_unique_entries_filtered(page_size, 0, &qf, true)?;
        let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();
        let mut count_map = std::collections::HashMap::new();
        for (entry, cnt) in entries.iter().zip(counts.iter()) {
            if let Some(id) = entry.id {
                count_map.insert(id, *cnt);
            }
        }
        (entries, count, count_map)
    } else {
        let count = usize::try_from(repo.count_filtered(&qf)?)?;
        let entries = repo.get_entries_filtered(page_size, 0, &qf)?;
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

    let mut app = SearchApp::new(SearchConfig {
        entries,
        initial_query: initial_query.map(String::from),
        total_items: total_count,
        page: 1,
        page_size,
        tags,
        unique_mode: effective_unique,
        unique_counts,
        filter_after,
        filter_before,
        filter_tag_id: tag_id,
        filter_exit_code: exit_code,
        filter_executor_type: executor.map(String::from),
        start_date_input: after.map(String::from),
        end_date_input: before.map(String::from),
        tag_filter_input: tag.map(String::from),
        exit_code_input: exit_code.map(|ec| ec.to_string()),
        executor_filter_input: executor.map(String::from),
        bookmarked_commands,
        filter_cwd: cwd.map(String::from),
        noted_entry_ids,
        context_boost,
        show_detail_pane,
        show_risk_in_search,
        search_field: field.to_string(),
    });

    let result = app.run(&mut terminal, repo);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}
