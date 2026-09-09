use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table,
};
use ratatui::Terminal;

use crate::models::Entry;
use crate::repository::Repository;
use crate::risk::{self, RiskLevel, SessionRisk};
use crate::theme::theme;
use crate::util::{dirs_home, shorten_path};

use super::{
    compute_agent_counts, format_datetime, format_full_datetime, load_entries, truncate,
    PagedTable, Period,
};

const PAGE_SIZE: usize = 50;
/// Cap on the in-memory search box, matching the scope of a filter field
/// (not a document editor).
const MAX_SEARCH_LEN: usize = 200;

enum DashboardAction {
    Continue,
    Quit,
    OpenPrompts,
}

struct AgentApp {
    entries: Vec<Entry>,
    /// Filtered indices into `entries`, recent first
    visible: Vec<usize>,
    risk_summary: SessionRisk,
    agent_counts: Vec<(String, usize)>,
    agent_names: Vec<String>,

    /// Precomputed count of high-risk entries in `visible` (for header display).
    visible_high_risk_count: usize,

    // Filters
    period: Period,
    agent_filter: Option<usize>,
    risk_filter: bool,
    cli_executor: Option<String>,
    cwd_filter: Option<String>,
    /// Always-on live filter over `entries[i].command`, like `suv search`.
    search: String,

    // Pagination + row selection
    pager: PagedTable,

    // UI state
    detail_open: bool,

    home: String,
    status_message: Option<(String, std::time::Instant)>,
}

impl AgentApp {
    fn new(
        repo: &Repository,
        initial_after_ms: Option<i64>,
        executor: Option<&str>,
        cwd: Option<&str>,
    ) -> Self {
        let home = dirs_home();
        let period = Period::from_after_ms(initial_after_ms);

        let entries = load_entries(repo, initial_after_ms, executor, cwd);
        let risk_summary = risk::session_risk(&entries);
        let agent_counts = compute_agent_counts(&entries);
        let agent_names: Vec<String> = agent_counts.iter().map(|(n, _)| n.clone()).collect();
        // Recent first
        let visible: Vec<usize> = (0..entries.len()).rev().collect();
        let visible_high_risk_count = visible
            .iter()
            .filter(|&&i| risk::risk_level(&entries[i].command) >= RiskLevel::High)
            .count();

        let mut app = Self {
            entries,
            visible,
            risk_summary,
            agent_counts,
            agent_names,
            visible_high_risk_count,
            period,
            agent_filter: None,
            risk_filter: false,
            cli_executor: executor.map(String::from),
            cwd_filter: cwd.map(String::from),
            search: String::new(),
            pager: PagedTable::new(PAGE_SIZE),
            detail_open: true,
            home,
            status_message: None,
        };
        if !app.visible.is_empty() {
            app.pager.state.select(Some(0));
        }
        app
    }

    fn reload(&mut self, repo: &Repository) {
        let after_ms = self.period.after_ms();
        self.entries = load_entries(
            repo,
            after_ms,
            self.cli_executor.as_deref(),
            self.cwd_filter.as_deref(),
        );
        self.risk_summary = risk::session_risk(&self.entries);
        self.agent_counts = compute_agent_counts(&self.entries);
        self.agent_names = self.agent_counts.iter().map(|(n, _)| n.clone()).collect();
        if let Some(idx) = self.agent_filter {
            if idx >= self.agent_names.len() {
                self.agent_filter = None;
            }
        }
        self.rebuild_visible();
    }

    fn rebuild_visible(&mut self) {
        let agent_name = self
            .agent_filter
            .and_then(|i| self.agent_names.get(i).cloned());
        let needle = self.search.trim().to_lowercase();

        let mut high_risk_count = 0usize;

        // Recent first — compute risk on-demand during filter pass
        self.visible = (0..self.entries.len())
            .rev()
            .filter(|&i| {
                if let Some(ref name) = agent_name {
                    let entry_agent = self.entries[i].executor.as_deref().unwrap_or("unknown");
                    if entry_agent != name {
                        return false;
                    }
                }
                if !needle.is_empty() && !self.entries[i].command.to_lowercase().contains(&needle) {
                    return false;
                }
                if self.risk_filter {
                    let rl = risk::risk_level(&self.entries[i].command);
                    if rl < RiskLevel::Medium {
                        return false;
                    }
                    if rl >= RiskLevel::High {
                        high_risk_count += 1;
                    }
                    return true;
                }
                if risk::risk_level(&self.entries[i].command) >= RiskLevel::High {
                    high_risk_count += 1;
                }
                true
            })
            .collect();

        self.visible_high_risk_count = high_risk_count;
        self.pager.reset(self.visible.len());
    }

