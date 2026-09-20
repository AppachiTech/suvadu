use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::backend::Backend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState,
};
use ratatui::Terminal;

use crate::models::SessionSummary;
use crate::theme::theme;
use crate::util::{self, format_duration_ms};

use chrono::{Local, TimeZone};

/// Result of handling a key in normal mode.
enum PickerAction {
    /// Continue the event loop.
    Continue,
    /// Exit the picker with the given result.
    Exit(Option<String>),
}

// ── Filter state ────────────────────────────────────────────

const NUM_FILTER_FIELDS: usize = 3;
const PAGE_SIZE: usize = 50;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum KindFilter {
    #[default]
    All,
    Human,
    Ai,
}

impl KindFilter {
    const fn next(self) -> Self {
        match self {
            Self::All => Self::Human,
            Self::Human => Self::Ai,
            Self::Ai => Self::All,
        }
    }
}

#[derive(Default)]
struct PickerFilter {
    // Live search (session ID / tag — always active)
    search: String,

    // Filter popup inputs
    tag_input: String,
    start_date_input: String,
    end_date_input: String,
    focus_index: usize, // 0=tag, 1=start date, 2=end date
    popup_open: bool,

    // Applied filter values
    tag_query: String,
    after_ms: Option<i64>,
    before_ms: Option<i64>,
    kind: KindFilter,
}

// ── App ─────────────────────────────────────────────────────

struct PickerApp {
    sessions: Vec<SessionSummary>,
    visible: Vec<usize>,
    table_state: TableState,
    /// One-based page within the filtered session list.
    page: usize,
    filter: PickerFilter,
}

fn compact_model(summary: &SessionSummary) -> String {
    let Some(model) = summary.model.as_deref() else {
        return "—".into();
    };
    let earlier = summary.models.len().saturating_sub(1);
    if earlier == 0 {
        model.into()
    } else {
        format!("{model} +{earlier}")
    }
}

#[allow(clippy::cast_precision_loss)]
fn format_tokens(summary: &SessionSummary) -> String {
    let Some(tokens) = summary.total_tokens else {
        return "—".into();
    };
    let value = if tokens >= 1_000_000 {
        format!("{:.1}m", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    };
    if summary.usage_complete {
        value
    } else {
        format!("~{value}")
    }
}

impl PickerApp {
    fn new(sessions: Vec<SessionSummary>) -> Self {
        let visible: Vec<usize> = (0..sessions.len()).collect();
        let mut table_state = TableState::default();
        if !visible.is_empty() {
            table_state.select(Some(0));
        }
        Self {
            sessions,
            visible,
            table_state,
            page: 1,
            filter: PickerFilter::default(),
        }
    }

    fn rebuild_visible(&mut self) {
        let search = self.filter.search.to_lowercase();
        let tq = &self.filter.tag_query;
        let after = self.filter.after_ms;
        let before = self.filter.before_ms;

        self.visible = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                // Live search: match session ID or tag
                let search_ok = search.is_empty()
                    || s.id.to_lowercase().contains(&search)
                    || s.tag_name
                        .as_deref()
                        .is_some_and(|t| t.to_lowercase().contains(&search))
                    || s.agent
                        .as_deref()
                        .is_some_and(|value| value.to_lowercase().contains(&search))
                    || s.models
                        .iter()
                        .any(|value| value.to_lowercase().contains(&search))
                    || s.hostname.to_lowercase().contains(&search)
                    || s.cwd
                        .as_deref()
                        .is_some_and(|value| value.to_lowercase().contains(&search))
                    || s.preview
                        .as_deref()
                        .is_some_and(|value| value.to_lowercase().contains(&search));

                // Tag filter (from popup)
                let tag_ok = tq.is_empty()
                    || s.tag_name
                        .as_deref()
                        .is_some_and(|t| t.to_lowercase().contains(tq));

                // Date range: session has any command in the range
                // The session activity range overlaps the selected date range.
                let after_ok = after.is_none_or(|ms| s.last_activity_at >= ms);
                let before_ok = before.is_none_or(|ms| s.first_activity_at <= ms);

                let kind_ok = match self.filter.kind {
                    KindFilter::All => true,
                    KindFilter::Human => s.kind == crate::models::SessionKind::Human,
                    KindFilter::Ai => s.kind == crate::models::SessionKind::Ai,
                };

                search_ok && tag_ok && after_ok && before_ok && kind_ok
            })
            .map(|(i, _)| i)
            .collect();

        self.page = 1;
        self.table_state
            .select((!self.visible.is_empty()).then_some(0));
    }

    fn active_filter_count(&self) -> usize {
        let mut n = 0;
        if !self.filter.tag_query.is_empty() {
            n += 1;
        }
        if self.filter.after_ms.is_some() {
            n += 1;
        }
        if self.filter.before_ms.is_some() {
            n += 1;
        }
        if self.filter.kind != KindFilter::All {
            n += 1;
        }
        n
    }

    fn clear_filters(&mut self) {
        self.filter.tag_input.clear();
        self.filter.start_date_input.clear();
        self.filter.end_date_input.clear();
        self.filter.tag_query.clear();
        self.filter.after_ms = None;
        self.filter.before_ms = None;
        self.filter.kind = KindFilter::All;
        self.rebuild_visible();
    }

    fn cycle_kind_filter(&mut self) {
        self.filter.kind = self.filter.kind.next();
        self.rebuild_visible();
    }

    fn next(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        let page_len = self.page_len();
        let selected = self.table_state.selected().unwrap_or_default();
        if selected + 1 < page_len {
            self.table_state.select(Some(selected + 1));
        } else if self.page < self.total_pages() {
            self.page += 1;
            self.table_state.select(Some(0));
        }
    }

    fn prev(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        let selected = self.table_state.selected().unwrap_or_default();
        if selected > 0 {
            self.table_state.select(Some(selected - 1));
        } else if self.page > 1 {
            self.page -= 1;
            self.table_state
                .select(Some(self.page_len().saturating_sub(1)));
        }
    }

    fn total_pages(&self) -> usize {
        self.visible.len().div_ceil(PAGE_SIZE).max(1)
    }

    fn page_bounds(&self) -> (usize, usize) {
        let start = (self.page - 1) * PAGE_SIZE;
        (start, (start + PAGE_SIZE).min(self.visible.len()))
    }

    fn page_len(&self) -> usize {
        let (start, end) = self.page_bounds();
        end.saturating_sub(start)
    }

    fn next_page(&mut self) {
        if self.page < self.total_pages() {
            self.page += 1;
            self.table_state.select(Some(0));
        }
    }

    const fn prev_page(&mut self) {
        if self.page > 1 {
            self.page -= 1;
            self.table_state.select(Some(0));
        }
    }

    fn selected_session_id(&self) -> Option<&str> {
        let (start, _) = self.page_bounds();
        self.table_state
            .selected()
            .and_then(|i| self.visible.get(start + i))
            .map(|&idx| self.sessions[idx].id.as_str())
    }
}

