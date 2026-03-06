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

use super::{centered_rect, fill_text, SearchApp};

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
            let unique_badge = if self.unique_mode { " [unique]" } else { "" };

            let search_border_color = if self.filter_mode {
                t.border
            } else {
                t.border_focus
            };
            let search_title = if self.filter_mode {
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
            if self.detail_pane_open {
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
            if self.filter_mode {
                self.render_filter_popup(f, f.area());
            } else if self.goto_dialog_open {
                self.render_goto_dialog(f, f.area());
            } else if self.delete_dialog_open {
                self.render_delete_dialog(f, f.area());
            } else if self.tag_dialog_open {
                self.render_tag_dialog(f, f.area());
            } else if self.note_dialog_open {
                self.render_note_dialog(f, f.area());
            }
        })?;

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn render_footer(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();
        let total_pages = self.total_items.div_ceil(self.page_size).max(1);
        let progress_pct = if total_pages > 0 {
            (self.page * 100) / total_pages
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

        let badge_key_style = Style::default().bg(t.badge_bg).fg(t.text);
        let badge_label_style = Style::default().fg(t.text_secondary);

        let mut help_badges = vec![
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
                if self.unique_mode {
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
                if self.filter_cwd.is_some() {
                    " All Dirs  "
                } else {
                    " Here  "
                },
                badge_label_style,
            ),
            Span::styled(" ^S ", badge_key_style),
            Span::styled(
                if self.context_boost {
                    " Recent  "
                } else {
                    " Smart  "
                },
                badge_label_style,
            ),
            Span::styled(" Tab ", badge_key_style),
            Span::styled(
                if self.detail_pane_open {
                    " Hide  "
                } else {
                    " Detail  "
                },
                badge_label_style,
            ),
        ];

        // Active filter badges
        if self.filter_after.is_some() || self.filter_before.is_some() {
            help_badges.push(Span::styled(
                " date ",
                Style::default().bg(t.info).fg(Color::Black),
            ));
            help_badges.push(Span::raw(" "));
        }
        if self.filter_tag_id.is_some() {
            help_badges.push(Span::styled(
                " tag ",
                Style::default().bg(t.warning).fg(Color::Black),
            ));
            help_badges.push(Span::raw(" "));
        }
        if self.filter_exit_code.is_some() {
            help_badges.push(Span::styled(
                " exit ",
                Style::default().bg(t.error).fg(Color::White),
            ));
            help_badges.push(Span::raw(" "));
        }
        if self.filter_executor_type.is_some() {
            help_badges.push(Span::styled(
                " exec ",
                Style::default()
                    .bg(Color::Rgb(147, 51, 234))
                    .fg(Color::White),
            ));
            help_badges.push(Span::raw(" "));
        }
        if self.filter_cwd.is_some() {
            help_badges.push(Span::styled(
                " dir ",
                Style::default()
                    .bg(Color::Rgb(6, 182, 212))
                    .fg(Color::Black),
            ));
            help_badges.push(Span::raw(" "));
        }
        if self.context_boost {
            help_badges.push(Span::styled(
                " smart ",
                Style::default().bg(t.success).fg(Color::Black),
            ));
            help_badges.push(Span::raw(" "));
        }

        // Page progress
        let page_info = format!(" {}/{} ({progress_pct}%) ", self.page, total_pages);
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

    #[allow(clippy::cast_precision_loss)]
    #[allow(clippy::too_many_lines)]
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

        // Time (12) + Session/Tag (16) + Executor (10) + Path (12) + Status (6) + Duration (8) = 64
        let fixed_width: u16 = 12 + 16 + 10 + 12 + 6 + 8;
        // When the terminal is too narrow for all columns, show only the Command column
        let compact = table_area.width < 100;
        let command_col_width = if compact {
            table_area.width.saturating_sub(6)
        } else {
            table_area.width.saturating_sub(fixed_width + 6)
        };

        let rows: Vec<Row> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let is_selected = self.table_state.selected() == Some(i);

                #[allow(clippy::cast_precision_loss)]
                let duration_secs = entry.duration_ms as f64 / 1000.0;

                // Normalize: if timestamp looks like microseconds (16+ digits), convert to ms
                let ts_ms = if entry.started_at > 9_999_999_999_999 {
                    entry.started_at / 1000
                } else {
                    entry.started_at
                };
                let start_time = Local
                    .timestamp_millis_opt(ts_ms)
                    .single()
                    .unwrap_or_else(|| {
                        chrono::DateTime::from_timestamp_millis(0)
                            .unwrap()
                            .with_timezone(&Local)
                    });
                let time_str = start_time.format("%m-%d %H:%M").to_string();
                let duration_str = format!("{duration_secs:.1}s");

                // Combine session and tag
                let session_short = &entry.session_id[..8];

                let session_tag_display = entry.tag_name.as_ref().map_or_else(
                    || session_short.to_string(),
                    |tag| format!("{session_short} ({tag})"),
                );

                // Wrap session/tag if selected
                let st_display = if is_selected {
                    fill_text(&session_tag_display, 25)
                } else {
                    session_tag_display
                };

                // Shorten path to ../last_folder (full path shown in detail pane)
                let path_display = std::path::Path::new(&entry.cwd)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map_or_else(|| entry.cwd.clone(), |last| format!("../{last}"));

                // Format executor with icon
                let executor_icon = match entry.executor_type.as_deref() {
                    Some("human") => "👤",
                    Some("bot" | "agent") => "🤖",
                    Some("ide") => "💻",
                    Some("ci") => "⚙️",
                    Some("programmatic") => "⚡",
                    _ => "❓",
                };
                let executor_display = entry.executor.as_ref().map_or_else(
                    || executor_icon.to_string(),
                    |exec_name| format!("{executor_icon} {exec_name}"),
                );

                // Get occurrence count if in unique mode
                let count_display = if self.unique_mode {
                    format!(
                        "({}) ",
                        self.unique_counts.get(&entry.id.unwrap_or(0)).unwrap_or(&1)
                    )
                } else {
                    String::new()
                };

                // Bookmark and note indicators
                let bookmark_prefix = if self.bookmarked_commands.contains(&entry.command) {
                    "★ "
                } else {
                    ""
                };
                let note_prefix = if entry
                    .id
                    .is_some_and(|id| self.noted_entry_ids.contains(&id))
                {
                    "📝"
                } else {
                    ""
                };

                // Command text
                let cmd_text = format!(
                    "{}{}{}{}",
                    note_prefix, bookmark_prefix, count_display, entry.command
                );

                // Syntax highlighting for ALL rows
                let command_display = if is_selected {
                    Self::highlight_command(&cmd_text, true, command_col_width as usize)
                } else {
                    Self::highlight_command(&cmd_text, false, 0)
                };

                let cmd_height = u16::try_from(command_display.lines.len())
                    .unwrap_or(1)
                    .max(1);
                let st_height = u16::try_from(st_display.lines().count())
                    .unwrap_or(1)
                    .max(1);

                let height = cmd_height.max(st_height);

                // Check if this entry is from the current directory (for context boost highlight)
                let is_local = self.context_boost
                    && self
                        .current_cwd
                        .as_deref()
                        .is_some_and(|cwd| entry.cwd == cwd);

                // Styles: prominent selection with blue background
                let (
                    bg_style,
                    time_style,
                    session_style,
                    executor_style,
                    path_style,
                    duration_style,
                ) = if is_selected {
                    let sel = Style::default().bg(t.selection_bg);
                    (
                        sel,
                        sel.fg(t.selection_fg).add_modifier(Modifier::BOLD),
                        sel.fg(t.info).add_modifier(Modifier::BOLD),
                        sel.fg(t.warning).add_modifier(Modifier::BOLD),
                        if is_local {
                            sel.fg(t.primary).add_modifier(Modifier::BOLD)
                        } else {
                            sel.fg(t.selection_fg)
                        },
                        sel.fg(t.text_secondary),
                    )
                } else {
                    let base = Style::default();
                    (
                        base,
                        base.fg(t.text_muted),
                        base.fg(t.info),
                        base.fg(t.warning),
                        if is_local {
                            base.fg(t.primary)
                        } else {
                            base.fg(t.text_secondary)
                        },
                        base.fg(t.text_muted),
                    )
                };

                let exit_display = match entry.exit_code {
                    Some(0) => "✔".to_string(),
                    Some(code) => format!("✘ {code}"),
                    None => "○".to_string(),
                };
                let exit_style_item = match entry.exit_code {
                    Some(0) => bg_style.fg(t.success),
                    Some(_) => bg_style.fg(t.error),
                    None => bg_style.fg(t.text_muted),
                };

                if self.unique_mode || compact {
                    Row::new(vec![Cell::from(command_display)])
                        .height(height)
                        .style(bg_style)
                } else {
                    Row::new(vec![
                        Cell::from(time_str).style(time_style),
                        Cell::from(command_display),
                        Cell::from(st_display).style(session_style),
                        Cell::from(executor_display).style(executor_style),
                        Cell::from(path_display).style(path_style),
                        Cell::from(exit_display).style(exit_style_item),
                        Cell::from(duration_str).style(duration_style),
                    ])
                    .height(height)
                    .style(bg_style)
                }
            })
            .collect();

        let widths = if self.unique_mode || compact {
            vec![Constraint::Percentage(100)]
        } else {
            vec![
                Constraint::Length(12), // Time
                Constraint::Min(10),    // Command
                Constraint::Length(16), // Session/Tag
                Constraint::Length(10), // Executor
                Constraint::Length(12), // Path (../folder)
                Constraint::Length(6),  // Status
                Constraint::Length(8),  // Duration
            ]
        };

        let header_row = if self.unique_mode || compact {
            Row::new(vec!["Command".to_string()])
        } else {
            Row::new(vec![
                "Time".to_string(),
                "Command".to_string(),
                "Session/Tag".to_string(),
                "Executor".to_string(),
                "Path".to_string(),
                "Status".to_string(),
                "Duration".to_string(),
            ])
        };

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
                    .title({
                        if self.total_items == 0 {
                            "History (0/0)".to_string()
                        } else {
                            let start_index = (self.page - 1) * self.page_size + 1;
                            let end_index = start_index + self.entries.len() - 1;
                            format!(
                                "History ({}-{} / {})",
                                start_index, end_index, self.total_items
                            )
                        }
                    }),
            )
            .highlight_symbol(" > ");

        f.render_stateful_widget(table, table_area, &mut self.table_state);

        // Scrollbar
        let total_pages = self.total_items.div_ceil(self.page_size).max(1);
        let mut scrollbar_state =
            ScrollbarState::new(total_pages).position(self.page.saturating_sub(1));
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_style(Style::default().fg(t.primary_dim))
            .track_style(Style::default().fg(t.border));
        f.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn render_detail_pane(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let entry = self.get_selected_entry();

        let block = Block::default()
            .title(" Detail ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border));

        if let Some(entry) = entry {
            let label_style = Style::default()
                .fg(t.text_secondary)
                .add_modifier(Modifier::BOLD);
            let value_style = Style::default().fg(t.text);

            // Timestamp
            let ts_ms = if entry.started_at > 9_999_999_999_999 {
                entry.started_at / 1000
            } else {
                entry.started_at
            };
            let start_time = Local
                .timestamp_millis_opt(ts_ms)
                .single()
                .unwrap_or_else(|| {
                    chrono::DateTime::from_timestamp_millis(0)
                        .unwrap()
                        .with_timezone(&Local)
                });
            let time_str = start_time.format("%Y-%m-%d %H:%M:%S").to_string();

            #[allow(clippy::cast_precision_loss)]
            let duration_secs = entry.duration_ms as f64 / 1000.0;

            let exit_str = match entry.exit_code {
                Some(0) => "✔ 0 (success)".to_string(),
                Some(code) => format!("✘ {code} (failed)"),
                None => "○ (unknown)".to_string(),
            };

            let executor_str = match (&entry.executor_type, &entry.executor) {
                (Some(t), Some(n)) => format!("{t}: {n}"),
                (Some(t), None) => t.clone(),
                _ => "unknown".to_string(),
            };

            let session_str = &entry.session_id[..8.min(entry.session_id.len())];
            let tag_str = entry.tag_name.as_deref().unwrap_or("none");

            let is_bookmarked = self.bookmarked_commands.contains(&entry.command);
            let has_note = entry
                .id
                .is_some_and(|id| self.noted_entry_ids.contains(&id));

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
                    Span::styled(session_str.to_string(), value_style),
                ]),
                Line::from(vec![
                    Span::styled("Tag      ", label_style),
                    Span::styled(tag_str.to_string(), value_style),
                ]),
                Line::from(vec![
                    Span::styled("Executor ", label_style),
                    Span::styled(executor_str, value_style),
                ]),
            ];

            // Risk assessment
            if self.show_risk_in_search {
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

    #[allow(clippy::too_many_lines)]
    pub(super) fn render_filter_popup(&self, f: &mut ratatui::Frame, area: Rect) {
        let t = theme();

        let block = Block::default()
            .title(" Filters ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.primary).add_modifier(Modifier::BOLD))
            .style(Style::default().bg(t.bg_elevated));

        let popup_area = centered_rect(60, 50, area);
        f.render_widget(Clear, popup_area);
        f.render_widget(block, popup_area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints(
                [
                    Constraint::Length(1), // Progress indicator
                    Constraint::Length(3), // Start Date
                    Constraint::Length(3), // End Date
                    Constraint::Length(3), // Tag
                    Constraint::Length(3), // Exit Code
                    Constraint::Length(3), // Executor
                    Constraint::Min(0),    // Help
                ]
                .as_ref(),
            )
            .split(popup_area);

        // Field progress indicator
        let progress_blocks: Vec<Span> = (0..5)
            .map(|i| {
                if i == self.focus_index {
                    Span::styled(" ■ ", Style::default().fg(t.primary))
                } else {
                    Span::styled(" □ ", Style::default().fg(t.text_muted))
                }
            })
            .collect();
        let field_names = ["Start Date", "End Date", "Tag", "Exit Code", "Executor"];
        let mut progress_line = progress_blocks;
        progress_line.push(Span::styled(
            format!(
                "  Field {} of 5: {}",
                self.focus_index + 1,
                field_names[self.focus_index]
            ),
            Style::default().fg(t.text_secondary),
        ));
        f.render_widget(
            Paragraph::new(Line::from(progress_line)).alignment(Alignment::Center),
            chunks[0],
        );

        let fields: Vec<(&str, &str, &str)> = vec![
            (
                "Start Date (After)",
                &self.start_date_input,
                "e.g. today, yesterday, 2024-01-15",
            ),
            (
                "End Date (Before)",
                &self.end_date_input,
                "e.g. today, 3 days ago, 2024-12-31",
            ),
            ("Tag Name", &self.tag_filter_input, "e.g. work, personal"),
            (
                "Exit Code",
                &self.exit_code_input,
                "e.g. 0 (success), 1 (failure)",
            ),
            (
                "Executor",
                &self.executor_filter_input,
                "e.g. human, agent, ide, ci, vscode",
            ),
        ];

        for (i, (title, value, hint)) in fields.iter().enumerate() {
            let is_focused = self.focus_index == i;
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

        let help_text = Paragraph::new("Tab/S-Tab: switch fields  |  Enter: apply  |  Esc: cancel")
            .alignment(Alignment::Center)
            .style(Style::default().fg(t.text_muted));
        f.render_widget(help_text, chunks[6]);
    }

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
            .style(Style::default().bg(Color::Rgb(40, 10, 10)));

        // Show command preview
        let cmd_preview = self
            .get_selected_entry()
            .map(|e| {
                if e.command.len() > 50 {
                    format!("{}...", &e.command[..47])
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
        let total_pages = self.total_items.div_ceil(self.page_size).max(1);

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

        let input = Paragraph::new(self.goto_input.as_str())
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

        let input = Paragraph::new(self.note_input.as_str())
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

    fn highlight_command(
        command: &str,
        _is_selected: bool,
        width: usize,
    ) -> ratatui::text::Text<'static> {
        crate::util::highlight_command(command, width)
    }
}
