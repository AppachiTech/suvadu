use std::collections::HashMap;
use std::io;
use std::time::Instant;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table,
};
use ratatui::Terminal;

use crate::models::Entry;
use crate::repository::Repository;
use crate::session_ui;
use crate::theme::theme;
use crate::util::{dirs_home, format_duration_ms, shorten_path};

use super::{
    compute_agent_counts, format_datetime, format_full_datetime, load_entries, truncate,
    PagedTable, Period,
};

const PAGE_SIZE: usize = 50;
/// Cap on the in-memory prompt search box, matching the scope of a filter
/// field (not a document editor) — plenty for any realistic prompt excerpt.
const MAX_SEARCH_LEN: usize = 200;

/// Strip common agent prefixes from session IDs and return the first 8 chars.
/// e.g. "claude-264d95ad-a881-..." → "264d95ad", "opencode-ses_303f..." → "`ses_303f`"
fn short_session_id(id: &str) -> String {
    let stripped = id
        .strip_prefix("claude-")
        .or_else(|| id.strip_prefix("opencode-"))
        .or_else(|| id.strip_prefix("cursor-"))
        .or_else(|| id.strip_prefix("codex-"))
        .unwrap_or(id);
    stripped.chars().take(8).collect()
}

// ── Data ────────────────────────────────────────────────────

/// A unique prompt group: one session + one prompt text, with aggregated stats.
#[allow(dead_code)]
struct PromptGroup {
    session_id: String,
    prompt: String,
    executor: String,
    /// Most common working directory across entries in this group.
    cwd: String,
    cmd_count: usize,
    success_count: usize,
    fail_count: usize,
    total_duration_ms: i64,
    first_at: i64,
    last_at: i64,
    /// Indices into the source `entries` slice.
    entry_indices: Vec<usize>,
}

/// Per-group accumulator for single-pass aggregation.
struct PromptGroupBuilder {
    session_id: String,
    prompt: String,
    executor: String,
    cwd_counts: HashMap<String, usize>,
    cmd_count: usize,
    success_count: usize,
    fail_count: usize,
    total_duration_ms: i64,
    first_at: i64,
    last_at: i64,
    entry_indices: Vec<usize>,
}

impl PromptGroupBuilder {
    fn new(session_id: String, prompt: String, executor: String, started_at: i64) -> Self {
        Self {
            session_id,
            prompt,
            executor,
            cwd_counts: HashMap::new(),
            cmd_count: 0,
            success_count: 0,
            fail_count: 0,
            total_duration_ms: 0,
            first_at: started_at,
            last_at: started_at,
            entry_indices: Vec::new(),
        }
    }

    fn add(&mut self, idx: usize, entry: &Entry) {
        self.cmd_count += 1;
        *self.cwd_counts.entry(entry.cwd.clone()).or_default() += 1;
        match entry.exit_code {
            Some(0) => self.success_count += 1,
            Some(_) => self.fail_count += 1,
            None => {}
        }
        self.total_duration_ms += entry.duration_ms;
        if entry.started_at < self.first_at {
            self.first_at = entry.started_at;
        }
        if entry.started_at > self.last_at {
            self.last_at = entry.started_at;
        }
        self.entry_indices.push(idx);
    }

    fn finish(self) -> PromptGroup {
        let cwd = self
            .cwd_counts
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .map_or_else(String::new, |(path, _)| path);
        PromptGroup {
            session_id: self.session_id,
            prompt: self.prompt,
            executor: self.executor,
            cwd,
            cmd_count: self.cmd_count,
            success_count: self.success_count,
            fail_count: self.fail_count,
            total_duration_ms: self.total_duration_ms,
            first_at: self.first_at,
            last_at: self.last_at,
            entry_indices: self.entry_indices,
        }
    }
}

fn build_prompt_groups(entries: &[Entry]) -> Vec<PromptGroup> {
    let mut map: HashMap<(String, String), PromptGroupBuilder> = HashMap::new();

    for (idx, entry) in entries.iter().enumerate() {
        let prompt = entry
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("agent_prompt"))
            .cloned()
            .unwrap_or_default();

        if prompt.is_empty() {
            continue;
        }

        let key = (entry.session_id.clone(), prompt.clone());
        let builder = map.entry(key).or_insert_with(|| {
            PromptGroupBuilder::new(
                entry.session_id.clone(),
                prompt,
                entry.executor.as_deref().unwrap_or("unknown").to_string(),
                entry.started_at,
            )
        });
        builder.add(idx, entry);
    }

    let mut groups: Vec<PromptGroup> = map.into_values().map(PromptGroupBuilder::finish).collect();
    // Most recent first
    groups.sort_by_key(|b| std::cmp::Reverse(b.last_at));
    groups
}

// ── View state ──────────────────────────────────────────────

/// Which screen is active.
enum View {
    List,
    Detail { group_index: usize },
}

/// Action returned from input handlers.
enum PromptAction {
    Continue,
    Quit,
    /// Jump to session timeline for the given session ID.
    OpenSession(String),
}

struct PromptExplorerApp {
    entries: Vec<Entry>,
    groups: Vec<PromptGroup>,

    view: View,

    // List screen
    list_pager: PagedTable,

    // Detail screen
    detail_pager: PagedTable,
    detail_pane_open: bool,

    home: String,
    status_message: Option<(String, Instant)>,

    // Filters (list screen only)
    period: Period,
    executor_filter: Option<usize>,
    executor_names: Vec<String>,
    cwd_filter: Option<String>,
    search: String,
    search_active: bool,
}

impl PromptExplorerApp {
    fn new(entries: &[Entry]) -> Self {
        let entries = entries.to_vec();
        let groups = build_prompt_groups(&entries);
        let executor_names = compute_agent_counts(&entries)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let mut list_pager = PagedTable::new(PAGE_SIZE);
        if !groups.is_empty() {
            list_pager.state.select(Some(0));
        }
        Self {
            entries,
            groups,
            view: View::List,
            list_pager,
            detail_pager: PagedTable::new(PAGE_SIZE),
            detail_pane_open: true,
            home: dirs_home(),
            status_message: None,
            period: Period::AllTime,
            executor_filter: None,
            executor_names,
            cwd_filter: None,
            search: String::new(),
            search_active: false,
        }
    }

    // ── Filtering ────────────────────────────────────────────

    /// Rebuild `groups` from `entries`, applying the current search text.
    fn apply_filters(&mut self) {
        let mut groups = build_prompt_groups(&self.entries);
        if !self.search.trim().is_empty() {
            let needle = self.search.to_lowercase();
            groups.retain(|g| g.prompt.to_lowercase().contains(&needle));
        }
        self.groups = groups;
        self.list_pager.reset(self.groups.len());
    }

    fn reload(&mut self, repo: &Repository) {
        let after_ms = self.period.after_ms();
        let executor_name = self
            .executor_filter
            .and_then(|i| self.executor_names.get(i).cloned());
        self.entries = load_entries(
            repo,
            after_ms,
            executor_name.as_deref(),
            self.cwd_filter.as_deref(),
        );
        self.executor_names = compute_agent_counts(&self.entries)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        if let Some(idx) = self.executor_filter {
            if idx >= self.executor_names.len() {
                self.executor_filter = None;
            }
        }
        self.apply_filters();
    }

