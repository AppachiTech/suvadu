use crate::risk;
use crate::theme::theme;
use chrono::{Local, TimeZone};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table,
    },
    Terminal,
};
use std::io;

use super::format::{
    command_prefix, detail_placement, display_width, entry_row_styles, fit_hints, fit_prefix,
    format_executor, format_exit_code, no_results_lines, ColumnLayout, DetailPlacement, Hint,
    NoResults, StatusSegment, DETAIL_BOTTOM_HEIGHT, SELECTION_SYMBOL,
};
use super::highlight::{
    command_text, match_mask, raw_view, relative_age, wants_raw, CommandStyle, Fit, RawStyle,
};
use super::{centered_rect, DialogState, RecallScope, SearchApp};

/// Cells reserved for the `"+N"` marker that stands in for status-row filter
/// badges which do not fit.
const MORE_FILTERS_WIDTH: usize = 5;

/// Width of a detail-pane label column: the longest label (`Last path`,
/// `Last exit`) plus a space.
const DETAIL_LABEL_WIDTH: usize = 10;

/// Push one footer hint (`" ^F "` + `" Filter  "`) onto `spans`.
fn push_hint(spans: &mut Vec<Span<'static>>, hint: Hint, key: Style, label: Style) {
    spans.push(Span::styled(format!(" {} ", hint.key), key));
    spans.push(Span::styled(format!(" {}  ", hint.label), label));
}

impl SearchApp {
    pub(super) fn render(
        &mut self,
        terminal: &mut Terminal<crate::util::TtyCursorBackend<io::Stderr>>,
    ) -> io::Result<()> {
        terminal.draw(|f| self.draw(f))?;

        Ok(())
    }

