use std::collections::HashSet;
use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table, TableState, Wrap,
};
use ratatui::Terminal;

use crate::models::{Entry, Session};
use crate::session_ui::session_data::{AiSessionData, AiTimelineItem};
use crate::theme::theme;
use crate::util::{dirs_home, format_duration_ms, shorten_path};

use chrono::{Local, TimeZone};

/// Minimum idle gap (ms) to show a gap indicator row.
const GAP_THRESHOLD_MS: i64 = 120_000; // 2 minutes

const PAGE_SIZE: usize = 50;
const SPACIOUS_AI_INFO_MIN_HEIGHT: u16 = 24;

pub const fn session_info_height(terminal_height: u16) -> u16 {
    if terminal_height >= SPACIOUS_AI_INFO_MIN_HEIGHT {
        7
    } else {
        5
    }
}

fn compact_number(value: u64) -> String {
    let (divisor, suffix) = if value >= 1_000_000 {
        (1_000_000_u64, "M")
    } else if value >= 1_000 {
        (1_000_u64, "K")
    } else {
        return value.to_string();
    };
    let rounded_tenths = (u128::from(value) * 10 + u128::from(divisor) / 2) / u128::from(divisor);
    format!("{}.{:01}{suffix}", rounded_tenths / 10, rounded_tenths % 10)
}

pub fn align_info_groups(left: &str, right: &str, width: u16) -> String {
    let content_width = usize::from(width);
    let used = left.chars().count() + right.chars().count();
    let gap = content_width.saturating_sub(used).max(4);
    format!("{left}{}{right}", " ".repeat(gap))
}

fn ai_session_model(data: &AiSessionData, width: u16) -> String {
    let summary = &data.summary;
    if summary.models.len() > 1 && width >= 120 {
        summary.models.join(" → ")
    } else if summary.models.len() > 1 {
        format!(
            "{} +{}",
            summary.model.as_deref().unwrap_or("unknown model"),
            summary.models.len() - 1
        )
    } else {
        summary
            .model
            .clone()
            .unwrap_or_else(|| "unknown model".into())
    }
}

fn ai_session_prompt_count(data: &AiSessionData) -> usize {
    let mut prompts = HashSet::new();
    for item in &data.items {
        match item {
            AiTimelineItem::Prompt { text, turn_id, .. } => {
                let key = turn_id
                    .as_ref()
                    .map_or_else(|| format!("text:{text}"), |turn| format!("turn:{turn}"));
                prompts.insert(key);
            }
            AiTimelineItem::Command { entry, .. } => {
                let Some(context) = entry.context.as_ref() else {
                    continue;
                };
                let Some(prompt) = context.get("agent_prompt").filter(|text| !text.is_empty())
                else {
                    continue;
                };
                let key = context
                    .get("codex_turn_id")
                    .map_or_else(|| format!("text:{prompt}"), |turn| format!("turn:{turn}"));
                prompts.insert(key);
            }
            _ => {}
        }
    }
    prompts.len()
}

fn ai_session_token_metrics(
    data: &AiSessionData,
    width: u16,
) -> Option<Vec<(String, &'static str)>> {
    let summary = &data.summary;
    let total = data
        .usage
        .as_ref()
        .and_then(|usage| usage.total)
        .or(summary.total_tokens);
    let mut metrics = vec![(compact_number(total?), "total")];
    let usage = data.usage.as_ref();
    if width >= 78 {
        if let Some(input) = usage.and_then(|usage| usage.input) {
            metrics.push((compact_number(input), "input"));
        }
    }
    if width >= 100 {
        if let Some(cached) = usage.and_then(|usage| usage.cached_input) {
            metrics.push((compact_number(cached), "cached"));
        }
    }
    if width >= 78 {
        if let Some(output) = usage.and_then(|usage| usage.output) {
            metrics.push((compact_number(output), "output"));
        }
    }
    if width >= 100 {
        if let Some(reasoning) = usage.and_then(|usage| usage.reasoning_output) {
            metrics.push((compact_number(reasoning), "reasoning"));
        }
    }
    Some(metrics)
}

fn ai_session_tokens(data: &AiSessionData, width: u16) -> String {
    let Some(metrics) = ai_session_token_metrics(data, width) else {
        return " Tokens   unavailable".into();
    };
    let mut text = format!(
        " Tokens   {}",
        metrics
            .iter()
            .map(|(value, label)| format!("{value} {label}"))
            .collect::<Vec<_>>()
            .join("  │  ")
    );
    if !data.summary.usage_complete {
        text.push_str("  •  partial");
    }
    text
}

fn ai_session_token_line(
    data: &AiSessionData,
    width: u16,
    t: &crate::theme::Theme,
) -> Line<'static> {
    let label_style = Style::default().fg(t.text_secondary);
    let mut spans = vec![Span::styled(
        " Tokens   ",
        label_style.add_modifier(Modifier::BOLD),
    )];
    let Some(metrics) = ai_session_token_metrics(data, width) else {
        spans.push(Span::styled("unavailable", label_style));
        return Line::from(spans);
    };
    for (index, (value, label)) in metrics.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  │  ", Style::default().fg(t.border)));
        }
        spans.push(Span::styled(
            value,
            Style::default().fg(t.info).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}"), label_style));
    }
    if !data.summary.usage_complete {
        spans.push(Span::styled("  •  partial", Style::default().fg(t.warning)));
    }
    Line::from(spans)
}

pub fn session_activity_text(first_at: i64, last_at: i64, id: &str, width: u16) -> String {
    let first_date = Local.timestamp_millis_opt(first_at).single();
    let first = first_date.as_ref().map_or_else(
        || "??".into(),
        |date| date.format("%b %d, %H:%M:%S").to_string(),
    );
    let last = Local.timestamp_millis_opt(last_at).single().map_or_else(
        || "??".into(),
        |last| {
            if first_date
                .as_ref()
                .is_some_and(|first| first.date_naive() == last.date_naive())
            {
                last.format("%H:%M:%S").to_string()
            } else {
                last.format("%b %d, %H:%M:%S").to_string()
            }
        },
    );
    if width >= 70 {
        align_info_groups(&format!(" {first} → {last}"), &format!("ID  {id} "), width)
    } else {
        format!(" ID  {id}")
    }
}