// ── Columns ─────────────────────────────────────────────────

/// One column of the session table. Kept as data rather than inline cells so
/// the header, the widths and the values can never disagree, and so the
/// narrow-terminal column set is a testable choice instead of a guess about
/// what `Table` will squeeze out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionColumn {
    Id,
    Project,
    Preview,
    Tag,
    Kind,
    LastActive,
    Agent,
    Model,
    Tokens,
    Commands,
    Capture,
    Duration,
}

impl SessionColumn {
    const fn header(self) -> &'static str {
        match self {
            Self::Id => "Session",
            Self::Project => "Project",
            Self::Preview => "Preview",
            Self::Tag => "Tag",
            Self::Kind => "Type",
            Self::LastActive => "Last Active",
            Self::Agent => "Agent / Host",
            Self::Model => "Model",
            Self::Tokens => "Tokens",
            Self::Commands => "Cmds",
            Self::Capture => "Capture",
            Self::Duration => "Duration",
        }
    }

    const fn constraint(self) -> Constraint {
        match self {
            Self::Id => Constraint::Percentage(18),
            Self::Project | Self::Agent => Constraint::Length(14),
            Self::Preview => Constraint::Percentage(26),
            Self::Tag | Self::Model => Constraint::Length(10),
            Self::Kind | Self::Commands => Constraint::Length(5),
            Self::LastActive => Constraint::Length(12),
            Self::Tokens => Constraint::Length(7),
            Self::Capture => Constraint::Length(9),
            Self::Duration => Constraint::Min(7),
        }
    }

    fn style(self, t: &crate::theme::Theme) -> Style {
        match self {
            Self::Id => Style::default().fg(t.info).add_modifier(Modifier::BOLD),
            Self::Project | Self::Tag => Style::default().fg(t.primary),
            Self::Preview => Style::default().fg(t.text),
            Self::Kind | Self::Agent | Self::Commands => Style::default().fg(t.text_secondary),
            Self::LastActive | Self::Duration => Style::default().fg(t.text_muted),
            Self::Model => Style::default().fg(t.badge_executor),
            Self::Tokens => Style::default().fg(t.info),
            Self::Capture => Style::default().fg(t.warning),
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn value(self, s: &SessionSummary) -> String {
        match self {
            // Display may drop a redundant agent prefix, but the picker
            // always hands back `s.id` itself — the deterministic ID stays
            // the session's identity.
            Self::Id => {
                s.id.strip_prefix("claude-")
                    .or_else(|| s.id.strip_prefix("opencode-"))
                    .or_else(|| s.id.strip_prefix("cursor-"))
                    .unwrap_or(&s.id)
                    .to_owned()
            }
            Self::Project => s.cwd.as_deref().map_or_else(|| "—".into(), project_name),
            Self::Preview => s.preview.clone().unwrap_or_else(|| "—".into()),
            Self::Tag => s.tag_name.clone().unwrap_or_else(|| "—".into()),
            Self::Kind => s.kind.to_string(),
            Self::LastActive => Local
                .timestamp_millis_opt(crate::util::normalize_display_ms(s.last_activity_at))
                .single()
                .map_or_else(
                    || "??-?? ??:??".into(),
                    |dt| dt.format("%m-%d %H:%M").to_string(),
                ),
            Self::Agent => s.agent.as_deref().unwrap_or(&s.hostname).to_owned(),
            Self::Model => compact_model(s),
            Self::Tokens => format_tokens(s),
            Self::Commands => s.cmd_count.to_string(),
            // Deliberately not derived from success_count: every command can
            // have succeeded while records are still missing.
            Self::Capture => s.capture.as_ref().map_or_else(
                || "—".into(),
                |capture| {
                    if capture.complete {
                        "full".into()
                    } else {
                        format!("gaps: {}", capture.known_missing.len())
                    }
                },
            ),
            Self::Duration => {
                if s.last_activity_at > s.first_activity_at {
                    format_duration_ms(s.last_activity_at - s.first_activity_at)
                } else {
                    "—".into()
                }
            }
        }
    }
}

/// Last path component, so a row reads `suvadu` rather than a long home path.
fn project_name(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".into();
    }
    trimmed
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed)
        .to_owned()
}

