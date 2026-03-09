use std::collections::HashMap;
use std::io;

use chrono::{Datelike, Duration, Local};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Sparkline,
    Table, TableState,
};
use ratatui::Terminal;

use crate::models::Stats;
use crate::repository::Repository;
use crate::theme::theme;
use crate::util::{dirs_home, format_count, format_duration_ms, shorten_path};

// ── Period selector ──────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Period {
    Days7,
    Days30,
    Days90,
    AllTime,
}

impl Period {
    const fn days(self) -> Option<usize> {
        match self {
            Self::Days7 => Some(7),
            Self::Days30 => Some(30),
            Self::Days90 => Some(90),
            Self::AllTime => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Days7 => "7d",
            Self::Days30 => "30d",
            Self::Days90 => "90d",
            Self::AllTime => "All",
        }
    }

    const fn heatmap_days(self) -> usize {
        match self {
            Self::Days7 => 30,
            Self::Days30 => 90,
            Self::Days90 => 180,
            Self::AllTime => 365,
        }
    }
}

// ── Focus management ─────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Hourly,
    TopPrograms,
    TopCommands,
    TopDirs,
}

impl Focus {
    const fn next(self) -> Self {
        match self {
            Self::Hourly => Self::TopPrograms,
            Self::TopPrograms => Self::TopCommands,
            Self::TopCommands => Self::TopDirs,
            Self::TopDirs => Self::Hourly,
        }
    }

    const fn prev(self) -> Self {
        match self {
            Self::Hourly => Self::TopDirs,
            Self::TopPrograms => Self::Hourly,
            Self::TopCommands => Self::TopPrograms,
            Self::TopDirs => Self::TopCommands,
        }
    }
}

// ── App state ────────────────────────────────────────────────

struct StatsApp {
    stats: Stats,
    daily_activity: Vec<(String, u32, i64)>,
    period: Period,
    focus: Focus,
    commands_table_state: TableState,
    dirs_table_state: TableState,
    programs_table_state: TableState,
    program_groups: Vec<(String, i64)>,
    show_executor: bool,
    top_n: usize,
    tag_id: Option<i64>,
    tag_name: Option<String>,
}

impl StatsApp {
    fn new(
        repo: &Repository,
        period: Period,
        top_n: usize,
        tag_id: Option<i64>,
        tag_name: Option<String>,
    ) -> Self {
        let stats = repo
            .get_stats(period.days(), top_n, tag_id)
            .unwrap_or_else(|_| Stats {
                total_commands: 0,
                unique_commands: 0,
                success_count: 0,
                failure_count: 0,
                avg_duration_ms: 0,
                top_commands: Vec::new(),
                top_directories: Vec::new(),
                hourly_distribution: Vec::new(),
                executor_breakdown: Vec::new(),
                period_days: period.days(),
            });

        let daily_activity = repo
            .get_daily_activity(period.heatmap_days(), tag_id)
            .unwrap_or_default();

        let program_groups = compute_program_groups(&stats.top_commands);

        let mut app = Self {
            stats,
            daily_activity,
            period,
            focus: Focus::TopCommands,
            commands_table_state: TableState::default(),
            dirs_table_state: TableState::default(),
            programs_table_state: TableState::default(),
            program_groups,
            show_executor: false,
            top_n,
            tag_id,
            tag_name,
        };

        if !app.stats.top_commands.is_empty() {
            app.commands_table_state.select(Some(0));
        }
        if !app.stats.top_directories.is_empty() {
            app.dirs_table_state.select(Some(0));
        }
        if !app.program_groups.is_empty() {
            app.programs_table_state.select(Some(0));
        }

        app
    }

    fn reload(&mut self, repo: &Repository) {
        if let Ok(s) = repo.get_stats(self.period.days(), self.top_n, self.tag_id) {
            self.stats = s;
        }
        if let Ok(d) = repo.get_daily_activity(self.period.heatmap_days(), self.tag_id) {
            self.daily_activity = d;
        }
        self.program_groups = compute_program_groups(&self.stats.top_commands);
        self.commands_table_state = TableState::default();
        self.dirs_table_state = TableState::default();
        self.programs_table_state = TableState::default();
        if !self.stats.top_commands.is_empty() {
            self.commands_table_state.select(Some(0));
        }
        if !self.stats.top_directories.is_empty() {
            self.dirs_table_state.select(Some(0));
        }
        if !self.program_groups.is_empty() {
            self.programs_table_state.select(Some(0));
        }
    }

