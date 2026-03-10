mod data;
mod format;
mod input;
mod render;

#[cfg(test)]
mod tests;

use crate::models::{Entry, SearchField, Tag};
use crate::repository::{QueryFilter, Repository};
use crate::util;
use arboard::Clipboard;
use chrono::Local;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::{
    backend::CrosstermBackend,
    widgets::{ListState, TableState},
    Terminal,
};
use std::io;

#[derive(Clone, Debug)]
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

/// Mutually exclusive dialog overlay states.
#[derive(Default)]
pub enum DialogState {
    #[default]
    None,
    Filter,
    Delete {
        entry_id: i64,
    },
    GoToPage {
        input: String,
    },
    TagAssociation,
    Note {
        entry_id: i64,
        input: String,
    },
}

/// Display/mode options that control how search results are presented.
pub struct ViewOptions {
    pub unique_mode: bool,
    pub context_boost: bool,
    pub detail_pane_open: bool,
    pub search_field: SearchField,
    pub current_cwd: Option<String>,
}

/// Active filter values and filter-dialog text inputs.
pub struct FilterState {
    // Applied filter values
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub tag_id: Option<i64>,
    pub exit_code: Option<i32>,
    pub executor_type: Option<String>,
    pub cwd: Option<String>,

    // Dialog text inputs (persist across filter-dialog open/close)
    pub start_date_input: String,
    pub end_date_input: String,
    pub tag_filter_input: String,
    pub exit_code_input: String,
    pub executor_filter_input: String,
    pub focus_index: usize, // 0=start, 1=end, 2=tag, 3=exit, 4=executor
}

/// Pagination state.
pub struct PaginationState {
    pub page: usize, // 1-based index
    pub total_items: usize,
    pub page_size: usize,
}

/// Configuration bundle for constructing a `SearchApp`, reducing constructor parameter count.
pub struct SearchConfig {
    pub entries: Vec<Entry>,
    pub initial_query: Option<String>,
    pub total_items: usize,
    pub page: usize,
    pub page_size: usize,
    pub tags: Vec<Tag>,
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
    pub show_risk_in_search: bool,
    pub view: ViewOptions,
}

pub struct SearchApp {
    query: String,
    entries: Vec<Entry>,
    table_state: TableState,

    pub pagination: PaginationState,
    pub filters: FilterState,
    dialog: DialogState,
    pub view: ViewOptions,
    show_risk_in_search: bool,

    // Unique mode counts (keyed by entry ID)
    pub unique_counts: std::collections::HashMap<i64, i64>,

    // Tags (for tag association dialog)
    tags: Vec<Tag>,
    tag_list_state: ListState,

    // Annotations
    noted_entry_ids: std::collections::HashSet<i64>,
    bookmarked_commands: std::collections::HashSet<String>,

    // Fuzzy search: cached scored results for pagination
    fuzzy_results: Vec<Entry>,

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