/// Which columns a terminal this wide shows. Project, preview, time and
/// agent are what actually tell two rows apart, so they survive every width;
/// token counts and durations are the first to go.
const fn session_columns(width: u16) -> &'static [SessionColumn] {
    use SessionColumn::{
        Agent, Capture, Commands, Duration, Id, Kind, LastActive, Model, Preview, Project, Tag,
        Tokens,
    };
    const NARROW: &[SessionColumn] = &[Id, Project, Preview, LastActive, Agent];
    const MEDIUM: &[SessionColumn] = &[
        Id, Project, Preview, Kind, LastActive, Agent, Capture, Commands,
    ];
    const WIDE: &[SessionColumn] = &[
        Id, Project, Preview, Tag, Kind, LastActive, Agent, Model, Tokens, Commands, Capture,
        Duration,
    ];
    if width < 100 {
        NARROW
    } else if width < 140 {
        MEDIUM
    } else {
        WIDE
    }
}

/// Header/value pairs for one row at this width — the single place row
/// content is decided, shared by rendering and tests.
fn session_row_values(s: &SessionSummary, width: u16) -> Vec<(&'static str, String)> {
    session_columns(width)
        .iter()
        .map(|column| (column.header(), column.value(s)))
        .collect()
}

// ── Rendering ───────────────────────────────────────────────

impl PickerApp {
    fn build_session_row<'a>(s: &SessionSummary, t: &crate::theme::Theme, width: u16) -> Row<'a> {
        Row::new(
            session_columns(width)
                .iter()
                .zip(session_row_values(s, width))
                .map(|(column, (_, value))| Cell::from(value).style(column.style(t))),
        )
    }