    /// Returns false to quit.
    fn handle_input(&mut self, key: KeyEvent, repo: &Repository) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return false,
            KeyCode::Char('1') => {
                self.period = Period::Days7;
                self.reload(repo);
            }
            KeyCode::Char('2') => {
                self.period = Period::Days30;
                self.reload(repo);
            }
            KeyCode::Char('3') => {
                self.period = Period::Days90;
                self.reload(repo);
            }
            KeyCode::Char('4') => {
                self.period = Period::AllTime;
                self.reload(repo);
            }
            KeyCode::Char('e') => self.show_executor = !self.show_executor,
            KeyCode::Tab => {
                self.focus = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.focus.prev()
                } else {
                    self.focus.next()
                };
            }
            KeyCode::BackTab => {
                self.focus = self.focus.prev();
            }
            KeyCode::Up => self.move_selection_up(),
            KeyCode::Down => self.move_selection_down(),
            _ => {}
        }
        true
    }

    const fn move_selection_up(&mut self) {
        match self.focus {
            Focus::TopCommands => {
                if let Some(cur) = self.commands_table_state.selected() {
                    self.commands_table_state
                        .select(Some(cur.saturating_sub(1)));
                }
            }
            Focus::TopDirs => {
                if let Some(cur) = self.dirs_table_state.selected() {
                    self.dirs_table_state.select(Some(cur.saturating_sub(1)));
                }
            }
            Focus::TopPrograms => {
                if let Some(cur) = self.programs_table_state.selected() {
                    self.programs_table_state
                        .select(Some(cur.saturating_sub(1)));
                }
            }
            Focus::Hourly => {}
        }
    }

    fn move_selection_down(&mut self) {
        match self.focus {
            Focus::TopCommands => {
                let max = self.stats.top_commands.len().saturating_sub(1);
                if let Some(cur) = self.commands_table_state.selected() {
                    self.commands_table_state
                        .select(Some(cur.saturating_add(1).min(max)));
                }
            }
            Focus::TopDirs => {
                let max = self.stats.top_directories.len().saturating_sub(1);
                if let Some(cur) = self.dirs_table_state.selected() {
                    self.dirs_table_state
                        .select(Some(cur.saturating_add(1).min(max)));
                }
            }
            Focus::TopPrograms => {
                let max = self.program_groups.len().saturating_sub(1);
                if let Some(cur) = self.programs_table_state.selected() {
                    self.programs_table_state
                        .select(Some(cur.saturating_add(1).min(max)));
                }
            }
            Focus::Hourly => {}
        }
    }

    // ── Main render ──────────────────────────────────────────

    fn render(&mut self, f: &mut ratatui::Frame) {
        let size = f.area();
        let compact = size.height < 30;

        let chunks = if compact {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // header
                    Constraint::Length(3), // metrics
                    Constraint::Length(9), // heatmap
                    Constraint::Min(0),    // panels
                    Constraint::Length(1), // footer
                ])
                .split(size)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),  // header
                    Constraint::Length(3),  // metrics
                    Constraint::Length(11), // heatmap
                    Constraint::Length(4),  // sparkline
                    Constraint::Min(0),     // panels
                    Constraint::Length(1),  // footer
                ])
                .split(size)
        };

        self.render_header(f, chunks[0]);
        self.render_metrics(f, chunks[1]);
        self.render_heatmap(f, chunks[2]);

        if compact {
            self.render_panels(f, chunks[3]);
            self.render_footer(f, chunks[4]);
        } else {
            self.render_sparkline(f, chunks[3]);
            self.render_panels(f, chunks[4]);
            self.render_footer(f, chunks[5]);
        }
    }

    // ── Header ───────────────────────────────────────────────

    #[allow(clippy::cast_possible_truncation)]
    fn render_header(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let mut title_spans: Vec<Span> = vec![Span::styled(
            " Suvadu Stats ",
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        )];

        if let Some(ref tag) = self.tag_name {
            title_spans.push(Span::styled(
                format!(" tag:{tag} "),
                Style::default()
                    .fg(Color::Black)
                    .bg(t.warning)
                    .add_modifier(Modifier::BOLD),
            ));
        }

        let periods = [
            Period::Days7,
            Period::Days30,
            Period::Days90,
            Period::AllTime,
        ];
        let mut period_spans: Vec<Span> = Vec::new();
        for p in &periods {
            if *p == self.period {
                period_spans.push(Span::styled(
                    format!(" {} ", p.label()),
                    Style::default()
                        .bg(t.primary)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                period_spans.push(Span::styled(
                    format!(" {} ", p.label()),
                    Style::default().fg(t.text_muted),
                ));
            }
            period_spans.push(Span::raw(" "));
        }

        let title_width: usize = title_spans.iter().map(Span::width).sum();
        let period_width: usize = period_spans.iter().map(Span::width).sum();
        let padding = area
            .width
            .saturating_sub(title_width as u16 + period_width as u16);

        let mut spans = title_spans;
        spans.push(Span::raw(" ".repeat(padding as usize)));
        spans.extend(period_spans);

        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    // ── Metrics bar ──────────────────────────────────────────

    #[allow(clippy::cast_precision_loss)]
    fn render_metrics(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let s = &self.stats;

        let content = if self.show_executor {
            let mut spans: Vec<Span> = vec![Span::styled(
                " Executors: ",
                Style::default()
                    .fg(t.text_secondary)
                    .add_modifier(Modifier::BOLD),
            )];
            for (exec, count) in &s.executor_breakdown {
                spans.push(Span::styled(
                    format!("{exec} "),
                    Style::default().fg(t.text_secondary),
                ));
                spans.push(Span::styled(
                    format!("{}  ", format_count(*count)),
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ));
            }
            Line::from(spans)
        } else {
            let success_rate = if s.total_commands > 0 {
                (s.success_count as f64 / s.total_commands as f64) * 100.0
            } else {
                0.0
            };
            let rate_color = if success_rate >= 90.0 {
                t.success
            } else if success_rate >= 70.0 {
                t.warning
            } else {
                t.error
            };
            let duration = format_duration_ms(s.avg_duration_ms);

            Line::from(vec![
                Span::styled(" Total ", Style::default().fg(t.text_secondary)),
                Span::styled(
                    format_count(s.total_commands),
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  │  Unique ", Style::default().fg(t.text_secondary)),
                Span::styled(
                    format_count(s.unique_commands),
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  │  Success ", Style::default().fg(t.text_secondary)),
                Span::styled(
                    format!("{success_rate:.1}%"),
                    Style::default().fg(rate_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  │  Avg Duration ", Style::default().fg(t.text_secondary)),
                Span::styled(
                    duration,
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
            ])
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border));

        let paragraph = Paragraph::new(content).block(block);
        f.render_widget(paragraph, area);
    }

    // ── Contribution heatmap ─────────────────────────────────

    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    fn render_heatmap(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let block = Block::default()
            .title(Span::styled(
                " Activity ",
                Style::default().fg(t.text_secondary),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border));

        let inner = block.inner(area);
        f.render_widget(block, area);

        if inner.height < 3 || inner.width < 10 {
            return;
        }

        let colors = heatmap_colors(t);

        // Build a map from date string to count
        let mut day_counts: HashMap<String, i64> = HashMap::new();
        for (date, _dow, count) in &self.daily_activity {
            day_counts.insert(date.clone(), *count);
        }

        let max_count = day_counts.values().copied().max().unwrap_or(0).max(1);

        // Calculate grid dimensions
        let label_width: u16 = 5;
        let cell_width: u16 = 2;
        let available = inner.width.saturating_sub(label_width);
        let num_weeks = (available / cell_width).min(52) as usize;

        if num_weeks == 0 {
            return;
        }

        let today = Local::now().date_naive();
        let today_dow = today.weekday().num_days_from_sunday() as usize;

        // Show 7 day rows (show all or compact 3)
        let show_all_days = inner.height >= 9;
        let day_labels: Vec<(usize, &str)> = if show_all_days {
            vec![
                (0, "Sun"),
                (1, "Mon"),
                (2, "Tue"),
                (3, "Wed"),
                (4, "Thu"),
                (5, "Fri"),
                (6, "Sat"),
            ]
        } else {
            vec![(1, "Mon"), (3, "Wed"), (5, "Fri")]
        };

        let mut lines: Vec<Line> = Vec::new();

        for &(target_dow, label) in &day_labels {
            let mut spans: Vec<Span> = vec![Span::styled(
                format!("{label:<4} "),
                Style::default().fg(t.text_muted),
            )];

            for week_offset in (0..num_weeks).rev() {
                let total_days_back = week_offset * 7 + (7 + today_dow - target_dow) % 7;
                // For the current week (offset 0), only show days up to today
                if week_offset == 0 && target_dow > today_dow {
                    spans.push(Span::styled("  ", Style::default()));
                    continue;
                }

                let cell_date = today - Duration::days(total_days_back as i64);
                let date_str = cell_date.format("%Y-%m-%d").to_string();
                let count = day_counts.get(&date_str).copied().unwrap_or(0);
                let level = intensity_level(count, max_count);

                spans.push(Span::styled("  ", Style::default().bg(colors[level])));
            }

            lines.push(Line::from(spans));
        }

        // Add month labels at the bottom
        let mut month_spans: Vec<Span> = vec![Span::raw("     ")];
        let mut last_month = 0u32;
        for week_offset in (0..num_weeks).rev() {
            let total_days_back = week_offset * 7 + today_dow;
            let cell_date = today - Duration::days(total_days_back as i64);
            let month = cell_date.month();
            if month == last_month {
                month_spans.push(Span::raw("  "));
            } else {
                let name = month_abbrev(month);
                month_spans.push(Span::styled(
                    format!("{name:<2}"),
                    Style::default().fg(t.text_muted),
                ));
                last_month = month;
            }
        }
        if inner.height > day_labels.len() as u16 + 1 {
            lines.push(Line::from(month_spans));
        }

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }

    // ── Sparkline ────────────────────────────────────────────

    fn render_sparkline(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let data = self.build_daily_counts();

        let block = Block::default()
            .title(Span::styled(
                " Daily Trend ",
                Style::default().fg(t.text_secondary),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border));

        let sparkline = Sparkline::default()
            .block(block)
            .data(data.iter().copied())
            .style(Style::default().fg(t.primary));

        f.render_widget(sparkline, area);
    }

    #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
    fn build_daily_counts(&self) -> Vec<u64> {
        let today = Local::now().date_naive();
        let num_days = self.period.heatmap_days();
        let mut day_map: HashMap<String, i64> = HashMap::new();
        for (date, _dow, count) in &self.daily_activity {
            day_map.insert(date.clone(), *count);
        }
        let mut data = Vec::with_capacity(num_days);
        for i in (0..num_days).rev() {
            let d = today - Duration::days(i as i64);
            let key = d.format("%Y-%m-%d").to_string();
            let count = day_map.get(&key).copied().unwrap_or(0);
            data.push(count.max(0) as u64);
        }
        data
    }

    // ── Bottom panels ────────────────────────────────────────

    fn render_panels(&mut self, f: &mut ratatui::Frame, area: Rect) {
        if area.width < 60 {
            match self.focus {
                Focus::Hourly => self.render_hourly(f, area),
                Focus::TopPrograms => self.render_top_programs(f, area),
                Focus::TopCommands => self.render_top_commands(f, area),
                Focus::TopDirs => self.render_top_dirs(f, area),
            }
            return;
        }

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(20),
                Constraint::Percentage(30),
                Constraint::Percentage(30),
            ])
            .split(area);

        self.render_hourly(f, cols[0]);
        self.render_top_programs(f, cols[1]);
        self.render_top_commands(f, cols[2]);
        self.render_top_dirs(f, cols[3]);
    }

    // ── Hourly distribution ──────────────────────────────────

    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn render_hourly(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let focused = self.focus == Focus::Hourly;
        let border_color = if focused { t.border_focus } else { t.border };

        let block = Block::default()
            .title(Span::styled(
                " Hourly ",
                Style::default().fg(if focused { t.primary } else { t.text_secondary }),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);
        f.render_widget(block, area);

        if inner.height < 1 || inner.width < 8 {
            return;
        }

        let mut hourly: [i64; 24] = [0; 24];
        for &(hour, count) in &self.stats.hourly_distribution {
            let h = hour as usize;
            if h < 24 {
                hourly[h] = count;
            }
        }
        let max_count = hourly.iter().copied().max().unwrap_or(1).max(1);
        let total: i64 = hourly.iter().sum();
        // Reserve space: "HH " (3) + bar + " XX%" (4)
        let bar_width = inner.width.saturating_sub(8) as usize;

        let available_rows = inner.height as usize;
        let step = if available_rows >= 24 {
            1
        } else {
            24 / available_rows.max(1)
        };

        let mut lines: Vec<Line> = Vec::new();
        for h in (0..24).step_by(step.max(1)) {
            if lines.len() >= available_rows {
                break;
            }
            let count = hourly[h];
            let bar_len = ((count as f64 / max_count as f64) * bar_width as f64).round() as usize;
            let bar: String = "█".repeat(bar_len);
            let pct = if total > 0 {
                (count as f64 / total as f64) * 100.0
            } else {
                0.0
            };

            lines.push(Line::from(vec![
                Span::styled(format!("{h:02} "), Style::default().fg(t.text_muted)),
                Span::styled(bar, Style::default().fg(t.primary)),
                Span::styled(
                    format!(" {pct:>2.0}%"),
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                ),
            ]));
        }

        f.render_widget(Paragraph::new(lines), inner);
    }

    // ── Top programs table ────────────────────────────────────

    #[allow(clippy::cast_possible_truncation)]
    fn render_top_programs(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let focused = self.focus == Focus::TopPrograms;
        let border_color = if focused { t.border_focus } else { t.border };

        let block = Block::default()
            .title(Span::styled(
                " Top Programs ",
                Style::default().fg(if focused { t.primary } else { t.text_secondary }),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);

        let rows: Vec<Row> = self
            .program_groups
            .iter()
            .enumerate()
            .map(|(i, (prog, count))| {
                let prog_display = truncate_str(prog, inner.width.saturating_sub(12) as usize);
                Row::new(vec![
                    format!("{:>2}", i + 1),
                    prog_display,
                    format_count(*count),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(7),
        ];

        let table = Table::new(rows, widths)
            .block(block)
            .row_highlight_style(Style::default().bg(t.selection_bg).fg(t.selection_fg))
            .highlight_symbol(" > ");

        f.render_stateful_widget(table, area, &mut self.programs_table_state);

        if self.program_groups.len() > inner.height as usize && focused {
            let mut scrollbar_state = ScrollbarState::new(self.program_groups.len())
                .position(self.programs_table_state.selected().unwrap_or(0));
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(Style::default().fg(t.primary_dim));
            f.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
        }
    }

    // ── Top commands table ───────────────────────────────────

    #[allow(clippy::cast_possible_truncation)]
    fn render_top_commands(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let focused = self.focus == Focus::TopCommands;
        let border_color = if focused { t.border_focus } else { t.border };

        let block = Block::default()
            .title(Span::styled(
                " Top Commands ",
                Style::default().fg(if focused { t.primary } else { t.text_secondary }),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);

        let rows: Vec<Row> = self
            .stats
            .top_commands
            .iter()
            .enumerate()
            .map(|(i, (cmd, count))| {
                let cmd_display = truncate_str(cmd, inner.width.saturating_sub(12) as usize);
                Row::new(vec![
                    format!("{:>2}", i + 1),
                    cmd_display,
                    format_count(*count),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(7),
        ];

        let table = Table::new(rows, widths)
            .block(block)
            .row_highlight_style(Style::default().bg(t.selection_bg).fg(t.selection_fg))
            .highlight_symbol(" > ");

        f.render_stateful_widget(table, area, &mut self.commands_table_state);

        if self.stats.top_commands.len() > inner.height as usize && focused {
            let mut scrollbar_state = ScrollbarState::new(self.stats.top_commands.len())
                .position(self.commands_table_state.selected().unwrap_or(0));
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(Style::default().fg(t.primary_dim));
            f.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
        }
    }

    // ── Top directories table ────────────────────────────────

    #[allow(clippy::cast_possible_truncation)]
    fn render_top_dirs(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let focused = self.focus == Focus::TopDirs;
        let border_color = if focused { t.border_focus } else { t.border };

        let block = Block::default()
            .title(Span::styled(
                " Top Directories ",
                Style::default().fg(if focused { t.primary } else { t.text_secondary }),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);
        let home = dirs_home();

        let rows: Vec<Row> = self
            .stats
            .top_directories
            .iter()
            .enumerate()
            .map(|(i, (dir, count))| {
                let display = shorten_path(dir, &home);
                let display = truncate_str(&display, inner.width.saturating_sub(12) as usize);
                Row::new(vec![format!("{:>2}", i + 1), display, format_count(*count)])
            })
            .collect();

        let widths = [
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(7),
        ];

        let table = Table::new(rows, widths)
            .block(block)
            .row_highlight_style(Style::default().bg(t.selection_bg).fg(t.selection_fg))
            .highlight_symbol(" > ");

        f.render_stateful_widget(table, area, &mut self.dirs_table_state);

        if self.stats.top_directories.len() > inner.height as usize && focused {
            let mut scrollbar_state = ScrollbarState::new(self.stats.top_directories.len())
                .position(self.dirs_table_state.selected().unwrap_or(0));
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(Style::default().fg(t.primary_dim));
            f.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
        }
    }

    // ── Footer ───────────────────────────────────────────────

    fn render_footer(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let badge_key = Style::default().bg(t.badge_bg).fg(t.text);
        let badge_label = Style::default().fg(t.text_secondary);

        let spans = vec![
            Span::styled(" Esc ", badge_key),
            Span::styled(" Quit  ", badge_label),
            Span::styled(" 1 ", badge_key),
            Span::styled(" 7d  ", badge_label),
            Span::styled(" 2 ", badge_key),
            Span::styled(" 30d  ", badge_label),
            Span::styled(" 3 ", badge_key),
            Span::styled(" 90d  ", badge_label),
            Span::styled(" 4 ", badge_key),
            Span::styled(" All  ", badge_label),
            Span::styled(" Tab ", badge_key),
            Span::styled(" Focus  ", badge_label),
            Span::styled(" e ", badge_key),
            Span::styled(
                if self.show_executor {
                    " Summary  "
                } else {
                    " Executors  "
                },
                badge_label,
            ),
        ];

        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

// ── Public entry point ───────────────────────────────────────

pub fn run_stats_ui<B: Backend>(
    terminal: &mut Terminal<B>,
    repo: &Repository,
    initial_days: Option<usize>,
    top_n: usize,
    tag_id: Option<i64>,
    tag_name: Option<&str>,
) -> io::Result<()>
where
    io::Error: From<B::Error>,
{
    let period = match initial_days {
        Some(d) if d <= 7 => Period::Days7,
        Some(d) if d <= 30 => Period::Days30,
        Some(d) if d <= 90 => Period::Days90,
        _ => Period::AllTime,
    };

    let mut app = StatsApp::new(repo, period, top_n, tag_id, tag_name.map(String::from));

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

// ── Helpers ──────────────────────────────────────────────────

const fn heatmap_colors(t: &crate::theme::Theme) -> [Color; 5] {
    [
        t.bg_elevated,
        t.heatmap_low,
        t.heatmap_mid,
        t.primary_dim,
        t.primary,
    ]
}

#[allow(clippy::cast_precision_loss)]
fn intensity_level(count: i64, max: i64) -> usize {
    if count == 0 || max == 0 {
        return 0;
    }
    let ratio = count as f64 / max as f64;
    if ratio <= 0.25 {
        1
    } else if ratio <= 0.50 {
        2
    } else if ratio <= 0.75 {
        3
    } else {
        4
    }
}

const fn month_abbrev(month: u32) -> &'static str {
    match month {
        1 => "Ja",
        2 => "Fe",
        3 => "Mr",
        4 => "Ap",
        5 => "Ma",
        6 => "Jn",
        7 => "Jl",
        8 => "Au",
        9 => "Se",
        10 => "Oc",
        11 => "No",
        12 => "De",
        _ => "  ",
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    crate::util::truncate_str(s, max_len, "...")
}

fn compute_program_groups(top_commands: &[(String, i64)]) -> Vec<(String, i64)> {
    let mut groups: HashMap<String, i64> = HashMap::new();
    for (cmd, count) in top_commands {
        let program = cmd.split_whitespace().next().unwrap_or(cmd);
        *groups.entry(program.to_string()).or_insert(0) += count;
    }
    let mut sorted: Vec<(String, i64)> = groups.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intensity_level_zero_count() {
        assert_eq!(intensity_level(0, 100), 0);
    }

    #[test]
    fn intensity_level_zero_max() {
        assert_eq!(intensity_level(50, 0), 0);
    }

    #[test]
    fn intensity_level_low() {
        assert_eq!(intensity_level(20, 100), 1);
    }

    #[test]
    fn intensity_level_medium_low() {
        assert_eq!(intensity_level(50, 100), 2);
    }

    #[test]
    fn intensity_level_medium_high() {
        assert_eq!(intensity_level(75, 100), 3);
    }

    #[test]
    fn intensity_level_high() {
        assert_eq!(intensity_level(90, 100), 4);
    }

    #[test]
    fn intensity_level_exact_boundaries() {
        // 25% boundary
        assert_eq!(intensity_level(25, 100), 1);
        // 50% boundary
        assert_eq!(intensity_level(50, 100), 2);
        // 75% boundary
        assert_eq!(intensity_level(75, 100), 3);
        // 76% -> level 4
        assert_eq!(intensity_level(76, 100), 4);
    }

    #[test]
    fn intensity_level_equal_count_and_max() {
        assert_eq!(intensity_level(100, 100), 4);
    }

    #[test]
    fn month_abbrev_all_months() {
        assert_eq!(month_abbrev(1), "Ja");
        assert_eq!(month_abbrev(2), "Fe");
        assert_eq!(month_abbrev(3), "Mr");
        assert_eq!(month_abbrev(4), "Ap");
        assert_eq!(month_abbrev(5), "Ma");
        assert_eq!(month_abbrev(6), "Jn");
        assert_eq!(month_abbrev(7), "Jl");
        assert_eq!(month_abbrev(8), "Au");
        assert_eq!(month_abbrev(9), "Se");
        assert_eq!(month_abbrev(10), "Oc");
        assert_eq!(month_abbrev(11), "No");
        assert_eq!(month_abbrev(12), "De");
    }

    #[test]
    fn month_abbrev_out_of_range() {
        assert_eq!(month_abbrev(0), "  ");
        assert_eq!(month_abbrev(13), "  ");
    }

    #[test]
    fn compute_program_groups_empty() {
        let result = compute_program_groups(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn compute_program_groups_single_program() {
        let commands = vec![("git status".to_string(), 5), ("git push".to_string(), 3)];
        let groups = compute_program_groups(&commands);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "git");
        assert_eq!(groups[0].1, 8);
    }

    #[test]
    fn compute_program_groups_multiple_programs() {
        let commands = vec![
            ("cargo build".to_string(), 10),
            ("git status".to_string(), 5),
            ("cargo test".to_string(), 8),
            ("ls -la".to_string(), 3),
        ];
        let groups = compute_program_groups(&commands);
        assert_eq!(groups[0].0, "cargo");
        assert_eq!(groups[0].1, 18);
        assert_eq!(groups[1].0, "git");
        assert_eq!(groups[1].1, 5);
        assert_eq!(groups[2].0, "ls");
        assert_eq!(groups[2].1, 3);
    }

    #[test]
    fn compute_program_groups_single_word_command() {
        let commands = vec![("ls".to_string(), 10)];
        let groups = compute_program_groups(&commands);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "ls");
        assert_eq!(groups[0].1, 10);
    }

    #[test]
    fn test_period_days() {
        assert_eq!(Period::Days7.days(), Some(7));
        assert_eq!(Period::Days30.days(), Some(30));
        assert_eq!(Period::Days90.days(), Some(90));
        assert_eq!(Period::AllTime.days(), None);
    }

    #[test]
    fn test_period_labels() {
        assert_eq!(Period::Days7.label(), "7d");
        assert_eq!(Period::Days30.label(), "30d");
        assert_eq!(Period::Days90.label(), "90d");
        assert_eq!(Period::AllTime.label(), "All");
    }

    #[test]
    fn test_period_heatmap_days() {
        assert_eq!(Period::Days7.heatmap_days(), 30);
        assert_eq!(Period::Days30.heatmap_days(), 90);
        assert_eq!(Period::Days90.heatmap_days(), 180);
        assert_eq!(Period::AllTime.heatmap_days(), 365);
    }

    #[test]
    fn test_focus_next_cycle() {
        let f = Focus::Hourly;
        let f = f.next();
        assert_eq!(f, Focus::TopPrograms);
        let f = f.next();
        assert_eq!(f, Focus::TopCommands);
        let f = f.next();
        assert_eq!(f, Focus::TopDirs);
        let f = f.next();
        assert_eq!(f, Focus::Hourly); // wraps
    }

    #[test]
    fn test_focus_prev_cycle() {
        let f = Focus::Hourly;
        let f = f.prev();
        assert_eq!(f, Focus::TopDirs);
        let f = f.prev();
        assert_eq!(f, Focus::TopCommands);
        let f = f.prev();
        assert_eq!(f, Focus::TopPrograms);
        let f = f.prev();
        assert_eq!(f, Focus::Hourly); // wraps
    }

    #[test]
    fn test_stats_app_initial_state() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let app = StatsApp::new(&repo, Period::Days30, 10, None, None);

        assert_eq!(app.period, Period::Days30);
        assert_eq!(app.focus, Focus::TopCommands);
        assert!(!app.show_executor);
        assert_eq!(app.top_n, 10);
        assert!(app.tag_id.is_none());
        assert!(app.tag_name.is_none());
    }

    #[test]
    fn test_stats_app_focus_via_handle_input() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = StatsApp::new(&repo, Period::Days7, 10, None, None);

        assert_eq!(app.focus, Focus::TopCommands);

        // Tab cycles focus forward
        let cont = app.handle_input(KeyEvent::from(KeyCode::Tab), &repo);
        assert!(cont);
        assert_eq!(app.focus, Focus::TopDirs);

        let cont = app.handle_input(KeyEvent::from(KeyCode::Tab), &repo);
        assert!(cont);
        assert_eq!(app.focus, Focus::Hourly);

        let cont = app.handle_input(KeyEvent::from(KeyCode::Tab), &repo);
        assert!(cont);
        assert_eq!(app.focus, Focus::TopPrograms);

        let cont = app.handle_input(KeyEvent::from(KeyCode::Tab), &repo);
        assert!(cont);
        assert_eq!(app.focus, Focus::TopCommands); // wraps

        // BackTab cycles focus backward
        let cont = app.handle_input(KeyEvent::from(KeyCode::BackTab), &repo);
        assert!(cont);
        assert_eq!(app.focus, Focus::TopPrograms);
    }

    #[test]
    fn test_stats_app_period_change() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = StatsApp::new(&repo, Period::Days7, 10, None, None);

        assert_eq!(app.period, Period::Days7);

        app.handle_input(KeyEvent::from(KeyCode::Char('2')), &repo);
        assert_eq!(app.period, Period::Days30);

        app.handle_input(KeyEvent::from(KeyCode::Char('3')), &repo);
        assert_eq!(app.period, Period::Days90);

        app.handle_input(KeyEvent::from(KeyCode::Char('4')), &repo);
        assert_eq!(app.period, Period::AllTime);

        app.handle_input(KeyEvent::from(KeyCode::Char('1')), &repo);
        assert_eq!(app.period, Period::Days7);
    }

    #[test]
    fn test_stats_app_toggle_executor() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = StatsApp::new(&repo, Period::Days7, 10, None, None);

        assert!(!app.show_executor);

        app.handle_input(KeyEvent::from(KeyCode::Char('e')), &repo);
        assert!(app.show_executor);

        app.handle_input(KeyEvent::from(KeyCode::Char('e')), &repo);
        assert!(!app.show_executor);
    }

    #[test]
    fn test_stats_app_quit() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let mut app = StatsApp::new(&repo, Period::Days7, 10, None, None);

        let cont = app.handle_input(KeyEvent::from(KeyCode::Char('q')), &repo);
        assert!(!cont); // false = quit

        let mut app = StatsApp::new(&repo, Period::Days7, 10, None, None);
        let cont = app.handle_input(KeyEvent::from(KeyCode::Esc), &repo);
        assert!(!cont);
    }

    #[test]
    fn test_stats_app_selection_movement() {
        use crate::models::{Entry, Session};

        let (_dir, repo) = crate::test_utils::test_repo();
        let now = chrono::Local::now().timestamp_millis();

        // Insert a session first (entries have a FK on session_id)
        let session = Session::new("test-host".to_string(), now);
        repo.insert_session(&session).unwrap();
        let sid = session.id.clone();

        // Insert some commands so the tables are non-empty
        repo.insert_entry(&Entry::new(
            sid.clone(),
            "git status".to_string(),
            "/tmp".to_string(),
            Some(0),
            now - 300,
            now - 200,
        ))
        .unwrap();
        repo.insert_entry(&Entry::new(
            sid.clone(),
            "cargo build".to_string(),
            "/tmp".to_string(),
            Some(0),
            now - 200,
            now - 100,
        ))
        .unwrap();
        repo.insert_entry(&Entry::new(
            sid,
            "ls -la".to_string(),
            "/home".to_string(),
            Some(0),
            now - 100,
            now - 50,
        ))
        .unwrap();

        let mut app = StatsApp::new(&repo, Period::AllTime, 10, None, None);

        // Focus is on TopCommands by default; table should have selection at 0
        assert_eq!(app.focus, Focus::TopCommands);
        assert_eq!(app.commands_table_state.selected(), Some(0));

        // Move down
        app.handle_input(KeyEvent::from(KeyCode::Down), &repo);
        assert_eq!(app.commands_table_state.selected(), Some(1));

        // Move up back to 0
        app.handle_input(KeyEvent::from(KeyCode::Up), &repo);
        assert_eq!(app.commands_table_state.selected(), Some(0));

        // Move up at 0 stays at 0 (saturating)
        app.handle_input(KeyEvent::from(KeyCode::Up), &repo);
        assert_eq!(app.commands_table_state.selected(), Some(0));
    }

    #[test]
    fn test_stats_app_selection_on_different_panels() {
        use crate::models::{Entry, Session};

        let (_dir, repo) = crate::test_utils::test_repo();
        let now = chrono::Local::now().timestamp_millis();

        let session = Session::new("test-host".to_string(), now);
        repo.insert_session(&session).unwrap();
        let sid = session.id.clone();

        repo.insert_entry(&Entry::new(
            sid.clone(),
            "git status".to_string(),
            "/tmp/a".to_string(),
            Some(0),
            now - 300,
            now - 200,
        ))
        .unwrap();
        repo.insert_entry(&Entry::new(
            sid,
            "cargo test".to_string(),
            "/tmp/b".to_string(),
            Some(0),
            now - 200,
            now - 100,
        ))
        .unwrap();

        let mut app = StatsApp::new(&repo, Period::AllTime, 10, None, None);

        // Switch to TopDirs
        app.focus = Focus::TopDirs;
        assert_eq!(app.dirs_table_state.selected(), Some(0));

        app.handle_input(KeyEvent::from(KeyCode::Down), &repo);
        assert_eq!(app.dirs_table_state.selected(), Some(1));

        // Switch to TopPrograms
        app.focus = Focus::TopPrograms;
        assert_eq!(app.programs_table_state.selected(), Some(0));

        app.handle_input(KeyEvent::from(KeyCode::Down), &repo);
        assert_eq!(app.programs_table_state.selected(), Some(1));

        // Hourly focus: Up/Down have no effect (no selection state for hourly)
        app.focus = Focus::Hourly;
        app.handle_input(KeyEvent::from(KeyCode::Down), &repo);
        app.handle_input(KeyEvent::from(KeyCode::Up), &repo);
        // No assertion needed - just confirming no panic
    }

    #[test]
    fn test_build_daily_counts() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let app = StatsApp::new(&repo, Period::Days7, 10, None, None);

        let counts = app.build_daily_counts();
        // heatmap_days for Days7 = 30
        assert_eq!(counts.len(), 30);
        // With an empty repo, all counts should be 0
        assert!(counts.iter().all(|&c| c == 0));
    }

    #[test]
    fn test_stats_app_with_tag() {
        let (_dir, repo) = crate::test_utils::test_repo();
        let app = StatsApp::new(&repo, Period::Days30, 5, Some(42), Some("work".to_string()));

        assert_eq!(app.tag_id, Some(42));
        assert_eq!(app.tag_name, Some("work".to_string()));
        assert_eq!(app.top_n, 5);
    }
}