    /// Backend-independent draw pass. Kept separate from [`Self::render`] so
    /// tests can render the whole screen into a `TestBackend`.
    pub(super) fn draw(&mut self, f: &mut ratatui::Frame) {
        let t = theme();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Branding
                Constraint::Length(3), // Query input
                Constraint::Length(1), // Scope / mode status row
                Constraint::Min(0),    // Results list
                Constraint::Length(2), // Help text
            ])
            .split(f.area());

        // Minimalist Header
        let branding = Line::from(vec![Span::styled(
            "SUVADU SEARCH",
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        )]);
        f.render_widget(
            Paragraph::new(branding).alignment(Alignment::Center),
            chunks[0],
        );

        // Search bar. It holds only what was typed: active filters are
        // listed once in the status row, and grouping is named in the
        // results title, so neither can be mistaken for query text.
        // Any overlay (filter, help, delete, goto, tag, note) takes focus
        // away from the search box, so it renders dimmed while one is up.
        let overlay_focused = !matches!(self.dialog, DialogState::None);
        let vim_normal = self.vim_enabled && self.vim_mode == super::VimMode::Normal;
        let search_border_color = if overlay_focused || vim_normal {
            t.border
        } else {
            t.border_focus
        };
        let search_title = if overlay_focused {
            "Search"
        } else if vim_normal {
            "Search (Normal)"
        } else {
            "Search (Typing)"
        };
        let query = Paragraph::new(self.query.clone())
            .style(Style::default().fg(t.text))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(search_border_color))
                    .title(search_title),
            );
        f.render_widget(query, chunks[1]);

        // Persistent scope / matching-mode status row
        self.render_status_row(f, chunks[2]);

        // Results Table + Optional Detail Pane
        self.render_results_area(f, chunks[3]);

        // Footer
        self.render_footer(f, chunks[4]);

        // Render Overlays
        match self.dialog {
            DialogState::Filter => self.render_filter_popup(f, f.area()),
            DialogState::GoToPage { .. } => self.render_goto_dialog(f, f.area()),
            DialogState::Delete { .. } => self.render_delete_dialog(f, f.area()),
            DialogState::TagAssociation => self.render_tag_dialog(f, f.area()),
            DialogState::Note { .. } => self.render_note_dialog(f, f.area()),
            DialogState::Help => self.render_help_dialog(f, f.area()),
            DialogState::RawView { .. } => self.render_raw_view(f, f.area()),
            DialogState::None => {}
        }
    }

    // --- results area placement (PROD-03) ---

    /// Render the results table plus, if it fits, the detail pane.
    ///
    /// On narrow terminals a 30% side pane squeezes the command column until
    /// results are unreadable, so the pane moves underneath the results (where
    /// it also has the full width for wrapping multiline commands).
    fn render_results_area(&mut self, f: &mut ratatui::Frame, area: Rect) {
        match detail_placement(self.view.detail_pane_open, area.width, area.height) {
            DetailPlacement::Right => {
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Percentage(70), // Results
                        Constraint::Percentage(30), // Detail
                    ])
                    .split(area);
                self.render_results_table(f, chunks[0]);
                self.render_detail_pane(f, chunks[1]);
            }
            DetailPlacement::Bottom => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(5), Constraint::Length(DETAIL_BOTTOM_HEIGHT)])
                    .split(area);
                self.render_results_table(f, chunks[0]);
                self.render_detail_pane(f, chunks[1]);
            }
            DetailPlacement::Hidden => {
                self.detail_scroll.page = 0;
                self.detail_scroll.overflow = false;
                self.set_detail_focus(false);
                self.render_results_table(f, area);
            }
        }
    }

    // --- persistent status row (PROD-03) ---

    /// Scope, matching mode, ranking and agent visibility, as a compact
    /// always-visible row between the search box and the results. Whether
    /// rows are grouped by command is stated by the results title instead
    /// ("Commands 1-50 of 139"), next to the count it changes.
    pub(super) fn render_status_row(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let label_style = Style::default()
            .fg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let value_style = Style::default()
            .bg(t.primary)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD);

        let width = area.width as usize;
        let segments = self.status_segments();
        let badges = self.active_filter_badges();
        // An active filter restricts every result, so it is never the one
        // that does not fit: room for the first badge — and a "+N" for any
        // others — is set aside before the segments are fitted, and the
        // least important segments give way instead.
        let reserved = badges.first().map_or(0, |(text, _)| {
            display_width(text)
                + 1
                + if badges.len() > 1 {
                    MORE_FILTERS_WIDTH
                } else {
                    0
                }
        });
        let shown = fit_prefix(
            &segments,
            width.saturating_sub(reserved),
            StatusSegment::width,
        );

        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut used = 0usize;
        for segment in &segments[..shown] {
            spans.push(Span::styled(format!(" {} ", segment.label), label_style));
            spans.push(Span::styled(format!(" {} ", segment.value), value_style));
            spans.push(Span::raw(" "));
            used += segment.width();
        }

        // Active filters trail the fixed segments; any that do not fit are
        // replaced by a "+N" marker instead of being clipped.
        let mut remaining = width.saturating_sub(used);
        let mut hidden = 0usize;
        let count = badges.len();
        for (i, (text, style)) in badges.into_iter().enumerate() {
            let badge_width = display_width(&text) + 1;
            // Keep room for a "+N" only while there are badges after this one.
            let marker_room = if i + 1 < count { MORE_FILTERS_WIDTH } else { 0 };
            if hidden == 0 && badge_width + marker_room <= remaining {
                spans.push(Span::styled(text, style));
                spans.push(Span::raw(" "));
                remaining -= badge_width;
            } else {
                hidden += 1;
            }
        }
        if hidden > 0 {
            let marker = format!(" +{hidden} ");
            if display_width(&marker) <= remaining {
                spans.push(Span::styled(marker, Style::default().fg(t.text_muted)));
            }
        }

        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// The fixed status segments, in the order they are dropped from the right
    /// when the terminal is too narrow for all of them.
    fn status_segments(&self) -> Vec<StatusSegment> {
        vec![
            // Scope (^P) and Match (^X) come first: they decide which entries
            // are eligible at all. Agents is a restriction too. Rank only
            // reorders what the others chose, so it is dropped first when
            // the terminal is narrow.
            StatusSegment::new("Scope", self.recall.scope.status_value()),
            StatusSegment::new("Match", self.recall.match_mode.label()),
            StatusSegment::new(
                "Agents",
                if self.filters.show_agents {
                    "Shown"
                } else {
                    "Hidden"
                },
            ),
            StatusSegment::new(
                "Rank",
                if self.view.context_boost {
                    "Smart"
                } else {
                    "Recent"
                },
            ),
        ]
    }

    /// Colored badges for the filters that are not covered by a fixed segment.
    fn active_filter_badges(&self) -> Vec<(String, Style)> {
        let t = theme();
        let mut badges = Vec::new();
        if self.filters.after.is_some() || self.filters.before.is_some() {
            badges.push((
                " date ".to_string(),
                Style::default().bg(t.info).fg(Color::Black),
            ));
        }
        if self.filters.tag_id.is_some() {
            badges.push((
                " tag ".to_string(),
                Style::default().bg(t.warning).fg(Color::Black),
            ));
        }
        if self.filters.exit_code.is_some() {
            badges.push((
                " exit ".to_string(),
                Style::default().bg(t.error).fg(Color::White),
            ));
        }
        if self.filters.executor_type.is_some() {
            badges.push((
                " exec ".to_string(),
                Style::default().bg(t.badge_executor).fg(Color::White),
            ));
        }
        if self.filters.failed_only {
            badges.push((
                " failed ".to_string(),
                Style::default().bg(t.error).fg(Color::White),
            ));
        }
        if self.filters.bookmarks_only {
            badges.push((
                " marked ".to_string(),
                Style::default().bg(t.warning).fg(Color::Black),
            ));
        }
        badges
    }

    // --- render_footer (decomposed) ---

    /// Footer hints, laid out to the terminal width.
    ///
    /// Help and the cancel hint are pinned, the accept/navigate/filter/detail
    /// hints come next, and secondary actions fill whatever is left. A hint
    /// that does not fit entirely is dropped — never clipped mid-badge. Every
    /// dropped shortcut is still listed in the help overlay (`?`).
    pub(super) fn render_footer(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let badge_key_style = Style::default().bg(t.badge_bg).fg(t.text);
        let badge_label_style = Style::default().fg(t.text_secondary);

        let mut budget = area.width as usize;

        // 1. Vim mode badge — says which key set is live.
        let mode = self.vim_mode_badge();
        let show_mode = mode
            .as_ref()
            .is_some_and(|(text, _)| display_width(text) + 4 <= budget);
        if let (true, Some((text, _))) = (show_mode, mode.as_ref()) {
            budget -= display_width(text) + 4;
        }

        // 2. Help stays discoverable at every width.
        let help = Hint::new("?", "Help");
        let show_help = help.width() <= budget;
        if show_help {
            budget -= help.width();
        }

        // 3. Cancel / quit.
        let quit = self.quit_hint();
        let show_quit = quit.width() <= budget;
        if show_quit {
            budget -= quit.width();
        }

        // 4. Transient status message (result of the last action).
        let status = self.transient_status_message();
        let show_status = status
            .as_ref()
            .is_some_and(|msg| display_width(msg) + 2 <= budget);
        if let (true, Some(msg)) = (show_status, status.as_ref()) {
            budget -= display_width(msg) + 2;
        }

        // 5. Ordered hints, dropped whole from the tail.
        let hints = self.footer_hints();
        let shown = fit_hints(&hints, budget);
        budget -= hints[..shown].iter().map(Hint::width).sum::<usize>();

        // 6. Page position, only if there is room left over.
        let page_info = self.page_indicator();
        let show_page = display_width(&page_info) + 2 <= budget;

        let mut spans: Vec<Span<'static>> = Vec::new();
        if let (true, Some((text, style))) = (show_mode, mode) {
            spans.push(Span::styled(format!(" {text} "), style));
            spans.push(Span::styled("  ", badge_label_style));
        }
        if show_quit {
            push_hint(&mut spans, quit, badge_key_style, badge_label_style);
        }
        for hint in &hints[..shown] {
            push_hint(&mut spans, *hint, badge_key_style, badge_label_style);
        }
        if show_help {
            push_hint(&mut spans, help, badge_key_style, badge_label_style);
        }
        if show_page {
            spans.push(Span::styled(
                format!(" {page_info} "),
                Style::default().fg(t.text_muted),
            ));
        }
        if let (true, Some(msg)) = (show_status, status) {
            spans.push(Span::styled(
                format!(" {msg} "),
                Style::default().fg(t.success).add_modifier(Modifier::BOLD),
            ));
        }

        let help_paragraph = Paragraph::new(Line::from(spans)).block(
            Block::default()
                .borders(Borders::TOP)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border)),
        );
        f.render_widget(help_paragraph, area);
    }

    /// The vim mode indicator, when vim keys are enabled.
    fn vim_mode_badge(&self) -> Option<(&'static str, Style)> {
        if !self.vim_enabled {
            return None;
        }
        let t = theme();
        let is_normal = self.vim_mode == super::VimMode::Normal;
        Some((
            if is_normal { "NORMAL" } else { "INSERT" },
            Style::default()
                .bg(if is_normal { t.primary } else { t.success })
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ))
    }

    /// The cancel hint: quit, or leave vim insert mode. Opened from Suvadu
    /// Home, leaving returns there, so it says so.
    const fn quit_hint(&self) -> Hint {
        let leave = self.leave_label;
        if self.detail_scroll.focused {
            Hint::new("Esc", "Back")
        } else if self.vim_enabled {
            if matches!(self.vim_mode, super::VimMode::Normal) {
                Hint::new("q", leave)
            } else {
                Hint::new("Esc", "Normal")
            }
        } else {
            Hint::new("Esc", leave)
        }
    }

    /// Footer hints in priority order: the few actions a recall needs.
    /// Labels are fixed (the current state is shown in the status row and
    /// the results title instead) so badges do not jump around as the user
    /// toggles modes. Everything else — copy, bookmark, notes, tags, delete,
    /// goto, agents, failed-only, reset, ranking — is in the help overlay
    /// (`?`), however wide the terminal: width alone is no reason to spread
    /// every capability along the bottom edge.
    ///
    /// Enter is "Use", not "Run": from Ctrl+R the command lands on the
    /// prompt to edit or run, and `suv search` prints it. Nothing executes.
    fn footer_hints(&self) -> Vec<Hint> {
        if self.detail_scroll.focused {
            return vec![
                Hint::new("\u{2191}\u{2193}", "Scroll"),
                Hint::new("PgUp/PgDn", "Page"),
                Hint::new("\u{21e7}Tab", "List"),
            ];
        }
        let mut hints = if self.vim_enabled && self.vim_mode == super::VimMode::Normal {
            vec![
                Hint::new("j/k", "Nav"),
                Hint::new("\u{21b5}", "Use"),
                Hint::new("/", "Search"),
                Hint::new("^F", "Filter"),
                Hint::new("Tab", "Detail"),
                Hint::new("^U/^D", "Scroll"),
                Hint::new("^X", "Mode"),
                Hint::new("^P", "Scope"),
            ]
        } else {
            // Grouping (^U) is offered in the results title, beside the
            // count it changes, where no width can crowd it out.
            vec![
                Hint::new("\u{21b5}", "Use"),
                Hint::new("\u{2191}\u{2193}", "Nav"),
                Hint::new("^F", "Filter"),
                Hint::new("Tab", "Detail"),
                Hint::new("^X", "Mode"),
                Hint::new("^P", "Scope"),
            ]
        };
        // Moving into the pane is offered only while it has more to show,
        // and last, so it never costs ^X/^P their place at narrow widths;
        // the pane's scrollbar already shows there is more.
        if self.detail_scroll.overflow {
            hints.push(Hint::new("\u{21e7}Tab", "Pane"));
        }
        hints
    }

    /// `"2/7 (28%)"` — current page position.
    fn page_indicator(&self) -> String {
        let total_pages = self
            .pagination
            .total_items
            .div_ceil(self.pagination.page_size)
            .max(1);
        let progress_pct = (self.pagination.page * 100)
            .checked_div(total_pages)
            .unwrap_or(0);
        format!("{}/{total_pages} ({progress_pct}%)", self.pagination.page)
    }

    /// The last action's result, while it is still fresh.
    fn transient_status_message(&self) -> Option<String> {
        self.status_message.as_ref().and_then(|(msg, time)| {
            (time.elapsed() < std::time::Duration::from_secs(2)).then(|| msg.clone())
        })
    }

    // --- render_results_table (decomposed) ---

    pub(super) fn render_results_table(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        // Reserve 1 column for scrollbar
        let table_area = Rect {
            width: area.width.saturating_sub(1),
            ..area
        };
        let scrollbar_area = Rect {
            x: area.x + area.width.saturating_sub(1),
            width: 1,
            ..area
        };

        let layout = ColumnLayout::for_view(
            table_area.width,
            self.view.unique_mode,
            self.filters.show_agents,
        );
        let command_col_width = layout.command_col_width(table_area.width);
        let selected = self.table_state.selected();

        let rows: Vec<Row> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                self.build_entry_row(entry, selected == Some(i), &layout, command_col_width)
            })
            .collect();

        let widths = layout.constraints();
        let header_row = layout.header_row();
        let title = self.build_table_title();

        if self.entries.is_empty() {
            if self.searching {
                // An empty result from an earlier query says nothing about
                // this one; do not explain it as if it did.
                let panel = Paragraph::new(Line::from(Span::styled(
                    "  Searching\u{2026}",
                    Style::default().fg(t.text_secondary),
                )))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(t.border))
                        .title(title),
                );
                f.render_widget(panel, area);
            } else {
                self.render_no_results(f, area, &title);
            }
            return;
        }

        let table = Table::new(rows, widths)
            .header(
                header_row
                    .style(
                        Style::default()
                            .fg(t.text_secondary)
                            .add_modifier(Modifier::BOLD),
                    )
                    .bottom_margin(1),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border))
                    .title(title),
            )
            .highlight_symbol(SELECTION_SYMBOL);

        f.render_stateful_widget(table, table_area, &mut self.table_state);

        // Scrollbar
        let total_pages = self
            .pagination
            .total_items
            .div_ceil(self.pagination.page_size)
            .max(1);
        let mut scrollbar_state =
            ScrollbarState::new(total_pages).position(self.pagination.page.saturating_sub(1));
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(Style::default().fg(t.primary_dim))
            .track_style(Style::default().fg(t.border));
        f.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }

    /// The empty state: what was searched, how, where, and which key undoes
    /// each narrowing. Never a silently widened retry.
    fn render_no_results(&self, f: &mut ratatui::Frame, area: Rect, title: &str) {
        let t = theme();
        let scope_detail = match self.recall.scope {
            RecallScope::Directory | RecallScope::Workspace => self.filters.cwd.as_deref(),
            RecallScope::Session => self.recall.context.session_id.as_deref(),
            RecallScope::All => None,
        };
        let state = NoResults {
            query: &self.query,
            mode: self.recall.match_mode,
            scope: self.recall.scope,
            scope_detail,
            agents_hidden: !self.filters.show_agents,
            failed_only: self.filters.failed_only,
            bookmarks_only: self.filters.bookmarks_only,
            other_filters: self.dialog_filter_count(),
        };

        let lines: Vec<Line<'static>> = no_results_lines(&state)
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                let style = if i == 0 {
                    Style::default().fg(t.text).add_modifier(Modifier::BOLD)
                } else if text.starts_with('^') {
                    Style::default().fg(t.primary)
                } else {
                    Style::default().fg(t.text_secondary)
                };
                Line::from(Span::styled(format!("  {text}"), style))
            })
            .collect();

        let panel = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(title.to_string()),
        );
        f.render_widget(panel, area);
    }

    /// Narrowings that come from the filter dialog rather than a scope or a
    /// toggle with its own key.
    const fn dialog_filter_count(&self) -> usize {
        let mut count = 0;
        if self.filters.after.is_some() || self.filters.before.is_some() {
            count += 1;
        }
        if self.filters.tag_id.is_some() {
            count += 1;
        }
        if self.filters.exit_code.is_some() {
            count += 1;
        }
        if self.filters.executor_type.is_some() {
            count += 1;
        }
        count
    }

    fn build_entry_row(
        &self,
        entry: &crate::models::Entry,
        is_selected: bool,
        layout: &ColumnLayout,
        command_col_width: u16,
    ) -> Row<'static> {
        let t = theme();

        let ts_ms = crate::util::normalize_display_ms(entry.started_at);
        let time_str = Local.timestamp_millis_opt(ts_ms).single().map_or_else(
            || "??-?? ??:??".into(),
            |dt| dt.format("%m-%d %H:%M").to_string(),
        );

        let path_display = std::path::Path::new(&entry.cwd)
            .file_name()
            .and_then(|n| n.to_str())
            .map_or_else(|| entry.cwd.clone(), |last| format!("../{last}"));

        // A narrow grouped list has no Runs column, so the count rides in
        // front of the command instead.
        let prefix = command_prefix(
            self,
            entry,
            self.view.unique_mode && matches!(layout, ColumnLayout::Compact),
        );
        let prefix = if prefix.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(prefix, Style::default().fg(t.text_secondary))]
        };
        // Show why the row matched — only when the query searched the
        // command text, not a directory, session or executor.
        let mask = if self.view.search_field == crate::models::SearchField::Command {
            match_mask(&entry.command, &self.query, self.recall.match_mode)
        } else {
            Vec::new()
        };
        // The selected row wraps so every character of it can be read;
        // the others stay one line and say so when they are cut short.
        let command_display = command_text(
            &prefix,
            &entry.command,
            if command_col_width == 0 {
                // No room at all: let the cell clip, as it would anyway.
                Fit::Unlimited
            } else if is_selected {
                Fit::Wrap(command_col_width as usize)
            } else {
                Fit::Truncate(command_col_width as usize)
            },
            &mask,
            &CommandStyle::from_theme(t),
        );
        let height = u16::try_from(command_display.lines.len())
            .unwrap_or(1)
            .max(1);

        let is_local = self.view.context_boost
            && self
                .view
                .current_cwd
                .as_deref()
                .is_some_and(|cwd| entry.cwd == cwd);

        let styles = entry_row_styles(t, is_selected, is_local);
        let (exit_display, exit_style_item) = format_exit_code(entry, styles.bg);

        let cells = match *layout {
            ColumnLayout::Compact => vec![Cell::from(command_display)],
            ColumnLayout::SemiCompact => vec![
                Cell::from(time_str).style(styles.time),
                Cell::from(command_display),
                Cell::from(exit_display).style(exit_style_item),
            ],
            ColumnLayout::Full => vec![
                Cell::from(time_str).style(styles.time),
                Cell::from(command_display),
                Cell::from(path_display).style(styles.path),
                Cell::from(exit_display).style(exit_style_item),
            ],
            ColumnLayout::FullWithAgents => vec![
                Cell::from(time_str).style(styles.time),
                Cell::from(command_display),
                Cell::from(format_executor(entry)).style(styles.executor),
                Cell::from(path_display).style(styles.path),
                Cell::from(exit_display).style(exit_style_item),
            ],
            // The row stands for every matching run of this exact command;
            // its time is the latest of them.
            ColumnLayout::Grouped => vec![
                Cell::from(command_display),
                Cell::from(relative_age(Local::now().timestamp_millis(), ts_ms)).style(styles.time),
                Cell::from(format!("{}\u{d7}", self.unique_count(entry))).style(styles.duration),
            ],
        };
        Row::new(cells).height(height).style(styles.bg)
    }

    /// `"Executions 1-50 of 558 · ^U group"`, or
    /// `"Commands 1-50 of 139 · ^U every run"` when rows are grouped by
    /// command — the count says what it counts, and the key that switches
    /// it sits beside it.
    fn build_table_title(&self) -> String {
        let noun = if self.view.unique_mode {
            "Commands"
        } else {
            "Executions"
        };
        let counts = if self.pagination.total_items == 0 {
            format!("{noun} (none)")
        } else {
            let start_index = (self.pagination.page - 1) * self.pagination.page_size + 1;
            let end_index = start_index + self.entries.len().saturating_sub(1);
            format!(
                "{noun} {start_index}-{end_index} of {}",
                self.pagination.total_items
            )
        };
        // These rows answer an earlier query; say so until the current one
        // arrives rather than pass them off as its results.
        let searching = if self.searching {
            " \u{b7} searching\u{2026}"
        } else {
            ""
        };
        // In vim normal mode ^U scrolls, so it is not offered there.
        let switch = if self.vim_enabled && self.vim_mode == super::VimMode::Normal {
            ""
        } else if self.view.unique_mode {
            " \u{b7} ^U every run"
        } else {
            " \u{b7} ^U group"
        };
        format!("{counts}{searching}{switch}")
    }

    // --- render_detail_pane (decomposed) ---

    pub(super) fn render_detail_pane(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        // Focused the way a `suv stats` panel is: highlighted border and title.
        let focused = self.detail_scroll.focused;
        let block = Block::default()
            .title(Span::styled(
                " Detail ",
                if focused {
                    Style::default().fg(t.primary)
                } else {
                    Style::default()
                },
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if focused { t.border_focus } else { t.border }));
        let inner = block.inner(area);
        self.detail_scroll.page = inner.height.max(1);
        self.detail_scroll.overflow = false;

        if let Some(entry) = self.get_selected_entry() {
            // Details that do not fit give up the pane's last column to a
            // scrollbar outside its border, as the results table does, and
            // wrap again to the narrower inside. Narrower only adds rows, so
            // they still overflow.
            let detail = self.build_detail_lines(entry);
            let wrap = |width: u16| -> Vec<Line<'static>> {
                detail
                    .iter()
                    .flat_map(|line| crate::util::wrap_line(line, width.into(), true))
                    .collect()
            };
            let mut lines = wrap(inner.width);
            let overflow = lines.len() > inner.height as usize;
            let pane = if overflow {
                lines = wrap(inner.width.saturating_sub(1));
                Rect {
                    width: area.width.saturating_sub(1),
                    ..area
                }
            } else {
                area
            };
            let total = u16::try_from(lines.len()).unwrap_or(u16::MAX);
            let max_offset = total.saturating_sub(inner.height);
            let offset = self.detail_scroll.offset.min(max_offset);
            self.detail_scroll.offset = offset;
            self.detail_scroll.overflow = overflow;

            // Where the view is, only when there is more than fits.
            let block = if overflow {
                let last = (offset + inner.height).min(total);
                block.title(
                    Line::from(format!(" {}\u{2013}{last}/{total} ", offset + 1))
                        .right_aligned()
                        .style(Style::default().fg(t.text_muted)),
                )
            } else {
                block
            };
            f.render_widget(Paragraph::new(lines).block(block).scroll((offset, 0)), pane);
            if overflow {
                let mut state = ScrollbarState::new(usize::from(max_offset) + 1)
                    .viewport_content_length(usize::from(inner.height))
                    .position(usize::from(offset));
                let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .thumb_style(Style::default().fg(t.primary_dim))
                    .track_style(Style::default().fg(t.border));
                f.render_stateful_widget(scrollbar, area, &mut state);
            }
        } else {
            let empty = Paragraph::new("No entry selected")
                .block(block)
                .style(Style::default().fg(t.text_muted))
                .alignment(Alignment::Center);
            f.render_widget(empty, area);
        }
    }

    #[allow(clippy::cast_precision_loss)]
    pub(super) fn build_detail_lines(&self, entry: &crate::models::Entry) -> Vec<Line<'static>> {
        let t = theme();
        let label_style = Style::default()
            .fg(t.text_secondary)
            .add_modifier(Modifier::BOLD);
        let value_style = Style::default().fg(t.text);

        let ts_ms = crate::util::normalize_display_ms(entry.started_at);
        let time_str = Local.timestamp_millis_opt(ts_ms).single().map_or_else(
            || "????-??-?? ??:??:??".into(),
            |dt| dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        );

        let duration_secs = entry.duration_ms as f64 / 1000.0;

        let exit_str = match entry.exit_code {
            Some(0) => "✔ 0 (success)".to_string(),
            Some(code) => format!("✘ {code} (failed)"),
            None => "○ (unknown)".to_string(),
        };

        let executor_str = match (&entry.executor_type, &entry.executor) {
            (Some(et), Some(n)) => format!("{et}: {n}"),
            (Some(et), None) => et.clone(),
            _ => "unknown".to_string(),
        };

        let session_str = entry.session_id.clone();
        let tag_str = entry.tag_name.as_deref().unwrap_or("none").to_string();

        // A grouped row stands for many runs; the details below are its
        // latest matching one, and say so rather than imply every run went
        // the same way.
        let grouped = self.view.unique_mode;
        let (path_label, exit_label, time_label) = if grouped {
            ("Last path", "Last exit", "Last run")
        } else {
            ("Path", "Exit", "Time")
        };
        // Labels are padded to one width with at least one space after the
        // longest, so no label ever runs into its value.
        let field = |label: &str, value: String| {
            Line::from(vec![
                Span::styled(format!("{label:<DETAIL_LABEL_WIDTH$}"), label_style),
                Span::styled(value, value_style),
            ])
        };
        let mut lines = vec![
            Line::from(vec![Span::styled("Command  ", label_style)]),
            Line::from(vec![Span::styled(
                entry.command.clone(),
                Style::default().fg(t.primary),
            )]),
        ];
        // Tabs, line breaks and edge spaces cannot be read off the text
        // above; spell them out rather than leave two commands looking alike.
        if wants_raw(&entry.command) {
            let raw = raw_view(&entry.command, 0, &RawStyle::from_theme(t));
            for (i, raw_line) in raw.into_iter().enumerate() {
                let label = if i == 0 { "Raw" } else { "" };
                let mut spans = vec![Span::styled(
                    format!("{label:<DETAIL_LABEL_WIDTH$}"),
                    label_style,
                )];
                spans.extend(raw_line.spans);
                lines.push(Line::from(spans));
            }
        }
        // Where it ran and how it ended lead: below the results the pane
        // shows only a few rows, and those two decide whether to reuse it.
        lines.push(field(path_label, entry.cwd.clone()));
        lines.push(field(exit_label, exit_str));
        lines.push(field(time_label, time_str));
        if grouped {
            lines.push(field(
                "Runs",
                format!("{} matching", self.unique_count(entry)),
            ));
        }
        lines.extend([
            field("Duration", format!("{duration_secs:.2}s")),
            field("Session", session_str),
            field("Tag", tag_str),
            field("Executor", executor_str),
        ]);

        // Agent prompt (if present)
        if let Some(ctx) = &entry.context {
            if let Some(prompt) = ctx.get("agent_prompt") {
                let t = theme();
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled("Prompt", label_style)]));
                lines.push(Line::from(vec![Span::styled(
                    prompt.clone(),
                    Style::default().fg(t.info),
                )]));
            }
        }

        self.append_risk_line(&mut lines, entry, label_style);
        self.append_bookmark_note_lines(&mut lines, entry, value_style);

        lines
    }

    fn append_risk_line(
        &self,
        lines: &mut Vec<Line<'static>>,
        entry: &crate::models::Entry,
        label_style: Style,
    ) {
        if !self.show_risk_in_search {
            return;
        }
        let t = theme();
        let assessment = risk::assess_risk(&entry.command);
        let risk_level = assessment
            .as_ref()
            .map_or(risk::RiskLevel::None, |a| a.level);
        if risk_level > risk::RiskLevel::None {
            let risk_color = match risk_level {
                risk::RiskLevel::Critical => t.risk_critical,
                risk::RiskLevel::High => t.risk_high,
                risk::RiskLevel::Medium => t.risk_medium,
                risk::RiskLevel::Low | risk::RiskLevel::None => t.risk_low,
            };
            let risk_text = format!(
                "{} {}{}",
                risk_level.icon(),
                risk_level.label(),
                assessment
                    .as_ref()
                    .map_or(String::new(), |a| format!(" ({})", a.category))
            );
            lines.push(Line::from(vec![
                Span::styled(format!("{:<DETAIL_LABEL_WIDTH$}", "Risk"), label_style),
                Span::styled(risk_text, Style::default().fg(risk_color)),
            ]));
        }
    }

    fn append_bookmark_note_lines(
        &self,
        lines: &mut Vec<Line<'static>>,
        entry: &crate::models::Entry,
        value_style: Style,
    ) {
        let t = theme();
        let is_bookmarked = self.bookmarked_commands.contains(&entry.command);
        let has_note = entry
            .id
            .is_some_and(|id| self.noted_entry_ids.contains(&id));

        if is_bookmarked || has_note {
            lines.push(Line::from(""));
            if is_bookmarked {
                lines.push(Line::from(vec![
                    Span::styled("★ ", Style::default().fg(t.warning)),
                    Span::styled("Bookmarked", value_style),
                ]));
            }
            if has_note {
                lines.push(Line::from(vec![
                    Span::styled("📝 ", Style::default()),
                    Span::styled("Has note", value_style),
                ]));
            }
        }
    }

    // --- render_filter_popup (decomposed) ---

    pub(super) fn render_filter_popup(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let block = Block::default()
            .title(" Filters ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.primary).add_modifier(Modifier::BOLD))
            .style(Style::default().bg(t.bg_elevated));

        // The filter layout needs: 2 (border) + 2+2 (margin) + 1 (progress)
        // + 5*3 (fields) + 1 (help) = 23 rows ideal.
        // On small terminals, use all available height; on larger ones cap at 50%.
        let popup_height = 23u16.min(area.height.saturating_sub(2));
        let popup_width = (area.width * 60 / 100).max(30).min(area.width);
        let popup_area = Rect {
            x: area.x + (area.width.saturating_sub(popup_width)) / 2,
            y: area.y + (area.height.saturating_sub(popup_height)) / 2,
            width: popup_width,
            height: popup_height,
        };
        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);

        // Available inner height after border (2) + margin (4)
        let inner_height = popup_height.saturating_sub(6);
        // Each filter field is 3 rows; progress is 1; help is 1.
        // With ≤12 inner rows we can't fit all fields — show only the
        // focused field and its neighbors to avoid clipping.
        let show_all = inner_height >= 17; // 1 + 5*3 + 1

        if show_all {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(2)
                .constraints([
                    Constraint::Length(1), // Progress indicator
                    Constraint::Length(3), // Start Date
                    Constraint::Length(3), // End Date
                    Constraint::Length(3), // Tag
                    Constraint::Length(3), // Exit Code
                    Constraint::Length(3), // Executor
                    Constraint::Min(0),    // Help
                ])
                .split(popup_area);

            self.render_filter_progress(f, chunks[0]);
            self.render_filter_fields(f, &chunks);

            let help_text =
                Paragraph::new("Tab/S-Tab: switch fields  |  Enter: apply  |  Esc: cancel")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(t.text_muted));
            f.render_widget(help_text, chunks[6]);
        } else {
            // Compact mode: show progress + only the focused field + help.
            // This fits in as little as 7 inner rows (1 + 3 + 1 + margin).
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(2)
                .constraints([
                    Constraint::Length(1), // Progress indicator
                    Constraint::Length(3), // Focused field
                    Constraint::Min(0),    // Help
                ])
                .split(popup_area);

            self.render_filter_progress(f, chunks[0]);
            self.render_single_filter_field(f, chunks[1]);

            let help_text = Paragraph::new("Tab/S-Tab: switch  |  Enter: apply  |  Esc: cancel")
                .alignment(Alignment::Center)
                .style(Style::default().fg(t.text_muted));
            f.render_widget(help_text, chunks[2]);
        }
    }

    fn render_filter_progress(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let focus = self.filters.focus_index;
        let mut progress_line: Vec<Span> = (0..5)
            .map(|i| {
                if i == focus {
                    Span::styled(" ■ ", Style::default().fg(t.primary))
                } else {
                    Span::styled(" □ ", Style::default().fg(t.text_muted))
                }
            })
            .collect();
        let field_names = ["Start Date", "End Date", "Tag", "Exit Code", "Executor"];
        progress_line.push(Span::styled(
            format!("  Field {} of 5: {}", focus + 1, field_names[focus]),
            Style::default().fg(t.text_secondary),
        ));
        f.render_widget(
            Paragraph::new(Line::from(progress_line)).alignment(Alignment::Center),
            area,
        );
    }

    fn render_filter_fields(&self, f: &mut ratatui::Frame, chunks: &[Rect]) {
        let t = theme();
        // Text input fields (0..=3)
        let text_fields: Vec<(&str, &str, &str)> = vec![
            (
                "Start Date (After)",
                &self.filters.start_date_input,
                "e.g. today, yesterday, 2024-01-15",
            ),
            (
                "End Date (Before)",
                &self.filters.end_date_input,
                "e.g. today, 3 days ago, 2024-12-31",
            ),
            (
                "Tag Name",
                &self.filters.tag_filter_input,
                "e.g. work, personal",
            ),
            (
                "Exit Code",
                &self.filters.exit_code_input,
                "e.g. 0 (success), 1 (failure)",
            ),
        ];

        for (i, (title, value, hint)) in text_fields.iter().enumerate() {
            let is_focused = self.filters.focus_index == i;
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

        // Executor selector (field 4)
        let is_exec_focused = self.filters.focus_index == 4;
        let exec_border = if is_exec_focused {
            t.border_focus
        } else {
            t.border
        };
        let sel = self.filters.executor_sel;
        let display = if sel == 0 {
            "All".to_string()
        } else {
            self.filters
                .executors
                .get(sel - 1)
                .cloned()
                .unwrap_or_else(|| "All".to_string())
        };
        let exec_style = if is_exec_focused {
            Style::default().fg(t.text)
        } else {
            Style::default().fg(t.text_secondary)
        };
        let hint_suffix = if is_exec_focused {
            "  ↑↓ to select"
        } else {
            ""
        };
        let exec_widget = Paragraph::new(format!("  {display}{hint_suffix}"))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(exec_border))
                    .title(format!(
                        "Executor{}",
                        if is_exec_focused { " *" } else { "" }
                    )),
            )
            .style(exec_style);
        f.render_widget(exec_widget, chunks[5]);
    }

    /// Compact mode: render only the currently focused filter field.
    fn render_single_filter_field(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let i = self.filters.focus_index.min(4);

        if i == 4 {
            // Executor selector in compact mode
            let sel = self.filters.executor_sel;
            let display = if sel == 0 {
                "All".to_string()
            } else {
                self.filters
                    .executors
                    .get(sel - 1)
                    .cloned()
                    .unwrap_or_else(|| "All".to_string())
            };
            let widget = Paragraph::new(format!("  {display}  ↑↓ to select"))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(t.border_focus))
                        .title("Executor *"),
                )
                .style(Style::default().fg(t.text));
            f.render_widget(widget, area);
            return;
        }

        let fields: [(&str, &str, &str); 4] = [
            (
                "Start Date (After)",
                &self.filters.start_date_input,
                "e.g. today, yesterday, 2024-01-15",
            ),
            (
                "End Date (Before)",
                &self.filters.end_date_input,
                "e.g. today, 3 days ago, 2024-12-31",
            ),
            (
                "Tag Name",
                &self.filters.tag_filter_input,
                "e.g. work, personal",
            ),
            (
                "Exit Code",
                &self.filters.exit_code_input,
                "e.g. 0 (success), 1 (failure)",
            ),
        ];

        let (title, value, hint) = fields[i];

        let display_text = if value.is_empty() {
            hint.to_string()
        } else {
            value.to_string()
        };
        let text_style = if value.is_empty() {
            Style::default().fg(t.text_muted)
        } else {
            Style::default().fg(t.text)
        };

        let input = Paragraph::new(display_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border_focus))
                    .title(format!("{title} *")),
            )
            .style(text_style);
        f.render_widget(input, area);
    }

    // --- Unchanged dialog functions ---

    pub(super) fn render_tag_dialog(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let block = Block::default()
            .title(" Associate Session with Tag ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.info))
            .style(Style::default().bg(t.bg_elevated));

        let popup_area = centered_rect(50, 40, area);
        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([Constraint::Min(0), Constraint::Length(1)].as_ref())
            .split(popup_area);

        let items: Vec<ListItem> = self
            .tags
            .iter()
            .map(|tag| {
                ListItem::new(format!(
                    " {} : {}",
                    tag.name,
                    tag.description.clone().unwrap_or_default()
                ))
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::NONE))
            .highlight_style(
                Style::default()
                    .bg(t.selection_bg)
                    .fg(t.selection_fg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(" > ");

        f.render_stateful_widget(list, chunks[0], &mut self.tag_list_state);

        let help = Paragraph::new("Enter: Select  |  Esc: Cancel")
            .alignment(Alignment::Center)
            .style(Style::default().fg(t.text_muted));
        f.render_widget(help, chunks[1]);
    }

    #[allow(clippy::unused_self)]
    pub(super) fn render_delete_dialog(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let popup_area = centered_rect(50, 25, area);
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Delete Entry ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.error))
            .style(Style::default().bg(t.error_bg));

        // Show command preview
        let cmd_preview = self
            .get_selected_entry()
            .map(|e| {
                if e.command.chars().count() > 50 {
                    crate::util::truncate_str(&e.command, 50, "...")
                } else {
                    e.command.clone()
                }
            })
            .unwrap_or_default();

        let content = vec![
            Line::from(""),
            Line::from(Span::styled(
                cmd_preview,
                Style::default().fg(t.text).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("Delete this entry?"),
            Line::from(""),
            Line::from(vec![
                Span::styled(" [Y] ", Style::default().bg(t.error).fg(Color::White)),
                Span::raw(" Yes   "),
                Span::styled(" [N] ", Style::default().bg(t.badge_bg).fg(t.text)),
                Span::raw(" No"),
            ]),
        ];

        let confirm_text = Paragraph::new(content)
            .block(block)
            .alignment(Alignment::Center);

        f.render_widget(confirm_text, popup_area);
    }

    pub(super) fn render_goto_dialog(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let total_pages = self
            .pagination
            .total_items
            .div_ceil(self.pagination.page_size)
            .max(1);

        let block = Block::default()
            .title(" Go To Page ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.primary))
            .style(Style::default().bg(t.bg_elevated));

        let popup_area = centered_rect(30, 20, area);
        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);

        let inner_layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([Constraint::Length(3), Constraint::Length(1)].as_ref())
            .split(popup_area);

        let goto_text = if let DialogState::GoToPage { ref input } = self.dialog {
            input.as_str()
        } else {
            ""
        };
        let input = Paragraph::new(goto_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border_focus))
                    .title(format!("Page (1-{total_pages})")),
            )
            .style(Style::default().fg(t.text));

        f.render_widget(input, inner_layout[0]);

        let hint = Paragraph::new("Enter: go  |  Esc: cancel")
            .alignment(Alignment::Center)
            .style(Style::default().fg(t.text_muted));
        f.render_widget(hint, inner_layout[1]);
    }

    pub(super) fn render_note_dialog(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let block = Block::default()
            .title(" Add Note (Enter: save, Esc: cancel, empty: delete) ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.warning))
            .style(Style::default().bg(t.bg_elevated));

        let popup_area = centered_rect(50, 20, area);
        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);

        let inner_layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([Constraint::Length(3), Constraint::Length(1)].as_ref())
            .split(popup_area);

        let note_text = if let DialogState::Note { ref input, .. } = self.dialog {
            input.as_str()
        } else {
            ""
        };
        let input = Paragraph::new(note_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border_focus))
                    .title("Note"),
            )
            .style(Style::default().fg(t.text));

        f.render_widget(input, inner_layout[0]);

        let hint = Paragraph::new("Enter: save  |  Esc: cancel  |  Empty = delete note")
            .alignment(Alignment::Center)
            .style(Style::default().fg(t.text_muted));
        f.render_widget(hint, inner_layout[1]);
    }

    /// The raw view (`^V`): the selected command with every character
    /// visible, in a scrollable overlay so no length hides any of it.
    pub(super) fn render_raw_view(&mut self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let DialogState::RawView {
            ref command,
            ref mut scroll,
        } = self.dialog
        else {
            return;
        };
        let popup = centered_rect(90, 80, area);
        f.render_widget(Clear, popup);
        let block = Block::default()
            .title(" Raw command ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.primary))
            .style(Style::default().bg(t.bg_elevated));
        let inner = block.inner(popup);
        f.render_widget(block, popup);
        let [info, body] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);

        let lines = raw_view(command, body.width as usize, &RawStyle::from_theme(t));
        let max_scroll =
            u16::try_from(lines.len().saturating_sub(body.height as usize)).unwrap_or(u16::MAX);
        *scroll = (*scroll).min(max_scroll);
        let summary = format!(
            "{} characters, {} bytes  \u{b7} = space  \u{2191}\u{2193} PgUp/PgDn scroll  Esc close{}",
            command.chars().count(),
            command.len(),
            if max_scroll > 0 {
                format!("  (line {} of {})", *scroll + 1, lines.len())
            } else {
                String::new()
            }
        );
        f.render_widget(
            Paragraph::new(Span::styled(summary, Style::default().fg(t.text_muted))),
            info,
        );
        f.render_widget(Paragraph::new(lines).scroll((*scroll, 0)), body);
    }

    pub(super) fn render_help_dialog(&self, f: &mut ratatui::Frame, area: Rect) {
        // Use self to stay consistent with other dialog render methods.
        let _ = &self.dialog;

        let t = theme();
        let popup_area = centered_rect(90, 85, area);
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Keyboard Shortcuts ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.primary))
            .style(Style::default().bg(t.bg_elevated));

        // Two columns so the full shortcut list — including everything the
        // footer had to drop — fits an 80x24 terminal without scrolling.
        let inner = block.inner(popup_area);
        f.render_widget(block, popup_area);

        let (left, right) = build_help_columns(t, self.vim_enabled);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(inner);
        f.render_widget(Paragraph::new(left), columns[0]);
        f.render_widget(Paragraph::new(right), columns[1]);
    }
}