    fn total_pages(&self) -> usize {
        self.pager.total_pages(self.visible.len())
    }

    /// Indices into `visible` for the current page.
    fn page_slice(&self) -> &[usize] {
        let (start, end) = self.pager.bounds(self.visible.len());
        &self.visible[start..end]
    }

    fn selected_entry(&self) -> Option<&Entry> {
        let (page_offset, _) = self.pager.bounds(self.visible.len());
        self.pager
            .selected()
            .and_then(|i| self.visible.get(page_offset + i))
            .map(|&idx| &self.entries[idx])
    }

    fn selected_risk(&self) -> RiskLevel {
        self.selected_entry()
            .map_or(RiskLevel::None, |e| risk::risk_level(&e.command))
    }

    // ── Input ────────────────────────────────────────────────

    fn handle_input(
        &mut self,
        key: crossterm::event::KeyEvent,
        repo: &Repository,
    ) -> DashboardAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('p') => return DashboardAction::OpenPrompts,
                // Period cycle. A single-key cycle (not 1-4 jump keys) because
                // the search box below eats bare digits — they need to stay
                // typable into the query. Ctrl+P is already "open Prompts" on
                // this screen, so this uses Ctrl+F (mirrors suv search's own
                // "^F Filter" mnemonic) instead of colliding with it.
                KeyCode::Char('f') => {
                    self.period = match self.period {
                        Period::Today => Period::Days7,
                        Period::Days7 => Period::Days30,
                        Period::Days30 => Period::AllTime,
                        Period::AllTime => Period::Today,
                    };
                    self.reload(repo);
                }
                // Agent filter
                KeyCode::Char('a') => {
                    if self.agent_names.is_empty() {
                        self.agent_filter = None;
                    } else {
                        self.agent_filter = match self.agent_filter {
                            None => Some(0),
                            Some(i) if i + 1 >= self.agent_names.len() => None,
                            Some(i) => Some(i + 1),
                        };
                    }
                    self.rebuild_visible();
                }
                // Risk filter
                KeyCode::Char('r') => {
                    self.risk_filter = !self.risk_filter;
                    self.rebuild_visible();
                }
                // Copy
                KeyCode::Char('y') => {
                    if let Some(entry) = self.selected_entry() {
                        match arboard::Clipboard::new()
                            .and_then(|mut c| c.set_text(entry.command.clone()))
                        {
                            Ok(()) => {
                                self.status_message =
                                    Some(("Copied!".into(), std::time::Instant::now()));
                            }
                            Err(_) => {
                                self.status_message =
                                    Some(("Copy failed".into(), std::time::Instant::now()));
                            }
                        }
                    }
                }
                _ => {}
            }
            return DashboardAction::Continue;
        }
        match key.code {
            // Always-on search, like suv search: Esc quits outright rather
            // than clearing the query first, since there's no separate
            // "typing mode" to fall back out of.
            KeyCode::Esc => return DashboardAction::Quit,
            // Detail pane
            KeyCode::Tab => {
                self.detail_open = !self.detail_open;
            }
            // Page navigation
            KeyCode::Left => self.pager.prev_page(self.visible.len()),
            KeyCode::Right => self.pager.next_page(self.visible.len()),
            // Row navigation
            KeyCode::Up => self.pager.move_up(),
            KeyCode::Down => {
                let len = self.page_slice().len();
                self.pager.move_down(len);
            }
            KeyCode::Home if !self.page_slice().is_empty() => {
                self.pager.state.select(Some(0));
            }
            KeyCode::End if !self.page_slice().is_empty() => {
                let len = self.page_slice().len();
                self.pager.select_last(len);
            }
            // Search box: any other typed character is query text, live-
            // filtering the command list as you type.
            KeyCode::Backspace => {
                self.search.pop();
                self.rebuild_visible();
            }
            KeyCode::Char(c) if self.search.len() + c.len_utf8() <= MAX_SEARCH_LEN => {
                self.search.push(c);
                self.rebuild_visible();
            }
            _ => {}
        }
        DashboardAction::Continue
    }

    // ── Render ───────────────────────────────────────────────

    fn render(&mut self, f: &mut ratatui::Frame) {
        let t = theme();
        let size = f.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header
                Constraint::Length(3), // search box (always-on, like suv search)
                Constraint::Length(1), // filters: period + agent
                Constraint::Length(3), // summary: agents | risk boxes, side by side
                Constraint::Min(8),    // body
                Constraint::Length(1), // footer
            ])
            .split(size);

        Self::render_header(f, chunks[0], t);
        self.render_search_box(f, chunks[1], t);
        self.render_filter_line(f, chunks[2], t);

        let summary = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[3]);
        self.render_agents_box(f, summary[0], t);
        self.render_risk_box(f, summary[1], t);

        // Body: table | detail (optional)
        if self.detail_open {
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
                .split(chunks[4]);
            self.render_table(f, body[0], t);
            self.render_detail(f, body[1], t);
        } else {
            self.render_table(f, chunks[4], t);
        }

        self.render_footer(f, chunks[5], t);
    }

    fn render_header(f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let header_line = Line::from(vec![Span::styled(
            "SUVADU AGENT DASHBOARD",
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        )]);
        f.render_widget(
            Paragraph::new(header_line).alignment(Alignment::Center),
            area,
        );
    }

    fn render_search_box(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let box_widget = Paragraph::new(self.search.as_str())
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

    fn render_filter_line(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let label_style = Style::default()
            .fg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let mut spans = vec![Span::styled(" Period  ", label_style)];

        for p in [
            Period::Today,
            Period::Days7,
            Period::Days30,
            Period::AllTime,
        ] {
            let is_active = p == self.period;
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

        let agent_label = self
            .agent_filter
            .and_then(|i| self.agent_names.get(i))
            .map_or_else(|| "All agents".to_string(), Clone::clone);
        spans.push(Span::styled("   Agent  ", label_style));
        spans.push(Span::styled(
            agent_label,
            Style::default().fg(t.badge_executor),
        ));

        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_agents_box(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let mut spans = Vec::new();

        if self.agent_counts.is_empty() {
            spans.push(Span::styled("none", Style::default().fg(t.text_muted)));
        } else {
            for (i, (name, count)) in self.agent_counts.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw("   "));
                }
                let is_filtered = self
                    .agent_filter
                    .and_then(|idx| self.agent_names.get(idx))
                    .is_some_and(|n| n == name);
                let name_style = if is_filtered {
                    Style::default().fg(t.primary).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.text)
                };
                spans.push(Span::styled(truncate(name, 16), name_style));
                spans.push(Span::styled(
                    format!(" {count}"),
                    Style::default().fg(t.text_muted),
                ));
            }
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(" Agents ");
        f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
    }

    fn render_risk_box(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let label_style = Style::default()
            .fg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let mut spans = Vec::new();

        if self.risk_summary.critical_count > 0 {
            spans.push(Span::styled(
                format!("⚠ {} critical   ", self.risk_summary.critical_count),
                Style::default().fg(t.risk_critical),
            ));
        }
        if self.risk_summary.high_count > 0 {
            spans.push(Span::styled(
                format!("⚠ {} high   ", self.risk_summary.high_count),
                Style::default().fg(t.risk_high),
            ));
        }
        if self.risk_summary.medium_count > 0 {
            spans.push(Span::styled(
                format!("⚡ {} medium   ", self.risk_summary.medium_count),
                Style::default().fg(t.risk_medium),
            ));
        }
        let safe = self.risk_summary.safe_count + self.risk_summary.low_count;
        spans.push(Span::styled(
            format!("✔ {safe} safe"),
            Style::default().fg(t.success),
        ));

        let total = self.entries.len();
        let success = self
            .entries
            .iter()
            .filter(|e| e.exit_code == Some(0))
            .count();
        #[allow(clippy::cast_precision_loss)]
        let rate = if total > 0 {
            success as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        spans.push(Span::styled("     Success  ", label_style));
        spans.push(Span::styled(
            format!("{rate:.1}%"),
            Style::default().fg(t.text),
        ));

        let failures = self.risk_summary.failed_commands.len();
        if failures > 0 {
            spans.push(Span::styled("   Failures  ", label_style));
            spans.push(Span::styled(
                format!("{failures}"),
                Style::default().fg(t.error),
            ));
        }

        if self.risk_filter {
            spans.push(Span::styled(
                "   [risk-only]",
                Style::default().fg(t.warning).add_modifier(Modifier::BOLD),
            ));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(" Risk ");
        f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
    }

    fn render_table(&mut self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let scrollbar_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            width: 1,
            ..area
        };
        let table_area = Rect {
            width: area.width.saturating_sub(1),
            ..area
        };

        let page_items: Vec<usize> = self.page_slice().to_vec();
        let rows = Self::build_table_rows(&self.entries, &self.home, &page_items, t);
        let title = self.build_table_title(&page_items);

        let header = Row::new(vec![
            Cell::from("Time"),
            Cell::from("Command"),
            Cell::from("Executor"),
            Cell::from("Path"),
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
            Constraint::Min(10),
            Constraint::Length(12),
            Constraint::Length(20),
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
                    .title(title),
            );

        f.render_stateful_widget(table, table_area, &mut self.pager.state);

        if self.visible.is_empty() {
            let hint = Paragraph::new(Line::from(Span::styled(
                "  No agent commands found. Try a broader time range or check integration setup.",
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

        let total_pages = self.total_pages();
        let mut scrollbar_state =
            ScrollbarState::new(total_pages).position(self.pager.page.saturating_sub(1));
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(Style::default().fg(t.primary_dim))
                .track_style(Style::default().fg(t.border)),
            scrollbar_area,
            &mut scrollbar_state,
        );
    }

    fn build_table_rows<'a>(
        entries: &'a [Entry],
        home: &str,
        page_items: &[usize],
        t: &crate::theme::Theme,
    ) -> Vec<Row<'a>> {
        page_items
            .iter()
            .map(|&idx| {
                let entry = &entries[idx];
                let rl = risk::risk_level(&entry.command);

                let time = format_datetime(entry.started_at);
                let executor = entry.executor.as_deref().unwrap_or("unknown");
                let path_full = shorten_path(&entry.cwd, home);
                let path_display = crate::util::truncate_str_start(&path_full, 18, "...");
                let command_display = crate::util::highlight_command(&entry.command, 0);

                let risk_icon = rl.icon();
                let exit_display = match entry.exit_code {
                    Some(0) => format!("✔ {risk_icon}"),
                    Some(c) => format!("✘ {c} {risk_icon}"),
                    None => format!("○ {risk_icon}"),
                };

                #[allow(clippy::cast_precision_loss)]
                let dur = entry.duration_ms as f64 / 1000.0;
                let dur_str = format!("{dur:.1}s");

                let exit_style = match entry.exit_code {
                    Some(0) => Style::default().fg(t.success),
                    Some(_) => Style::default().fg(t.error),
                    None => Style::default().fg(t.text_muted),
                };

                Row::new(vec![
                    Cell::from(time).style(Style::default().fg(t.text_muted)),
                    Cell::from(command_display),
                    Cell::from(executor).style(Style::default().fg(t.badge_executor)),
                    Cell::from(path_display).style(Style::default().fg(t.badge_path)),
                    Cell::from(exit_display).style(exit_style),
                    Cell::from(dur_str).style(Style::default().fg(t.text_muted)),
                ])
            })
            .collect()
    }

    fn build_table_title(&self, page_items: &[usize]) -> String {
        if self.visible.is_empty() {
            "Agent Commands (0/0)".to_string()
        } else {
            let start = (self.pager.page - 1) * self.pager.page_size + 1;
            let end = start + page_items.len().saturating_sub(1);
            format!("Agent Commands ({start}-{end} / {})", self.visible.len())
        }
    }

    fn render_detail(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(" Detail ")
            .title_style(
                Style::default()
                    .fg(t.text_secondary)
                    .add_modifier(Modifier::BOLD),
            );

        let inner = block.inner(area);
        f.render_widget(block, area);

        let Some(entry) = self.selected_entry() else {
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
        let rl = self.selected_risk();
        let max_w = inner.width.saturating_sub(2) as usize;

        let mut lines = Vec::new();
        Self::build_detail_command(&mut lines, entry, max_w, label, t);
        Self::build_detail_metadata(&mut lines, entry, &self.home, label, val, t);
        lines.push(Line::from(""));
        Self::build_detail_risk(&mut lines, entry, rl, label, t);
        Self::build_detail_prompt(&mut lines, entry, max_w, label, t);

        f.render_widget(Paragraph::new(lines), inner);
    }

    fn build_detail_command(
        lines: &mut Vec<Line>,
        entry: &Entry,
        max_w: usize,
        label: Style,
        t: &crate::theme::Theme,
    ) {
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
    }

    fn build_detail_metadata(
        lines: &mut Vec<Line>,
        entry: &Entry,
        home: &str,
        label: Style,
        val: Style,
        t: &crate::theme::Theme,
    ) {
        let path = shorten_path(&entry.cwd, home);
        lines.push(Line::from(vec![
            Span::styled("Path     ", label),
            Span::styled(path, val),
        ]));

        let time_str = format_full_datetime(entry.started_at);
        lines.push(Line::from(vec![
            Span::styled("Time     ", label),
            Span::styled(time_str, val),
        ]));

        #[allow(clippy::cast_precision_loss)]
        let dur_secs = entry.duration_ms as f64 / 1000.0;
        lines.push(Line::from(vec![
            Span::styled("Duration ", label),
            Span::styled(format!("{dur_secs:.2}s"), val),
        ]));

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

        lines.push(Line::from(vec![
            Span::styled("Session  ", label),
            Span::styled(entry.session_id.clone(), val),
        ]));
    }

    fn build_detail_risk(
        lines: &mut Vec<Line>,
        entry: &Entry,
        rl: RiskLevel,
        label: Style,
        t: &crate::theme::Theme,
    ) {
        if rl > RiskLevel::None {
            if let Some(a) = risk::assess_risk(&entry.command) {
                let risk_color = match a.level {
                    RiskLevel::Critical => t.risk_critical,
                    RiskLevel::High => t.risk_high,
                    RiskLevel::Medium => t.risk_medium,
                    RiskLevel::Low => t.risk_low,
                    RiskLevel::None => t.text_muted,
                };
                lines.push(Line::from(vec![
                    Span::styled("Risk     ", label),
                    Span::styled(
                        format!(
                            "{} {} — {}",
                            a.level.icon(),
                            a.level.label().to_uppercase(),
                            a.category,
                        ),
                        Style::default().fg(risk_color),
                    ),
                ]));
                lines.push(Line::from(Span::styled(
                    format!("         {}", a.description),
                    Style::default().fg(t.text_muted),
                )));
                lines.push(Line::from(""));
            }
        }
    }

    fn build_detail_prompt(
        lines: &mut Vec<Line>,
        entry: &Entry,
        max_w: usize,
        label: Style,
        t: &crate::theme::Theme,
    ) {
        if let Some(ctx) = &entry.context {
            if let Some(prompt) = ctx.get("agent_prompt") {
                lines.push(Line::from(Span::styled("Prompt", label)));
                let prompt_chars: Vec<char> = prompt.chars().collect();
                for chunk in prompt_chars.chunks(max_w.max(1)) {
                    let chunk_str: String = chunk.iter().collect();
                    lines.push(Line::from(Span::styled(
                        format!(" {chunk_str}"),
                        Style::default().fg(t.info),
                    )));
                }
            }
        }
    }

    fn render_footer(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        // Matches suv search's exact footer style/colors and Quit-first ordering.
        let badge_key = Style::default().bg(t.badge_bg).fg(t.text);
        let badge_label = Style::default().fg(t.text_secondary);

        let total_pages = self.total_pages();

        let mut spans = vec![
            Span::styled(" Esc ", badge_key),
            Span::styled(" Quit  ", badge_label),
            Span::styled(" ^F ", badge_key),
            Span::styled(" Period  ", badge_label),
            Span::styled(" ^A ", badge_key),
            Span::styled(" Agent  ", badge_label),
            Span::styled(" ^R ", badge_key),
            Span::styled(
                if self.risk_filter {
                    " All  "
                } else {
                    " Risk only  "
                },
                badge_label,
            ),
            Span::styled(" ^P ", badge_key),
            Span::styled(" Prompts  ", badge_label),
            Span::styled(" ^Y ", badge_key),
            Span::styled(" Copy  ", badge_label),
            Span::styled(" Tab ", badge_key),
            Span::styled(" Detail  ", badge_label),
            Span::styled(" ←→ ", badge_key),
            Span::styled(" Page  ", badge_label),
        ];

        spans.push(Span::styled(
            format!(" {}/{total_pages} ", self.pager.page),
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

// ── Public entry: Agent Dashboard ────────────────────────────

pub fn run_agent_ui<B: Backend>(
    terminal: &mut Terminal<B>,
    repo: &Repository,
    initial_after_ms: Option<i64>,
    executor: Option<&str>,
    cwd: Option<&str>,
) -> io::Result<()>
where
    io::Error: From<B::Error>,
{
    let mut app = AgentApp::new(repo, initial_after_ms, executor, cwd);

    loop {
        terminal.draw(|f| app.render(f))?;

        // Poll with timeout so stale status messages get cleared even without user input
        let timeout = if app.status_message.is_some() {
            std::time::Duration::from_secs(2)
        } else {
            std::time::Duration::from_mins(1)
        };
        if !event::poll(timeout)? {
            continue; // timeout — re-render to clear stale status
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match app.handle_input(key, repo) {
                DashboardAction::Quit => return Ok(()),
                DashboardAction::OpenPrompts => {
                    super::prompts::run_prompt_explorer(
                        terminal,
                        &app.entries,
                        Some(repo),
                        app.period.after_ms(),
                        app.cli_executor.as_deref(),
                        app.cwd_filter.as_deref(),
                    )?;
                }
                DashboardAction::Continue => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(cmd: &str, executor: Option<&str>, cwd: &str) -> Entry {
        let mut e = Entry::new(
            "sess1".into(),
            cmd.into(),
            cwd.into(),
            Some(0),
            1_000_000,
            1_001_000,
        );
        e.executor_type = Some("agent".into());
        e.executor = executor.map(String::from);
        e
    }

    fn make_app(entries: Vec<Entry>) -> AgentApp {
        let visible: Vec<usize> = (0..entries.len()).rev().collect();
        let agent_counts = compute_agent_counts(&entries);
        let agent_names: Vec<String> = agent_counts.iter().map(|(n, _)| n.clone()).collect();
        let risk_summary = risk::session_risk(&entries);
        let visible_high_risk_count = visible
            .iter()
            .filter(|&&i| risk::risk_level(&entries[i].command) >= RiskLevel::High)
            .count();
        let mut app = AgentApp {
            entries,
            visible,
            risk_summary,
            agent_counts,
            agent_names,
            visible_high_risk_count,
            period: Period::AllTime,
            agent_filter: None,
            risk_filter: false,
            cli_executor: None,
            cwd_filter: None,
            search: String::new(),
            pager: PagedTable::new(PAGE_SIZE),
            detail_open: true,
            home: "/home/test".into(),
            status_message: None,
        };
        if !app.visible.is_empty() {
            app.pager.state.select(Some(0));
        }
        app
    }

    // ── build_table_title ──

    #[test]
    fn build_table_title_empty() {
        let app = make_app(vec![]);
        assert_eq!(app.build_table_title(&[]), "Agent Commands (0/0)");
    }

    #[test]
    fn build_table_title_with_items() {
        let entries = vec![
            make_entry("ls", Some("claude"), "/tmp"),
            make_entry("pwd", Some("claude"), "/home"),
        ];
        let app = make_app(entries);
        let page_items: Vec<usize> = app.page_slice().to_vec();
        let title = app.build_table_title(&page_items);
        assert!(title.contains("1-2"));
        assert!(title.contains("/ 2"));
    }

    // ── total_pages ──

    #[test]
    fn total_pages_empty() {
        let app = make_app(vec![]);
        assert_eq!(app.total_pages(), 1);
    }

    #[test]
    fn total_pages_within_one_page() {
        let entries: Vec<Entry> = (0..5)
            .map(|i| make_entry(&format!("cmd{i}"), Some("claude"), "/tmp"))
            .collect();
        let app = make_app(entries);
        assert_eq!(app.total_pages(), 1);
    }

    // ── page_slice ──

    #[test]
    fn page_slice_empty() {
        let app = make_app(vec![]);
        assert!(app.page_slice().is_empty());
    }

    #[test]
    fn page_slice_returns_correct_count() {
        let entries: Vec<Entry> = (0..5)
            .map(|i| make_entry(&format!("cmd{i}"), Some("claude"), "/tmp"))
            .collect();
        let app = make_app(entries);
        assert_eq!(app.page_slice().len(), 5);
    }

    // ── selected_entry ──

    #[test]
    fn selected_entry_returns_entry() {
        let entries = vec![
            make_entry("first", Some("claude"), "/tmp"),
            make_entry("second", Some("claude"), "/tmp"),
        ];
        let app = make_app(entries);
        let sel = app.selected_entry().unwrap();
        // visible is reversed, so index 0 in visible = last entry
        assert_eq!(sel.command, "second");
    }

    #[test]
    fn selected_entry_empty() {
        let app = make_app(vec![]);
        assert!(app.selected_entry().is_none());
    }

    // ── selected_risk ──

    #[test]
    fn selected_risk_safe_command() {
        let entries = vec![make_entry("ls -la", Some("claude"), "/tmp")];
        let app = make_app(entries);
        assert!(app.selected_risk() <= RiskLevel::Low);
    }

    #[test]
    fn selected_risk_dangerous_command() {
        let entries = vec![make_entry("rm -rf /", Some("claude"), "/tmp")];
        let app = make_app(entries);
        assert!(app.selected_risk() >= RiskLevel::High);
    }

    // ── rebuild_visible ──

    #[test]
    fn rebuild_visible_no_filter() {
        let entries = vec![
            make_entry("ls", Some("claude"), "/tmp"),
            make_entry("pwd", Some("cursor"), "/home"),
        ];
        let mut app = make_app(entries);
        app.rebuild_visible();
        assert_eq!(app.visible.len(), 2);
    }

    #[test]
    fn rebuild_visible_agent_filter() {
        let entries = vec![
            make_entry("ls", Some("claude"), "/tmp"),
            make_entry("pwd", Some("cursor"), "/home"),
            make_entry("cat", Some("claude"), "/tmp"),
        ];
        let mut app = make_app(entries);
        // Filter to first agent (sorted by count, claude has 2)
        app.agent_filter = Some(0);
        app.rebuild_visible();
        assert_eq!(app.visible.len(), 2);
    }

    #[test]
    fn rebuild_visible_risk_filter() {
        let entries = vec![
            make_entry("ls", Some("claude"), "/tmp"),
            make_entry("rm -rf /important", Some("claude"), "/tmp"),
        ];
        let mut app = make_app(entries);
        app.risk_filter = true;
        app.rebuild_visible();
        // Only the risky command should pass
        assert!(app.visible.len() <= 2);
    }

    #[test]
    fn rebuild_visible_resets_page_and_selection() {
        let entries = vec![make_entry("ls", Some("claude"), "/tmp")];
        let mut app = make_app(entries);
        app.pager.page = 3;
        app.rebuild_visible();
        assert_eq!(app.pager.page, 1);
        assert_eq!(app.pager.state.selected(), Some(0));
    }

    #[test]
    fn rebuild_visible_empty_selects_none() {
        let entries = vec![make_entry("ls", Some("claude"), "/tmp")];
        let mut app = make_app(entries);
        // Filter to non-existent agent
        app.agent_names = vec!["nonexistent".into()];
        app.agent_filter = Some(0);
        app.rebuild_visible();
        assert!(app.visible.is_empty());
        assert!(app.pager.state.selected().is_none());
    }

    // ── handle_input ───────────────────────────────────────────

    #[test]
    fn esc_quits() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let entries = vec![make_entry("ls", Some("claude"), "/tmp")];
        let mut app = make_app(entries);
        let esc = crossterm::event::KeyEvent::from(KeyCode::Esc);
        assert!(matches!(
            app.handle_input(esc, &repo),
            DashboardAction::Quit
        ));
    }

    #[test]
    fn tab_toggles_detail_pane() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let entries = vec![make_entry("ls", Some("claude"), "/tmp")];
        let mut app = make_app(entries);
        assert!(app.detail_open);
        let tab = crossterm::event::KeyEvent::from(KeyCode::Tab);
        app.handle_input(tab, &repo);
        assert!(!app.detail_open);
    }

    #[test]
    fn visible_high_risk_count_tracked() {
        let entries = vec![
            make_entry("ls", Some("claude"), "/tmp"),
            make_entry("rm -rf /", Some("claude"), "/tmp"),
        ];
        let app = make_app(entries);
        // The rm -rf should be counted as high risk
        assert!(app.visible_high_risk_count >= 1);
    }

    // ── Ctrl+<letter> filter shortcuts ───────────────────────

    #[test]
    fn ctrl_a_cycles_agent_filter_via_handle_input() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let entries = vec![
            make_entry("ls", Some("claude"), "/tmp"),
            make_entry("pwd", Some("cursor"), "/tmp"),
        ];
        let mut app = make_app(entries);
        assert_eq!(app.agent_filter, None);
        let ctrl_a = crossterm::event::KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        app.handle_input(ctrl_a, &repo);
        assert_eq!(app.agent_filter, Some(0));
    }

    #[test]
    fn ctrl_r_toggles_risk_filter_via_handle_input() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let entries = vec![make_entry("ls", Some("claude"), "/tmp")];
        let mut app = make_app(entries);
        assert!(!app.risk_filter);
        let ctrl_r = crossterm::event::KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        app.handle_input(ctrl_r, &repo);
        assert!(app.risk_filter);
    }

    #[test]
    fn bare_a_and_r_type_into_search_instead_of_toggling_filters() {
        // The search box is always-on, so bare letters are query text, not
        // shortcuts — only Ctrl-modified keys are shortcuts on this screen.
        let (_dir, repo) = crate::test_utils::test_repo();
        let entries = vec![make_entry("ls", Some("claude"), "/tmp")];
        let mut app = make_app(entries);

        let a = crossterm::event::KeyEvent::from(KeyCode::Char('a'));
        app.handle_input(a, &repo);
        assert_eq!(app.agent_filter, None);

        let r = crossterm::event::KeyEvent::from(KeyCode::Char('r'));
        app.handle_input(r, &repo);
        assert!(!app.risk_filter);
        assert_eq!(app.search, "ar");
    }

    #[test]
    fn typing_filters_command_list_live() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let entries = vec![
            make_entry("git status", Some("claude"), "/tmp"),
            make_entry("cargo build", Some("claude"), "/tmp"),
        ];
        let mut app = make_app(entries);
        assert_eq!(app.visible.len(), 2);

        for c in "cargo".chars() {
            app.handle_input(crossterm::event::KeyEvent::from(KeyCode::Char(c)), &repo);
        }
        assert_eq!(app.search, "cargo");
        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.entries[app.visible[0]].command, "cargo build");
    }

    #[test]
    fn backspace_removes_last_search_char_and_refilters() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let entries = vec![
            make_entry("git status", Some("claude"), "/tmp"),
            make_entry("cargo build", Some("claude"), "/tmp"),
        ];
        let mut app = make_app(entries);
        app.search = "cargo".to_string();
        app.rebuild_visible();
        assert_eq!(app.visible.len(), 1);

        let bs = crossterm::event::KeyEvent::from(KeyCode::Backspace);
        app.handle_input(bs, &repo);
        assert_eq!(app.search, "carg");
        assert_eq!(app.visible.len(), 1); // still matches "cargo build"
    }

    #[test]
    fn ctrl_f_cycles_period_and_reloads() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let entries = vec![make_entry("ls", Some("claude"), "/tmp")];
        let mut app = make_app(entries);
        app.period = Period::AllTime;

        let ctrl_f = crossterm::event::KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        app.handle_input(ctrl_f, &repo);
        assert_eq!(app.period, Period::Today);
        app.handle_input(ctrl_f, &repo);
        assert_eq!(app.period, Period::Days7);
        app.handle_input(ctrl_f, &repo);
        assert_eq!(app.period, Period::Days30);
        app.handle_input(ctrl_f, &repo);
        assert_eq!(app.period, Period::AllTime);
    }

    #[test]
    fn bare_digit_types_into_search_instead_of_changing_period() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let entries = vec![make_entry("ls", Some("claude"), "/tmp")];
        let mut app = make_app(entries);
        let original_period = app.period;

        let one = crossterm::event::KeyEvent::from(KeyCode::Char('1'));
        app.handle_input(one, &repo);
        assert_eq!(app.period, original_period);
        assert_eq!(app.search, "1");
    }
}