pub fn command_summary_line(
    title: &'static str,
    total: usize,
    success: usize,
    failed: usize,
    t: &crate::theme::Theme,
) -> Line<'static> {
    let label = Style::default().fg(t.text_secondary);
    let mut spans = vec![
        Span::styled(format!(" {title}   "), label.add_modifier(Modifier::BOLD)),
        Span::styled(
            total.to_string(),
            Style::default().fg(t.info).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" total", label),
        Span::styled("  │  ", Style::default().fg(t.border)),
        Span::styled(
            success.to_string(),
            Style::default().fg(t.success).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" success", label),
    ];
    if failed > 0 {
        spans.push(Span::styled("  │  ", Style::default().fg(t.border)));
        spans.push(Span::styled(
            failed.to_string(),
            Style::default().fg(t.error).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" failed", label));
    }
    Line::from(spans)
}

fn ai_session_info_text(data: &AiSessionData, width: u16) -> [String; 3] {
    let summary = &data.summary;
    let agent = summary.agent.as_deref().unwrap_or("unknown agent");
    let model = ai_session_model(data, width);
    let commands = if summary.cmd_count == 1 {
        "1 command".into()
    } else {
        format!("{} commands", summary.cmd_count)
    };
    let prompt_count = ai_session_prompt_count(data);
    let prompts = if prompt_count == 1 {
        "1 prompt".into()
    } else {
        format!("{prompt_count} prompts")
    };
    let duration = format_duration_ms(
        summary
            .last_activity_at
            .saturating_sub(summary.first_activity_at),
    );
    let tag = summary
        .tag_name
        .as_deref()
        .map_or_else(String::new, |tag| format!("  •  #{tag}"));
    let identity = align_info_groups(
        &format!(" {agent}  •  {model}{tag}"),
        &format!("{prompts}  •  {commands}  •  {duration} "),
        width,
    );

    [
        identity,
        ai_session_tokens(data, width),
        session_activity_text(
            summary.first_activity_at,
            summary.last_activity_at,
            &summary.id,
            width,
        ),
    ]
}

/// A row in the timeline view — either a real entry or a gap indicator.
enum TimelineRow {
    Entry(usize), // index into entries vec
    Gap(i64),     // gap duration in ms
}

struct SessionApp {
    session: Session,
    tag_name: Option<String>,
    entries: Vec<Entry>,
    noted_ids: HashSet<i64>,
    timeline: Vec<TimelineRow>,

    // UI state
    table_state: TableState,
    page: usize, // 1-based
    page_size: usize,
    detail_open: bool,
    home: String,
    status_message: Option<(String, std::time::Instant)>,
}

impl SessionApp {
    fn new(
        session: Session,
        tag_name: Option<String>,
        entries: Vec<Entry>,
        noted_ids: HashSet<i64>,
    ) -> Self {
        let timeline = Self::build_timeline(&entries);
        let mut app = Self {
            session,
            tag_name,
            entries,
            noted_ids,
            timeline,
            table_state: TableState::default(),
            page: 1,
            page_size: PAGE_SIZE,
            detail_open: true,
            home: dirs_home(),
            status_message: None,
        };
        if !app.timeline.is_empty() {
            app.table_state.select(Some(0));
        }
        app
    }

    fn build_timeline(entries: &[Entry]) -> Vec<TimelineRow> {
        let mut rows = Vec::new();
        for (i, entry) in entries.iter().enumerate() {
            if i > 0 {
                let prev_ended = entries[i - 1].ended_at;
                let gap = entry.started_at.saturating_sub(prev_ended);
                if gap >= GAP_THRESHOLD_MS {
                    rows.push(TimelineRow::Gap(gap));
                }
            }
            rows.push(TimelineRow::Entry(i));
        }
        rows
    }

    fn total_pages(&self) -> usize {
        self.timeline.len().div_ceil(self.page_size).max(1)
    }

    fn page_slice(&self) -> &[TimelineRow] {
        let start = (self.page - 1) * self.page_size;
        let end = (start + self.page_size).min(self.timeline.len());
        if start >= self.timeline.len() {
            &[]
        } else {
            &self.timeline[start..end]
        }
    }

    fn selected_entry(&self) -> Option<&Entry> {
        let page_offset = (self.page - 1) * self.page_size;
        self.table_state
            .selected()
            .and_then(|i| self.timeline.get(page_offset + i))
            .and_then(|row| match row {
                TimelineRow::Entry(idx) => Some(&self.entries[*idx]),
                TimelineRow::Gap(_) => None,
            })
    }

    /// Move selection to next entry row, skipping gap rows.
    fn move_down(&mut self) {
        let page_offset = (self.page - 1) * self.page_size;
        let current = page_offset + self.table_state.selected().unwrap_or_default();
        if let Some(next) = ((current + 1)..self.timeline.len())
            .find(|index| matches!(self.timeline[*index], TimelineRow::Entry(_)))
        {
            self.page = next / self.page_size + 1;
            self.table_state.select(Some(next % self.page_size));
        }
    }

    /// Move selection to previous entry row, skipping gap rows.
    fn move_up(&mut self) {
        let page_offset = (self.page - 1) * self.page_size;
        let current = page_offset + self.table_state.selected().unwrap_or_default();
        if let Some(previous) = (0..current)
            .rev()
            .find(|index| matches!(self.timeline[*index], TimelineRow::Entry(_)))
        {
            self.page = previous / self.page_size + 1;
            self.table_state.select(Some(previous % self.page_size));
        }
    }