    fn render_picker(&mut self, f: &mut ratatui::Frame) {
        let t = theme();
        let size = f.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header
                Constraint::Length(3), // search bar
                Constraint::Min(5),    // table
                Constraint::Length(1), // footer
            ])
            .split(size);

        Self::render_header(f, chunks[0], t);
        let filter_count = self.active_filter_count();
        self.render_search_bar(f, chunks[1], t, filter_count);
        self.render_session_table(f, chunks[2], t, filter_count);
        self.render_footer(f, chunks[3], t, filter_count);

        // Filter popup overlay
        if self.filter.popup_open {
            self.render_filter_popup(f, size);
        }
    }

    fn render_header(f: &mut ratatui::Frame, area: Rect, t: &crate::theme::Theme) {
        let header_line = Line::from(vec![Span::styled(
            "SUVADU SESSIONS",
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        )]);
        f.render_widget(
            Paragraph::new(header_line).alignment(Alignment::Center),
            area,
        );
    }

    fn render_search_bar(
        &self,
        f: &mut ratatui::Frame,
        area: Rect,
        t: &crate::theme::Theme,
        filter_count: usize,
    ) {
        let filter_badge = if filter_count > 0 {
            format!(
                " [{filter_count} filter{}]",
                if filter_count > 1 { "s" } else { "" }
            )
        } else {
            String::new()
        };

        let in_popup = self.filter.popup_open;
        let search_border = if in_popup { t.border } else { t.border_focus };
        let search_title = if in_popup {
            "Search"
        } else {
            "Search (Typing)"
        };
        let query_display = format!("{}{filter_badge}", self.filter.search);
        let search_bar = Paragraph::new(query_display)
            .style(Style::default().fg(t.text))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(search_border))
                    .title(search_title),
            );
        f.render_widget(search_bar, area);
    }

    fn render_session_table(
        &mut self,
        f: &mut ratatui::Frame,
        area: Rect,
        t: &crate::theme::Theme,
        filter_count: usize,
    ) {
        let showing = self.visible.len();
        let total = self.sessions.len();
        let (start, end) = self.page_bounds();
        let range = if showing == 0 {
            "0".into()
        } else {
            format!("{}–{end}", start + 1)
        };
        let page = format!("Page {}/{}", self.page, self.total_pages());
        let title = if self.filter.search.is_empty() && filter_count == 0 {
            format!(" Sessions ({range} of {total})  •  {page} ")
        } else {
            format!(" Sessions ({range} of {showing} matches, {total} total)  •  {page} ")
        };

        let columns = session_columns(area.width);
        let table_header = Row::new(columns.iter().map(|column| Cell::from(column.header())))
            .style(
                Style::default()
                    .fg(t.text_secondary)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1);

        let rows: Vec<Row> = self
            .visible
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .map(|&idx| Self::build_session_row(&self.sessions[idx], t, area.width))
            .collect();

        let widths = columns.iter().map(|column| column.constraint());

        let table = Table::new(rows, widths)
            .header(table_header)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border))
                    .title(Span::styled(
                        title,
                        Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
                    )),
            )
            .row_highlight_style(
                Style::default()
                    .bg(t.selection_bg)
                    .fg(t.selection_fg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(" > ");

        f.render_stateful_widget(table, area, &mut self.table_state);

        if self.visible.is_empty() && !self.sessions.is_empty() {
            let hint = Paragraph::new(Span::styled(
                "  No sessions match. Clear search or filters.",
                Style::default().fg(t.text_muted),
            ));
            let hint_area = Rect {
                x: area.x + 2,
                y: area.y + 3,
                width: area.width.saturating_sub(4),
                height: 1,
            };
            f.render_widget(hint, hint_area);
        }
    }

    fn render_footer(
        &self,
        f: &mut ratatui::Frame,
        area: Rect,
        t: &crate::theme::Theme,
        filter_count: usize,
    ) {
        let badge_key = Style::default().bg(t.badge_bg).fg(t.text);
        let badge_label = Style::default().fg(t.text_secondary);

        let mut footer_spans = vec![
            Span::styled(" Esc ", badge_key),
            Span::styled(" Quit  ", badge_label),
            Span::styled(" \u{2191}\u{2193} ", badge_key),
            Span::styled(" Navigate  ", badge_label),
            Span::styled(" Enter ", badge_key),
            Span::styled(" Open  ", badge_label),
            Span::styled(" ←→ ", badge_key),
            Span::styled(
                format!(" Page {}/{}  ", self.page, self.total_pages()),
                badge_label,
            ),
            Span::styled(" ^F ", badge_key),
            Span::styled(" Filter  ", badge_label),
            Span::styled(" ^T ", badge_key),
            Span::styled(
                format!(
                    " Type:{}  ",
                    match self.filter.kind {
                        KindFilter::All => "All",
                        KindFilter::Human => "Human",
                        KindFilter::Ai => "AI",
                    }
                ),
                badge_label,
            ),
        ];
        if filter_count > 0 {
            footer_spans.push(Span::styled(" ^X ", badge_key));
            footer_spans.push(Span::styled(" Clear ", badge_label));
        }

        f.render_widget(Paragraph::new(Line::from(footer_spans)), area);
    }

    fn render_filter_popup(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let block = Block::default()
            .title(" Filters ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.primary).add_modifier(Modifier::BOLD))
            .style(Style::default().bg(t.bg_elevated));

        let popup_height = 16u16.min(area.height.saturating_sub(2));
        let popup_width = (area.width * 50 / 100).max(30).min(area.width);
        let popup_area = Rect {
            x: area.x + (area.width.saturating_sub(popup_width)) / 2,
            y: area.y + (area.height.saturating_sub(popup_height)) / 2,
            width: popup_width,
            height: popup_height,
        };
        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints([
                Constraint::Length(1), // Progress
                Constraint::Length(3), // Tag
                Constraint::Length(3), // Start Date
                Constraint::Length(3), // End Date
                Constraint::Min(0),    // Help
            ])
            .split(popup_area);

        // Progress
        self.render_filter_progress(f, chunks[0]);

        // Fields
        let fields: [(&str, &str, &str); NUM_FILTER_FIELDS] = [
            ("Tag Name", &self.filter.tag_input, "e.g. work, personal"),
            (
                "Start Date (After)",
                &self.filter.start_date_input,
                "e.g. today, yesterday, 2024-01-15",
            ),
            (
                "End Date (Before)",
                &self.filter.end_date_input,
                "e.g. today, yesterday, 2024-12-31",
            ),
        ];

        for (i, (title, value, hint)) in fields.iter().enumerate() {
            let is_focused = self.filter.focus_index == i;
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

        // Help
        let help_text = Paragraph::new("Tab/S-Tab: switch fields  |  Enter: apply  |  Esc: cancel")
            .alignment(Alignment::Center)
            .style(Style::default().fg(t.text_muted));
        f.render_widget(help_text, chunks[4]);
    }

    fn render_filter_progress(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let focus = self.filter.focus_index;
        let mut spans: Vec<Span> = (0..NUM_FILTER_FIELDS)
            .map(|i| {
                if i == focus {
                    Span::styled(" ■ ", Style::default().fg(t.primary))
                } else {
                    Span::styled(" □ ", Style::default().fg(t.text_muted))
                }
            })
            .collect();
        let names = ["Tag", "Start Date", "End Date"];
        spans.push(Span::styled(
            format!(
                "  Field {} of {}: {}",
                focus + 1,
                NUM_FILTER_FIELDS,
                names[focus]
            ),
            Style::default().fg(t.text_secondary),
        ));
        f.render_widget(
            Paragraph::new(Line::from(spans)).alignment(Alignment::Center),
            area,
        );
    }
}

// ── Public entry point ──────────────────────────────────────

impl PickerApp {
    fn handle_filter_popup_key(&mut self, key: crossterm::event::KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter.popup_open = false;
                // Discard pending edits -- restore from applied values
                self.filter.tag_input = self.filter.tag_query.clone();
                self.filter.start_date_input = self
                    .filter
                    .after_ms
                    .map_or_else(String::new, |_| self.filter.start_date_input.clone());
                self.filter.end_date_input = self
                    .filter
                    .before_ms
                    .map_or_else(String::new, |_| self.filter.end_date_input.clone());
            }
            KeyCode::Tab => {
                self.filter.focus_index = (self.filter.focus_index + 1) % NUM_FILTER_FIELDS;
            }
            KeyCode::BackTab => {
                self.filter.focus_index = if self.filter.focus_index == 0 {
                    NUM_FILTER_FIELDS - 1
                } else {
                    self.filter.focus_index - 1
                };
            }
            KeyCode::Enter => {
                // Apply filters
                self.filter.tag_query = self.filter.tag_input.trim().to_lowercase();
                self.filter.after_ms = if self.filter.start_date_input.is_empty() {
                    None
                } else {
                    util::parse_date_input(&self.filter.start_date_input, false)
                };
                self.filter.before_ms = if self.filter.end_date_input.is_empty() {
                    None
                } else {
                    util::parse_date_input(&self.filter.end_date_input, true)
                };
                self.filter.popup_open = false;
                self.rebuild_visible();
            }
            KeyCode::Backspace => match self.filter.focus_index {
                0 => {
                    self.filter.tag_input.pop();
                }
                1 => {
                    self.filter.start_date_input.pop();
                }
                2 => {
                    self.filter.end_date_input.pop();
                }
                _ => {}
            },
            KeyCode::Char(c) => match self.filter.focus_index {
                0 => self.filter.tag_input.push(c),
                1 => self.filter.start_date_input.push(c),
                2 => self.filter.end_date_input.push(c),
                _ => {}
            },
            _ => {}
        }
    }

    /// Handle a key event in normal (non-popup) mode. The search box is
    /// always-on, like `suv search`: any plain (non-Ctrl) key is query
    /// text, never a shortcut, so Ctrl-modified keys are checked first and
    /// Esc quits outright rather than clearing the query first.
    fn handle_normal_key(&mut self, key: crossterm::event::KeyEvent) -> PickerAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('f') => {
                    self.filter.popup_open = true;
                    self.filter.focus_index = 0;
                }
                KeyCode::Char('x') => self.clear_filters(),
                KeyCode::Char('t') => self.cycle_kind_filter(),
                _ => {}
            }
            return PickerAction::Continue;
        }
        match key.code {
            KeyCode::Esc => return PickerAction::Exit(None),
            KeyCode::Enter => {
                return PickerAction::Exit(self.selected_session_id().map(String::from));
            }
            KeyCode::Down => self.next(),
            KeyCode::Up => self.prev(),
            KeyCode::Right | KeyCode::PageDown => self.next_page(),
            KeyCode::Left | KeyCode::PageUp => self.prev_page(),
            KeyCode::Backspace => {
                self.filter.search.pop();
                self.rebuild_visible();
            }
            KeyCode::Char(c) => {
                self.filter.search.push(c);
                self.rebuild_visible();
            }
            _ => {}
        }
        PickerAction::Continue
    }
}

