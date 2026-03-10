use crate::risk;
use crate::theme::theme;
use chrono::{Local, TimeZone};
use ratatui::{
    backend::CrosstermBackend,
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
    build_command_text, entry_row_styles, format_executor, format_exit_code, ColumnLayout,
};
use super::{centered_rect, fill_text, DialogState, SearchApp};

impl SearchApp {
    pub(super) fn render(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    ) -> io::Result<()> {
        terminal.draw(|f| {
            let t = theme();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // Branding
                    Constraint::Length(3), // Query input
                    Constraint::Min(0),    // Results list
                    Constraint::Length(2), // Help text
                ])
                .split(f.area());

            // Minimalist Header
            let branding = Line::from(vec![Span::styled(
                "Suvadu",
                Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
            )]);
            f.render_widget(
                Paragraph::new(branding).alignment(Alignment::Center),
                chunks[0],
            );

            // Dynamic Search Bar
            let active_filters = self.active_filter_count();
            let filter_badge = if active_filters > 0 {
                format!(
                    " [{active_filters} filter{}]",
                    if active_filters > 1 { "s" } else { "" }
                )
            } else {
                String::new()
            };
            let unique_badge = if self.view.unique_mode {
                " [unique]"
            } else {
                ""
            };

            let in_filter = matches!(self.dialog, DialogState::Filter);
            let search_border_color = if in_filter { t.border } else { t.border_focus };
            let search_title = if in_filter {
                "Search"
            } else {
                "Search (Typing)"
            };
            let query_display = format!("{}{filter_badge}{unique_badge}", self.query);
            let query = Paragraph::new(query_display)
                .style(Style::default().fg(t.text))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(search_border_color))
                        .title(search_title),
                );
            f.render_widget(query, chunks[1]);

            // Results Table + Optional Detail Pane
            if self.view.detail_pane_open {
                let result_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Percentage(70), // Results
                        Constraint::Percentage(30), // Detail
                    ])
                    .split(chunks[2]);
                self.render_results_table(f, result_chunks[0]);
                self.render_detail_pane(f, result_chunks[1]);
            } else {
                self.render_results_table(f, chunks[2]);
            }

            // Footer
            self.render_footer(f, chunks[3]);

            // Render Overlays
            match self.dialog {
                DialogState::Filter => self.render_filter_popup(f, f.area()),
                DialogState::GoToPage { .. } => self.render_goto_dialog(f, f.area()),
                DialogState::Delete { .. } => self.render_delete_dialog(f, f.area()),
                DialogState::TagAssociation => self.render_tag_dialog(f, f.area()),
                DialogState::Note { .. } => self.render_note_dialog(f, f.area()),
                DialogState::Help => self.render_help_dialog(f, f.area()),
                DialogState::None => {}
            }
        })?;

        Ok(())
    }

    // --- render_footer (decomposed) ---

    pub(super) fn render_footer(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let total_pages = self
            .pagination
            .total_items
            .div_ceil(self.pagination.page_size)
            .max(1);
        let progress_pct = if total_pages > 0 {
            (self.pagination.page * 100) / total_pages
        } else {
            0
        };

        let status_text = if let Some((msg, time)) = &self.status_message {
            if time.elapsed() < std::time::Duration::from_secs(2) {
                Some(msg.clone())
            } else {
                None
            }
        } else {
            None
        };

        let mut help_badges = self.build_help_badges();
        self.append_active_filter_badges(&mut help_badges);

        // Page progress
        let page_info = format!(
            " {}/{} ({progress_pct}%) ",
            self.pagination.page, total_pages
        );
        help_badges.push(Span::styled(page_info, Style::default().fg(t.text_muted)));

        if let Some(msg) = status_text {
            help_badges.push(Span::styled(
                format!(" {msg} "),
                Style::default().fg(t.success).add_modifier(Modifier::BOLD),
            ));
        }

        let help_line = Line::from(help_badges);
        let help_paragraph = Paragraph::new(help_line).block(
            Block::default()
                .borders(Borders::TOP)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border)),
        );
        f.render_widget(help_paragraph, area);
    }

    fn build_help_badges(&self) -> Vec<Span<'static>> {
        let t = theme();
        let badge_key_style = Style::default().bg(t.badge_bg).fg(t.text);
        let badge_label_style = Style::default().fg(t.text_secondary);

        vec![
            Span::styled(" Esc ", badge_key_style),
            Span::styled(" Quit  ", badge_label_style),
            Span::styled(" ^F ", badge_key_style),
            Span::styled(" Filter  ", badge_label_style),
            Span::styled(" ^D ", badge_key_style),
            Span::styled(" Delete  ", badge_label_style),
            Span::styled(" ^T ", badge_key_style),
            Span::styled(" Tag  ", badge_label_style),
            Span::styled(" ^G ", badge_key_style),
            Span::styled(" Goto  ", badge_label_style),
            Span::styled(" ^U ", badge_key_style),
            Span::styled(
                if self.view.unique_mode {
                    " All  "
                } else {
                    " Unique  "
                },
                badge_label_style,
            ),
            Span::styled(" ^Y ", badge_key_style),
            Span::styled(" Copy  ", badge_label_style),
            Span::styled(" ^B ", badge_key_style),
            Span::styled(" Bookmark  ", badge_label_style),
            Span::styled(" ^N ", badge_key_style),
            Span::styled(" Note  ", badge_label_style),
            Span::styled(" ^L ", badge_key_style),
            Span::styled(
                if self.filters.cwd.is_some() {
                    " All Dirs  "
                } else {
                    " Here  "
                },
                badge_label_style,
            ),
            Span::styled(" ^S ", badge_key_style),
            Span::styled(
                if self.view.context_boost {
                    " Recent  "
                } else {
                    " Smart  "
                },
                badge_label_style,
            ),
            Span::styled(" Tab ", badge_key_style),
            Span::styled(
                if self.view.detail_pane_open {
                    " Hide  "
                } else {
                    " Detail  "
                },
                badge_label_style,
            ),
            Span::styled(" ? ", badge_key_style),
            Span::styled(" Help  ", badge_label_style),
        ]
    }

    fn append_active_filter_badges(&self, badges: &mut Vec<Span<'static>>) {
        let t = theme();

        if self.filters.after.is_some() || self.filters.before.is_some() {
            badges.push(Span::styled(
                " date ",
                Style::default().bg(t.info).fg(Color::Black),
            ));
            badges.push(Span::raw(" "));
        }
        if self.filters.tag_id.is_some() {
            badges.push(Span::styled(
                " tag ",
                Style::default().bg(t.warning).fg(Color::Black),
            ));
            badges.push(Span::raw(" "));
        }
        if self.filters.exit_code.is_some() {
            badges.push(Span::styled(
                " exit ",
                Style::default().bg(t.error).fg(Color::White),
            ));
            badges.push(Span::raw(" "));
        }
        if self.filters.executor_type.is_some() {
            badges.push(Span::styled(
                " exec ",
                Style::default().bg(t.badge_executor).fg(Color::White),
            ));
            badges.push(Span::raw(" "));
        }
        if self.filters.cwd.is_some() {
            badges.push(Span::styled(
                " dir ",
                Style::default().bg(t.badge_path).fg(Color::Black),
            ));
            badges.push(Span::raw(" "));
        }
        if self.view.context_boost {
            badges.push(Span::styled(
                " smart ",
                Style::default().bg(t.success).fg(Color::Black),
            ));
            badges.push(Span::raw(" "));
        }
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

        let layout = if self.view.unique_mode {
            ColumnLayout::Compact
        } else {
            ColumnLayout::from_width(table_area.width)
        };
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
            .highlight_symbol(" > ");

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

    #[allow(clippy::cast_precision_loss)]
    fn build_entry_row(
        &self,
        entry: &crate::models::Entry,
        is_selected: bool,
        layout: &ColumnLayout,
        command_col_width: u16,
    ) -> Row<'static> {
        let t = theme();

        let duration_secs = entry.duration_ms as f64 / 1000.0;
        let ts_ms = crate::util::normalize_display_ms(entry.started_at);
        let time_str = Local.timestamp_millis_opt(ts_ms).single().map_or_else(
            || "??-?? ??:??".into(),
            |dt| dt.format("%m-%d %H:%M").to_string(),
        );
        let duration_str = format!("{duration_secs:.1}s");

        let session_short: String = entry.session_id.chars().take(8).collect();
        let session_tag_display = entry.tag_name.as_ref().map_or_else(
            || session_short.clone(),
            |tag| format!("{session_short} ({tag})"),
        );
        let st_display = if is_selected {
            fill_text(&session_tag_display, 25)
        } else {
            session_tag_display
        };

        let path_display = std::path::Path::new(&entry.cwd)
            .file_name()
            .and_then(|n| n.to_str())
            .map_or_else(|| entry.cwd.clone(), |last| format!("../{last}"));

        let executor_display = format_executor(entry);
        let cmd_text = build_command_text(self, entry);
        let command_display = if is_selected {
            Self::highlight_command(&cmd_text, command_col_width as usize)
        } else {
            Self::highlight_command(&cmd_text, 0)
        };

        let cmd_height = u16::try_from(command_display.lines.len())
            .unwrap_or(1)
            .max(1);
        let st_height = u16::try_from(st_display.lines().count())
            .unwrap_or(1)
            .max(1);
        let height = cmd_height.max(st_height);

        let is_local = self.view.context_boost
            && self
                .view
                .current_cwd
                .as_deref()
                .is_some_and(|cwd| entry.cwd == cwd);

        let styles = entry_row_styles(t, is_selected, is_local);
        let (exit_display, exit_style_item) = format_exit_code(entry, styles.bg);

        match *layout {
            ColumnLayout::Compact => Row::new(vec![Cell::from(command_display)])
                .height(height)
                .style(styles.bg),
            ColumnLayout::SemiCompact => Row::new(vec![
                Cell::from(time_str).style(styles.time),
                Cell::from(command_display),
                Cell::from(exit_display).style(exit_style_item),
            ])
            .height(height)
            .style(styles.bg),
            ColumnLayout::Full => Row::new(vec![
                Cell::from(time_str).style(styles.time),
                Cell::from(command_display),
                Cell::from(st_display).style(styles.session),
                Cell::from(executor_display).style(styles.executor),
                Cell::from(path_display).style(styles.path),
                Cell::from(exit_display).style(exit_style_item),
                Cell::from(duration_str).style(styles.duration),
            ])
            .height(height)
            .style(styles.bg),
        }
    }

    fn build_table_title(&self) -> String {
        if self.pagination.total_items == 0 {
            "History (0/0)".to_string()
        } else {
            let start_index = (self.pagination.page - 1) * self.pagination.page_size + 1;
            let end_index = start_index + self.entries.len().saturating_sub(1);
            format!(
                "History ({}-{} / {})",
                start_index, end_index, self.pagination.total_items
            )
        }
    }

    // --- render_detail_pane (decomposed) ---

    pub(super) fn render_detail_pane(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let entry = self.get_selected_entry();

        let block = Block::default()
            .title(" Detail ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border));

        if let Some(entry) = entry {
            let lines = self.build_detail_lines(entry);
            let paragraph = Paragraph::new(lines)
                .block(block)
                .wrap(ratatui::widgets::Wrap { trim: false });
            f.render_widget(paragraph, area);
        } else {
            let empty = Paragraph::new("No entry selected")
                .block(block)
                .style(Style::default().fg(t.text_muted))
                .alignment(Alignment::Center);
            f.render_widget(empty, area);
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn build_detail_lines(&self, entry: &crate::models::Entry) -> Vec<Line<'static>> {
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

        let session_str: String = entry.session_id.chars().take(8).collect();
        let tag_str = entry.tag_name.as_deref().unwrap_or("none").to_string();

        let mut lines = vec![
            Line::from(vec![Span::styled("Command  ", label_style)]),
            Line::from(vec![Span::styled(
                entry.command.clone(),
                Style::default().fg(t.primary),
            )]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Path     ", label_style),
                Span::styled(entry.cwd.clone(), value_style),
            ]),
            Line::from(vec![
                Span::styled("Time     ", label_style),
                Span::styled(time_str, value_style),
            ]),
            Line::from(vec![
                Span::styled("Duration ", label_style),
                Span::styled(format!("{duration_secs:.2}s"), value_style),
            ]),
            Line::from(vec![
                Span::styled("Exit     ", label_style),
                Span::styled(exit_str, value_style),
            ]),
            Line::from(vec![
                Span::styled("Session  ", label_style),
                Span::styled(session_str, value_style),
            ]),
            Line::from(vec![
                Span::styled("Tag      ", label_style),
                Span::styled(tag_str, value_style),
            ]),
            Line::from(vec![
                Span::styled("Executor ", label_style),
                Span::styled(executor_str, value_style),
            ]),
        ];

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
                Span::styled("Risk     ", label_style),
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
        let fields: Vec<(&str, &str, &str)> = vec![
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
            (
                "Executor",
                &self.filters.executor_filter_input,
                "e.g. human, agent, ide, ci, vscode",
            ),
        ];

        for (i, (title, value, hint)) in fields.iter().enumerate() {
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
    }

    /// Compact mode: render only the currently focused filter field.
    fn render_single_filter_field(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let fields: [(&str, &str, &str); 5] = [
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
            (
                "Executor",
                &self.filters.executor_filter_input,
                "e.g. human, agent, ide, ci, vscode",
            ),
        ];

        let i = self.filters.focus_index.min(fields.len() - 1);
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

    pub(super) fn render_help_dialog(&self, f: &mut ratatui::Frame, area: Rect) {
        // Use self to stay consistent with other dialog render methods.
        let _ = &self.dialog;

        let t = theme();
        let popup_area = centered_rect(70, 80, area);
        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Keyboard Shortcuts ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.primary))
            .style(Style::default().bg(t.bg_elevated));

        let lines = build_help_lines(t);
        let content = Paragraph::new(lines).block(block);
        f.render_widget(content, popup_area);
    }

    fn highlight_command(command: &str, width: usize) -> ratatui::text::Text<'static> {
        crate::util::highlight_command(command, width)
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

fn build_help_lines(t: &crate::theme::Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        help_section("\u{2500}\u{2500} Navigation \u{2500}\u{2500}", t),
        help_row(
            "  \u{2191}/\u{2193}/j/k        ",
            "Move selection up/down",
            t,
        ),
        help_row("  PgUp/PgDn       ", "Previous/next page", t),
        help_row("  ^G              ", "Go to page...", t),
        help_row("  Home/End        ", "First/last entry", t),
        Line::from(""),
        help_section("\u{2500}\u{2500} Actions \u{2500}\u{2500}", t),
        help_row("  Enter           ", "Select command", t),
        help_row("  ^Y              ", "Copy command to clipboard", t),
        help_row("  ^D              ", "Delete entry", t),
        help_row("  ^B              ", "Toggle bookmark", t),
        help_row("  ^N              ", "Add/edit note", t),
        Line::from(""),
        help_section("\u{2500}\u{2500} Filters \u{2500}\u{2500}", t),
        help_row(
            "  ^F              ",
            "Filter dialog (date, tag, exit code)",
            t,
        ),
        help_row("  ^T              ", "Tag current session", t),
        help_row("  ^L              ", "Toggle directory filter", t),
        Line::from(""),
        help_section("\u{2500}\u{2500} Display \u{2500}\u{2500}", t),
        help_row("  ^U              ", "Toggle unique/all mode", t),
        help_row("  Tab             ", "Toggle detail pane", t),
        help_row("  ^S              ", "Toggle context boost", t),
        Line::from(""),
        help_section("\u{2500}\u{2500} Other \u{2500}\u{2500}", t),
        help_row("  ?/F1            ", "This help", t),
        help_row("  Esc/q           ", "Exit", t),
        Line::from(""),
        Line::from(Span::styled(
            "Press any key to close",
            Style::default().fg(t.text_muted),
        )),
    ]
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
        assert_eq!(constraints.len(), 7);
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
        // Full layout has 7 columns = 7 header cells
        assert_eq!(layout.constraints().len(), 7);
    }

    #[test]
    fn test_column_layout_command_width() {
        // Compact: table_width - 6
        assert_eq!(ColumnLayout::Compact.command_col_width(80), 74);

        // SemiCompact: table_width - (12 + 6) - 6 = table_width - 24
        assert_eq!(ColumnLayout::SemiCompact.command_col_width(100), 76);

        // Full: table_width - 64 - 6 = table_width - 70
        assert_eq!(ColumnLayout::Full.command_col_width(150), 80);
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
            unique_counts,
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands,
            filter_cwd: None,
            noted_entry_ids,
            show_risk_in_search: false,
            view: super::super::ViewOptions {
                unique_mode: unique,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
            },
        };

        (super::SearchApp::new(config), entry)
    }

    #[test]
    fn test_build_command_text_plain() {
        let (app, entry) = make_search_app_for_build_text(false, false, false);
        let text = build_command_text(&app, &entry);
        assert_eq!(text, "cargo test");
    }

    #[test]
    fn test_build_command_text_bookmarked() {
        let (app, entry) = make_search_app_for_build_text(true, false, false);
        let text = build_command_text(&app, &entry);
        assert!(text.contains('★'));
        assert!(text.contains("cargo test"));
    }

    #[test]
    fn test_build_command_text_unique_mode() {
        let (app, entry) = make_search_app_for_build_text(false, true, false);
        let text = build_command_text(&app, &entry);
        assert!(text.contains("(5)"));
        assert!(text.contains("cargo test"));
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
            unique_counts: std::collections::HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: std::collections::HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: std::collections::HashSet::new(),
            show_risk_in_search: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
            },
        };
        let app = super::SearchApp::new(config);
        let title = app.build_table_title();
        assert_eq!(title, "History (0/0)");
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
            unique_counts: std::collections::HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: std::collections::HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: std::collections::HashSet::new(),
            show_risk_in_search: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
            },
        };
        let app = super::SearchApp::new(config);
        let title = app.build_table_title();
        assert_eq!(title, "History (1-50 / 100)");
    }

    // ========================================================================
    // Additional tests
    // ========================================================================

    // --- active_filter_count tests ---

    fn make_search_app_with_filters(
        after: Option<i64>,
        before: Option<i64>,
        tag_id: Option<i64>,
        exit_code: Option<i32>,
        executor_type: Option<String>,
    ) -> super::SearchApp {
        let config = super::super::SearchConfig {
            entries: vec![],
            initial_query: None,
            total_items: 0,
            page: 1,
            page_size: 50,
            tags: vec![],
            unique_counts: std::collections::HashMap::new(),
            filter_after: after,
            filter_before: before,
            filter_tag_id: tag_id,
            filter_exit_code: exit_code,
            filter_executor_type: executor_type,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: std::collections::HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: std::collections::HashSet::new(),
            show_risk_in_search: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
            },
        };
        super::SearchApp::new(config)
    }

    #[test]
    fn test_active_filter_count_none() {
        let app = make_search_app_with_filters(None, None, None, None, None);
        assert_eq!(app.active_filter_count(), 0);
    }

    #[test]
    fn test_active_filter_count_one_after() {
        let app = make_search_app_with_filters(Some(1_700_000_000), None, None, None, None);
        assert_eq!(app.active_filter_count(), 1);
    }

    #[test]
    fn test_active_filter_count_one_before() {
        let app = make_search_app_with_filters(None, Some(1_700_000_000), None, None, None);
        assert_eq!(app.active_filter_count(), 1);
    }

    #[test]
    fn test_active_filter_count_one_tag() {
        let app = make_search_app_with_filters(None, None, Some(1), None, None);
        assert_eq!(app.active_filter_count(), 1);
    }

    #[test]
    fn test_active_filter_count_one_exit_code() {
        let app = make_search_app_with_filters(None, None, None, Some(0), None);
        assert_eq!(app.active_filter_count(), 1);
    }

    #[test]
    fn test_active_filter_count_one_executor() {
        let app = make_search_app_with_filters(None, None, None, None, Some("human".to_string()));
        assert_eq!(app.active_filter_count(), 1);
    }

    #[test]
    fn test_active_filter_count_two() {
        let app = make_search_app_with_filters(Some(1_700_000_000), None, Some(5), None, None);
        assert_eq!(app.active_filter_count(), 2);
    }

    #[test]
    fn test_active_filter_count_all_five() {
        let app = make_search_app_with_filters(
            Some(1_700_000_000),
            Some(1_800_000_000),
            Some(1),
            Some(0),
            Some("agent".to_string()),
        );
        assert_eq!(app.active_filter_count(), 5);
    }

    // --- build_command_text with noted entry ---

    #[test]
    fn test_build_command_text_noted() {
        let (app, entry) = make_search_app_for_build_text(false, false, true);
        let text = build_command_text(&app, &entry);
        // The note prefix is "📝"
        assert!(text.contains('📝'));
        assert!(text.contains("cargo test"));
    }

    // --- build_command_text with all three decorations ---

    #[test]
    fn test_build_command_text_bookmarked_unique_noted() {
        let (app, entry) = make_search_app_for_build_text(true, true, true);
        let text = build_command_text(&app, &entry);
        assert!(text.contains('📝'));
        assert!(text.contains('★'));
        assert!(text.contains("(5)"));
        assert!(text.contains("cargo test"));
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
            unique_counts: std::collections::HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: std::collections::HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: std::collections::HashSet::new(),
            show_risk_in_search: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
            },
        };
        let app = super::SearchApp::new(config);
        let title = app.build_table_title();
        // page=2, page_size=50: start_index = (2-1)*50+1 = 51, end_index = 51+50-1 = 100
        assert_eq!(title, "History (51-100 / 120)");
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
            unique_counts: std::collections::HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: std::collections::HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: std::collections::HashSet::new(),
            show_risk_in_search: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
            },
        };
        let app = super::SearchApp::new(config);
        let title = app.build_table_title();
        // page=1, page_size=50, 1 entry: start=1, end=1
        assert_eq!(title, "History (1-1 / 1)");
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
            unique_counts: std::collections::HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: std::collections::HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: std::collections::HashSet::new(),
            show_risk_in_search: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
            },
        };
        let app = super::SearchApp::new(config);
        let title = app.build_table_title();
        assert_eq!(title, "History (1-50 / 50)");
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
        // When selected + local, path should be primary color with bold
        let expected_path = Style::default()
            .bg(t.selection_bg)
            .fg(t.primary)
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
        // When not selected + local, path should be primary color (no bold)
        let expected_path = Style::default().fg(t.primary);
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
            unique_counts: HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: HashSet::new(),
            filter_cwd: None,
            noted_entry_ids,
            show_risk_in_search: false,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
            },
        };
        let app = super::SearchApp::new(config);
        let text = build_command_text(&app, &entry);
        // Should just be the command, no decorations, and no panic
        assert_eq!(text, "ls -la");
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
            unique_counts: HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: HashSet::new(),
            filter_cwd: None,
            noted_entry_ids: HashSet::new(),
            show_risk_in_search: false,
            view: super::super::ViewOptions {
                unique_mode: true,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
            },
        };
        let app = super::SearchApp::new(config);
        let text = build_command_text(&app, &entry);
        // id=None → unwrap_or(0), unique_counts empty → unwrap_or(&1)
        // Should show "(1) echo hello"
        assert!(text.contains("(1)"));
        assert!(text.contains("echo hello"));
    }

    // --- fill_text tests ---

    #[test]
    fn test_fill_text_empty_string() {
        let result = fill_text("", 40);
        assert_eq!(result, "");
    }

    #[test]
    fn test_fill_text_shorter_than_width() {
        let result = fill_text("hello world", 40);
        // Fits in one line, no wrapping
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_fill_text_needs_wrapping() {
        let result = fill_text("one two three four five", 10);
        // Words should wrap at width boundary
        assert!(result.contains('\n'));
    }

    #[test]
    fn test_fill_text_width_zero() {
        let result = fill_text("hello world", 0);
        // width=0 returns the text as-is
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_fill_text_single_long_word() {
        // A single word longer than width should still appear (no infinite loop)
        let result = fill_text("superlongword", 5);
        // The word doesn't split mid-word because split_inclusive(' ') won't break it
        assert!(result.contains("superlongword"));
    }

    #[test]
    fn test_fill_text_exact_width() {
        // "hello " is 6 chars, "world" is 5 chars; total 11
        let result = fill_text("hello world", 11);
        // Should fit in one line
        assert!(!result.contains('\n'));
    }

    // --- command_col_width with narrow widths (saturating_sub) ---

    #[test]
    fn test_command_col_width_compact_narrow() {
        // Compact: table_width.saturating_sub(6)
        // width=3 → 3-6 would underflow, saturating_sub gives 0
        assert_eq!(ColumnLayout::Compact.command_col_width(3), 0);
    }

    #[test]
    fn test_command_col_width_compact_zero() {
        assert_eq!(ColumnLayout::Compact.command_col_width(0), 0);
    }

    #[test]
    fn test_command_col_width_semi_narrow() {
        // SemiCompact: table_width.saturating_sub(12 + 6 + 6) = saturating_sub(24)
        // width=10 → 0
        assert_eq!(ColumnLayout::SemiCompact.command_col_width(10), 0);
    }

    #[test]
    fn test_command_col_width_full_narrow() {
        // Full: table_width.saturating_sub(64 + 6) = saturating_sub(70)
        // width=50 → 0
        assert_eq!(ColumnLayout::Full.command_col_width(50), 0);
    }

    #[test]
    fn test_command_col_width_full_exact() {
        // Full: 70 - 70 = 0
        assert_eq!(ColumnLayout::Full.command_col_width(70), 0);
    }

    #[test]
    fn test_command_col_width_full_one_over() {
        // Full: 71 - 70 = 1
        assert_eq!(ColumnLayout::Full.command_col_width(71), 1);
    }

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
            unique_counts: std::collections::HashMap::new(),
            filter_after: None,
            filter_before: None,
            filter_tag_id: None,
            filter_exit_code: None,
            filter_executor_type: None,
            start_date_input: None,
            end_date_input: None,
            tag_filter_input: None,
            exit_code_input: None,
            executor_filter_input: None,
            bookmarked_commands: bookmarked,
            filter_cwd: None,
            noted_entry_ids: noted,
            show_risk_in_search: show_risk,
            view: super::super::ViewOptions {
                unique_mode: false,
                context_boost: false,
                detail_pane_open: false,
                search_field: crate::models::SearchField::Command,
                current_cwd: None,
            },
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
        assert!(lines.len() >= 10, "should have at least 10 base lines");
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

        // Session ID should be truncated to first 8 chars
        assert!(
            text.contains("abcdefgh"),
            "should contain truncated session ID"
        );
        assert!(
            !text.contains("abcdefgh-1234"),
            "should NOT contain full session ID beyond 8 chars"
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