    fn handle_input(&mut self, key: crossterm::event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return false,
            KeyCode::Tab => self.detail_open = !self.detail_open,
            // Copy
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
            // Page navigation
            KeyCode::Left | KeyCode::PageUp if self.page > 1 => {
                self.page -= 1;
                self.table_state.select(Some(0));
            }
            KeyCode::Right | KeyCode::PageDown if self.page < self.total_pages() => {
                self.page += 1;
                self.table_state.select(Some(0));
            }
            // Row navigation
            KeyCode::Up | KeyCode::Char('k') => self.move_up(),
            KeyCode::Down | KeyCode::Char('j') => self.move_down(),
            KeyCode::Home | KeyCode::Char('g') if !self.page_slice().is_empty() => {
                self.page = 1;
                self.table_state.select(Some(0));
            }
            KeyCode::End | KeyCode::Char('G') if !self.timeline.is_empty() => {
                self.page = self.total_pages();
                let last = self.page_slice().len().saturating_sub(1);
                self.table_state.select(Some(last));
            }
            _ => {}
        }
        true
    }

    // ── Render ───────────────────────────────────────────────

    fn render(&mut self, f: &mut ratatui::Frame) {
        let t = theme();
        let info_height = session_info_height(f.area().height);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header
                Constraint::Length(info_height),
                Constraint::Min(6),    // body
                Constraint::Length(1), // footer
            ])
            .split(f.area());

        Self::render_header(f, chunks[0], t);
        self.render_info_box(f, chunks[1], t);

        if self.detail_open {
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
                .split(chunks[2]);
            self.render_table(f, body[0], t);
            self.render_detail(f, body[1], t);
        } else {
            self.render_table(f, chunks[2], t);
        }

        self.render_footer(f, chunks[3], t);
    }

    fn render_header(f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let header_line = Line::from(vec![Span::styled(
            "SUVADU SESSION TIMELINE",
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        )]);
        f.render_widget(
            Paragraph::new(header_line).alignment(Alignment::Center),
            area,
        );
    }

    fn info_text(&self, width: u16) -> [String; 3] {
        let total = self.entries.len();
        let duration_ms = self
            .entries
            .first()
            .zip(self.entries.last())
            .map_or(0, |(first, last)| {
                last.ended_at.saturating_sub(first.started_at)
            });
        let tag = self
            .tag_name
            .as_deref()
            .map_or_else(String::new, |tag| format!("  •  #{tag}"));
        let identity = align_info_groups(
            &format!(" human  •  {}{tag}", self.session.hostname),
            &format!(
                "{} {}  •  {} ",
                total,
                if total == 1 { "command" } else { "commands" },
                format_duration_ms(duration_ms)
            ),
            width,
        );
        let success = self
            .entries
            .iter()
            .filter(|entry| entry.exit_code == Some(0))
            .count();
        let failed = self
            .entries
            .iter()
            .filter(|entry| entry.exit_code.is_some_and(|code| code != 0))
            .count();
        let metrics =
            format!(" Commands   {total} total  │  {success} success  │  {failed} failed");
        let first = self
            .entries
            .first()
            .map_or(self.session.created_at, |entry| entry.started_at);
        let last = self
            .entries
            .last()
            .map_or(self.session.created_at, |entry| entry.ended_at);
        let activity = session_activity_text(
            crate::util::normalize_display_ms(first),
            crate::util::normalize_display_ms(last),
            &self.session.id,
            width,
        );
        [identity, metrics, activity]
    }

    fn render_info_box(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let total = self.entries.len();
        let success = self
            .entries
            .iter()
            .filter(|entry| entry.exit_code == Some(0))
            .count();
        let failed = self
            .entries
            .iter()
            .filter(|entry| entry.exit_code.is_some_and(|code| code != 0))
            .count();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(Span::styled(
                " Human Session ",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        let lines = self.info_text(inner.width);
        let content = if area.height >= 7 {
            vec![
                Line::styled(lines[0].clone(), Style::default().fg(t.text)),
                Line::default(),
                command_summary_line("Commands", total, success, failed, t),
                Line::default(),
                Line::styled(lines[2].clone(), Style::default().fg(t.text_muted)),
            ]
        } else {
            vec![
                Line::styled(lines[0].clone(), Style::default().fg(t.text)),
                command_summary_line("Commands", total, success, failed, t),
                Line::styled(lines[2].clone(), Style::default().fg(t.text_muted)),
            ]
        };
        f.render_widget(block, area);
        f.render_widget(Paragraph::new(content), inner);
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

        let header = Row::new(vec![
            Cell::from("Time"),
            Cell::from("Command"),
            Cell::from("Directory"),
            Cell::from("St"),
            Cell::from("Duration"),
        ])
        .style(
            Style::default()
                .fg(t.text_secondary)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

        let widths = [
            Constraint::Length(9),  // Time HH:MM:SS
            Constraint::Min(10),    // Command
            Constraint::Length(22), // Directory
            Constraint::Length(5),  // Status
            Constraint::Length(8),  // Duration
        ];

        let entry_count = self.entries.len();
        let title = if entry_count == 0 {
            " Timeline (0) ".to_string()
        } else {
            let pg = self.page;
            let tp = self.total_pages();
            format!(" Timeline ({entry_count} commands) {pg}/{tp} ")
        };

        let rows = build_table_rows(
            &self.timeline,
            &self.entries,
            self.page,
            self.page_size,
            &self.home,
            &self.noted_ids,
            t,
        );

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

        Self::render_empty_state(f, &self.entries, table_area, t);
        self.render_scrollbar(f, scrollbar_area, t);
    }

    fn render_empty_state(
        f: &mut ratatui::Frame,
        entries: &[Entry],
        table_area: Rect,
        t: &crate::theme::Theme,
    ) {
        if entries.is_empty() {
            let hint = Paragraph::new(Line::from(Span::styled(
                "  No commands in this session.",
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
    }

    fn render_scrollbar(
        &self,
        f: &mut ratatui::Frame,
        scrollbar_area: Rect,
        t: &crate::theme::Theme,
    ) {
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

        let lines = self.render_detail_fields(entry, inner.width, t);
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn render_detail_fields<'a>(
        &self,
        entry: &Entry,
        width: u16,
        t: &crate::theme::Theme,
    ) -> Vec<Line<'a>> {
        let label = Style::default()
            .fg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let val = Style::default().fg(t.text);
        let max_w = width.saturating_sub(2) as usize;

        let mut lines = Vec::new();

        // Command (wraps by chars for UTF-8 safety)
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

        // Started
        let time_str = Local
            .timestamp_millis_opt(entry.started_at)
            .single()
            .map_or_else(
                || "??".to_string(),
                |dt| dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            );
        lines.push(Line::from(vec![
            Span::styled("Started  ", label),
            Span::styled(time_str, val),
        ]));

        // Duration
        lines.push(Line::from(vec![
            Span::styled("Duration ", label),
            Span::styled(format_duration_ms(entry.duration_ms), val),
        ]));

        // Exit
        let exit_str = match entry.exit_code {
            Some(0) => "✓ 0 (success)".to_string(),
            Some(c) => format!("✗ {c} (failed)"),
            None => "• (unknown)".to_string(),
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
            _ => "—".to_string(),
        };
        lines.push(Line::from(vec![
            Span::styled("Executor ", label),
            Span::styled(executor, val),
        ]));

        // Note
        if let Some(entry_id) = entry.id {
            if self.noted_ids.contains(&entry_id) {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "📝 Has note (use 'suv note' to view)",
                    Style::default().fg(t.info),
                )));
            }
        }

        lines
    }

    fn render_footer(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let badge_key = Style::default().bg(t.badge_bg).fg(t.text);
        let badge_label = Style::default().fg(t.text_secondary);

        let mut spans = vec![
            Span::styled(" q/Esc ", badge_key),
            Span::styled(" Quit  ", badge_label),
            Span::styled(" ↑↓ ", badge_key),
            Span::styled(" Navigate  ", badge_label),
            Span::styled(" ←→ ", badge_key),
            Span::styled(" Page  ", badge_label),
            Span::styled(" Tab ", badge_key),
            Span::styled(" Detail  ", badge_label),
            Span::styled(" g/G ", badge_key),
            Span::styled(" First/Last  ", badge_label),
            Span::styled(" ^Y ", badge_key),
            Span::styled(" Copy ", badge_label),
        ];

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

// ── Free functions for table row building (avoids &self borrow conflicts) ──

fn build_table_rows(
    timeline: &[TimelineRow],
    entries: &[Entry],
    page: usize,
    page_size: usize,
    home: &str,
    noted_ids: &HashSet<i64>,
    t: &crate::theme::Theme,
) -> Vec<Row<'static>> {
    let start = (page - 1) * page_size;
    let end = (start + page_size).min(timeline.len());
    let page_items = if start >= timeline.len() {
        &[][..]
    } else {
        &timeline[start..end]
    };

    let mut prev_cwd: Option<&str> = None;
    if start > 0 {
        for row in timeline[..start].iter().rev() {
            if let TimelineRow::Entry(idx) = row {
                prev_cwd = Some(&entries[*idx].cwd);
                break;
            }
        }
    }

    page_items
        .iter()
        .map(|row| match row {
            TimelineRow::Gap(gap_ms) => render_gap_row(*gap_ms, t),
            TimelineRow::Entry(idx) => {
                let entry = &entries[*idx];
                let row = render_entry_row(entry, prev_cwd, home, noted_ids, t);
                prev_cwd = Some(&entry.cwd);
                row
            }
        })
        .collect()
}

fn render_gap_row(gap_ms: i64, t: &crate::theme::Theme) -> Row<'static> {
    let label = format!("── {} idle ──", format_duration_ms(gap_ms));
    Row::new(vec![
        Cell::from(""),
        Cell::from(label).style(Style::default().fg(t.text_muted)),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
    ])
    .style(Style::default().fg(t.text_muted))
}

fn render_entry_row(
    entry: &Entry,
    prev_cwd: Option<&str>,
    home: &str,
    noted_ids: &HashSet<i64>,
    t: &crate::theme::Theme,
) -> Row<'static> {
    let time = Local
        .timestamp_millis_opt(entry.started_at)
        .single()
        .map_or_else(|| "??:??:??".into(), |dt| dt.format("%H:%M:%S").to_string());

    let dir_full = shorten_path(&entry.cwd, home);
    let dir_changed = prev_cwd.is_some_and(|p| p != entry.cwd);
    let dir_style = if dir_changed {
        Style::default().fg(t.info).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(t.badge_path)
    };
    let dir_display = crate::util::truncate_str_start(&dir_full, 20, "..");

    let command_display = crate::util::highlight_command(&entry.command, 0);

    let has_note = entry.id.is_some_and(|id| noted_ids.contains(&id));
    let (status, status_style) = match entry.exit_code {
        Some(0) => (
            if has_note { "✓📝" } else { "✓" }.to_string(),
            Style::default().fg(t.success),
        ),
        Some(code) => (
            if has_note {
                format!("✗{code}📝")
            } else {
                format!("✗{code}")
            },
            Style::default().fg(t.error),
        ),
        None => (
            if has_note { "•📝" } else { "•" }.to_string(),
            Style::default().fg(t.text_muted),
        ),
    };

    let dur = format_duration_ms(entry.duration_ms);

    Row::new(vec![
        Cell::from(time).style(Style::default().fg(t.text_muted)),
        Cell::from(command_display),
        Cell::from(dir_display).style(dir_style),
        Cell::from(status).style(status_style),
        Cell::from(dur).style(Style::default().fg(t.text_muted)),
    ])
}

struct AiSessionApp {
    data: AiSessionData,
    table_state: TableState,
    page: usize,
    page_size: usize,
    detail_open: bool,
    home: String,
    status_message: Option<(String, std::time::Instant)>,
}

impl AiSessionApp {
    fn new(data: AiSessionData) -> Self {
        let mut table_state = TableState::default();
        if !data.items.is_empty() {
            table_state.select(Some(0));
        }
        Self {
            data,
            table_state,
            page: 1,
            page_size: PAGE_SIZE,
            detail_open: true,
            home: dirs_home(),
            status_message: None,
        }
    }

    fn total_pages(&self) -> usize {
        self.data.items.len().div_ceil(self.page_size).max(1)
    }

    fn page_items(&self) -> &[AiTimelineItem] {
        let start = (self.page - 1) * self.page_size;
        let end = (start + self.page_size).min(self.data.items.len());
        self.data.items.get(start..end).unwrap_or_default()
    }

    fn selected_item(&self) -> Option<&AiTimelineItem> {
        let offset = (self.page - 1) * self.page_size;
        self.table_state
            .selected()
            .and_then(|index| self.data.items.get(offset + index))
    }

    fn selected_linked_command(&self) -> Option<&Entry> {
        match self.selected_item()? {
            AiTimelineItem::Command { entry, .. }
                if entry
                    .context
                    .as_ref()
                    .and_then(|context| context.get("agent_prompt"))
                    .is_some_and(|prompt| !prompt.is_empty()) =>
            {
                Some(entry)
            }
            _ => None,
        }
    }

    fn command_entries(&self) -> Vec<Entry> {
        self.data
            .items
            .iter()
            .filter_map(|item| match item {
                AiTimelineItem::Command { entry, .. } => Some(entry.clone()),
                _ => None,
            })
            .collect()
    }

    fn handle_input(&mut self, key: crossterm::event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return false,
            KeyCode::Tab => self.detail_open = !self.detail_open,
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(text) = self.selected_item().map(AiTimelineItem::copy_text) {
                    let copied = arboard::Clipboard::new()
                        .and_then(|mut clipboard| clipboard.set_text(text.to_owned()))
                        .is_ok();
                    self.status_message = Some((
                        if copied { "Copied!" } else { "Copy failed" }.into(),
                        std::time::Instant::now(),
                    ));
                }
            }
            KeyCode::Left | KeyCode::PageUp if self.page > 1 => {
                self.page -= 1;
                self.table_state.select(Some(0));
            }
            KeyCode::Right | KeyCode::PageDown if self.page < self.total_pages() => {
                self.page += 1;
                self.table_state.select(Some(0));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let current = self.table_state.selected().unwrap_or_default();
                if current > 0 {
                    self.table_state.select(Some(current - 1));
                } else if self.page > 1 {
                    self.page -= 1;
                    self.table_state
                        .select(Some(self.page_items().len().saturating_sub(1)));
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let last = self.page_items().len().saturating_sub(1);
                let current = self.table_state.selected().unwrap_or_default();
                if current < last {
                    self.table_state.select(Some(current + 1));
                } else if self.page < self.total_pages() {
                    self.page += 1;
                    self.table_state.select(Some(0));
                }
            }
            KeyCode::Home | KeyCode::Char('g') if !self.data.items.is_empty() => {
                self.page = 1;
                self.table_state.select(Some(0));
            }
            KeyCode::End | KeyCode::Char('G') if !self.data.items.is_empty() => {
                self.page = self.total_pages();
                self.table_state
                    .select(Some(self.page_items().len().saturating_sub(1)));
            }
            _ => {}
        }
        true
    }

    fn render(&mut self, f: &mut ratatui::Frame) {
        let t = theme();
        let info_height = session_info_height(f.area().height);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(info_height),
                Constraint::Min(6),
                Constraint::Length(1),
            ])
            .split(f.area());
        SessionApp::render_header(f, chunks[0], t);
        self.render_info(f, chunks[1], t);
        if self.detail_open {
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                .split(chunks[2]);
            self.render_table(f, body[0], t);
            self.render_detail(f, body[1], t);
        } else {
            self.render_table(f, chunks[2], t);
        }
        self.render_footer(f, chunks[3], t);
    }

    fn render_info(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(Span::styled(
                " AI Session ",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        let lines = ai_session_info_text(&self.data, inner.width);
        let token_line = ai_session_token_line(&self.data, inner.width, t);
        let content = if area.height >= 7 {
            vec![
                Line::styled(lines[0].clone(), Style::default().fg(t.text)),
                Line::default(),
                token_line,
                Line::default(),
                Line::styled(lines[2].clone(), Style::default().fg(t.text_muted)),
            ]
        } else {
            vec![
                Line::styled(lines[0].clone(), Style::default().fg(t.text)),
                token_line,
                Line::styled(lines[2].clone(), Style::default().fg(t.text_muted)),
            ]
        };
        f.render_widget(block, area);
        f.render_widget(Paragraph::new(content), inner);
    }

    fn render_table(&mut self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let rows = self
            .page_items()
            .iter()
            .map(|item| {
                let time = Local
                    .timestamp_millis_opt(item.at())
                    .single()
                    .map_or_else(|| "??:??:??".into(), |dt| dt.format("%H:%M:%S").to_string());
                let preview = item.copy_text().replace('\n', " ");
                let preview = crate::util::truncate_str(&preview, 80, "…");
                let status = match item {
                    AiTimelineItem::Command { entry, .. } => match entry.exit_code {
                        Some(0) => "✓".into(),
                        Some(code) => format!("✗{code}"),
                        None => "•".into(),
                    },
                    AiTimelineItem::Interrupted { .. } => "!".into(),
                    _ => String::new(),
                };
                Row::new(vec![
                    Cell::from(time).style(Style::default().fg(t.text_muted)),
                    Cell::from(item.kind_label()).style(Style::default().fg(t.primary)),
                    Cell::from(preview),
                    Cell::from(crate::util::truncate_str_start(
                        &shorten_path(item.cwd(), &self.home),
                        20,
                        "..",
                    )),
                    Cell::from(item.model().unwrap_or(&status).to_owned())
                        .style(Style::default().fg(t.text_secondary)),
                ])
            })
            .collect::<Vec<_>>();
        let header = Row::new(["Time", "Type", "Activity", "Directory", "Model / Status"])
            .style(
                Style::default()
                    .fg(t.text_secondary)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1);
        let title = format!(
            " Timeline ({} items) {}/{} ",
            self.data.items.len(),
            self.page,
            self.total_pages()
        );
        let table = Table::new(
            rows,
            [
                Constraint::Length(9),
                Constraint::Length(12),
                Constraint::Min(18),
                Constraint::Length(22),
                Constraint::Length(18),
            ],
        )
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
        f.render_stateful_widget(table, area, &mut self.table_state);
        if self.data.items.is_empty() {
            let hint = Paragraph::new(" No captured conversation items in this session.")
                .style(Style::default().fg(t.text_muted));
            f.render_widget(
                hint,
                Rect {
                    x: area.x + 1,
                    y: area.y + 2,
                    width: area.width.saturating_sub(2),
                    height: 1,
                },
            );
        }
    }

    fn render_detail(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(" Detail ");
        let inner = block.inner(area);
        f.render_widget(block, area);
        let Some(item) = self.selected_item() else {
            f.render_widget(Paragraph::new(" No item selected"), inner);
            return;
        };
        let label = Style::default()
            .fg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let mut lines = vec![
            Line::from(Span::styled(item.kind_label(), label)),
            Line::from(Span::styled(
                item.copy_text().to_owned(),
                Style::default().fg(t.primary),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("Source  ", label),
                Span::raw(item.source_id().to_owned()),
            ]),
            Line::from(vec![
                Span::styled("Path    ", label),
                Span::raw(shorten_path(item.cwd(), &self.home)),
            ]),
        ];
        if let Some(turn) = item.turn_id() {
            lines.push(Line::from(vec![
                Span::styled("Turn    ", label),
                Span::raw(turn.to_owned()),
            ]));
        }
        if let Some(model) = item.model() {
            lines.push(Line::from(vec![
                Span::styled("Model   ", label),
                Span::raw(model.to_owned()),
            ]));
        }
        if let AiTimelineItem::Command { entry, .. } = item {
            if let Some(prompt) = entry
                .context
                .as_ref()
                .and_then(|context| context.get("agent_prompt"))
                .filter(|prompt| !prompt.is_empty())
            {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("Prompt", label)));
                let max_chars = usize::from(inner.width.saturating_sub(1)).saturating_mul(3);
                lines.push(Line::from(Span::styled(
                    crate::util::truncate_str(prompt, max_chars, "…"),
                    Style::default().fg(t.info),
                )));
                lines.push(Line::from(Span::styled(
                    "Enter  Open in Prompt Explorer",
                    Style::default().fg(t.text_muted),
                )));
            }
            lines.push(Line::from(vec![
                Span::styled("Exit    ", label),
                Span::raw(
                    entry
                        .exit_code
                        .map_or_else(|| "unknown".into(), |v| v.to_string()),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Duration", label),
                Span::raw(format!("  {}", format_duration_ms(entry.duration_ms))),
            ]));
            let executor = match (&entry.executor_type, &entry.executor) {
                (Some(kind), Some(name)) => format!("{kind}: {name}"),
                (Some(kind), None) => kind.clone(),
                (None, Some(name)) => name.clone(),
                (None, None) => "unknown".into(),
            };
            lines.push(Line::from(vec![
                Span::styled("Executor", label),
                Span::raw(format!("  {executor}")),
            ]));
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

    fn render_footer(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let key = Style::default().bg(t.badge_bg).fg(t.text);
        let label = Style::default().fg(t.text_secondary);
        let mut spans = vec![
            Span::styled(" q/Esc ", key),
            Span::styled(" Back  ", label),
            Span::styled(" ↑↓ ", key),
            Span::styled(" Navigate  ", label),
            Span::styled(" ←→ ", key),
            Span::styled(" Page  ", label),
            Span::styled(" Tab ", key),
            Span::styled(" Detail  ", label),
            Span::styled(" ^Y ", key),
            Span::styled(" Copy ", label),
        ];
        if self.selected_linked_command().is_some() {
            spans.push(Span::styled(" Enter ", key));
            spans.push(Span::styled(" Prompt  ", label));
        }
        if let Some((message, at)) = &self.status_message {
            if at.elapsed() < std::time::Duration::from_secs(2) {
                spans.push(Span::styled(
                    format!(" {message} "),
                    Style::default().fg(t.success),
                ));
            }
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

pub fn run_ai_session_timeline<B: Backend>(
    terminal: &mut Terminal<B>,
    data: AiSessionData,
) -> io::Result<()>
where
    io::Error: From<B::Error>,
{
    let mut app = AiSessionApp::new(data);
    loop {
        terminal.draw(|frame| app.render(frame))?;
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
            if key.code == KeyCode::Enter {
                if let Some(selected) = app.selected_linked_command().cloned() {
                    let entries = app.command_entries();
                    crate::agent_ui::run_prompt_detail(terminal, &entries, &selected)?;
                }
                continue;
            }
            if !app.handle_input(key) {
                return Ok(());
            }
        }
    }
}

pub fn run_session_timeline<B: Backend>(
    terminal: &mut Terminal<B>,
    session: Session,
    tag_name: Option<String>,
    entries: Vec<Entry>,
    noted_ids: HashSet<i64>,
) -> io::Result<()>
where
    io::Error: From<B::Error>,
{
    let mut app = SessionApp::new(session, tag_name, entries, noted_ids);

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
            if !app.handle_input(key) {
                return Ok(());
            }
        }
    }
}

// ── Public entry ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{SessionKind, SessionSummary};
    use std::collections::HashMap;

    fn make_entry(started_at: i64, ended_at: i64) -> Entry {
        Entry::new(
            "sess1".into(),
            "echo hi".into(),
            "/tmp".into(),
            Some(0),
            started_at,
            ended_at,
        )
    }

    fn make_session() -> Session {
        Session::new("test-host".into(), 1_000_000)
    }

    fn make_ai_data() -> crate::session_ui::AiSessionData {
        crate::session_ui::AiSessionData {
            summary: SessionSummary {
                id: "codex-test".into(),
                kind: SessionKind::Ai,
                hostname: String::new(),
                cwd: Some("/work".into()),
                agent: Some("openai-codex".into()),
                model: Some("gpt-test".into()),
                models: vec!["gpt-test".into()],
                total_tokens: Some(123),
                usage_complete: true,
                event_count: 1,
                created_at: 1_000,
                tag_name: None,
                cmd_count: 0,
                success_count: 0,
                first_activity_at: 1_000,
                last_activity_at: 2_000,
            },
            usage: Some(crate::session_ui::session_data::AiSessionUsage {
                total: Some(123),
                input: Some(100),
                cached_input: Some(40),
                output: Some(20),
                reasoning_output: Some(10),
            }),
            items: vec![crate::session_ui::session_data::AiTimelineItem::Prompt {
                at: 1_000,
                text: "Explain this".into(),
                cwd: "/work".into(),
                source_id: "prompt-1".into(),
                turn_id: Some("turn-1".into()),
                model: Some("gpt-test".into()),
            }],
        }
    }

    #[test]
    fn ai_timeline_selects_conversation_items_for_detail_and_copy() {
        let app = AiSessionApp::new(make_ai_data());
        assert_eq!(app.selected_item().unwrap().copy_text(), "Explain this");
        assert_eq!(app.total_pages(), 1);
    }

    #[test]
    fn ai_session_info_groups_identity_usage_and_activity() {
        let data = make_ai_data();
        let lines = ai_session_info_text(&data, 160);

        assert!(lines[0].starts_with(" openai-codex  •  gpt-test"));
        assert!(lines[0].ends_with("1 prompt  •  0 commands  •  1.0s "));
        assert_eq!(
            lines[1],
            " Tokens   123 total  │  100 input  │  40 cached  │  20 output  │  10 reasoning"
        );
        assert!(lines[2].contains('→'));
        assert!(lines[2].contains("ID  codex-test"));
    }

    #[test]
    fn ai_session_info_preserves_the_full_session_id() {
        let mut data = make_ai_data();
        data.summary.id = "codex-01a0960d-f3ff-76b0-adb7-72f78b1796dd".into();

        let lines = ai_session_info_text(&data, 160);

        assert!(lines[2].contains(&data.summary.id));
        assert!(!lines[2].contains('…'));
    }

    #[test]
    fn ai_session_prompt_count_falls_back_to_linked_command_context() {
        let mut data = make_ai_data();
        data.items.clear();
        let mut entry = make_entry(1_000, 1_100);
        entry.context = Some(HashMap::from([
            ("agent_prompt".into(), "Run the tests".into()),
            ("codex_turn_id".into(), "turn-1".into()),
        ]));
        data.items.push(AiTimelineItem::Command {
            entry,
            source_id: "command-1".into(),
        });

        assert_eq!(ai_session_prompt_count(&data), 1);
        assert!(ai_session_info_text(&data, 160)[0].contains("1 prompt"));
    }

    #[test]
    fn human_session_summary_uses_the_shared_three_row_layout() {
        let entries = vec![make_entry(1_000, 1_100), make_entry(2_000, 2_100)];
        let app = SessionApp::new(make_session(), None, entries, HashSet::new());

        let lines = app.info_text(160);

        assert!(lines[0].contains("test-host"));
        assert!(lines[0].contains("2 commands"));
        assert!(lines[1].contains("Commands"));
        assert!(lines[1].contains("2 total"));
        assert!(lines[2].contains("ID  "));
    }

    #[test]
    fn ai_session_info_keeps_total_tokens_on_narrow_terminals() {
        let data = make_ai_data();
        let lines = ai_session_info_text(&data, 70);

        assert_eq!(lines[1], " Tokens   123 total");
        assert!(!lines[1].contains("cached"));
        assert!(!lines[1].contains("reasoning"));
    }

    #[test]
    fn ai_session_info_compacts_optional_details_as_width_shrinks() {
        let mut data = make_ai_data();
        data.summary.models = vec!["model-a".into(), "gpt-test".into()];

        let wide = ai_session_info_text(&data, 160);
        assert!(wide[0].contains("model-a → gpt-test"));
        assert!(wide[1].contains("40 cached"));
        assert!(wide[1].contains("10 reasoning"));

        let medium = ai_session_info_text(&data, 90);
        assert!(medium[0].contains("gpt-test +1"));
        assert!(medium[1].contains("100 input"));
        assert!(medium[1].contains("20 output"));
        assert!(!medium[1].contains("cached"));
        assert!(!medium[1].contains("reasoning"));
    }

    #[test]
    fn ai_session_info_uses_spacious_height_when_room_allows() {
        assert_eq!(session_info_height(30), 7);
        assert_eq!(session_info_height(23), 5);
    }

    #[test]
    fn ai_session_token_metrics_keep_values_separate_from_labels() {
        let metrics = ai_session_token_metrics(&make_ai_data(), 160).unwrap();
        assert_eq!(
            metrics,
            vec![
                ("123".into(), "total"),
                ("100".into(), "input"),
                ("40".into(), "cached"),
                ("20".into(), "output"),
                ("10".into(), "reasoning"),
            ]
        );

        let line = ai_session_token_line(&make_ai_data(), 160, theme());
        assert_eq!(line.spans[1].style.fg, Some(theme().info));
        assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[2].style.fg, Some(theme().text_secondary));
    }

    // ── build_timeline ──

    #[test]
    fn build_timeline_empty_entries() {
        let rows = SessionApp::build_timeline(&[]);
        assert!(rows.is_empty());
    }

    #[test]
    fn build_timeline_single_entry() {
        let entries = vec![make_entry(1000, 2000)];
        let rows = SessionApp::build_timeline(&entries);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], TimelineRow::Entry(0)));
    }

    #[test]
    fn build_timeline_no_gap_below_threshold() {
        // Two entries 1 minute apart — below GAP_THRESHOLD_MS (2 min)
        let entries = vec![
            make_entry(1_000_000, 1_010_000),
            make_entry(1_070_000, 1_080_000), // 60s gap
        ];
        let rows = SessionApp::build_timeline(&entries);
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0], TimelineRow::Entry(0)));
        assert!(matches!(rows[1], TimelineRow::Entry(1)));
    }

    #[test]
    fn build_timeline_inserts_gap_above_threshold() {
        // Two entries 3 minutes apart — above GAP_THRESHOLD_MS
        let entries = vec![
            make_entry(1_000_000, 1_010_000),
            make_entry(1_200_000, 1_210_000), // 190s gap > 120s threshold
        ];
        let rows = SessionApp::build_timeline(&entries);
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0], TimelineRow::Entry(0)));
        assert!(matches!(rows[1], TimelineRow::Gap(_)));
        assert!(matches!(rows[2], TimelineRow::Entry(1)));
    }

    #[test]
    fn build_timeline_gap_duration_correct() {
        let entries = vec![
            make_entry(1_000_000, 1_010_000),
            make_entry(1_200_000, 1_210_000),
        ];
        let rows = SessionApp::build_timeline(&entries);
        if let TimelineRow::Gap(gap) = rows[1] {
            assert_eq!(gap, 1_200_000 - 1_010_000);
        } else {
            panic!("Expected Gap row");
        }
    }

    #[test]
    fn build_timeline_saturating_sub_no_underflow() {
        // ended_at > next started_at (out-of-order timestamps)
        let entries = vec![
            make_entry(5_000_000, 6_000_000),
            make_entry(5_500_000, 5_600_000), // started_at < prev ended_at
        ];
        let rows = SessionApp::build_timeline(&entries);
        // Gap should be 0 (saturating_sub), not inserted since < threshold
        assert_eq!(rows.len(), 2);
    }

    // ── SessionApp navigation ──

    #[test]
    fn total_pages_empty() {
        let app = SessionApp::new(make_session(), None, vec![], HashSet::new());
        assert_eq!(app.total_pages(), 1);
    }

    #[test]
    fn total_pages_one_page() {
        let entries: Vec<Entry> = (0..10)
            .map(|i| make_entry(i * 1000, i * 1000 + 500))
            .collect();
        let app = SessionApp::new(make_session(), None, entries, HashSet::new());
        assert_eq!(app.total_pages(), 1);
    }

    #[test]
    fn page_slice_empty() {
        let app = SessionApp::new(make_session(), None, vec![], HashSet::new());
        assert!(app.page_slice().is_empty());
    }

    #[test]
    fn selected_entry_with_entries() {
        let entries = vec![make_entry(1000, 2000), make_entry(3000, 4000)];
        let app = SessionApp::new(make_session(), None, entries, HashSet::new());
        let sel = app.selected_entry();
        assert!(sel.is_some());
        assert_eq!(sel.unwrap().started_at, 1000);
    }

    #[test]
    fn selected_entry_empty() {
        let app = SessionApp::new(make_session(), None, vec![], HashSet::new());
        assert!(app.selected_entry().is_none());
    }

    #[test]
    fn move_down_advances_selection() {
        let entries = vec![
            make_entry(1000, 2000),
            make_entry(3000, 4000),
            make_entry(5000, 6000),
        ];
        let mut app = SessionApp::new(make_session(), None, entries, HashSet::new());
        assert_eq!(app.table_state.selected(), Some(0));
        app.move_down();
        assert_eq!(app.table_state.selected(), Some(1));
    }

    #[test]
    fn move_up_at_zero_stays() {
        let entries = vec![make_entry(1000, 2000), make_entry(3000, 4000)];
        let mut app = SessionApp::new(make_session(), None, entries, HashSet::new());
        assert_eq!(app.table_state.selected(), Some(0));
        app.move_up();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn human_row_navigation_crosses_page_boundaries() {
        let entries = (0..51)
            .map(|index| make_entry(index * 1_000, index * 1_000 + 500))
            .collect();
        let mut app = SessionApp::new(make_session(), None, entries, HashSet::new());
        app.table_state.select(Some(49));

        app.move_down();
        assert_eq!(app.page, 2);
        assert_eq!(app.table_state.selected(), Some(0));

        app.move_up();
        assert_eq!(app.page, 1);
        assert_eq!(app.table_state.selected(), Some(49));
    }

    #[test]
    fn ai_row_navigation_crosses_page_boundaries() {
        let mut data = make_ai_data();
        data.items = (0..51)
            .map(|index| AiTimelineItem::Prompt {
                at: index,
                text: format!("prompt {index}"),
                cwd: "/work".into(),
                source_id: format!("prompt-{index}"),
                turn_id: None,
                model: Some("gpt-test".into()),
            })
            .collect();
        let mut app = AiSessionApp::new(data);
        app.table_state.select(Some(49));

        app.handle_input(crossterm::event::KeyEvent::from(KeyCode::Down));
        assert_eq!(app.page, 2);
        assert_eq!(app.table_state.selected(), Some(0));

        app.handle_input(crossterm::event::KeyEvent::from(KeyCode::Up));
        assert_eq!(app.page, 1);
        assert_eq!(app.table_state.selected(), Some(49));
    }

    #[test]
    fn handle_input_q_returns_false() {
        let entries = vec![make_entry(1000, 2000)];
        let mut app = SessionApp::new(make_session(), None, entries, HashSet::new());
        let key = crossterm::event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(!app.handle_input(key));
    }

    #[test]
    fn handle_input_esc_returns_false() {
        let entries = vec![make_entry(1000, 2000)];
        let mut app = SessionApp::new(make_session(), None, entries, HashSet::new());
        let key = crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.handle_input(key));
    }

    #[test]
    fn handle_input_tab_toggles_detail() {
        let entries = vec![make_entry(1000, 2000)];
        let mut app = SessionApp::new(make_session(), None, entries, HashSet::new());
        assert!(app.detail_open);
        let key = crossterm::event::KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        assert!(app.handle_input(key));
        assert!(!app.detail_open);
        assert!(app.handle_input(key));
        assert!(app.detail_open);
    }

    #[test]
    fn handle_input_j_moves_down() {
        let entries = vec![make_entry(1000, 2000), make_entry(3000, 4000)];
        let mut app = SessionApp::new(make_session(), None, entries, HashSet::new());
        let key = crossterm::event::KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        app.handle_input(key);
        assert_eq!(app.table_state.selected(), Some(1));
    }

    #[test]
    fn handle_input_k_moves_up() {
        let entries = vec![make_entry(1000, 2000), make_entry(3000, 4000)];
        let mut app = SessionApp::new(make_session(), None, entries, HashSet::new());
        // Move to row 1, then back up
        let j = crossterm::event::KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let k = crossterm::event::KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        app.handle_input(j);
        assert_eq!(app.table_state.selected(), Some(1));
        app.handle_input(k);
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn handle_input_g_resets_to_page_one() {
        let entries = vec![make_entry(1000, 2000)];
        let mut app = SessionApp::new(make_session(), None, entries, HashSet::new());
        // Already on page 1 with data, 'g' should keep page 1 and select 0
        let key = crossterm::event::KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        app.handle_input(key);
        assert_eq!(app.page, 1);
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn handle_input_end_goes_to_last_page() {
        let entries = vec![make_entry(1000, 2000)];
        let mut app = SessionApp::new(make_session(), None, entries, HashSet::new());
        let key = crossterm::event::KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        app.handle_input(key);
        assert_eq!(app.page, app.total_pages());
    }
}