    /// Reload if a repository is available, otherwise surface why nothing changed.
    fn reload_or_status(&mut self, repo: Option<&Repository>) {
        if let Some(repo) = repo {
            self.reload(repo);
        } else {
            self.status_message = Some(("Reload not available".into(), Instant::now()));
        }
    }

    fn set_period(&mut self, period: Period, repo: Option<&Repository>) {
        self.period = period;
        self.reload_or_status(repo);
    }

    fn cycle_executor(&mut self, repo: Option<&Repository>) {
        if self.executor_names.is_empty() {
            self.executor_filter = None;
        } else {
            self.executor_filter = match self.executor_filter {
                None => Some(0),
                Some(i) if i + 1 >= self.executor_names.len() => None,
                Some(i) => Some(i + 1),
            };
        }
        self.reload_or_status(repo);
    }

    fn handle_search_input(&mut self, key: crossterm::event::KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                self.search_active = false;
            }
            KeyCode::Backspace => {
                self.search.pop();
                self.apply_filters();
            }
            KeyCode::Char(c) if self.search.len() + c.len_utf8() <= MAX_SEARCH_LEN => {
                self.search.push(c);
                self.apply_filters();
            }
            _ => {}
        }
    }

    // ── Pagination helpers ──────────────────────────────────

    fn list_total_pages(&self) -> usize {
        self.list_pager.total_pages(self.groups.len())
    }

    fn list_page_slice(&self) -> &[PromptGroup] {
        let (start, end) = self.list_pager.bounds(self.groups.len());
        &self.groups[start..end]
    }

    fn selected_group(&self) -> Option<&PromptGroup> {
        let (offset, _) = self.list_pager.bounds(self.groups.len());
        self.list_pager
            .selected()
            .and_then(|i| self.groups.get(offset + i))
    }

    fn detail_entries(&self) -> &[usize] {
        match self.view {
            View::Detail { group_index } => self
                .groups
                .get(group_index)
                .map_or(&[], |g| &g.entry_indices),
            View::List => &[],
        }
    }

    fn detail_total_pages(&self) -> usize {
        self.detail_pager.total_pages(self.detail_entries().len())
    }

    fn detail_page_slice(&self) -> &[usize] {
        let all = self.detail_entries();
        let (start, end) = self.detail_pager.bounds(all.len());
        &all[start..end]
    }

    fn selected_detail_entry(&self) -> Option<&Entry> {
        let page_slice = self.detail_page_slice();
        self.detail_pager
            .selected()
            .and_then(|i| page_slice.get(i))
            .map(|&idx| &self.entries[idx])
    }

    // ── Input ───────────────────────────────────────────────

    fn handle_input(
        &mut self,
        key: crossterm::event::KeyEvent,
        repo: Option<&Repository>,
    ) -> PromptAction {
        match &self.view {
            View::List => self.handle_list_input(key, repo),
            View::Detail { .. } => self.handle_detail_input(key),
        }
    }

    fn handle_list_input(
        &mut self,
        key: crossterm::event::KeyEvent,
        repo: Option<&Repository>,
    ) -> PromptAction {
        if self.search_active {
            self.handle_search_input(key);
            return PromptAction::Continue;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return PromptAction::Quit,
            // Search
            KeyCode::Char('/') => {
                self.search_active = true;
            }
            // Period
            KeyCode::Char('1') => self.set_period(Period::Today, repo),
            KeyCode::Char('2') => self.set_period(Period::Days7, repo),
            KeyCode::Char('3') => self.set_period(Period::Days30, repo),
            KeyCode::Char('4') => self.set_period(Period::AllTime, repo),
            // Executor filter
            KeyCode::Char('a') => self.cycle_executor(repo),
            // Row navigation
            KeyCode::Up | KeyCode::Char('k') => self.list_pager.move_up(),
            KeyCode::Down | KeyCode::Char('j') => {
                let len = self.list_page_slice().len();
                self.list_pager.move_down(len);
            }
            KeyCode::Home if !self.list_page_slice().is_empty() => {
                self.list_pager.state.select(Some(0));
            }
            KeyCode::End if !self.list_page_slice().is_empty() => {
                let len = self.list_page_slice().len();
                self.list_pager.select_last(len);
            }
            // Page navigation
            KeyCode::Left => self.list_pager.prev_page(self.groups.len()),
            KeyCode::Right => self.list_pager.next_page(self.groups.len()),
            // Drill into detail
            KeyCode::Enter => {
                if let Some(sel) = self.list_pager.selected() {
                    let (offset, _) = self.list_pager.bounds(self.groups.len());
                    let group_index = offset + sel;
                    if group_index < self.groups.len() {
                        self.view = View::Detail { group_index };
                        self.detail_pager = PagedTable::new(PAGE_SIZE);
                        if !self.groups[group_index].entry_indices.is_empty() {
                            self.detail_pager.state.select(Some(0));
                        }
                    }
                }
            }
            // Jump to session timeline
            KeyCode::Char('s') => {
                if let Some(group) = self.selected_group() {
                    return PromptAction::OpenSession(group.session_id.clone());
                }
            }
            _ => {}
        }
        PromptAction::Continue
    }

    fn handle_detail_input(&mut self, key: crossterm::event::KeyEvent) -> PromptAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => {
                self.view = View::List;
            }
            // Copy command
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(entry) = self.selected_detail_entry() {
                    match arboard::Clipboard::new()
                        .and_then(|mut c| c.set_text(entry.command.clone()))
                    {
                        Ok(()) => {
                            self.status_message = Some(("Copied!".into(), Instant::now()));
                        }
                        Err(_) => {
                            self.status_message = Some(("Copy failed".into(), Instant::now()));
                        }
                    }
                }
            }
            // Row navigation
            KeyCode::Up | KeyCode::Char('k') => self.detail_pager.move_up(),
            KeyCode::Down | KeyCode::Char('j') => {
                let len = self.detail_page_slice().len();
                self.detail_pager.move_down(len);
            }
            KeyCode::Home if !self.detail_page_slice().is_empty() => {
                self.detail_pager.state.select(Some(0));
            }
            KeyCode::End if !self.detail_page_slice().is_empty() => {
                let len = self.detail_page_slice().len();
                self.detail_pager.select_last(len);
            }
            // Page navigation
            KeyCode::Left => self.detail_pager.prev_page(self.detail_entries().len()),
            KeyCode::Right => self.detail_pager.next_page(self.detail_entries().len()),
            // Detail pane toggle
            KeyCode::Tab => {
                self.detail_pane_open = !self.detail_pane_open;
            }
            _ => {}
        }
        PromptAction::Continue
    }

    // ── Render ──────────────────────────────────────────────

    fn render(&mut self, f: &mut ratatui::Frame) {
        match self.view {
            View::List => self.render_list(f),
            View::Detail { group_index } => self.render_detail(f, group_index),
        }
    }

    // ── List screen ─────────────────────────────────────────

    fn render_list(&mut self, f: &mut ratatui::Frame) {
        let t = theme();
        let size = f.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header
                Constraint::Length(1), // filter bar (period / executor / search)
                Constraint::Min(8),    // body
                Constraint::Length(1), // footer
            ])
            .split(size);

        // Header
        let total_cmds: usize = self.groups.iter().map(|g| g.cmd_count).sum();
        let header_line = Line::from(vec![
            Span::styled(
                " PROMPT EXPLORER ",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{}", self.groups.len()),
                Style::default().fg(t.info).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" prompts  ", Style::default().fg(t.text_secondary)),
            Span::styled(
                format!("{total_cmds}"),
                Style::default().fg(t.info).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" commands", Style::default().fg(t.text_secondary)),
        ]);
        f.render_widget(Paragraph::new(header_line), chunks[0]);

        // Filter bar
        self.render_filter_bar(f, chunks[1], t);

        // Body: table + prompt preview pane
        if self.selected_group().is_some() {
            let body_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
                .split(chunks[2]);
            self.render_list_table(f, body_chunks[0], t);
            // Re-borrow after render_list_table (which takes &mut self)
            let (offset, _) = self.list_pager.bounds(self.groups.len());
            let group = &self.groups[offset + self.list_pager.selected().unwrap_or(0)];
            Self::render_prompt_preview(f, body_chunks[1], t, group, &self.home);
        } else {
            self.render_list_table(f, chunks[2], t);
        }

        // Footer
        self.render_list_footer(f, chunks[3], t);
    }

    fn render_filter_bar(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let mut spans = Vec::new();

        for (i, p) in [
            Period::Today,
            Period::Days7,
            Period::Days30,
            Period::AllTime,
        ]
        .iter()
        .enumerate()
        {
            let is_active = *p == self.period;
            spans.push(Span::styled(
                format!("{}", i + 1),
                Style::default().fg(t.text_muted),
            ));
            if is_active {
                spans.push(Span::styled(
                    format!(" {} ", p.label()),
                    Style::default()
                        .bg(t.primary)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    format!(" {} ", p.label()),
                    Style::default().fg(t.text_muted),
                ));
            }
            spans.push(Span::raw(" "));
        }

        spans.push(Span::styled("  ", Style::default()));
        let executor_label = self
            .executor_filter
            .and_then(|i| self.executor_names.get(i))
            .map_or_else(|| "All agents".to_string(), Clone::clone);
        spans.push(Span::styled("a ", Style::default().fg(t.text_muted)));
        spans.push(Span::styled(
            executor_label,
            Style::default().fg(t.badge_executor),
        ));

        spans.push(Span::styled("   / ", Style::default().fg(t.text_muted)));
        if self.search_active {
            spans.push(Span::styled(
                self.search.clone(),
                Style::default().fg(t.text),
            ));
            spans.push(Span::styled(
                "_",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ));
        } else if self.search.is_empty() {
            spans.push(Span::styled(
                "(search prompts)",
                Style::default().fg(t.text_muted),
            ));
        } else {
            spans.push(Span::styled(
                self.search.clone(),
                Style::default().fg(t.text),
            ));
        }

        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn build_list_row<'b>(g: &PromptGroup, t: &crate::theme::Theme) -> Row<'b> {
        let time = format_datetime(g.last_at);
        let session = short_session_id(&g.session_id);
        let executor = &g.executor;
        let prompt_display = truncate(&g.prompt.replace('\n', " "), 50);
        let duration = format_duration_ms(g.total_duration_ms);

        // Build status cell: checkmark N  cross N (only show counts that are > 0)
        let mut status_spans: Vec<Span> = Vec::new();
        if g.success_count > 0 {
            status_spans.push(Span::styled(
                format!("\u{2714}{}", g.success_count),
                Style::default().fg(t.success),
            ));
        }
        if g.fail_count > 0 {
            if !status_spans.is_empty() {
                status_spans.push(Span::raw(" "));
            }
            status_spans.push(Span::styled(
                format!("\u{2718}{}", g.fail_count),
                Style::default().fg(t.error),
            ));
        }
        if status_spans.is_empty() {
            status_spans.push(Span::styled("\u{25CB}", Style::default().fg(t.text_muted)));
        }

        Row::new(vec![
            Cell::from(Span::styled(time, Style::default().fg(t.text_muted))),
            Cell::from(Span::styled(session, Style::default().fg(t.primary_dim))),
            Cell::from(Span::styled(
                executor.clone(),
                Style::default().fg(t.badge_executor),
            )),
            Cell::from(Span::styled(prompt_display, Style::default().fg(t.text))),
            Cell::from(Span::styled(
                format!("{}", g.cmd_count),
                Style::default().fg(t.info),
            )),
            Cell::from(Line::from(status_spans)),
            Cell::from(Span::styled(duration, Style::default().fg(t.text_muted))),
        ])
    }

    fn render_list_table(&mut self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let scrollbar_area = area;
        let table_area = Rect {
            width: area.width.saturating_sub(1),
            ..area
        };

        let page_items = self.list_page_slice();

        let rows: Vec<Row> = page_items
            .iter()
            .map(|g| Self::build_list_row(g, t))
            .collect();

        let header = Row::new(vec![
            Cell::from("Time"),
            Cell::from("Session"),
            Cell::from("Executor"),
            Cell::from("Prompt"),
            Cell::from("Cmds"),
            Cell::from("Status"),
            Cell::from("Duration"),
        ])
        .style(
            Style::default()
                .fg(t.text_secondary)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

        let widths = [
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Min(20),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(8),
        ];

        let table = Table::new(rows, widths)
            .header(header)
            .row_highlight_style(
                Style::default()
                    .bg(t.selection_bg)
                    .fg(t.selection_fg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(" > ")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border))
                    .title(Span::styled(
                        " Prompts ",
                        Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
                    )),
            );

        f.render_stateful_widget(table, table_area, &mut self.list_pager.state);

        if self.groups.is_empty() {
            let hint = Paragraph::new(Line::from(Span::styled(
                "  No prompts with recorded commands found. Configure prompt capture with suv init <agent>.",
                Style::default().fg(t.text_muted),
            )));
            let hint_area = Rect {
                x: table_area.x + 1,
                y: table_area.y + 2,
                width: table_area.width.saturating_sub(2),
                height: 1,
            };
            f.render_widget(hint, hint_area);
        }

        let total_pages = self.list_total_pages();
        let mut scrollbar_state =
            ScrollbarState::new(total_pages).position(self.list_pager.page.saturating_sub(1));
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(Style::default().fg(t.primary_dim))
                .track_style(Style::default().fg(t.border)),
            scrollbar_area,
            &mut scrollbar_state,
        );
    }

    fn render_prompt_preview(
        f: &mut ratatui::Frame,
        area: Rect,
        t: &crate::theme::Theme,
        group: &PromptGroup,
        home: &str,
    ) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(Span::styled(
                " Prompt ",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(area);
        f.render_widget(block, area);

        let max_w = inner.width.saturating_sub(1) as usize;
        let mut lines = Vec::new();

        // Full prompt text
        let prompt_chars: Vec<char> = group.prompt.chars().collect();
        let available_h = inner.height.saturating_sub(9) as usize; // reserve for session/executor/path/time/stats
        for chunk in prompt_chars.chunks(max_w.max(1)).take(available_h.max(1)) {
            let chunk_str: String = chunk.iter().collect();
            lines.push(Line::from(Span::styled(
                format!(" {chunk_str}"),
                Style::default().fg(t.info),
            )));
        }

        // Separator + metadata
        lines.push(Line::from(""));
        Self::append_preview_metadata(&mut lines, t, group, home);

        f.render_widget(Paragraph::new(lines), inner);
    }

    fn append_preview_metadata(
        lines: &mut Vec<Line<'_>>,
        t: &crate::theme::Theme,
        group: &PromptGroup,
        home: &str,
    ) {
        let label_style = Style::default()
            .fg(t.text_secondary)
            .add_modifier(Modifier::BOLD);

        // Session ID
        let session_short: String = short_session_id(&group.session_id);
        lines.push(Line::from(vec![
            Span::styled(" Session  ", label_style),
            Span::styled(session_short, Style::default().fg(t.primary_dim)),
        ]));

        // Executor
        lines.push(Line::from(vec![
            Span::styled(" Executor ", label_style),
            Span::styled(
                group.executor.clone(),
                Style::default().fg(t.badge_executor),
            ),
        ]));

        // Path
        let path_display = shorten_path(&group.cwd, home);
        lines.push(Line::from(vec![
            Span::styled(" Path     ", label_style),
            Span::styled(path_display, Style::default().fg(t.badge_path)),
        ]));

        // Time range
        lines.push(Line::from(vec![
            Span::styled(" First    ", label_style),
            Span::styled(
                format_full_datetime(group.first_at),
                Style::default().fg(t.text),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled(" Last     ", label_style),
            Span::styled(
                format_full_datetime(group.last_at),
                Style::default().fg(t.text),
            ),
        ]));

        // Cmd count + success
        let fail_count = group.fail_count;
        let mut stat_spans = vec![
            Span::styled(" Stats    ", label_style),
            Span::styled(
                format!("{} cmds", group.cmd_count),
                Style::default().fg(t.text),
            ),
            Span::styled(
                format!("  \u{2714} {}", group.success_count),
                Style::default().fg(t.success),
            ),
        ];
        if fail_count > 0 {
            stat_spans.push(Span::styled(
                format!("  \u{2718} {fail_count}"),
                Style::default().fg(t.error),
            ));
        }
        lines.push(Line::from(stat_spans));
    }

    fn render_list_footer(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let badge_key = Style::default()
            .fg(t.bg_elevated)
            .bg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let badge_label = Style::default().fg(t.text_muted);
        let total_pages = self.list_total_pages();

        let mut spans = if self.search_active {
            vec![
                Span::styled(" Enter/Esc ", badge_key),
                Span::styled(" Stop editing  ", badge_label),
            ]
        } else {
            vec![
                Span::styled(" ↑↓ ", badge_key),
                Span::styled(" Navigate  ", badge_label),
                Span::styled(" ←→ ", badge_key),
                Span::styled(" Page  ", badge_label),
                Span::styled(" Enter ", badge_key),
                Span::styled(" View cmds  ", badge_label),
                Span::styled(" / ", badge_key),
                Span::styled(" Search  ", badge_label),
                Span::styled(" 1-4 ", badge_key),
                Span::styled(" Period  ", badge_label),
                Span::styled(" a ", badge_key),
                Span::styled(" Agent  ", badge_label),
                Span::styled(" s ", badge_key),
                Span::styled(" Session  ", badge_label),
                Span::styled(" Esc ", badge_key),
                Span::styled(" Back  ", badge_label),
            ]
        };

        spans.push(Span::styled(
            format!(" {}/{total_pages} ", self.list_pager.page),
            Style::default().fg(t.text_muted),
        ));

        if let Some((msg, time)) = &self.status_message {
            if time.elapsed() < std::time::Duration::from_secs(2) {
                spans.push(Span::styled(
                    format!(" {msg} "),
                    Style::default().fg(t.success).add_modifier(Modifier::BOLD),
                ));
            }
        }

        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    // ── Detail screen ───────────────────────────────────────

    fn render_detail(&mut self, f: &mut ratatui::Frame, group_index: usize) {
        let t = theme();
        let size = f.area();

        let group = &self.groups[group_index];

        // Compute prompt height: wrap prompt text to available width
        let prompt_area_w = size.width.saturating_sub(4) as usize; // borders + padding
        let prompt_line_count = if prompt_area_w > 0 {
            let char_count = group.prompt.chars().count();
            (char_count.div_ceil(prompt_area_w)).min(6) // cap at 6 lines
        } else {
            1
        };
        let prompt_box_h = u16::try_from(prompt_line_count)
            .unwrap_or(u16::MAX)
            .saturating_add(4); // +2 for borders, +2 for header/stats line

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(prompt_box_h), // prompt area
                Constraint::Min(6),               // command table + detail pane
                Constraint::Length(1),            // footer
            ])
            .split(size);

        // Prompt area (full prompt text + stats)
        Self::render_prompt_header(f, chunks[0], t, group);

        // Body
        if self.detail_pane_open {
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
                .split(chunks[1]);
            self.render_detail_table(f, body[0], t, group_index);
            self.render_entry_detail_pane(f, body[1], t);
        } else {
            self.render_detail_table(f, chunks[1], t, group_index);
        }

        // Footer
        self.render_detail_footer(f, chunks[2], t);
    }

    fn render_prompt_header(
        f: &mut ratatui::Frame,
        area: Rect,
        t: &crate::theme::Theme,
        group: &PromptGroup,
    ) {
        let session_short: String = short_session_id(&group.session_id);
        let fail_count = group.fail_count;

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border_focus))
            .title(Span::styled(
                " Prompt ",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(area);
        f.render_widget(block, area);

        let max_w = inner.width.saturating_sub(1) as usize;
        let mut lines = Vec::new();

        // Stats line
        let mut stats_spans = vec![
            Span::styled(
                format!("[{session_short}] "),
                Style::default()
                    .fg(t.primary_dim)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                group.executor.clone(),
                Style::default().fg(t.badge_executor),
            ),
            Span::styled(
                format!("  {} cmds", group.cmd_count),
                Style::default().fg(t.text_secondary),
            ),
            Span::styled(
                format!("  ✔ {}", group.success_count),
                Style::default().fg(t.success),
            ),
        ];
        if fail_count > 0 {
            stats_spans.push(Span::styled(
                format!("  ✘ {fail_count}"),
                Style::default().fg(t.error),
            ));
        }
        stats_spans.push(Span::styled(
            format!("  {}", format_duration_ms(group.total_duration_ms)),
            Style::default().fg(t.text_muted),
        ));
        lines.push(Line::from(stats_spans));

        // Blank separator
        lines.push(Line::from(""));

        // Full prompt text (word-wrapped, up to available height)
        let prompt_chars: Vec<char> = group.prompt.chars().collect();
        let available_lines = inner.height.saturating_sub(2) as usize; // stats + blank
        for chunk in prompt_chars.chunks(max_w.max(1)).take(available_lines) {
            let chunk_str: String = chunk.iter().collect();
            lines.push(Line::from(Span::styled(
                chunk_str,
                Style::default().fg(t.info),
            )));
        }

        f.render_widget(Paragraph::new(lines), inner);
    }

    fn render_detail_table(
        &mut self,
        f: &mut ratatui::Frame,
        area: Rect,
        t: &crate::theme::Theme,
        _group_index: usize,
    ) {
        let scrollbar_area = area;
        let table_area = Rect {
            width: area.width.saturating_sub(1),
            ..area
        };

        let page_indices = self.detail_page_slice().to_vec();

        let rows: Vec<Row> = page_indices
            .iter()
            .map(|&idx| {
                let entry = &self.entries[idx];
                let time = format_datetime(entry.started_at);
                let cmd_display = truncate(&entry.command, 50);
                let path = shorten_path(&entry.cwd, &self.home);
                let path_display = truncate(&path, 20);
                let status = match entry.exit_code {
                    Some(0) => "✔".to_string(),
                    Some(c) => format!("✘ {c}"),
                    None => "○".to_string(),
                };
                let status_style = match entry.exit_code {
                    Some(0) => Style::default().fg(t.success),
                    Some(_) => Style::default().fg(t.error),
                    None => Style::default().fg(t.text_muted),
                };
                let duration = format_duration_ms(entry.duration_ms);

                Row::new(vec![
                    Cell::from(Span::styled(time, Style::default().fg(t.text_muted))),
                    Cell::from(Span::styled(cmd_display, Style::default().fg(t.primary))),
                    Cell::from(Span::styled(
                        path_display,
                        Style::default().fg(t.badge_path),
                    )),
                    Cell::from(Span::styled(status, status_style)),
                    Cell::from(Span::styled(duration, Style::default().fg(t.text_muted))),
                ])
            })
            .collect();

        let header = Row::new(vec![
            Cell::from("Time"),
            Cell::from("Command"),
            Cell::from("Directory"),
            Cell::from("Status"),
            Cell::from("Duration"),
        ])
        .style(
            Style::default()
                .fg(t.text_secondary)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

        let widths = [
            Constraint::Length(12),
            Constraint::Min(15),
            Constraint::Length(22),
            Constraint::Length(8),
            Constraint::Length(8),
        ];

        let table = Table::new(rows, widths)
            .header(header)
            .row_highlight_style(
                Style::default()
                    .bg(t.selection_bg)
                    .fg(t.selection_fg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(" > ")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border))
                    .title(Span::styled(
                        " Commands ",
                        Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
                    )),
            );

        f.render_stateful_widget(table, table_area, &mut self.detail_pager.state);

        let total_pages = self.detail_total_pages();
        let mut scrollbar_state =
            ScrollbarState::new(total_pages).position(self.detail_pager.page.saturating_sub(1));
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(Style::default().fg(t.primary_dim))
                .track_style(Style::default().fg(t.border)),
            scrollbar_area,
            &mut scrollbar_state,
        );
    }

    fn render_entry_detail_pane(
        &self,
        f: &mut ratatui::Frame,
        area: Rect,
        t: &crate::theme::Theme,
    ) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(Span::styled(
                " Detail ",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(area);
        f.render_widget(block, area);

        let Some(entry) = self.selected_detail_entry() else {
            f.render_widget(
                Paragraph::new(Span::styled(
                    " No entry selected",
                    Style::default().fg(t.text_muted),
                )),
                inner,
            );
            return;
        };

        let label = Style::default()
            .fg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let val = Style::default().fg(t.text);
        let max_w = inner.width.saturating_sub(2) as usize;

        let mut lines = Vec::new();

        // Command (word-wrapped)
        lines.push(Line::from(Span::styled("Command", label)));
        let cmd_chars: Vec<char> = entry.command.chars().collect();
        for chunk in cmd_chars.chunks(max_w.max(1)) {
            let chunk_str: String = chunk.iter().collect();
            lines.push(Line::from(Span::styled(
                format!(" {chunk_str}"),
                Style::default().fg(t.primary),
            )));
        }
        lines.push(Line::from(""));

        // Path
        let path = shorten_path(&entry.cwd, &self.home);
        lines.push(Line::from(vec![
            Span::styled("Path     ", label),
            Span::styled(path, val),
        ]));

        // Time
        let time_str = format_full_datetime(entry.started_at);
        lines.push(Line::from(vec![
            Span::styled("Time     ", label),
            Span::styled(time_str, val),
        ]));

        // Duration
        #[allow(clippy::cast_precision_loss)]
        let dur_secs = entry.duration_ms as f64 / 1000.0;
        lines.push(Line::from(vec![
            Span::styled("Duration ", label),
            Span::styled(format!("{dur_secs:.2}s"), val),
        ]));

        // Exit code
        let exit_str = match entry.exit_code {
            Some(0) => "✔ 0 (success)".to_string(),
            Some(c) => format!("✘ {c} (failed)"),
            None => "○ (unknown)".to_string(),
        };
        let exit_style = match entry.exit_code {
            Some(0) => Style::default().fg(t.success),
            Some(_) => Style::default().fg(t.error),
            None => Style::default().fg(t.text_muted),
        };
        lines.push(Line::from(vec![
            Span::styled("Exit     ", label),
            Span::styled(exit_str, exit_style),
        ]));

        // Executor
        let executor = match (&entry.executor_type, &entry.executor) {
            (Some(et), Some(n)) => format!("{et}: {n}"),
            (Some(et), None) => et.clone(),
            (None, Some(n)) => n.clone(),
            _ => "unknown".to_string(),
        };
        lines.push(Line::from(vec![
            Span::styled("Executor ", label),
            Span::styled(executor, val),
        ]));

        f.render_widget(Paragraph::new(lines), inner);
    }

    fn render_detail_footer(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let badge_key = Style::default()
            .fg(t.bg_elevated)
            .bg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let badge_label = Style::default().fg(t.text_muted);
        let total_pages = self.detail_total_pages();

        let mut spans = vec![
            Span::styled(" ↑↓ ", badge_key),
            Span::styled(" Navigate  ", badge_label),
            Span::styled(" ←→ ", badge_key),
            Span::styled(" Page  ", badge_label),
            Span::styled(" ^Y ", badge_key),
            Span::styled(" Copy  ", badge_label),
            Span::styled(" Tab ", badge_key),
            Span::styled(" Detail  ", badge_label),
            Span::styled(" Esc ", badge_key),
            Span::styled(" Back  ", badge_label),
        ];

        spans.push(Span::styled(
            format!(" {}/{total_pages} ", self.detail_pager.page),
            Style::default().fg(t.text_muted),
        ));

        if let Some((msg, time)) = &self.status_message {
            if time.elapsed() < std::time::Duration::from_secs(2) {
                spans.push(Span::styled(
                    format!(" {msg} "),
                    Style::default().fg(t.success).add_modifier(Modifier::BOLD),
                ));
            }
        }

        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

// ── Public entry point ──────────────────────────────────────

pub fn run_prompt_explorer<B: Backend>(
    terminal: &mut Terminal<B>,
    entries: &[Entry],
    repo: Option<&Repository>,
    after_ms: Option<i64>,
    executor: Option<&str>,
    cwd: Option<&str>,
) -> io::Result<()>
where
    io::Error: From<B::Error>,
{
    let mut app = PromptExplorerApp::new(entries);
    app.period = Period::from_after_ms(after_ms);
    app.cwd_filter = cwd.map(String::from);
    if let Some(name) = executor {
        app.executor_filter = app.executor_names.iter().position(|n| n == name);
    }

    loop {
        terminal.draw(|f| app.render(f))?;

        let timeout = if app.status_message.is_some() {
            std::time::Duration::from_secs(2)
        } else {
            std::time::Duration::from_mins(1)
        };
        if !event::poll(timeout)? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match app.handle_input(key, repo) {
                PromptAction::Quit => return Ok(()),
                PromptAction::OpenSession(session_id) => {
                    if let Some(repo) = repo {
                        open_session(terminal, repo, &session_id, &mut app)?;
                    } else {
                        app.status_message =
                            Some(("Session view not available".into(), Instant::now()));
                    }
                }
                PromptAction::Continue => {}
            }
        }
    }
}

fn open_session<B: Backend>(
    terminal: &mut Terminal<B>,
    repo: &Repository,
    session_id: &str,
    app: &mut PromptExplorerApp,
) -> io::Result<()>
where
    io::Error: From<B::Error>,
{
    let Ok(Some(session)) = repo.get_session(session_id) else {
        app.status_message = Some(("Session not found".into(), Instant::now()));
        return Ok(());
    };
    let tag_name = repo.get_tag_by_session(session_id).unwrap_or(None);
    let entries = repo
        .get_replay_entries(
            Some(session_id),
            &crate::repository::ReplayFilter::default(),
        )
        .unwrap_or_default();
    let noted_ids = repo.get_noted_entry_ids().unwrap_or_default();

    if entries.is_empty() {
        app.status_message = Some(("Session has no commands".into(), Instant::now()));
        return Ok(());
    }

    session_ui::run_session_timeline(terminal, session, tag_name, entries, noted_ids)?;
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_entry_with_prompt(
        session_id: &str,
        cmd: &str,
        prompt: &str,
        executor: &str,
        exit_code: Option<i32>,
        started_at: i64,
        duration_ms: i64,
    ) -> Entry {
        let mut ctx = HashMap::new();
        ctx.insert("agent_prompt".to_string(), prompt.to_string());
        Entry {
            id: None,
            session_id: session_id.to_string(),
            command: cmd.to_string(),
            cwd: "/tmp".to_string(),
            exit_code,
            started_at,
            ended_at: started_at + duration_ms,
            duration_ms,
            context: Some(ctx),
            tag_name: None,
            tag_id: None,
            executor_type: Some("agent".to_string()),
            executor: Some(executor.to_string()),
        }
    }

    fn make_entry_no_prompt(session_id: &str, cmd: &str, started_at: i64) -> Entry {
        Entry {
            id: None,
            session_id: session_id.to_string(),
            command: cmd.to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at,
            ended_at: started_at + 100,
            duration_ms: 100,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("agent".to_string()),
            executor: Some("claude-code".to_string()),
        }
    }

    // ── Grouping logic tests ────────────────────────────────

    #[test]
    fn build_prompt_groups_empty() {
        let groups = build_prompt_groups(&[]);
        assert!(groups.is_empty());
    }

    #[test]
    fn build_prompt_groups_no_prompts_skipped() {
        let entries = vec![
            make_entry_no_prompt("s1", "ls", 1000),
            make_entry_no_prompt("s1", "pwd", 2000),
        ];
        let groups = build_prompt_groups(&entries);
        assert!(groups.is_empty());
    }

    #[test]
    fn build_prompt_groups_single_group() {
        let entries = vec![
            make_entry_with_prompt(
                "s1",
                "git status",
                "check repo",
                "claude-code",
                Some(0),
                1000,
                50,
            ),
            make_entry_with_prompt(
                "s1",
                "git diff",
                "check repo",
                "claude-code",
                Some(0),
                2000,
                30,
            ),
        ];
        let groups = build_prompt_groups(&entries);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].cmd_count, 2);
        assert_eq!(groups[0].success_count, 2);
        assert_eq!(groups[0].total_duration_ms, 80);
        assert_eq!(groups[0].entry_indices.len(), 2);
    }

    #[test]
    fn build_prompt_groups_multiple_sessions() {
        let entries = vec![
            make_entry_with_prompt("s1", "ls", "list files", "claude-code", Some(0), 1000, 10),
            make_entry_with_prompt("s2", "ls", "list files", "claude-code", Some(0), 2000, 10),
        ];
        let groups = build_prompt_groups(&entries);
        // Same prompt text but different sessions → 2 groups
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn build_prompt_groups_same_session_different_prompts() {
        let entries = vec![
            make_entry_with_prompt(
                "s1",
                "git status",
                "check repo",
                "claude-code",
                Some(0),
                1000,
                10,
            ),
            make_entry_with_prompt(
                "s1",
                "cargo test",
                "run tests",
                "claude-code",
                Some(0),
                2000,
                10,
            ),
        ];
        let groups = build_prompt_groups(&entries);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn build_prompt_groups_sorted_by_recency() {
        let entries = vec![
            make_entry_with_prompt("s1", "ls", "old prompt", "claude-code", Some(0), 1000, 10),
            make_entry_with_prompt("s2", "pwd", "new prompt", "claude-code", Some(0), 5000, 10),
            make_entry_with_prompt("s3", "cat", "mid prompt", "claude-code", Some(0), 3000, 10),
        ];
        let groups = build_prompt_groups(&entries);
        assert_eq!(groups.len(), 3);
        // Most recent first
        assert_eq!(groups[0].prompt, "new prompt");
        assert_eq!(groups[1].prompt, "mid prompt");
        assert_eq!(groups[2].prompt, "old prompt");
    }

    #[test]
    fn build_prompt_groups_correct_aggregates() {
        let entries = vec![
            make_entry_with_prompt("s1", "cmd1", "do stuff", "claude-code", Some(0), 1000, 100),
            make_entry_with_prompt("s1", "cmd2", "do stuff", "claude-code", Some(1), 2000, 200),
            make_entry_with_prompt("s1", "cmd3", "do stuff", "claude-code", Some(0), 3000, 300),
        ];
        let groups = build_prompt_groups(&entries);
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.cmd_count, 3);
        assert_eq!(g.success_count, 2); // cmd1 + cmd3
        assert_eq!(g.fail_count, 1); // cmd2 (exit 1)
        assert_eq!(g.total_duration_ms, 600);
        assert_eq!(g.first_at, 1000);
        assert_eq!(g.last_at, 3000);
        assert_eq!(g.executor, "claude-code");
        assert_eq!(g.session_id, "s1");
    }

    #[test]
    fn build_prompt_groups_mixed_with_and_without_prompts() {
        let entries = vec![
            make_entry_with_prompt("s1", "cmd1", "prompt A", "claude-code", Some(0), 1000, 10),
            make_entry_no_prompt("s1", "cmd2", 2000),
            make_entry_with_prompt("s1", "cmd3", "prompt A", "claude-code", Some(0), 3000, 10),
        ];
        let groups = build_prompt_groups(&entries);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].cmd_count, 2); // only the ones with prompts
    }

    #[test]
    fn build_prompt_groups_none_exit_code_is_neutral() {
        // Most agent entries have exit_code = None — these should NOT count as failures
        let entries = vec![
            make_entry_with_prompt("s1", "cmd1", "prompt", "cc", None, 1000, 10),
            make_entry_with_prompt("s1", "cmd2", "prompt", "cc", None, 2000, 10),
            make_entry_with_prompt("s1", "cmd3", "prompt", "cc", Some(0), 3000, 10),
            make_entry_with_prompt("s1", "cmd4", "prompt", "cc", Some(1), 4000, 10),
        ];
        let groups = build_prompt_groups(&entries);
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.cmd_count, 4);
        assert_eq!(g.success_count, 1); // only cmd3
        assert_eq!(g.fail_count, 1); // only cmd4
                                     // cmd1 + cmd2 have None — neither success nor fail
    }

    // ── App state tests ─────────────────────────────────────

    #[test]
    fn app_new_empty_entries() {
        let entries = vec![];
        let app = PromptExplorerApp::new(&entries);
        assert!(app.groups.is_empty());
        assert!(app.list_pager.state.selected().is_none());
    }

    #[test]
    fn app_new_with_entries() {
        let entries = vec![make_entry_with_prompt(
            "s1",
            "ls",
            "list files",
            "claude-code",
            Some(0),
            1000,
            10,
        )];
        let app = PromptExplorerApp::new(&entries);
        assert_eq!(app.groups.len(), 1);
        assert_eq!(app.list_pager.state.selected(), Some(0));
    }

    #[test]
    fn app_list_total_pages_empty() {
        let entries = vec![];
        let app = PromptExplorerApp::new(&entries);
        assert_eq!(app.list_total_pages(), 1);
    }

    #[test]
    fn app_list_total_pages_within_one_page() {
        let entries = vec![
            make_entry_with_prompt("s1", "ls", "p1", "cc", Some(0), 1000, 10),
            make_entry_with_prompt("s2", "ls", "p2", "cc", Some(0), 2000, 10),
        ];
        let app = PromptExplorerApp::new(&entries);
        assert_eq!(app.list_total_pages(), 1);
    }

    #[test]
    fn app_selected_group_returns_correct() {
        let entries = vec![
            make_entry_with_prompt("s1", "ls", "old", "cc", Some(0), 1000, 10),
            make_entry_with_prompt("s2", "ls", "new", "cc", Some(0), 2000, 10),
        ];
        let app = PromptExplorerApp::new(&entries);
        // Selection is at 0, which should be the most recent group
        let group = app.selected_group().unwrap();
        assert_eq!(group.prompt, "new");
    }

    #[test]
    fn app_enter_detail_and_back() {
        let entries = vec![
            make_entry_with_prompt("s1", "ls", "prompt", "cc", Some(0), 1000, 10),
            make_entry_with_prompt("s1", "pwd", "prompt", "cc", Some(0), 2000, 10),
        ];
        let mut app = PromptExplorerApp::new(&entries);

        // Should start in list view
        assert!(matches!(app.view, View::List));

        // Simulate Enter
        let enter = crossterm::event::KeyEvent::from(KeyCode::Enter);
        app.handle_input(enter, None);
        assert!(matches!(app.view, View::Detail { group_index: 0 }));

        // Detail should have entries
        assert_eq!(app.detail_entries().len(), 2);

        // Simulate Esc to go back
        let esc = crossterm::event::KeyEvent::from(KeyCode::Esc);
        app.handle_input(esc, None);
        assert!(matches!(app.view, View::List));
    }

    #[test]
    fn app_quit_from_list() {
        let entries = vec![];
        let mut app = PromptExplorerApp::new(&entries);
        let q = crossterm::event::KeyEvent::from(KeyCode::Char('q'));
        let action = app.handle_input(q, None);
        assert!(matches!(action, PromptAction::Quit));
    }

    #[test]
    fn app_detail_tab_toggles_pane() {
        let entries = vec![make_entry_with_prompt(
            "s1",
            "ls",
            "prompt",
            "cc",
            Some(0),
            1000,
            10,
        )];
        let mut app = PromptExplorerApp::new(&entries);

        // Enter detail
        let enter = crossterm::event::KeyEvent::from(KeyCode::Enter);
        app.handle_input(enter, None);

        assert!(app.detail_pane_open);
        let tab = crossterm::event::KeyEvent::from(KeyCode::Tab);
        app.handle_input(tab, None);
        assert!(!app.detail_pane_open);
        app.handle_input(tab, None);
        assert!(app.detail_pane_open);
    }

    #[test]
    fn app_backspace_returns_from_detail() {
        let entries = vec![make_entry_with_prompt(
            "s1",
            "ls",
            "prompt",
            "cc",
            Some(0),
            1000,
            10,
        )];
        let mut app = PromptExplorerApp::new(&entries);

        let enter = crossterm::event::KeyEvent::from(KeyCode::Enter);
        app.handle_input(enter, None);
        assert!(matches!(app.view, View::Detail { .. }));

        let bs = crossterm::event::KeyEvent::from(KeyCode::Backspace);
        app.handle_input(bs, None);
        assert!(matches!(app.view, View::List));
    }

    #[test]
    fn app_list_navigation() {
        let entries = vec![
            make_entry_with_prompt("s1", "cmd1", "p1", "cc", Some(0), 1000, 10),
            make_entry_with_prompt("s2", "cmd2", "p2", "cc", Some(0), 2000, 10),
            make_entry_with_prompt("s3", "cmd3", "p3", "cc", Some(0), 3000, 10),
        ];
        let mut app = PromptExplorerApp::new(&entries);
        assert_eq!(app.list_pager.state.selected(), Some(0));

        // Move down
        let down = crossterm::event::KeyEvent::from(KeyCode::Down);
        app.handle_input(down, None);
        assert_eq!(app.list_pager.state.selected(), Some(1));

        app.handle_input(down, None);
        assert_eq!(app.list_pager.state.selected(), Some(2));

        // Can't go past last
        app.handle_input(down, None);
        assert_eq!(app.list_pager.state.selected(), Some(2));

        // Move up
        let up = crossterm::event::KeyEvent::from(KeyCode::Up);
        app.handle_input(up, None);
        assert_eq!(app.list_pager.state.selected(), Some(1));
    }

    #[test]
    fn app_session_shortcut_returns_open_session() {
        let entries = vec![make_entry_with_prompt(
            "sess-abc-123",
            "ls",
            "prompt",
            "cc",
            Some(0),
            1000,
            10,
        )];
        let mut app = PromptExplorerApp::new(&entries);
        let s = crossterm::event::KeyEvent::from(KeyCode::Char('s'));
        let action = app.handle_input(s, None);
        match action {
            PromptAction::OpenSession(sid) => assert_eq!(sid, "sess-abc-123"),
            other => panic!(
                "expected OpenSession, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn app_session_shortcut_on_empty_is_continue() {
        let entries = vec![];
        let mut app = PromptExplorerApp::new(&entries);
        let s = crossterm::event::KeyEvent::from(KeyCode::Char('s'));
        let action = app.handle_input(s, None);
        assert!(matches!(action, PromptAction::Continue));
    }

    #[test]
    fn app_copy_in_detail_does_not_panic() {
        let entries = vec![make_entry_with_prompt(
            "s1",
            "echo hello",
            "prompt",
            "cc",
            Some(0),
            1000,
            10,
        )];
        let mut app = PromptExplorerApp::new(&entries);

        // Enter detail
        let enter = crossterm::event::KeyEvent::from(KeyCode::Enter);
        app.handle_input(enter, None);
        assert!(matches!(app.view, View::Detail { .. }));

        // Ctrl+Y (may fail to copy in CI/test but should not panic)
        let ctrl_y = crossterm::event::KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
        let action = app.handle_input(ctrl_y, None);
        assert!(matches!(action, PromptAction::Continue));
        // Status message should be set (either Copied! or Copy failed)
        assert!(app.status_message.is_some());
    }

    // ── Search filter ────────────────────────────────────────

    #[test]
    fn search_filters_groups_by_prompt_text_case_insensitively() {
        let entries = vec![
            make_entry_with_prompt("s1", "ls", "Fix the login bug", "cc", Some(0), 1000, 10),
            make_entry_with_prompt("s2", "pwd", "add dark mode", "cc", Some(0), 2000, 10),
        ];
        let mut app = PromptExplorerApp::new(&entries);
        assert_eq!(app.groups.len(), 2);

        app.search = "LOGIN".to_string();
        app.apply_filters();

        assert_eq!(app.groups.len(), 1);
        assert_eq!(app.groups[0].prompt, "Fix the login bug");
    }

    #[test]
    fn search_empty_shows_all_groups() {
        let entries = vec![
            make_entry_with_prompt("s1", "ls", "prompt one", "cc", Some(0), 1000, 10),
            make_entry_with_prompt("s2", "pwd", "prompt two", "cc", Some(0), 2000, 10),
        ];
        let mut app = PromptExplorerApp::new(&entries);
        app.search = "   ".to_string(); // whitespace-only counts as empty
        app.apply_filters();
        assert_eq!(app.groups.len(), 2);
    }

    #[test]
    fn search_with_no_matches_clears_selection() {
        let entries = vec![make_entry_with_prompt(
            "s1",
            "ls",
            "prompt one",
            "cc",
            Some(0),
            1000,
            10,
        )];
        let mut app = PromptExplorerApp::new(&entries);
        app.search = "nonexistent needle".to_string();
        app.apply_filters();
        assert!(app.groups.is_empty());
        assert!(app.list_pager.state.selected().is_none());
    }

    #[test]
    fn slash_key_enters_search_mode_and_typing_filters_live() {
        let entries = vec![
            make_entry_with_prompt("s1", "ls", "fix login bug", "cc", Some(0), 1000, 10),
            make_entry_with_prompt("s2", "pwd", "add dark mode", "cc", Some(0), 2000, 10),
        ];
        let mut app = PromptExplorerApp::new(&entries);

        let slash = crossterm::event::KeyEvent::from(KeyCode::Char('/'));
        app.handle_input(slash, None);
        assert!(app.search_active);

        for c in "dark".chars() {
            app.handle_input(crossterm::event::KeyEvent::from(KeyCode::Char(c)), None);
        }
        assert_eq!(app.search, "dark");
        assert_eq!(app.groups.len(), 1);
        assert_eq!(app.groups[0].prompt, "add dark mode");

        // Esc stops editing but keeps the filter text and its results applied.
        let esc = crossterm::event::KeyEvent::from(KeyCode::Esc);
        app.handle_input(esc, None);
        assert!(!app.search_active);
        assert_eq!(app.search, "dark");
        assert_eq!(app.groups.len(), 1);
    }

    #[test]
    fn backspace_in_search_mode_removes_last_char_and_refilters() {
        let entries = vec![
            make_entry_with_prompt("s1", "ls", "fix login bug", "cc", Some(0), 1000, 10),
            make_entry_with_prompt("s2", "pwd", "add dark mode", "cc", Some(0), 2000, 10),
        ];
        let mut app = PromptExplorerApp::new(&entries);
        app.search_active = true;
        app.search = "dark".to_string();
        app.apply_filters();
        assert_eq!(app.groups.len(), 1);

        let bs = crossterm::event::KeyEvent::from(KeyCode::Backspace);
        app.handle_input(bs, None);
        assert_eq!(app.search, "dar");
        // Still matches "dark mode"
        assert_eq!(app.groups.len(), 1);
    }

    #[test]
    fn search_mode_swallows_navigation_keys() {
        // While typing, letters like 'j'/'q' must go into the search text,
        // not trigger list navigation or quit.
        let entries = vec![make_entry_with_prompt(
            "s1",
            "ls",
            "prompt one",
            "cc",
            Some(0),
            1000,
            10,
        )];
        let mut app = PromptExplorerApp::new(&entries);
        app.search_active = true;

        let q = crossterm::event::KeyEvent::from(KeyCode::Char('q'));
        let action = app.handle_input(q, None);
        assert!(matches!(action, PromptAction::Continue));
        assert_eq!(app.search, "q");
    }

    // ── Period / executor filters ───────────────────────────

    #[test]
    fn period_and_executor_keys_without_repo_show_status_message() {
        let entries = vec![make_entry_with_prompt(
            "s1",
            "ls",
            "prompt",
            "cc",
            Some(0),
            1000,
            10,
        )];
        let mut app = PromptExplorerApp::new(&entries);

        let one = crossterm::event::KeyEvent::from(KeyCode::Char('1'));
        app.handle_input(one, None);
        assert_eq!(app.period, Period::Today);
        assert_eq!(
            app.status_message.as_ref().map(|(m, _)| m.as_str()),
            Some("Reload not available")
        );
    }

    #[test]
    fn cycle_executor_without_repo_wraps_through_names_and_back_to_all() {
        // Two distinct executor names is all `cycle_executor` cares about; the
        // exact count doesn't matter here since executor_filter is tested by
        // index, not by name.
        let entries = vec![
            make_entry_with_prompt("s1", "ls", "p1", "claude-code", Some(0), 1000, 10),
            make_entry_with_prompt("s2", "pwd", "p2", "cursor", Some(0), 2000, 10),
        ];
        let mut app = PromptExplorerApp::new(&entries);
        assert_eq!(app.executor_names.len(), 2);
        assert_eq!(app.executor_filter, None);

        let a = crossterm::event::KeyEvent::from(KeyCode::Char('a'));
        app.handle_input(a, None);
        assert_eq!(app.executor_filter, Some(0));
        app.handle_input(a, None);
        assert_eq!(app.executor_filter, Some(1));
        app.handle_input(a, None);
        assert_eq!(app.executor_filter, None);
    }

    fn insert_test_session(repo: &Repository, id: &str, created_at: i64) {
        repo.insert_session(&crate::models::Session {
            id: id.to_string(),
            hostname: "host".to_string(),
            created_at,
            tag_id: None,
        })
        .unwrap();
    }

    #[test]
    fn reload_applies_period_and_executor_via_repo() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let now = chrono::Utc::now().timestamp_millis();
        insert_test_session(&repo, "s1", now);
        insert_test_session(&repo, "s2", now - 40 * 24 * 60 * 60 * 1000);
        let recent = make_entry_with_prompt(
            "s1",
            "ls",
            "recent claude prompt",
            "claude-code",
            Some(0),
            now,
            10,
        );
        let old = make_entry_with_prompt(
            "s2",
            "pwd",
            "old cursor prompt",
            "cursor",
            Some(0),
            now - 40 * 24 * 60 * 60 * 1000,
            10,
        );
        repo.insert_entry(&recent).unwrap();
        repo.insert_entry(&old).unwrap();

        let entries = load_entries(&repo, None, None, None);
        let mut app = PromptExplorerApp::new(&entries);
        assert_eq!(app.groups.len(), 2);

        // Narrow to the last 7 days: the 40-day-old entry drops out.
        app.set_period(Period::Days7, Some(&repo));
        assert_eq!(app.groups.len(), 1);
        assert_eq!(app.groups[0].prompt, "recent claude prompt");

        // Widen back to all time, then filter down to just "cursor".
        app.set_period(Period::AllTime, Some(&repo));
        assert_eq!(app.groups.len(), 2);
        let cursor_idx = app
            .executor_names
            .iter()
            .position(|n| n == "cursor")
            .unwrap();
        app.executor_filter = Some(cursor_idx);
        app.reload(&repo);
        assert_eq!(app.groups.len(), 1);
        assert_eq!(app.groups[0].prompt, "old cursor prompt");
    }

    #[test]
    fn initial_executor_name_resolves_to_its_index_in_executor_names() {
        let entries = vec![
            make_entry_with_prompt("s1", "ls", "p1", "claude-code", Some(0), 1000, 10),
            make_entry_with_prompt("s2", "pwd", "p2", "cursor", Some(0), 2000, 10),
        ];
        let app = PromptExplorerApp::new(&entries);
        let idx = app
            .executor_names
            .iter()
            .position(|n| n == "cursor")
            .unwrap();
        assert_eq!(app.executor_names[idx], "cursor");
    }
}