        let mut view = cfg.view;
        view.current_cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string());

        let mut app = Self {
            query,
            entries: cfg.entries,
            table_state: TableState::default(),

            pagination: PaginationState {
                page: cfg.page,
                total_items: cfg.total_items,
                page_size: cfg.page_size.max(1),
            },

            filters: FilterState {
                after: cfg.filter_after,
                before: cfg.filter_before,
                tag_id: cfg.filter_tag_id,
                exit_code: cfg.filter_exit_code,
                executor_type: cfg.filter_executor_type,
                cwd: cfg.filter_cwd,
                start_date_input: start_default,
                end_date_input: end_default,
                tag_filter_input: cfg.tag_filter_input.unwrap_or_default(),
                exit_code_input: cfg.exit_code_input.unwrap_or_default(),
                executor_filter_input: cfg.executor_filter_input.unwrap_or_default(),
                focus_index: 0,
            },

            dialog: DialogState::None,
            view,
            show_risk_in_search: cfg.show_risk_in_search,

            unique_counts: cfg.unique_counts,

            tags: cfg.tags,
            tag_list_state: ListState::default(),

            noted_entry_ids: cfg.noted_entry_ids,
            bookmarked_commands: cfg.bookmarked_commands,

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

    pub fn run(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
        repo: &Repository,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        loop {
            self.render(terminal)?;

            let timeout = if self.status_message.is_some() {
                std::time::Duration::from_secs(2)
            } else {
                std::time::Duration::from_secs(60)
            };
            if !event::poll(timeout)? {
                continue;
            }
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match self.handle_input(key) {
                        SearchAction::Select(cmd) => return Ok(Some(cmd)),
                        SearchAction::Exit => return Ok(None),
                        SearchAction::Reload => self.reload_entries(repo)?,
                        SearchAction::SetPage(page) => self.set_page(repo, page)?,
                        other => self.dispatch_action(other, repo)?,
                    }
                }
            }
        }
    }

    /// Handle side-effecting actions (copy, delete, notes, bookmarks, tags).
    fn dispatch_action(
        &mut self,
        action: SearchAction,
        repo: &Repository,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let now = std::time::Instant::now;
        match action {
            SearchAction::Copy(cmd) => match Clipboard::new() {
                Ok(mut clipboard) => {
                    if clipboard.set_text(cmd).is_ok() {
                        self.status_message = Some(("Copied!".into(), now()));
                    } else {
                        self.status_message = Some(("Copy failed".into(), now()));
                    }
                }
                Err(_) => {
                    self.status_message = Some(("Clipboard unavailable".into(), now()));
                }
            },
            SearchAction::Delete(id) => match repo.delete_entry(id) {
                Ok(()) => {
                    self.reload_entries(repo)?;
                    self.status_message = Some(("Deleted!".into(), now()));
                }
                Err(e) => {
                    self.status_message = Some((format!("Delete failed: {e}"), now()));
                }
            },
            SearchAction::SaveNote(entry_id, text) => match repo.upsert_note(entry_id, &text) {
                Ok(()) => {
                    self.noted_entry_ids.insert(entry_id);
                    self.status_message = Some(("Note saved!".into(), now()));
                }
                Err(e) => {
                    self.status_message = Some((format!("Note save failed: {e}"), now()));
                }
            },
            SearchAction::DeleteNote(entry_id) => match repo.delete_note(entry_id) {
                Ok(_) => {
                    self.noted_entry_ids.remove(&entry_id);
                    self.status_message = Some(("Note deleted".into(), now()));
                }
                Err(e) => {
                    self.status_message = Some((format!("Note delete failed: {e}"), now()));
                }
            },
            SearchAction::ToggleBookmark(cmd) => {
                if self.bookmarked_commands.contains(&cmd) {
                    match repo.remove_bookmark(&cmd) {
                        Ok(_) => {
                            self.bookmarked_commands.remove(&cmd);
                            self.status_message = Some(("Bookmark removed".into(), now()));
                        }
                        Err(e) => {
                            self.status_message =
                                Some((format!("Bookmark remove failed: {e}"), now()));
                        }
                    }
                } else {
                    match repo.add_bookmark(&cmd, None) {
                        Ok(_) => {
                            self.bookmarked_commands.insert(cmd);
                            self.status_message = Some(("Bookmarked!".into(), now()));
                        }
                        Err(e) => {
                            self.status_message = Some((format!("Bookmark failed: {e}"), now()));
                        }
                    }
                }
            }
            SearchAction::AssociateSession(tag_id) => {
                let sid = std::env::var("SUVADU_SESSION_ID").unwrap_or_default();
                if sid.is_empty() {
                    self.status_message = Some(("No session ID found".into(), now()));
                } else if let Err(e) = repo.tag_session(&sid, Some(tag_id)) {
                    self.status_message = Some((format!("Error: {e}"), now()));
                } else {
                    let tag_name = self
                        .tags
                        .iter()
                        .find(|t| t.id == tag_id)
                        .map(|t| t.name.clone())
                        .unwrap_or_default();
                    self.status_message = Some((format!("Session tagged: {tag_name}"), now()));
                }
            }
            SearchAction::Continue
            | SearchAction::Select(_)
            | SearchAction::Exit
            | SearchAction::Reload
            | SearchAction::SetPage(_) => {}
        }
        Ok(())
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

type SearchEntries = (Vec<Entry>, usize, std::collections::HashMap<i64, i64>);

fn load_search_entries(
    repo: &Repository,
    qf: &QueryFilter,
    page_size: usize,
    unique: bool,
) -> Result<SearchEntries, Box<dyn std::error::Error>> {
    if unique {
        let count = usize::try_from(repo.count_unique_filtered(qf)?)?;
        let unique_res = repo.get_unique_entries_filtered(page_size, 0, qf, true)?;
        let (entries, counts): (Vec<Entry>, Vec<i64>) = unique_res.into_iter().unzip();
        let mut count_map = std::collections::HashMap::new();
        for (entry, cnt) in entries.iter().zip(counts.iter()) {
            if let Some(id) = entry.id {
                count_map.insert(id, *cnt);
            }
        }
        Ok((entries, count, count_map))
    } else {
        let count = usize::try_from(repo.count_filtered(qf)?)?;
        let entries = repo.get_entries_filtered(page_size, 0, qf)?;
        Ok((entries, count, std::collections::HashMap::new()))
    }
}

/// Parameters for `run_search` — bundles the CLI flags into one struct
/// to avoid excessive positional arguments.
pub struct SearchArgs<'a> {
    pub initial_query: Option<&'a str>,
    pub unique_mode: bool,
    pub after: Option<&'a str>,
    pub before: Option<&'a str>,
    pub tag: Option<&'a str>,
    pub exit_code: Option<i32>,
    pub executor: Option<&'a str>,
    pub prefix_match: bool,
    pub cwd: Option<&'a str>,
    pub field: SearchField,
}

