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
}

// ── App ─────────────────────────────────────────────────────

struct PickerApp {
    sessions: Vec<SessionSummary>,
    visible: Vec<usize>,
    table_state: TableState,
    filter: PickerFilter,
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
                        .is_some_and(|t| t.to_lowercase().contains(&search));

                // Tag filter (from popup)
                let tag_ok = tq.is_empty()
                    || s.tag_name
                        .as_deref()
                        .is_some_and(|t| t.to_lowercase().contains(tq));

                // Date range: session has any command in the range
                // (first_cmd_at..=last_cmd_at overlaps with after..=before)
                let after_ok = after.is_none_or(|ms| s.last_cmd_at >= ms);
                let before_ok = before.is_none_or(|ms| s.first_cmd_at <= ms);

                search_ok && tag_ok && after_ok && before_ok
            })
            .map(|(i, _)| i)
            .collect();

        if self.visible.is_empty() {
            self.table_state.select(None);
        } else {
            self.table_state.select(Some(0));
        }
    }

    const fn active_filter_count(&self) -> usize {
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
        n
    }

    fn clear_filters(&mut self) {
        self.filter.tag_input.clear();
        self.filter.start_date_input.clear();
        self.filter.end_date_input.clear();
        self.filter.tag_query.clear();
        self.filter.after_ms = None;
        self.filter.before_ms = None;
        self.rebuild_visible();
    }

    fn next(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        let i = self
            .table_state
            .selected()
            .map_or(0, |i| (i + 1) % self.visible.len());
        self.table_state.select(Some(i));
    }

    fn prev(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        let i = self.table_state.selected().map_or(0, |i| {
            if i > 0 {
                i - 1
            } else {
                self.visible.len() - 1
            }
        });
        self.table_state.select(Some(i));
    }

    fn selected_session_id(&self) -> Option<&str> {
        self.table_state
            .selected()
            .and_then(|i| self.visible.get(i))
            .map(|&idx| self.sessions[idx].id.as_str())
    }
}

// ── Rendering ───────────────────────────────────────────────

impl PickerApp {
    #[allow(clippy::cast_precision_loss)]
    fn build_session_row<'a>(s: &SessionSummary, t: &crate::theme::Theme) -> Row<'a> {
        let fmt_ts = |ms: i64| -> String {
            Local
                .timestamp_millis_opt(crate::util::normalize_display_ms(ms))
                .single()
                .map_or_else(
                    || "??-?? ??:??".into(),
                    |dt| dt.format("%m-%d %H:%M").to_string(),
                )
        };

        let first_cmd = fmt_ts(s.first_cmd_at);
        let last_cmd = fmt_ts(s.last_cmd_at);

        let tag_str = s
            .tag_name
            .as_deref()
            .map_or_else(|| "—".to_string(), std::string::ToString::to_string);

        let rate = if s.cmd_count > 0 {
            s.success_count as f64 / s.cmd_count as f64 * 100.0
        } else {
            0.0
        };
        let rate_style = if rate >= 90.0 {
            Style::default().fg(t.success)
        } else if rate >= 70.0 {
            Style::default().fg(t.warning)
        } else {
            Style::default().fg(t.error)
        };

        let duration = if s.last_cmd_at > s.first_cmd_at {
            format_duration_ms(s.last_cmd_at - s.first_cmd_at)
        } else {
            "—".into()
        };

        let id_display: String =
            s.id.strip_prefix("claude-")
                .or_else(|| s.id.strip_prefix("opencode-"))
                .or_else(|| s.id.strip_prefix("cursor-"))
                .unwrap_or(&s.id)
                .to_string();

        Row::new(vec![
            Cell::from(id_display).style(Style::default().fg(t.info).add_modifier(Modifier::BOLD)),
            Cell::from(tag_str).style(Style::default().fg(t.primary)),
            Cell::from(first_cmd).style(Style::default().fg(t.text_muted)),
            Cell::from(last_cmd).style(Style::default().fg(t.text_muted)),
            Cell::from(format!("{}", s.cmd_count)).style(Style::default().fg(t.text_secondary)),
            Cell::from(format!("{rate:.0}%")).style(rate_style),
            Cell::from(duration).style(Style::default().fg(t.text_muted)),
        ])
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
        Self::render_footer(f, chunks[3], t, filter_count);

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
        let title = if self.filter.search.is_empty() && filter_count == 0 {
            format!(" Sessions ({total}) ")
        } else {
            format!(" Sessions ({showing}/{total}) ")
        };

        let table_header = Row::new(vec![
            Cell::from("Session"),
            Cell::from("Tag"),
            Cell::from("First Cmd"),
            Cell::from("Last Cmd"),
            Cell::from("Cmds"),
            Cell::from("Rate"),
            Cell::from("Duration"),
        ])
        .style(
            Style::default()
                .fg(t.text_secondary)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

        let rows: Vec<Row> = self
            .visible
            .iter()
            .map(|&idx| Self::build_session_row(&self.sessions[idx], t))
            .collect();

        let widths = [
            Constraint::Percentage(40),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Length(5),
            Constraint::Min(7),
        ];

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
            Span::styled(" ^F ", badge_key),
            Span::styled(" Filter  ", badge_label),
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

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_summary(id: &str, cmd_count: i64) -> SessionSummary {
        SessionSummary {
            id: id.to_string(),
            hostname: "test-host".to_string(),
            created_at: 1_700_000_000_000,
            tag_name: None,
            cmd_count,
            success_count: cmd_count,
            first_cmd_at: 1_700_000_000_000,
            last_cmd_at: 1_700_000_060_000,
        }
    }

    fn make_summary_tagged(id: &str, tag: &str) -> SessionSummary {
        SessionSummary {
            tag_name: Some(tag.to_string()),
            ..make_summary(id, 5)
        }
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
    fn next_wraps_around() {
        let mut app = PickerApp::new(vec![make_summary("s1", 5), make_summary("s2", 3)]);
        assert_eq!(app.table_state.selected(), Some(0));
        app.next();
        assert_eq!(app.table_state.selected(), Some(1));
        app.next();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn prev_wraps_around() {
        let mut app = PickerApp::new(vec![make_summary("s1", 5), make_summary("s2", 3)]);
        assert_eq!(app.table_state.selected(), Some(0));
        app.prev();
        assert_eq!(app.table_state.selected(), Some(1));
        app.prev();
        assert_eq!(app.table_state.selected(), Some(0));
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
