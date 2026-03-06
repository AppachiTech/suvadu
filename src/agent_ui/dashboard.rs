use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table, TableState,
};
use ratatui::Terminal;

use crate::models::Entry;
use crate::repository::Repository;
use crate::risk::{self, RiskLevel, SessionRisk};
use crate::theme::theme;
use crate::util::{dirs_home, shorten_path};

use super::{
    compute_agent_counts, compute_risk_levels, format_datetime, format_full_datetime, load_entries,
    truncate, Period,
};

const PAGE_SIZE: usize = 50;

struct AgentApp {
    entries: Vec<Entry>,
    /// Filtered indices into `entries`, recent first
    visible: Vec<usize>,
    risk_levels: Vec<RiskLevel>,
    risk_summary: SessionRisk,
    agent_counts: Vec<(String, usize)>,
    agent_names: Vec<String>,

    // Filters
    period: Period,
    agent_filter: Option<usize>,
    risk_filter: bool,
    cli_executor: Option<String>,
    cwd_filter: Option<String>,

    // Pagination
    page: usize, // 1-based
    page_size: usize,

    // UI state
    table_state: TableState,
    detail_open: bool,

    home: String,
}

impl AgentApp {
    fn new(
        repo: &Repository,
        initial_after_ms: Option<i64>,
        executor: Option<&str>,
        cwd: Option<&str>,
    ) -> Self {
        let home = dirs_home();
        let period = if initial_after_ms.is_none() {
            Period::AllTime
        } else {
            Period::Today
        };

        let entries = load_entries(repo, initial_after_ms, executor, cwd);
        let risk_levels = compute_risk_levels(&entries);
        let risk_summary = risk::session_risk(&entries);
        let agent_counts = compute_agent_counts(&entries);
        let agent_names: Vec<String> = agent_counts.iter().map(|(n, _)| n.clone()).collect();
        // Recent first
        let visible: Vec<usize> = (0..entries.len()).rev().collect();

        let mut app = Self {
            entries,
            visible,
            risk_levels,
            risk_summary,
            agent_counts,
            agent_names,
            period,
            agent_filter: None,
            risk_filter: false,
            cli_executor: executor.map(String::from),
            cwd_filter: cwd.map(String::from),
            page: 1,
            page_size: PAGE_SIZE,
            table_state: TableState::default(),
            detail_open: true,
            home,
        };
        if !app.visible.is_empty() {
            app.table_state.select(Some(0));
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
        self.risk_levels = compute_risk_levels(&self.entries);
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

        // Recent first
        self.visible = (0..self.entries.len())
            .rev()
            .filter(|&i| {
                if let Some(ref name) = agent_name {
                    let entry_agent = self.entries[i].executor.as_deref().unwrap_or("unknown");
                    if entry_agent != name {
                        return false;
                    }
                }
                if self.risk_filter && self.risk_levels[i] < RiskLevel::Medium {
                    return false;
                }
                true
            })
            .collect();

        self.page = 1;
        if self.visible.is_empty() {
            self.table_state.select(None);
        } else {
            self.table_state.select(Some(0));
        }
    }

    fn total_pages(&self) -> usize {
        self.visible.len().div_ceil(self.page_size).max(1)
    }

    /// Indices into `visible` for the current page.
    fn page_slice(&self) -> &[usize] {
        let start = (self.page - 1) * self.page_size;
        let end = (start + self.page_size).min(self.visible.len());
        if start >= self.visible.len() {
            &[]
        } else {
            &self.visible[start..end]
        }
    }

    fn selected_entry(&self) -> Option<&Entry> {
        let page_offset = (self.page - 1) * self.page_size;
        self.table_state
            .selected()
            .and_then(|i| self.visible.get(page_offset + i))
            .map(|&idx| &self.entries[idx])
    }

    fn selected_risk(&self) -> RiskLevel {
        let page_offset = (self.page - 1) * self.page_size;
        self.table_state
            .selected()
            .and_then(|i| self.visible.get(page_offset + i))
            .map_or(RiskLevel::None, |&idx| self.risk_levels[idx])
    }

    // ── Input ────────────────────────────────────────────────

    fn handle_input(&mut self, key: crossterm::event::KeyEvent, repo: &Repository) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return false,
            // Period
            KeyCode::Char('1') => {
                self.period = Period::Today;
                self.reload(repo);
            }
            KeyCode::Char('2') => {
                self.period = Period::Days7;
                self.reload(repo);
            }
            KeyCode::Char('3') => {
                self.period = Period::Days30;
                self.reload(repo);
            }
            KeyCode::Char('4') => {
                self.period = Period::AllTime;
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
            // Detail pane
            KeyCode::Tab => {
                self.detail_open = !self.detail_open;
            }
            // Copy
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(entry) = self.selected_entry() {
                    if let Ok(mut clip) = arboard::Clipboard::new() {
                        let _ = clip.set_text(entry.command.clone());
                    }
                }
            }
            // Page navigation
            KeyCode::Left => {
                if self.page > 1 {
                    self.page -= 1;
                    self.table_state.select(Some(0));
                }
            }
            KeyCode::Right => {
                if self.page < self.total_pages() {
                    self.page += 1;
                    self.table_state.select(Some(0));
                }
            }
            // Row navigation
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(cur) = self.table_state.selected() {
                    self.table_state.select(Some(cur.saturating_sub(1)));
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = self.page_slice().len().saturating_sub(1);
                if let Some(cur) = self.table_state.selected() {
                    self.table_state
                        .select(Some(cur.saturating_add(1).min(max)));
                }
            }
            KeyCode::Home => {
                if !self.page_slice().is_empty() {
                    self.table_state.select(Some(0));
                }
            }
            KeyCode::End => {
                if !self.page_slice().is_empty() {
                    self.table_state
                        .select(Some(self.page_slice().len().saturating_sub(1)));
                }
            }
            _ => {}
        }
        true
    }

    // ── Render ───────────────────────────────────────────────

    fn render(&mut self, f: &mut ratatui::Frame) {
        let t = theme();
        let size = f.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header
                Constraint::Min(8),    // body
                Constraint::Length(1), // footer
            ])
            .split(size);

        self.render_header(f, chunks[0], t);

        // Body: summary | table | detail (optional)
        if self.detail_open {
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(24),     // summary
                    Constraint::Percentage(70), // table
                    Constraint::Percentage(30), // detail
                ])
                .split(chunks[1]);
            self.render_summary(f, body[0], t);
            self.render_table(f, body[1], t);
            self.render_detail(f, body[2], t);
        } else {
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(24), Constraint::Min(30)])
                .split(chunks[1]);
            self.render_summary(f, body[0], t);
            self.render_table(f, body[1], t);
        }

        self.render_footer(f, chunks[2], t);
    }

    fn render_header(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let total = self.visible.len();
        let risk_count = self
            .visible
            .iter()
            .filter(|&&i| self.risk_levels[i] >= RiskLevel::High)
            .count();

        let agent_label = self
            .agent_filter
            .and_then(|i| self.agent_names.get(i))
            .map_or_else(|| "All agents".to_string(), Clone::clone);

        let mut spans = vec![
            Span::styled(
                " SUVADU AGENT MONITOR ",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", Style::default()),
        ];

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
            spans.push(Span::styled(
                format!(" {} ", p.label()),
                if is_active {
                    Style::default().fg(t.primary).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.text_secondary)
                },
            ));
        }

        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(
            format!("{agent_label} · {total} cmds"),
            Style::default().fg(t.text_secondary),
        ));
        if risk_count > 0 {
            spans.push(Span::styled(
                format!(" · ⚠ {risk_count}"),
                Style::default().fg(t.warning),
            ));
        }

        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    #[allow(clippy::too_many_lines)]
    fn render_summary(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let block = Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(t.border));

        let inner = block.inner(area);
        f.render_widget(block, area);

        let label_style = Style::default()
            .fg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let value_style = Style::default().fg(t.text);

        let mut lines = Vec::new();

        // Agents section
        lines.push(Line::from(Span::styled(" Agents", label_style)));
        for (name, count) in &self.agent_counts {
            let is_filtered = self
                .agent_filter
                .and_then(|i| self.agent_names.get(i))
                .is_some_and(|n| n == name);
            let dot = if is_filtered { "●" } else { " " };
            let dot_style = if is_filtered {
                Style::default().fg(t.primary)
            } else {
                Style::default().fg(t.text_muted)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {dot} "), dot_style),
                Span::styled(
                    truncate(name, 12),
                    if is_filtered {
                        Style::default().fg(t.primary).add_modifier(Modifier::BOLD)
                    } else {
                        value_style
                    },
                ),
                Span::styled(format!("  {count}"), Style::default().fg(t.text_muted)),
            ]));
        }

        lines.push(Line::from(""));

        // Risk section
        lines.push(Line::from(Span::styled(" Risk", label_style)));
        if self.risk_summary.critical_count > 0 {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    format!("⚠ {} critical", self.risk_summary.critical_count),
                    Style::default().fg(t.risk_critical),
                ),
            ]));
        }
        if self.risk_summary.high_count > 0 {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    format!("⚠ {} high", self.risk_summary.high_count),
                    Style::default().fg(t.risk_high),
                ),
            ]));
        }
        if self.risk_summary.medium_count > 0 {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    format!("⚡ {} medium", self.risk_summary.medium_count),
                    Style::default().fg(t.risk_medium),
                ),
            ]));
        }
        let safe = self.risk_summary.safe_count + self.risk_summary.low_count;
        lines.push(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(format!("✔ {safe} safe"), Style::default().fg(t.success)),
        ]));

        lines.push(Line::from(""));

        // Stats section
        lines.push(Line::from(Span::styled(" Stats", label_style)));
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
        lines.push(Line::from(vec![
            Span::styled("  Success: ", Style::default().fg(t.text_muted)),
            Span::styled(format!("{rate:.1}%"), value_style),
        ]));
        if !self.risk_summary.packages_installed.is_empty() {
            let pkg_count: usize = self
                .risk_summary
                .packages_installed
                .iter()
                .map(|p| p.packages.len())
                .sum();
            lines.push(Line::from(vec![
                Span::styled("  Packages: ", Style::default().fg(t.text_muted)),
                Span::styled(format!("{pkg_count}"), value_style),
            ]));
        }
        let failures = self.risk_summary.failed_commands.len();
        if failures > 0 {
            lines.push(Line::from(vec![
                Span::styled("  Failures: ", Style::default().fg(t.text_muted)),
                Span::styled(format!("{failures}"), Style::default().fg(t.error)),
            ]));
        }

        if self.risk_filter {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " [risk-only]",
                Style::default().fg(t.warning).add_modifier(Modifier::BOLD),
            )));
        }

        f.render_widget(Paragraph::new(lines), inner);
    }

    #[allow(clippy::too_many_lines)]
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

        let header = Row::new(vec![
            Cell::from("Time"),
            Cell::from("Executor"),
            Cell::from("Path"),
            Cell::from("Command"),
            Cell::from("Status"),
            Cell::from("Duration"),
        ])
        .style(
            Style::default()
                .fg(t.text_secondary)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

        let page_items = self.page_slice();

        let rows: Vec<Row> = page_items
            .iter()
            .map(|&idx| {
                let entry = &self.entries[idx];
                let rl = self.risk_levels[idx];

                let time = format_datetime(entry.started_at);
                let executor = entry.executor.as_deref().unwrap_or("unknown");

                let path_full = shorten_path(&entry.cwd, &self.home);
                let path_display = if path_full.len() > 18 {
                    format!("...{}", &path_full[path_full.len().saturating_sub(15)..])
                } else {
                    path_full
                };

                let cmd_w = table_area.width.saturating_sub(65) as usize;
                let cmd = truncate(&entry.command, cmd_w.max(10));

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
                    Cell::from(executor).style(Style::default().fg(t.warning)),
                    Cell::from(path_display).style(Style::default().fg(t.text_secondary)),
                    Cell::from(cmd).style(Style::default().fg(t.text)),
                    Cell::from(exit_display).style(exit_style),
                    Cell::from(dur_str).style(Style::default().fg(t.text_muted)),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(12), // Time (MM-DD HH:MM)
            Constraint::Length(12), // Executor
            Constraint::Length(20), // Path
            Constraint::Min(10),    // Command
            Constraint::Length(8),  // Status + risk icon
            Constraint::Length(8),  // Duration
        ];

        let title = if self.visible.is_empty() {
            "Agent Commands (0/0)".to_string()
        } else {
            let start = (self.page - 1) * self.page_size + 1;
            let end = start + page_items.len() - 1;
            format!("Agent Commands ({start}-{end} / {})", self.visible.len())
        };

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

        f.render_stateful_widget(table, table_area, &mut self.table_state);

        // Empty state hint
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

        // Scrollbar (page-based)
        let total_pages = self.total_pages();
        let mut scrollbar_state =
            ScrollbarState::new(total_pages).position(self.page.saturating_sub(1));
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(Style::default().fg(t.primary_dim))
                .track_style(Style::default().fg(t.border)),
            scrollbar_area,
            &mut scrollbar_state,
        );
    }

    #[allow(clippy::too_many_lines)]
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

        // Command (wraps)
        lines.push(Line::from(Span::styled("Command", label)));
        for chunk in entry
            .command
            .as_bytes()
            .chunks(max_w.max(1))
            .map(|c| std::str::from_utf8(c).unwrap_or(""))
        {
            lines.push(Line::from(Span::styled(
                format!(" {chunk}"),
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

        // Exit
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

        // Session
        let session = &entry.session_id[..8.min(entry.session_id.len())];
        lines.push(Line::from(vec![
            Span::styled("Session  ", label),
            Span::styled(session, val),
        ]));

        lines.push(Line::from(""));

        // Risk
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

        // Prompt
        if let Some(ctx) = &entry.context {
            if let Some(prompt) = ctx.get("agent_prompt") {
                lines.push(Line::from(Span::styled("Prompt", label)));
                for chunk in prompt
                    .as_bytes()
                    .chunks(max_w.max(1))
                    .map(|c| std::str::from_utf8(c).unwrap_or(""))
                {
                    lines.push(Line::from(Span::styled(
                        format!(" {chunk}"),
                        Style::default().fg(t.info),
                    )));
                }
            }
        }

        f.render_widget(Paragraph::new(lines), inner);
    }

    fn render_footer(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let badge_key = Style::default()
            .fg(t.bg_elevated)
            .bg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let badge_label = Style::default().fg(t.text_muted);

        let total_pages = self.total_pages();

        let mut spans = vec![
            Span::styled(" 1-4 ", badge_key),
            Span::styled(" Period  ", badge_label),
            Span::styled(" ←→ ", badge_key),
            Span::styled(" Page  ", badge_label),
            Span::styled(" Tab ", badge_key),
            Span::styled(" Detail  ", badge_label),
            Span::styled(" a ", badge_key),
            Span::styled(" Agent  ", badge_label),
            Span::styled(" r ", badge_key),
            Span::styled(
                if self.risk_filter {
                    " All  "
                } else {
                    " Risk only  "
                },
                badge_label,
            ),
            Span::styled(" ^Y ", badge_key),
            Span::styled(" Copy  ", badge_label),
            Span::styled(" q ", badge_key),
            Span::styled(" Quit  ", badge_label),
        ];

        spans.push(Span::styled(
            format!(" {}/{total_pages} ", self.page),
            Style::default().fg(t.text_muted),
        ));

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

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if !app.handle_input(key, repo) {
                return Ok(());
            }
        }
    }
}