pub fn run_session_picker<B: Backend>(
    terminal: &mut Terminal<B>,
    sessions: Vec<SessionSummary>,
) -> io::Result<Option<String>>
where
    io::Error: From<B::Error>,
{
    let mut app = PickerApp::new(sessions);

    loop {
        terminal.draw(|f| app.render_picker(f))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if app.filter.popup_open {
                app.handle_filter_popup_key(key);
            } else if let PickerAction::Exit(result) = app.handle_normal_key(key) {
                return Ok(result);
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_summary(id: &str, cmd_count: i64) -> SessionSummary {
        SessionSummary {
            id: id.to_string(),
            kind: crate::models::SessionKind::Human,
            hostname: "test-host".to_string(),
            cwd: Some("/tmp".into()),
            agent: None,
            model: None,
            models: Vec::new(),
            total_tokens: None,
            usage_complete: false,
            event_count: 0,
            created_at: 1_700_000_000_000,
            tag_name: None,
            cmd_count,
            success_count: cmd_count,
            first_activity_at: 1_700_000_000_000,
            last_activity_at: 1_700_000_060_000,
            preview: None,
            capture: None,
            revision: None,
        }
    }

    fn make_summary_tagged(id: &str, tag: &str) -> SessionSummary {
        SessionSummary {
            tag_name: Some(tag.to_string()),
            ..make_summary(id, 5)
        }
    }

    /// A row has to answer "which project, which agent, when, and what was
    /// it about" before an ID is useful, so those four never fall out of the
    /// column set — not even on a narrow terminal.
    #[test]
    fn every_terminal_width_keeps_project_agent_time_and_preview_columns() {
        for width in [60_u16, 80, 100, 130, 200] {
            let headers = session_columns(width)
                .iter()
                .map(|column| column.header())
                .collect::<Vec<_>>();
            for required in ["Project", "Preview", "Last Active", "Agent"] {
                assert!(
                    headers.iter().any(|header| header.contains(required)),
                    "width {width} dropped {required}: {headers:?}"
                );
            }
        }
    }

    #[test]
    fn session_row_shows_project_and_prompt_preview_not_just_an_id() {
        let summary = SessionSummary {
            cwd: Some("/Users/dev/work/suvadu".into()),
            agent: Some("openai-codex".into()),
            preview: Some("Fix the flaky parser test".into()),
            ..make_summary("codex-abc", 3)
        };
        let values = session_row_values(&summary, 200);
        assert_eq!(cell(&values, "Project"), "suvadu");
        assert_eq!(cell(&values, "Preview"), "Fix the flaky parser test");
        assert_eq!(cell(&values, "Agent / Host"), "openai-codex");
    }

    /// Capture completeness is its own column: a session where every command
    /// succeeded can still be missing records, and the row must not imply
    /// otherwise.
    #[test]
    fn capture_column_reports_gaps_separately_from_command_success() {
        let clean = SessionSummary {
            capture: Some(crate::models::CaptureStatus {
                complete: true,
                known_missing: Vec::new(),
            }),
            ..make_summary("codex-clean", 3)
        };
        let gapped = SessionSummary {
            success_count: 3,
            capture: Some(crate::models::CaptureStatus {
                complete: false,
                known_missing: vec!["Records from a paused window".into()],
            }),
            ..make_summary("codex-gapped", 3)
        };
        assert_eq!(cell(&session_row_values(&clean, 200), "Capture"), "full");
        assert_eq!(
            cell(&session_row_values(&gapped, 200), "Capture"),
            "gaps: 1"
        );
    }

    /// The row may shorten the ID for display, but the value the picker hands
    /// back has to stay the exact deterministic session ID.
    #[test]
    fn selection_returns_the_full_deterministic_id() {
        let app = PickerApp::new(vec![make_summary("claude-9f3c1d2e", 1)]);
        assert_eq!(app.selected_session_id(), Some("claude-9f3c1d2e"));
    }

    fn cell(values: &[(&'static str, String)], header: &str) -> String {
        values
            .iter()
            .find(|(name, _)| *name == header)
            .unwrap_or_else(|| panic!("no {header} column in {values:?}"))
            .1
            .clone()
    }

    #[test]
    fn new_empty_sessions_no_selection() {
        let app = PickerApp::new(vec![]);
        assert!(app.table_state.selected().is_none());
    }

    #[test]
    fn new_with_sessions_selects_first() {
        let app = PickerApp::new(vec![make_summary("s1", 5)]);
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn next_stops_at_last_session() {
        let mut app = PickerApp::new(vec![make_summary("s1", 5), make_summary("s2", 3)]);
        assert_eq!(app.table_state.selected(), Some(0));
        app.next();
        assert_eq!(app.table_state.selected(), Some(1));
        app.next();
        assert_eq!(app.table_state.selected(), Some(1));
    }

    #[test]
    fn prev_stops_at_first_session() {
        let mut app = PickerApp::new(vec![make_summary("s1", 5), make_summary("s2", 3)]);
        assert_eq!(app.table_state.selected(), Some(0));
        app.prev();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn row_navigation_crosses_session_picker_page_boundaries() {
        let sessions = (0..51)
            .map(|index| make_summary(&format!("s{index:02}"), 1))
            .collect();
        let mut app = PickerApp::new(sessions);
        app.table_state.select(Some(49));

        app.next();
        assert_eq!(app.page, 2);
        assert_eq!(app.selected_session_id(), Some("s50"));

        app.prev();
        assert_eq!(app.page, 1);
        assert_eq!(app.selected_session_id(), Some("s49"));
    }

    #[test]
    fn next_on_empty_does_nothing() {
        let mut app = PickerApp::new(vec![]);
        app.next();
        assert!(app.table_state.selected().is_none());
    }

    #[test]
    fn prev_on_empty_does_nothing() {
        let mut app = PickerApp::new(vec![]);
        app.prev();
        assert!(app.table_state.selected().is_none());
    }

    #[test]
    fn next_single_element_stays() {
        let mut app = PickerApp::new(vec![make_summary("s1", 5)]);
        app.next();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn right_and_left_arrows_move_between_fifty_session_pages() {
        let sessions = (0..51)
            .map(|index| make_summary(&format!("s{index:02}"), 1))
            .collect();
        let mut app = PickerApp::new(sessions);

        app.handle_normal_key(crossterm::event::KeyEvent::from(KeyCode::Right));
        assert_eq!(app.selected_session_id(), Some("s50"));

        app.handle_normal_key(crossterm::event::KeyEvent::from(KeyCode::Left));
        assert_eq!(app.selected_session_id(), Some("s00"));
    }

    #[test]
    fn prev_single_element_stays() {
        let mut app = PickerApp::new(vec![make_summary("s1", 5)]);
        app.prev();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    // ── Live search tests ───────────────────────────────────

    #[test]
    fn search_filters_by_session_id() {
        let mut app = PickerApp::new(vec![
            make_summary("claude-abc123", 5),
            make_summary("opencode-xyz", 3),
            make_summary("claude-abc999", 2),
        ]);
        app.filter.search = "abc".to_string();
        app.rebuild_visible();
        assert_eq!(app.visible.len(), 2);
    }

    #[test]
    fn search_filters_by_tag() {
        let mut app = PickerApp::new(vec![
            make_summary_tagged("s1", "work"),
            make_summary("s2", 3),
        ]);
        app.filter.search = "work".to_string();
        app.rebuild_visible();
        assert_eq!(app.visible.len(), 1);
    }

    #[test]
    fn search_matches_agent_model_host_and_working_directory() {
        let mut ai = make_summary("codex-session", 1);
        ai.kind = crate::models::SessionKind::Ai;
        ai.agent = Some("openai-codex".into());
        ai.model = Some("gpt-5.6-sol".into());
        ai.models = vec!["gpt-5.6-sol".into()];
        ai.hostname = "devbox".into();
        ai.cwd = Some("/work/suvadu".into());
        for query in ["openai", "5.6", "devbox", "suvadu"] {
            let mut app = PickerApp::new(vec![ai.clone(), make_summary("human", 2)]);
            app.filter.search = query.into();
            app.rebuild_visible();
            assert_eq!(app.visible, vec![0], "query: {query}");
        }
    }

    #[test]
    fn kind_filter_cycles_all_human_ai() {
        let human = make_summary("human", 2);
        let mut ai = make_summary("ai", 0);
        ai.kind = crate::models::SessionKind::Ai;
        let mut app = PickerApp::new(vec![human, ai]);

        app.cycle_kind_filter();
        assert_eq!(app.visible, vec![0]);
        app.cycle_kind_filter();
        assert_eq!(app.visible, vec![1]);
        app.cycle_kind_filter();
        assert_eq!(app.visible, vec![0, 1]);
    }

    #[test]
    fn compact_ai_metadata_uses_latest_model_and_reported_tokens() {
        let mut ai = make_summary("ai", 0);
        ai.models = vec!["model-a".into(), "model-b".into()];
        ai.model = Some("model-b".into());
        ai.total_tokens = Some(44_291);
        ai.usage_complete = true;

        assert_eq!(compact_model(&ai), "model-b +1");
        assert_eq!(format_tokens(&ai), "44.3k");
    }

    #[test]
    fn search_empty_shows_all() {
        let mut app = PickerApp::new(vec![make_summary("s1", 5), make_summary("s2", 3)]);
        app.filter.search.clear();
        app.rebuild_visible();
        assert_eq!(app.visible.len(), 2);
    }

    // ── Filter popup tests ──────────────────────────────────

    #[test]
    fn filter_tag_narrows_results() {
        let mut app = PickerApp::new(vec![
            make_summary_tagged("s1", "work"),
            make_summary("s2", 3),
            make_summary_tagged("s3", "personal"),
        ]);
        app.filter.tag_query = "work".to_string();
        app.rebuild_visible();
        assert_eq!(app.visible.len(), 1);
        assert_eq!(app.sessions[app.visible[0]].id, "s1");
    }

    #[test]
    fn filter_after_date_narrows_results() {
        let mut app = PickerApp::new(vec![
            make_summary("s1", 5), // last_cmd_at = 1_700_000_060_000
            make_summary("s2", 3),
        ]);
        // After timestamp beyond all sessions
        app.filter.after_ms = Some(1_800_000_000_000);
        app.rebuild_visible();
        assert!(app.visible.is_empty());

        // After before sessions — shows all
        app.filter.after_ms = Some(1_600_000_000_000);
        app.rebuild_visible();
        assert_eq!(app.visible.len(), 2);
    }

    #[test]
    fn filter_before_date_narrows_results() {
        let mut app = PickerApp::new(vec![
            make_summary("s1", 5), // first_cmd_at = 1_700_000_000_000
            make_summary("s2", 3),
        ]);
        // Before timestamp before all sessions
        app.filter.before_ms = Some(1_600_000_000_000);
        app.rebuild_visible();
        assert!(app.visible.is_empty());

        // Before after sessions — shows all
        app.filter.before_ms = Some(1_800_000_000_000);
        app.rebuild_visible();
        assert_eq!(app.visible.len(), 2);
    }

    #[test]
    fn filter_date_range_overlap() {
        let mut app = PickerApp::new(vec![
            make_summary("s1", 5), // first=1_700_000_000_000, last=1_700_000_060_000
        ]);
        // Range that overlaps with session
        app.filter.after_ms = Some(1_700_000_030_000);
        app.filter.before_ms = Some(1_700_000_090_000);
        app.rebuild_visible();
        assert_eq!(app.visible.len(), 1); // session has cmds in range
    }

    #[test]
    fn search_and_filter_combine() {
        let mut app = PickerApp::new(vec![
            make_summary_tagged("claude-abc", "work"),
            make_summary_tagged("claude-xyz", "work"),
            make_summary_tagged("claude-abc", "personal"),
        ]);
        app.filter.search = "abc".to_string();
        app.filter.tag_query = "work".to_string();
        app.rebuild_visible();
        assert_eq!(app.visible.len(), 1);
    }

    #[test]
    fn clear_filters_resets_all() {
        let mut app = PickerApp::new(vec![make_summary("s1", 5)]);
        app.filter.tag_query = "zzz".to_string();
        app.filter.after_ms = Some(9_999_999_999_999);
        app.filter.before_ms = Some(1);
        app.rebuild_visible();
        assert!(app.visible.is_empty());

        app.clear_filters();
        assert_eq!(app.visible.len(), 1);
        assert!(app.filter.tag_query.is_empty());
        assert!(app.filter.after_ms.is_none());
        assert!(app.filter.before_ms.is_none());
    }

    #[test]
    fn active_filter_count_works() {
        let mut app = PickerApp::new(vec![]);
        assert_eq!(app.active_filter_count(), 0);
        app.filter.tag_query = "work".to_string();
        assert_eq!(app.active_filter_count(), 1);
        app.filter.after_ms = Some(123);
        assert_eq!(app.active_filter_count(), 2);
        app.filter.before_ms = Some(456);
        assert_eq!(app.active_filter_count(), 3);
    }

    #[test]
    fn selected_session_id_returns_correct() {
        let app = PickerApp::new(vec![make_summary("s1", 5), make_summary("s2", 3)]);
        assert_eq!(app.selected_session_id(), Some("s1"));
    }

    #[test]
    fn selected_session_id_empty() {
        let app = PickerApp::new(vec![]);
        assert_eq!(app.selected_session_id(), None);
    }
}