pub fn run_search(
    repo: &Repository,
    args: &SearchArgs,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let config = crate::config::load_config().unwrap_or_else(|e| {
        eprintln!("suvadu: config load failed, using defaults: {e}");
        crate::config::Config::default()
    });
    let page_size = config.search.page_limit;
    let effective_unique = args.unique_mode || config.search.show_unique_by_default;
    let tags = repo.get_tags().unwrap_or_default();

    let tag_id = match args.tag.map(|t| repo.get_tag_id_by_name(t)).transpose() {
        Ok(opt) => opt.flatten(),
        Err(e) => {
            eprintln!("suvadu: tag lookup failed: {e}");
            None
        }
    };

    let filter_after = args.after.and_then(|s| util::parse_date_input(s, false));
    let filter_before = args.before.and_then(|s| util::parse_date_input(s, true));

    let qf = QueryFilter {
        after: filter_after,
        before: filter_before,
        tag_id,
        exit_code: args.exit_code,
        query: args.initial_query,
        prefix_match: args.prefix_match,
        executor: args.executor,
        cwd: args.cwd,
        field: args.field,
    };

    let (entries, total_count, unique_counts) =
        load_search_entries(repo, &qf, page_size, effective_unique)?;

    if entries.is_empty() && total_count == 0 {
        eprintln!("No history entries found matching filters.");
        return Ok(None);
    }

    let bookmarked_commands = repo.get_bookmarked_commands().unwrap_or_default();
    let noted_entry_ids = repo.get_noted_entry_ids().unwrap_or_default();

    let _guard = crate::util::TerminalGuardStderr::new()?;
    let backend = CrosstermBackend::new(io::stderr());
    let mut terminal = Terminal::new(backend)?;

    let mut app = SearchApp::new(SearchConfig {
        entries,
        initial_query: args.initial_query.map(String::from),
        total_items: total_count,
        page: 1,
        page_size,
        tags,
        unique_counts,
        filter_after,
        filter_before,
        filter_tag_id: tag_id,
        filter_exit_code: args.exit_code,
        filter_executor_type: args.executor.map(String::from),
        start_date_input: args.after.map(String::from),
        end_date_input: args.before.map(String::from),
        tag_filter_input: args.tag.map(String::from),
        exit_code_input: args.exit_code.map(|ec| ec.to_string()),
        executor_filter_input: args.executor.map(String::from),
        bookmarked_commands,
        filter_cwd: args.cwd.map(String::from),
        noted_entry_ids,
        show_risk_in_search: config.agent.show_risk_in_search,
        view: ViewOptions {
            unique_mode: effective_unique,
            context_boost: config.search.context_boost,
            detail_pane_open: config.search.show_detail_pane,
            search_field: args.field,
            current_cwd: None, // set in SearchApp::new
        },
    });

    let result = app.run(&mut terminal, repo);
    terminal.show_cursor()?;
    result
}
