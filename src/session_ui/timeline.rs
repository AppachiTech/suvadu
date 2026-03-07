use std::collections::HashSet;
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

use crate::models::{Entry, Session};
use crate::theme::theme;
use crate::util::{dirs_home, format_duration_ms, shorten_path};

use chrono::{Local, TimeZone};

/// Minimum idle gap (ms) to show a gap indicator row.
const GAP_THRESHOLD_MS: i64 = 120_000; // 2 minutes

const PAGE_SIZE: usize = 50;

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
                let gap = entry.started_at - prev_ended;
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
        let page_len = self.page_slice().len();
        if page_len == 0 {
            return;
        }
        let cur = self.table_state.selected().unwrap_or(0);
        let mut next = cur + 1;
        // Skip gap rows
        let page_offset = (self.page - 1) * self.page_size;
        while next < page_len {
            if matches!(
                self.timeline.get(page_offset + next),
                Some(TimelineRow::Entry(_))
            ) {
                break;
            }
            next += 1;
        }
        if next < page_len {
            self.table_state.select(Some(next));
        }
    }

    /// Move selection to previous entry row, skipping gap rows.
    fn move_up(&mut self) {
        let cur = self.table_state.selected().unwrap_or(0);
        if cur == 0 {
            return;
        }
        let page_offset = (self.page - 1) * self.page_size;
        let mut prev = cur - 1;
        loop {
            if matches!(
                self.timeline.get(page_offset + prev),
                Some(TimelineRow::Entry(_))
            ) {
                break;
            }
            if prev == 0 {
                // No entry row found above, stay at current
                return;
            }
            prev -= 1;
        }
        self.table_state.select(Some(prev));
    }

    fn handle_input(&mut self, key: crossterm::event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return false,
            KeyCode::Tab => self.detail_open = !self.detail_open,
            // Copy
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(entry) = self.selected_entry() {
                    if let Ok(mut clip) = arboard::Clipboard::new() {
                        let _ = clip.set_text(entry.command.clone());
                    }
                }
            }
            // Page navigation
            KeyCode::Left | KeyCode::PageUp => {
                if self.page > 1 {
                    self.page -= 1;
                    self.table_state.select(Some(0));
                }
            }
            KeyCode::Right | KeyCode::PageDown => {
                if self.page < self.total_pages() {
                    self.page += 1;
                    self.table_state.select(Some(0));
                }
            }
            // Row navigation
            KeyCode::Up | KeyCode::Char('k') => self.move_up(),
            KeyCode::Down | KeyCode::Char('j') => self.move_down(),
            KeyCode::Home | KeyCode::Char('g') => {
                if !self.page_slice().is_empty() {
                    self.page = 1;
                    self.table_state.select(Some(0));
                }
            }
            KeyCode::End | KeyCode::Char('G') => {
                if !self.timeline.is_empty() {
                    self.page = self.total_pages();
                    let last = self.page_slice().len().saturating_sub(1);
                    self.table_state.select(Some(last));
                }
            }
            _ => {}
        }
        true
    }

    // ── Render ───────────────────────────────────────────────

    fn render(&mut self, f: &mut ratatui::Frame) {
        let t = theme();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header
                Constraint::Min(8),    // body
                Constraint::Length(1), // footer
            ])
            .split(f.area());

        self.render_header(f, chunks[0], t);

        if self.detail_open {
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
                .split(chunks[1]);
            self.render_table(f, body[0], t);
            self.render_detail(f, body[1], t);
        } else {
            self.render_table(f, chunks[1], t);
        }

        Self::render_footer(f, chunks[2], t);
    }

    fn render_header(&self, f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let id_short: String = self.session.id.chars().take(8).collect();

        let session_time = Local
            .timestamp_millis_opt(crate::util::normalize_display_ms(self.session.created_at))
            .single()
            .map_or_else(
                || "??".to_string(),
                |dt| dt.format("%Y-%m-%d %H:%M").to_string(),
            );

        let total = self.entries.len();
        let success = self
            .entries
            .iter()
            .filter(|e| e.exit_code == Some(0))
            .count();

        let span_ms = self
            .entries
            .last()
            .map_or(0, |last| last.ended_at - self.entries[0].started_at);

        let mut spans = vec![
            Span::styled(
                " SESSION TIMELINE ",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{id_short}  "),
                Style::default().fg(t.info).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{}  ", self.session.hostname),
                Style::default().fg(t.text_secondary),
            ),
            Span::styled(
                format!("{session_time}  "),
                Style::default().fg(t.text_muted),
            ),
        ];

        if let Some(ref tag) = self.tag_name {
            spans.push(Span::styled(
                format!("{tag}  "),
                Style::default().fg(t.primary),
            ));
        }

        spans.push(Span::styled(
            format!("{total} cmds  {success}✓  {}✗", total - success),
            Style::default().fg(t.text_secondary),
        ));

        if span_ms > 0 {
            spans.push(Span::styled(
                format!("  {}", format_duration_ms(span_ms)),
                Style::default().fg(t.text_muted),
            ));
        }

        f.render_widget(Paragraph::new(Line::from(spans)), area);
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

    fn render_footer(f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let badge_key = Style::default()
            .fg(t.bg_elevated)
            .bg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let badge_label = Style::default().fg(t.text_muted);

        let spans = vec![
            Span::styled(" ↑↓ ", badge_key),
            Span::styled(" Navigate  ", badge_label),
            Span::styled(" ←→ ", badge_key),
            Span::styled(" Page  ", badge_label),
            Span::styled(" Tab ", badge_key),
            Span::styled(" Detail  ", badge_label),
            Span::styled(" g/G ", badge_key),
            Span::styled(" First/Last  ", badge_label),
            Span::styled(" ^Y ", badge_key),
            Span::styled(" Copy  ", badge_label),
            Span::styled(" q ", badge_key),
            Span::styled(" Quit  ", badge_label),
        ];

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
        Style::default().fg(t.text_secondary)
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

// ── Public entry ────────────────────────────────────────────

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