fn help_section(title: &'static str, t: &crate::theme::Theme) -> Line<'static> {
    Line::from(Span::styled(
        title,
        Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
    ))
}

fn help_row(key: &'static str, desc: &'static str, t: &crate::theme::Theme) -> Line<'static> {
    let key_style = Style::default().fg(t.text).add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(t.text_secondary);
    Line::from(vec![
        Span::styled(key, key_style),
        Span::styled(desc, desc_style),
    ])
}

/// The full shortcut reference, split into two columns.
///
/// Every shortcut lives here, including the advanced ones the footer drops on
/// narrow terminals, so `?` is always the complete answer.
fn build_help_columns(
    t: &crate::theme::Theme,
    vim_enabled: bool,
) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let mut left = vec![
        Line::from(""),
        help_section("\u{2500}\u{2500} Navigation \u{2500}\u{2500}", t),
    ];
    if vim_enabled {
        left.extend([
            help_row("  j/k \u{2191}/\u{2193}   ", "Move selection", t),
            help_row("  h/l \u{2190}/\u{2192}   ", "Prev/next page", t),
            help_row("  g/G       ", "First/last entry", t),
            help_row("  ^U/^D     ", "Half-page scroll", t),
        ]);
    } else {
        left.extend([
            help_row("  \u{2191}/\u{2193}       ", "Move selection", t),
            help_row("  \u{2190}/\u{2192}       ", "Prev/next page", t),
            help_row("  Home/End  ", "First/last entry", t),
        ]);
    }
    left.extend([
        help_row("  PgUp/PgDn ", "Page up/down", t),
        help_row("  ^G        ", "Go to page...", t),
        Line::from(""),
        help_section("\u{2500}\u{2500} Actions \u{2500}\u{2500}", t),
        help_row("  Enter     ", "Use command (to prompt)", t),
        help_row("  ^Y        ", "Copy to clipboard", t),
        help_row("  ^B        ", "Toggle bookmark", t),
        help_row("  ^N        ", "Add/edit note", t),
        help_row("  ^D        ", "Delete entry", t),
        help_row("  ^T        ", "Tag session", t),
        help_row("  ^V        ", "Inspect raw command", t),
    ]);
    // `?/F1` moves here so the right column has room for the recall
    // controls; both columns must still fit 18 rows at 80x24.
    if !vim_enabled {
        left.push(Line::from(""));
    }
    left.push(help_row("  ?/F1      ", "This help", t));

    // Matching mode and scope lead: they decide what is eligible. Ranking
    // and display only reorder what they chose.
    let mut right = vec![
        Line::from(""),
        help_section("\u{2500}\u{2500} Recall \u{2500}\u{2500}", t),
        help_row("  ^X        ", "Matching mode", t),
        help_row("  ^P        ", "Scope: all/dir/repo", t),
        help_row("  ^R        ", "Reset to all history", t),
        help_row("  ^L        ", "Scope: this dir", t),
        help_row("  ^A        ", "AI-agent commands", t),
        help_row("  ^F        ", "Filter dialog", t),
        help_row("  ^E        ", "Failed only", t),
        help_row("  ^O        ", "Bookmarked only", t),
    ];
    if !vim_enabled {
        right.push(Line::from(""));
    }
    right.extend([
        help_section("\u{2500}\u{2500} Display \u{2500}\u{2500}", t),
        help_row("  ^U        ", "Group by command", t),
        help_row("  ^S        ", "Rank smart/recent", t),
        help_row("  Tab       ", "Detail (\u{21e7}Tab: scroll)", t),
    ]);
    if vim_enabled {
        right.extend([
            help_row("  i or /    ", "Insert (type query)", t),
            help_row("  Esc       ", "Normal mode", t),
            help_row("  q         ", "Quit", t),
        ]);
    } else {
        right.push(help_row("  Esc       ", "Exit", t));
        right.push(Line::from(""));
    }
    right.push(Line::from(Span::styled(
        "Press any key to close",
        Style::default().fg(t.text_muted),
    )));

    (left, right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Entry;
    use ratatui::style::Style;

    fn make_entry(
        executor_type: Option<&str>,
        executor: Option<&str>,
        exit_code: Option<i32>,
    ) -> Entry {
        Entry {
            id: None,
            session_id: "s1".to_string(),
            command: "test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code,
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: executor_type.map(String::from),
            executor: executor.map(String::from),
        }
    }

    #[test]
    fn test_format_executor_human() {
        let e = make_entry(Some("human"), Some("zsh"), None);
        let result = format_executor(&e);
        assert!(result.contains("zsh"));
    }

    #[test]
    fn test_format_executor_agent() {
        let e = make_entry(Some("agent"), Some("claude-code"), None);
        let result = format_executor(&e);
        assert!(result.contains("claude-code"));
    }

    #[test]
    fn test_format_executor_none() {
        let e = make_entry(None, None, None);
        let result = format_executor(&e);
        assert!(!result.is_empty());
    }

    #[test]
    fn test_format_exit_code_success() {
        let e = make_entry(None, None, Some(0));
        let (display, _style) = format_exit_code(&e, Style::default());
        assert!(display.contains('✔'));
    }

    #[test]
    fn test_format_exit_code_failure() {
        let e = make_entry(None, None, Some(1));
        let (display, _style) = format_exit_code(&e, Style::default());
        assert!(display.contains('✘'));
        assert!(display.contains('1'));
    }

    #[test]
    fn test_format_exit_code_none() {
        let e = make_entry(None, None, None);
        let (display, _style) = format_exit_code(&e, Style::default());
        assert!(display.contains('○'));
    }

    // --- ColumnLayout tests ---

    #[test]
    fn test_column_layout_compact() {
        let layout = ColumnLayout::from_width(60);
        assert!(matches!(layout, ColumnLayout::Compact));
    }

    #[test]
    fn test_column_layout_semi_compact() {
        let layout = ColumnLayout::from_width(100);
        assert!(matches!(layout, ColumnLayout::SemiCompact));
    }

    #[test]
    fn test_column_layout_full() {
        let layout = ColumnLayout::from_width(150);
        assert!(matches!(layout, ColumnLayout::Full));
    }

    /// The command column's width as a real `Table` draws it: a row whose
    /// command cell holds more `x`s than can fit, counted in the buffer.
    fn drawn_command_width(layout: &ColumnLayout, table_width: u16) -> u16 {
        use ratatui::{buffer::Buffer, widgets::StatefulWidget, widgets::TableState};
        let cells: Vec<Cell> = (0..layout.constraints().len())
            .map(|i| {
                if i == layout.command_column() {
                    Cell::from("x".repeat(400))
                } else {
                    Cell::from("")
                }
            })
            .collect();
        let table = Table::new(vec![Row::new(cells)], layout.constraints())
            .block(Block::default().borders(Borders::ALL))
            .highlight_symbol(SELECTION_SYMBOL);
        let area = Rect::new(0, 0, table_width, 3);
        let mut buf = Buffer::empty(area);
        let mut state = TableState::default().with_selected(Some(0));
        StatefulWidget::render(table, area, &mut buf, &mut state);
        let count = buf.content.iter().filter(|c| c.symbol() == "x").count();
        u16::try_from(count).unwrap()
    }

    #[test]
    fn command_width_is_the_width_the_table_really_draws() {
        let layouts = [
            ColumnLayout::Compact,
            ColumnLayout::SemiCompact,
            ColumnLayout::Full,
            ColumnLayout::FullWithAgents,
            ColumnLayout::Grouped,
        ];
        for layout in &layouts {
            for width in [0_u16, 3, 30, 50, 60, 79, 80, 99, 111, 130, 150, 200] {
                assert_eq!(
                    layout.command_col_width(width),
                    drawn_command_width(layout, width),
                    "{layout:?} at a {width}-cell table"
                );
            }
        }
    }

    #[test]
    fn test_column_layout_for_view() {
        // Grouped rows get their own columns, down to a narrow terminal.
        assert!(matches!(
            ColumnLayout::for_view(60, true, false),
            ColumnLayout::Grouped
        ));
        assert!(matches!(
            ColumnLayout::for_view(49, true, false),
            ColumnLayout::Compact
        ));
        // Who ran a command is a column only when agent commands are shown.
        assert!(matches!(
            ColumnLayout::for_view(150, false, false),
            ColumnLayout::Full
        ));
        assert!(matches!(
            ColumnLayout::for_view(150, false, true),
            ColumnLayout::FullWithAgents
        ));
        assert!(matches!(
            ColumnLayout::for_view(100, false, true),
            ColumnLayout::SemiCompact
        ));
    }

    #[test]
    fn test_column_layout_constraints_compact() {
        let layout = ColumnLayout::Compact;
        let constraints = layout.constraints();
        assert_eq!(constraints.len(), 1);
    }

    #[test]
    fn test_column_layout_constraints_full() {
        let layout = ColumnLayout::Full;
        let constraints = layout.constraints();
        assert_eq!(constraints.len(), 4);
        assert_eq!(ColumnLayout::FullWithAgents.constraints().len(), 5);
        assert_eq!(ColumnLayout::Grouped.constraints().len(), 3);
    }

    #[test]
    fn test_column_layout_header_compact() {
        let layout = ColumnLayout::Compact;
        // header_row() uses Row::new(vec![...]) — just verify it doesn't panic
        // and the constraints count matches (1 column = 1 header cell)
        let _header = layout.header_row();
        assert_eq!(layout.constraints().len(), 1);
    }

    #[test]
    fn test_column_layout_header_full() {
        let layout = ColumnLayout::Full;
        let _header = layout.header_row();
        // Full layout has 4 columns = 4 header cells
        assert_eq!(layout.constraints().len(), 4);
    }

    // --- build_command_text tests ---

    fn make_search_app_for_build_text(
        bookmarked: bool,
        unique: bool,
        noted: bool,
    ) -> (super::SearchApp, Entry) {
        use std::collections::{HashMap, HashSet};

        let entry = Entry {
            id: Some(42),
            session_id: "s1".to_string(),
            command: "cargo test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("terminal".to_string()),
        };

        let mut bookmarked_commands = HashSet::new();
        if bookmarked {
            bookmarked_commands.insert("cargo test".to_string());
        }

        let mut noted_entry_ids = HashSet::new();
        if noted {
            noted_entry_ids.insert(42);
        }

        let mut unique_counts = HashMap::new();
        unique_counts.insert(42, 5);

        let config = super::super::SearchConfig {
            entries: vec![entry.clone()],
            initial_query: None,
            total_items: 1,
            page: 1,
            page_size: 50,
            tags: vec![],
            executors: vec![],
            unique_counts,
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            show_agents: false,
            failed_only: false,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands,
            filter_cwd: None,
            noted_entry_ids,
            show_risk_in_search: false,
            vim_enabled: false,
            view: super::super::ViewOptions {
                unique_mode: unique,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
                length_threshold: 80,
                human_boost_percent: 33,
                cwd_boost_percent: 50,
            },
            recall: super::super::RecallState::default(),
        };

        (super::SearchApp::new(config), entry)
    }

    #[test]
    fn test_command_prefix_plain() {
        let (app, entry) = make_search_app_for_build_text(false, false, false);
        assert_eq!(command_prefix(&app, &entry, false), "");
    }

    #[test]
    fn test_command_prefix_bookmarked() {
        let (app, entry) = make_search_app_for_build_text(true, false, false);
        assert_eq!(command_prefix(&app, &entry, false), "★ ");
    }

    #[test]
    fn test_command_prefix_count_only_when_asked() {
        let (app, entry) = make_search_app_for_build_text(false, true, false);
        // A narrow grouped list carries the run count in front of the
        // command; a wide one has a Runs column instead.
        assert_eq!(command_prefix(&app, &entry, true), "5\u{d7} ");
        assert_eq!(command_prefix(&app, &entry, false), "");
    }

    // --- entry_row_styles tests ---

    #[test]
    fn test_entry_row_styles_selected() {
        let t = crate::theme::theme();
        let styles = entry_row_styles(t, true, false);
        // When selected, bg should use the selection background color
        assert_eq!(styles.bg, Style::default().bg(t.selection_bg));
    }

    #[test]
    fn test_entry_row_styles_not_selected() {
        let t = crate::theme::theme();
        let styles = entry_row_styles(t, false, false);
        // When not selected, bg should be the default style (no background)
        assert_eq!(styles.bg, Style::default());
    }

    // --- build_table_title tests ---

    #[test]
    fn test_build_table_title_empty() {
        let config = super::super::SearchConfig {
            entries: vec![],
            initial_query: None,
            total_items: 0,
            page: 1,
            page_size: 50,
            tags: vec![],
            executors: vec![],
            unique_counts: std::collections::HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            show_agents: false,
            failed_only: false,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: std::collections::HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: std::collections::HashSet::new(),
            show_risk_in_search: false,
            vim_enabled: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
                length_threshold: 80,
                human_boost_percent: 33,
                cwd_boost_percent: 50,
            },
            recall: super::super::RecallState::default(),
        };
        let app = super::SearchApp::new(config);
        let title = app.build_table_title();
        assert_eq!(title, "Executions (none) \u{b7} ^U group");
    }

    #[test]
    fn test_build_table_title_with_items() {
        let entries: Vec<Entry> = (0..50).map(|i| make_entry(None, None, Some(i))).collect();
        let config = super::super::SearchConfig {
            entries,
            initial_query: None,
            total_items: 100,
            page: 1,
            page_size: 50,
            tags: vec![],
            executors: vec![],
            unique_counts: std::collections::HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            show_agents: false,
            failed_only: false,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: std::collections::HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: std::collections::HashSet::new(),
            show_risk_in_search: false,
            vim_enabled: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
                length_threshold: 80,
                human_boost_percent: 33,
                cwd_boost_percent: 50,
            },
            recall: super::super::RecallState::default(),
        };
        let app = super::SearchApp::new(config);
        let title = app.build_table_title();
        assert_eq!(title, "Executions 1-50 of 100 \u{b7} ^U group");
    }

    // ========================================================================
    // Additional tests
    // ========================================================================

    // --- build_command_text with noted entry ---

    #[test]
    fn test_command_prefix_noted() {
        let (app, entry) = make_search_app_for_build_text(false, false, true);
        assert_eq!(command_prefix(&app, &entry, false), "📝");
    }

    // --- build_command_text with all three decorations ---

    #[test]
    fn test_command_prefix_bookmarked_unique_noted() {
        let (app, entry) = make_search_app_for_build_text(true, true, true);
        assert_eq!(command_prefix(&app, &entry, true), "📝★ 5\u{d7} ");
    }

    // --- build_table_title page 2 ---

    #[test]
    fn test_build_table_title_page_two() {
        let entries: Vec<Entry> = (0..50).map(|i| make_entry(None, None, Some(i))).collect();
        let config = super::super::SearchConfig {
            entries,
            initial_query: None,
            total_items: 120,
            page: 2,
            page_size: 50,
            tags: vec![],
            executors: vec![],
            unique_counts: std::collections::HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            show_agents: false,
            failed_only: false,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: std::collections::HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: std::collections::HashSet::new(),
            show_risk_in_search: false,
            vim_enabled: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
                length_threshold: 80,
                human_boost_percent: 33,
                cwd_boost_percent: 50,
            },
            recall: super::super::RecallState::default(),
        };
        let app = super::SearchApp::new(config);
        let title = app.build_table_title();
        // page=2, page_size=50: start_index = (2-1)*50+1 = 51, end_index = 51+50-1 = 100
        assert_eq!(title, "Executions 51-100 of 120 \u{b7} ^U group");
    }

    // --- build_table_title single item ---

    #[test]
    fn test_build_table_title_single_item() {
        let entries = vec![make_entry(None, None, Some(0))];
        let config = super::super::SearchConfig {
            entries,
            initial_query: None,
            total_items: 1,
            page: 1,
            page_size: 50,
            tags: vec![],
            executors: vec![],
            unique_counts: std::collections::HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            show_agents: false,
            failed_only: false,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: std::collections::HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: std::collections::HashSet::new(),
            show_risk_in_search: false,
            vim_enabled: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
                length_threshold: 80,
                human_boost_percent: 33,
                cwd_boost_percent: 50,
            },
            recall: super::super::RecallState::default(),
        };
        let app = super::SearchApp::new(config);
        let title = app.build_table_title();
        // page=1, page_size=50, 1 entry: start=1, end=1
        assert_eq!(title, "Executions 1-1 of 1 \u{b7} ^U group");
    }

    // --- build_table_title exact page boundary ---

    #[test]
    fn test_build_table_title_exact_page_boundary() {
        let entries: Vec<Entry> = (0..50).map(|i| make_entry(None, None, Some(i))).collect();
        let config = super::super::SearchConfig {
            entries,
            initial_query: None,
            total_items: 50,
            page: 1,
            page_size: 50,
            tags: vec![],
            executors: vec![],
            unique_counts: std::collections::HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            show_agents: false,
            failed_only: false,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: std::collections::HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: std::collections::HashSet::new(),
            show_risk_in_search: false,
            vim_enabled: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
                length_threshold: 80,
                human_boost_percent: 33,
                cwd_boost_percent: 50,
            },
            recall: super::super::RecallState::default(),
        };
        let app = super::SearchApp::new(config);
        let title = app.build_table_title();
        assert_eq!(title, "Executions 1-50 of 50 \u{b7} ^U group");
    }

    // --- ColumnLayout::from_width boundary values ---

    #[test]
    fn test_column_layout_boundary_79_compact() {
        // 79 < 80 → Compact
        assert!(matches!(
            ColumnLayout::from_width(79),
            ColumnLayout::Compact
        ));
    }

    #[test]
    fn test_column_layout_boundary_80_semi_compact() {
        // 80 >= 80 and < 130 → SemiCompact
        assert!(matches!(
            ColumnLayout::from_width(80),
            ColumnLayout::SemiCompact
        ));
    }

    #[test]
    fn test_column_layout_boundary_129_semi_compact() {
        // 129 < 130 → SemiCompact
        assert!(matches!(
            ColumnLayout::from_width(129),
            ColumnLayout::SemiCompact
        ));
    }

    #[test]
    fn test_column_layout_boundary_130_full() {
        // 130 >= 130 → Full
        assert!(matches!(ColumnLayout::from_width(130), ColumnLayout::Full));
    }

    #[test]
    fn test_column_layout_boundary_0_compact() {
        // 0 < 80 → Compact
        assert!(matches!(ColumnLayout::from_width(0), ColumnLayout::Compact));
    }

    #[test]
    fn test_column_layout_boundary_u16_max_full() {
        // u16::MAX >= 130 → Full
        assert!(matches!(
            ColumnLayout::from_width(u16::MAX),
            ColumnLayout::Full
        ));
    }

    // --- ColumnLayout::SemiCompact constraints count ---

    #[test]
    fn test_column_layout_constraints_semi_compact() {
        let layout = ColumnLayout::SemiCompact;
        let constraints = layout.constraints();
        assert_eq!(constraints.len(), 3);
    }

    // --- format_executor with unknown type ---

    #[test]
    fn test_format_executor_unknown_type_no_executor() {
        let e = make_entry(Some("unknown"), None, None);
        let result = format_executor(&e);
        // unknown falls into _ => "❓", and executor is None so just icon
        assert!(result.contains('❓'));
    }

    #[test]
    fn test_format_executor_unknown_type_with_executor() {
        let e = make_entry(Some("unknown"), Some("custom-shell"), None);
        // The executor_type is "unknown" → icon = "❓"
        // executor = Some("custom-shell") → format is "❓ custom-shell"
        let result = format_executor(&e);
        assert!(result.contains('❓'));
        assert!(result.contains("custom-shell"));
    }

    #[test]
    fn test_format_executor_ide() {
        let e = make_entry(Some("ide"), Some("vscode"), None);
        let result = format_executor(&e);
        assert!(result.contains('💻'));
        assert!(result.contains("vscode"));
    }

    #[test]
    fn test_format_executor_ci() {
        let e = make_entry(Some("ci"), Some("github-actions"), None);
        let result = format_executor(&e);
        assert!(result.contains("github-actions"));
    }

    #[test]
    fn test_format_executor_programmatic() {
        let e = make_entry(Some("programmatic"), Some("script"), None);
        let result = format_executor(&e);
        assert!(result.contains("script"));
    }

    #[test]
    fn test_format_executor_bot() {
        let e = make_entry(Some("bot"), Some("mybot"), None);
        let result = format_executor(&e);
        // "bot" matches "bot" | "agent" arm → "🤖"
        assert!(result.contains('🤖'));
        assert!(result.contains("mybot"));
    }

    // --- format_exit_code for signal codes ---

    #[test]
    fn test_format_exit_code_127_command_not_found() {
        let e = make_entry(None, None, Some(127));
        let (display, _style) = format_exit_code(&e, Style::default());
        assert!(display.contains('✘'));
        assert!(display.contains("127"));
    }

    #[test]
    fn test_format_exit_code_130_sigint() {
        let e = make_entry(None, None, Some(130));
        let (display, _style) = format_exit_code(&e, Style::default());
        assert!(display.contains('✘'));
        assert!(display.contains("130"));
    }

    #[test]
    fn test_format_exit_code_137_sigkill() {
        let e = make_entry(None, None, Some(137));
        let (display, _style) = format_exit_code(&e, Style::default());
        assert!(display.contains('✘'));
        assert!(display.contains("137"));
    }

    #[test]
    fn test_format_exit_code_failure_style_uses_error_color() {
        let t = crate::theme::theme();
        let e = make_entry(None, None, Some(1));
        let (_display, style) = format_exit_code(&e, Style::default());
        let expected = Style::default().fg(t.error);
        assert_eq!(style, expected);
    }

    #[test]
    fn test_format_exit_code_success_style_uses_success_color() {
        let t = crate::theme::theme();
        let e = make_entry(None, None, Some(0));
        let (_display, style) = format_exit_code(&e, Style::default());
        let expected = Style::default().fg(t.success);
        assert_eq!(style, expected);
    }

    #[test]
    fn test_format_exit_code_none_style_uses_muted_color() {
        let t = crate::theme::theme();
        let e = make_entry(None, None, None);
        let (_display, style) = format_exit_code(&e, Style::default());
        let expected = Style::default().fg(t.text_muted);
        assert_eq!(style, expected);
    }

    // --- entry_row_styles with is_local ---

    #[test]
    fn test_entry_row_styles_selected_local() {
        let t = crate::theme::theme();
        let styles = entry_row_styles(t, true, true);
        // When selected + local, path should be badge_path color with bold
        let expected_path = Style::default()
            .bg(t.selection_bg)
            .fg(t.badge_path)
            .add_modifier(Modifier::BOLD);
        assert_eq!(styles.path, expected_path);
    }

    #[test]
    fn test_entry_row_styles_selected_not_local() {
        let t = crate::theme::theme();
        let styles = entry_row_styles(t, true, false);
        // When selected + not local, path should use selection_fg
        let expected_path = Style::default().bg(t.selection_bg).fg(t.selection_fg);
        assert_eq!(styles.path, expected_path);
    }

    #[test]
    fn test_entry_row_styles_not_selected_local() {
        let t = crate::theme::theme();
        let styles = entry_row_styles(t, false, true);
        // When not selected + local, path should be badge_path color
        let expected_path = Style::default().fg(t.badge_path);
        assert_eq!(styles.path, expected_path);
        // bg should be default (no background)
        assert_eq!(styles.bg, Style::default());
    }

    #[test]
    fn test_entry_row_styles_not_selected_not_local() {
        let t = crate::theme::theme();
        let styles = entry_row_styles(t, false, false);
        // When not selected + not local, path should use text_secondary
        let expected_path = Style::default().fg(t.text_secondary);
        assert_eq!(styles.path, expected_path);
    }

    // --- build_command_text with no ID ---

    #[test]
    fn test_build_command_text_no_id() {
        use std::collections::{HashMap, HashSet};

        let entry = Entry {
            id: None,
            session_id: "s1".to_string(),
            command: "ls -la".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: None,
            executor: None,
        };

        // noted_entry_ids has some values, but entry.id is None so no match
        let mut noted_entry_ids = HashSet::new();
        noted_entry_ids.insert(42);

        let config = super::super::SearchConfig {
            entries: vec![entry.clone()],
            initial_query: None,
            total_items: 1,
            page: 1,
            page_size: 50,
            tags: vec![],
            executors: vec![],
            unique_counts: HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            show_agents: false,
            failed_only: false,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: HashSet::new(),
            filter_cwd: None,
            noted_entry_ids,
            show_risk_in_search: false,
            vim_enabled: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
                length_threshold: 80,
                human_boost_percent: 33,
                cwd_boost_percent: 50,
            },
            recall: super::super::RecallState::default(),
        };
        let app = super::SearchApp::new(config);
        // No decorations, and no panic without an id.
        assert_eq!(command_prefix(&app, &entry, false), "");
    }

    #[test]
    fn test_build_command_text_no_id_unique_mode() {
        use std::collections::{HashMap, HashSet};

        let entry = Entry {
            id: None,
            session_id: "s1".to_string(),
            command: "echo hello".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: None,
            executor: None,
        };

        let config = super::super::SearchConfig {
            entries: vec![entry.clone()],
            initial_query: None,
            total_items: 1,
            page: 1,
            page_size: 50,
            tags: vec![],
            executors: vec![],
            unique_counts: HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            show_agents: false,
            failed_only: false,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: HashSet::new(),
            show_risk_in_search: false,
            vim_enabled: false,
            view: super::super::ViewOptions {
                unique_mode: true,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
                length_threshold: 80,
                human_boost_percent: 33,
                cwd_boost_percent: 50,
            },
            recall: super::super::RecallState::default(),
        };
        let app = super::SearchApp::new(config);
        // id=None → unwrap_or(0), unique_counts empty → a count of 1
        assert_eq!(command_prefix(&app, &entry, true), "1\u{d7} ");
    }

    // --- command_col_width with narrow widths (saturating_sub) ---

    // --- SemiCompact header row ---

    #[test]
    fn test_column_layout_header_semi_compact() {
        let layout = ColumnLayout::SemiCompact;
        // Should not panic and constraints should be 3
        let _header = layout.header_row();
        assert_eq!(layout.constraints().len(), 3);
    }

    // ========================================================================
    // build_detail_lines / append_risk_line / append_bookmark_note_lines tests
    // ========================================================================

    fn make_default_search_config(
        entries: Vec<Entry>,
        bookmarked: std::collections::HashSet<String>,
        noted: std::collections::HashSet<i64>,
        show_risk: bool,
    ) -> super::super::SearchConfig {
        super::super::SearchConfig {
            entries,
            initial_query: None,
            total_items: 0,
            page: 1,
            page_size: 50,
            tags: vec![],
            executors: vec![],
            unique_counts: std::collections::HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            show_agents: false,
            failed_only: false,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: bookmarked,
            filter_cwd: None,
            noted_entry_ids: noted,
            show_risk_in_search: show_risk,
            vim_enabled: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
                length_threshold: 80,
                human_boost_percent: 33,
                cwd_boost_percent: 50,
            },
            recall: super::super::RecallState::default(),
        }
    }

    fn lines_text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_build_detail_lines_basic() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(1),
            session_id: "abcdefgh-1234-5678-9012-345678901234".to_string(),
            command: "cargo test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("zsh".to_string()),
        };

        let config =
            make_default_search_config(vec![entry.clone()], HashSet::new(), HashSet::new(), false);
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(text.contains("cargo test"), "should contain command");
        assert!(text.contains("/tmp"), "should contain path");
        assert!(
            text.contains("✔ 0 (success)"),
            "should contain success exit"
        );
        assert!(text.contains("human: zsh"), "should contain executor");
        assert!(text.contains("none"), "should contain tag none");
        // Command label and text, path, exit, time, duration, session, tag,
        // executor — with where it ran and how it ended leading the fields.
        assert_eq!(lines.len(), 9);
        let path = text.find("/tmp").unwrap();
        let exit = text.find("(success)").unwrap();
        let session = text.find("Session").unwrap();
        assert!(
            path < exit && exit < session,
            "path and exit come first:\n{text}"
        );
    }

    #[test]
    fn test_build_detail_lines_failed_exit() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(1),
            session_id: "s1".to_string(),
            command: "cargo test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(1),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("zsh".to_string()),
        };

        let config =
            make_default_search_config(vec![entry.clone()], HashSet::new(), HashSet::new(), false);
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(
            text.contains("✘ 1 (failed)"),
            "should contain failed exit code"
        );
    }

    #[test]
    fn test_build_detail_lines_unknown_exit() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(1),
            session_id: "s1".to_string(),
            command: "cargo test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: None,
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("zsh".to_string()),
        };

        let config =
            make_default_search_config(vec![entry.clone()], HashSet::new(), HashSet::new(), false);
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(text.contains("○ (unknown)"), "should contain unknown exit");
    }

    #[test]
    fn test_build_detail_lines_executor_only_type() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(1),
            session_id: "s1".to_string(),
            command: "cargo test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("agent".to_string()),
            executor: None,
        };

        let config =
            make_default_search_config(vec![entry.clone()], HashSet::new(), HashSet::new(), false);
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(
            text.contains("agent"),
            "should contain executor type 'agent'"
        );
        // executor_type only (no executor name) should NOT produce a colon separator
        assert!(
            !text.contains("agent:"),
            "should not have colon when executor name is absent"
        );
    }

    #[test]
    fn test_build_detail_lines_no_executor() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(1),
            session_id: "s1".to_string(),
            command: "cargo test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: None,
            executor: None,
        };

        let config =
            make_default_search_config(vec![entry.clone()], HashSet::new(), HashSet::new(), false);
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(
            text.contains("unknown"),
            "should show 'unknown' when no executor info"
        );
    }

    #[test]
    fn test_build_detail_lines_with_tag() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(1),
            session_id: "s1".to_string(),
            command: "cargo test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: Some("work".to_string()),
            tag_id: Some(1),
            executor_type: Some("human".to_string()),
            executor: Some("zsh".to_string()),
        };

        let config =
            make_default_search_config(vec![entry.clone()], HashSet::new(), HashSet::new(), false);
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(text.contains("work"), "should contain tag name 'work'");
    }

    #[test]
    fn test_build_detail_lines_session_truncated() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(1),
            session_id: "abcdefgh-1234-5678-9012-345678901234".to_string(),
            command: "cargo test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("zsh".to_string()),
        };

        let config =
            make_default_search_config(vec![entry.clone()], HashSet::new(), HashSet::new(), false);
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        // Session ID should be shown in full
        assert!(
            text.contains("abcdefgh-1234"),
            "should contain full session ID"
        );
    }

    #[test]
    fn test_build_detail_lines_risk_disabled() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(1),
            session_id: "s1".to_string(),
            command: "rm -rf /".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("zsh".to_string()),
        };

        let config = make_default_search_config(
            vec![entry.clone()],
            HashSet::new(),
            HashSet::new(),
            false, // show_risk disabled
        );
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(
            !text.contains("Risk"),
            "should NOT show Risk line when risk display is disabled"
        );
    }

    #[test]
    fn test_build_detail_lines_risk_enabled_dangerous() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(1),
            session_id: "s1".to_string(),
            command: "rm -rf /".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("zsh".to_string()),
        };

        let config = make_default_search_config(
            vec![entry.clone()],
            HashSet::new(),
            HashSet::new(),
            true, // show_risk enabled
        );
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(
            text.contains("Risk"),
            "should show Risk line for dangerous command"
        );
    }

    #[test]
    fn test_build_detail_lines_risk_enabled_safe() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(1),
            session_id: "s1".to_string(),
            command: "ls".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("zsh".to_string()),
        };

        let config = make_default_search_config(
            vec![entry.clone()],
            HashSet::new(),
            HashSet::new(),
            true, // show_risk enabled, but safe command
        );
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(
            !text.contains("Risk"),
            "should NOT show Risk line for safe command even when risk display is enabled"
        );
    }

    #[test]
    fn test_build_detail_lines_bookmarked() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(1),
            session_id: "s1".to_string(),
            command: "cargo test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("zsh".to_string()),
        };

        let mut bookmarked = HashSet::new();
        bookmarked.insert("cargo test".to_string());

        let config =
            make_default_search_config(vec![entry.clone()], bookmarked, HashSet::new(), false);
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(text.contains("★"), "should contain bookmark star");
        assert!(text.contains("Bookmarked"), "should contain 'Bookmarked'");
    }

    #[test]
    fn test_build_detail_lines_noted() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(42),
            session_id: "s1".to_string(),
            command: "cargo test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("zsh".to_string()),
        };

        let mut noted = HashSet::new();
        noted.insert(42_i64);

        let config = make_default_search_config(vec![entry.clone()], HashSet::new(), noted, false);
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(text.contains("📝"), "should contain note emoji");
        assert!(text.contains("Has note"), "should contain 'Has note'");
    }

    #[test]
    fn test_build_detail_lines_bookmarked_and_noted() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(42),
            session_id: "s1".to_string(),
            command: "cargo test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("zsh".to_string()),
        };

        let mut bookmarked = HashSet::new();
        bookmarked.insert("cargo test".to_string());
        let mut noted = HashSet::new();
        noted.insert(42_i64);

        let config = make_default_search_config(vec![entry.clone()], bookmarked, noted, false);
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(text.contains("★"), "should contain bookmark star");
        assert!(text.contains("📝"), "should contain note emoji");
    }

    #[test]
    fn test_build_detail_lines_not_bookmarked_not_noted() {
        use std::collections::HashSet;

        let entry = Entry {
            id: Some(1),
            session_id: "s1".to_string(),
            command: "cargo test".to_string(),
            cwd: "/tmp".to_string(),
            exit_code: Some(0),
            started_at: 1_700_000_000_000,
            ended_at: 1_700_000_001_000,
            duration_ms: 1000,
            context: None,
            tag_name: None,
            tag_id: None,
            executor_type: Some("human".to_string()),
            executor: Some("zsh".to_string()),
        };

        let config =
            make_default_search_config(vec![entry.clone()], HashSet::new(), HashSet::new(), false);
        let app = super::SearchApp::new(config);
        let lines = app.build_detail_lines(&entry);
        let text = lines_text(&lines);

        assert!(!text.contains("★"), "should NOT contain bookmark star");
        assert!(!text.contains("📝"), "should NOT contain note emoji");
    }
}
