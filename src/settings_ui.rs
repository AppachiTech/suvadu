use crate::config::{save_config, Config};
use crate::theme::theme;
use crossterm::event::{self, Event, KeyCode};
use ratatui::{
    backend::Backend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};
use std::io;

#[derive(PartialEq)]
enum InputMode {
    Normal,
    Editing,
    ConfirmQuit,
}

struct AppState {
    config: Config,
    current_tab: usize,
    selected_item: usize,
    input_mode: InputMode,
    input_buffer: String,
    // Auto-tag multi-field form
    auto_tag_path_input: String,
    auto_tag_name_input: String,
    auto_tag_focus: usize, // 0 = path, 1 = name
    // (Tab Index -> Number of items)
    tab_items: Vec<usize>,
    exclusion_list_state: ListState,
    save_status: Option<String>,
    dirty: bool,
}

impl AppState {
    fn new(config: Config) -> Self {
        Self {
            config,
            current_tab: 0,
            selected_item: 0,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            auto_tag_path_input: String::new(),
            auto_tag_name_input: String::new(),
            auto_tag_focus: 0,
            // Tab 0: Search (5 items), Tab 1: Shell (2 + theme), Tab 2: Exclusions (Dynamic), Tab 3: Auto Tags (Dynamic)
            tab_items: vec![5, 3, 0, 0],
            exclusion_list_state: ListState::default(),
            save_status: None,
            dirty: false,
        }
    }

    const fn next_tab(&mut self) {
        self.current_tab = (self.current_tab + 1) % self.tab_items.len();
        self.selected_item = 0;
    }

    const fn prev_tab(&mut self) {
        if self.current_tab > 0 {
            self.current_tab -= 1;
        } else {
            self.current_tab = self.tab_items.len() - 1;
        }
        self.selected_item = 0;
    }

    fn next_item(&mut self) {
        let max = if self.current_tab == 2 {
            self.config.exclusions.len()
        } else if self.current_tab == 3 {
            self.config.auto_tags.len()
        } else {
            self.tab_items[self.current_tab]
        };

        if max > 0 {
            self.selected_item = (self.selected_item + 1) % max;
            if self.current_tab == 2 || self.current_tab == 3 {
                self.exclusion_list_state.select(Some(self.selected_item));
            }
        }
    }

    fn prev_item(&mut self) {
        let max = if self.current_tab == 2 {
            self.config.exclusions.len()
        } else if self.current_tab == 3 {
            self.config.auto_tags.len()
        } else {
            self.tab_items[self.current_tab]
        };

        if max > 0 {
            if self.selected_item > 0 {
                self.selected_item -= 1;
            } else {
                self.selected_item = max - 1;
            }
            if self.current_tab == 2 || self.current_tab == 3 {
                self.exclusion_list_state.select(Some(self.selected_item));
            }
        }
    }

    fn toggle_bool(&mut self) {
        match (self.current_tab, self.selected_item) {
            (0, 1) => {
                self.config.search.show_unique_by_default =
                    !self.config.search.show_unique_by_default;
                self.dirty = true;
            }
            (0, 2) => {
                self.config.search.filter_by_current_session_tag =
                    !self.config.search.filter_by_current_session_tag;
                self.dirty = true;
            }
            (0, 3) => {
                self.config.search.context_boost = !self.config.search.context_boost;
                self.dirty = true;
            }
            (0, 4) => {
                self.config.search.show_detail_pane = !self.config.search.show_detail_pane;
                self.dirty = true;
            }
            (1, 0) => {
                self.config.shell.enable_arrow_navigation =
                    !self.config.shell.enable_arrow_navigation;
                self.dirty = true;
            }
            (1, 1) => {
                self.config.agent.show_risk_in_search = !self.config.agent.show_risk_in_search;
                self.dirty = true;
            }
            (1, 2) => {
                self.config.theme = self.config.theme.next();
                self.dirty = true;
                self.save_status = Some(format!(
                    "Theme set to '{}' — save & restart to apply",
                    self.config.theme
                ));
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_lines)]
    fn handle_input(&mut self, key: event::KeyEvent) -> bool {
        match self.input_mode {
            InputMode::ConfirmQuit => match key.code {
                KeyCode::Char('y') => {
                    if let Err(e) = save_config(&self.config) {
                        self.save_status = Some(format!("Error saving: {e}"));
                    } else {
                        return false;
                    }
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Char('n') => return false,
                KeyCode::Esc => self.input_mode = InputMode::Normal,
                _ => {}
            },
            InputMode::Normal => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    if self.dirty {
                        self.input_mode = InputMode::ConfirmQuit;
                    } else {
                        return false;
                    }
                }
                KeyCode::Char('s') => {
                    if let Err(e) = save_config(&self.config) {
                        self.save_status = Some(format!("Error saving: {e}"));
                    } else {
                        self.save_status = Some("Settings saved!".to_string());
                        self.dirty = false;
                    }
                }
                KeyCode::Tab => self.next_tab(),
                KeyCode::BackTab => self.prev_tab(),
                KeyCode::Down | KeyCode::Char('j') => self.next_item(),
                KeyCode::Up | KeyCode::Char('k') => self.prev_item(),
                KeyCode::Char('a') if self.current_tab == 2 => {
                    self.input_mode = InputMode::Editing;
                    self.input_buffer.clear();
                }
                KeyCode::Char('a') if self.current_tab == 3 => {
                    self.input_mode = InputMode::Editing;
                    self.auto_tag_path_input.clear();
                    self.auto_tag_name_input.clear();
                    self.auto_tag_focus = 0;
                }
                KeyCode::Char('d') if self.current_tab == 2 => {
                    if !self.config.exclusions.is_empty() {
                        self.config.exclusions.remove(self.selected_item);
                        self.dirty = true;
                        if self.selected_item >= self.config.exclusions.len()
                            && !self.config.exclusions.is_empty()
                        {
                            self.selected_item = self.config.exclusions.len() - 1;
                        } else if self.config.exclusions.is_empty() {
                            self.selected_item = 0;
                        }
                        self.exclusion_list_state
                            .select(if self.config.exclusions.is_empty() {
                                None
                            } else {
                                Some(self.selected_item)
                            });
                    }
                }
                KeyCode::Char('d') if self.current_tab == 3 => {
                    if !self.config.auto_tags.is_empty() {
                        self.dirty = true;
                        let mut auto_tags: Vec<_> = self.config.auto_tags.keys().cloned().collect();
                        auto_tags.sort();
                        if let Some(key) = auto_tags.get(self.selected_item) {
                            self.config.auto_tags.remove(key);
                        }

                        if self.selected_item >= self.config.auto_tags.len()
                            && !self.config.auto_tags.is_empty()
                        {
                            self.selected_item = self.config.auto_tags.len() - 1;
                        } else if self.config.auto_tags.is_empty() {
                            self.selected_item = 0;
                        }
                        self.exclusion_list_state
                            .select(if self.config.auto_tags.is_empty() {
                                None
                            } else {
                                Some(self.selected_item)
                            });
                    }
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    // Enter/Space toggles bools or enters edit mode for numbers/text
                    match (self.current_tab, self.selected_item) {
                        (0, 0) => {
                            // Page Limit
                            self.input_mode = InputMode::Editing;
                            self.input_buffer = self.config.search.page_limit.to_string();
                        }
                        _ => self.toggle_bool(),
                    }
                }
                _ => {}
            },
            InputMode::Editing => match key.code {
                KeyCode::Enter => {
                    if (self.current_tab, self.selected_item) == (0, 0) {
                        if let Ok(n) = self.input_buffer.parse::<usize>() {
                            self.config.search.page_limit = n.clamp(10, 5000);
                            self.dirty = true;
                            self.save_status = Some(format!(
                                "Page limit set to {}",
                                self.config.search.page_limit
                            ));
                        } else {
                            self.save_status = Some("Invalid number".to_string());
                        }
                        self.input_mode = InputMode::Normal;
                    } else if self.current_tab == 2 && !self.input_buffer.is_empty() {
                        self.config.exclusions.push(self.input_buffer.clone());
                        self.dirty = true;
                        self.save_status = Some(format!("Added exclusion: {}", self.input_buffer));
                        // Select the new item
                        self.selected_item = self.config.exclusions.len() - 1;
                        self.exclusion_list_state.select(Some(self.selected_item));
                        self.input_mode = InputMode::Normal;
                    } else if self.current_tab == 3 {
                        // Auto-tag dual-field form
                        if self.auto_tag_focus == 0 {
                            // Move from Path to Tag
                            self.auto_tag_focus = 1;
                        } else {
                            // Submit
                            if !self.auto_tag_path_input.is_empty()
                                && !self.auto_tag_name_input.is_empty()
                            {
                                self.config.auto_tags.insert(
                                    self.auto_tag_path_input.trim().to_string(),
                                    self.auto_tag_name_input.trim().to_string(),
                                );
                                self.dirty = true;
                                self.save_status = Some(format!(
                                    "Added auto-tag: {} -> {}",
                                    self.auto_tag_path_input.trim(),
                                    self.auto_tag_name_input.trim()
                                ));
                                // Select the newly added item (sorted position)
                                let path_key = self.auto_tag_path_input.trim().to_string();
                                let mut sorted_keys: Vec<_> =
                                    self.config.auto_tags.keys().cloned().collect();
                                sorted_keys.sort();
                                self.selected_item =
                                    sorted_keys.iter().position(|k| k == &path_key).unwrap_or(0);
                                self.exclusion_list_state.select(Some(self.selected_item));
                                self.input_mode = InputMode::Normal;
                            } else {
                                self.save_status =
                                    Some("Both Path and Tag are required".to_string());
                            }
                        }
                    } else {
                        self.input_mode = InputMode::Normal;
                    }
                }
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Tab if self.current_tab == 3 => {
                    // Toggle focus between path and tag
                    self.auto_tag_focus = 1 - self.auto_tag_focus;
                }
                KeyCode::Char(c) => {
                    const MAX_SETTINGS_INPUT: usize = 500;
                    if self.current_tab == 3 {
                        if self.auto_tag_focus == 0 {
                            if self.auto_tag_path_input.len() < MAX_SETTINGS_INPUT {
                                self.auto_tag_path_input.push(c);
                            }
                        } else if self.auto_tag_name_input.len() < MAX_SETTINGS_INPUT {
                            self.auto_tag_name_input.push(c);
                        }
                    } else if self.input_buffer.len() < MAX_SETTINGS_INPUT {
                        self.input_buffer.push(c);
                    }
                }
                KeyCode::Backspace => {
                    if self.current_tab == 3 {
                        if self.auto_tag_focus == 0 {
                            self.auto_tag_path_input.pop();
                        } else {
                            self.auto_tag_name_input.pop();
                        }
                    } else {
                        self.input_buffer.pop();
                    }
                }
                _ => {}
            },
        }
        true
    }
}

pub fn run_settings_ui<B: Backend>(terminal: &mut Terminal<B>, config: Config) -> io::Result<()>
where
    io::Error: From<B::Error>,
{
    let mut app = AppState::new(config);

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if let Event::Key(key) = event::read()? {
            if !app.handle_input(key) {
                return Ok(());
            }
        }
    }
}

fn ui(f: &mut ratatui::Frame, app: &mut AppState) {
    let size = f.area();
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(
            [
                Constraint::Length(1), // Minimalist Header
                Constraint::Min(0),    // Main content (sidebar + panel)
                Constraint::Length(2), // Status/Help
            ]
            .as_ref(),
        )
        .split(size);

    let t = theme();

    // Minimalist Header
    let branding = Line::from(vec![Span::styled(
        "Suvadu Settings",
        Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
    )]);

    let title = Paragraph::new(branding).alignment(ratatui::layout::Alignment::Center);
    f.render_widget(title, main_chunks[0]);

    // Horizontal split: Sidebar (25%) + Content (75%)
    let horizontal_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)].as_ref())
        .split(main_chunks[1]);

    // Render sidebar (category list)
    render_sidebar(f, app, horizontal_chunks[0]);

    // Render content panel with description
    render_content_panel(f, app, horizontal_chunks[1]);

    // Badge-style Footer
    let status_text = app
        .save_status
        .as_ref()
        .map_or_else(String::new, |msg| format!("{msg}  "));

    let badge_key = Style::default().bg(t.badge_bg).fg(t.text);
    let badge_label = Style::default().fg(t.text_secondary);

    let mut help_badges = match app.input_mode {
        InputMode::Normal => vec![
            Span::styled(" q ", badge_key),
            Span::styled(" Quit  ", badge_label),
            Span::styled(" s ", badge_key),
            Span::styled(" Save  ", badge_label),
            Span::styled(" ↑/↓ ", badge_key),
            Span::styled(" Navigate  ", badge_label),
        ],
        InputMode::Editing => vec![
            Span::styled(" Enter ", badge_key),
            Span::styled(" Confirm  ", badge_label),
            Span::styled(" Esc ", badge_key),
            Span::styled(" Cancel  ", badge_label),
        ],
        InputMode::ConfirmQuit => vec![
            Span::styled(
                " Unsaved changes! ",
                Style::default().fg(t.warning).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" y ", badge_key),
            Span::styled(" Save & Quit  ", badge_label),
            Span::styled(" n ", badge_key),
            Span::styled(" Discard & Quit  ", badge_label),
            Span::styled(" Esc ", badge_key),
            Span::styled(" Cancel  ", badge_label),
        ],
    };

    if app.input_mode == InputMode::Normal && (app.current_tab == 2 || app.current_tab == 3) {
        help_badges.push(Span::styled(" a ", badge_key));
        help_badges.push(Span::styled(" Add  ", badge_label));
        help_badges.push(Span::styled(" d ", badge_key));
        help_badges.push(Span::styled(" Delete  ", badge_label));
    } else if app.input_mode == InputMode::Normal {
        help_badges.push(Span::styled(" Space ", badge_key));
        help_badges.push(Span::styled(" Toggle/Edit  ", badge_label));
    }

    if !status_text.is_empty() {
        help_badges.push(Span::styled(
            format!(" {status_text} "),
            Style::default().fg(t.success).add_modifier(Modifier::BOLD),
        ));
    }

    let help_line = Line::from(help_badges);
    let status = Paragraph::new(help_line).block(
        Block::default()
            .borders(Borders::TOP)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border)),
    );
    f.render_widget(status, main_chunks[2]);

    // Render input popup if editing
    if app.input_mode == InputMode::Editing {
        if app.current_tab == 3 {
            render_auto_tag_popup(f, app);
        } else {
            render_input_popup(f, &app.input_buffer);
        }
    }
}

fn setting_toggle<'a>(label: &str, enabled: bool, selected: bool) -> ListItem<'a> {
    let t = theme();
    let icon = if enabled { "✔" } else { "○" };
    let icon_color = if enabled { t.success } else { t.text_muted };
    let arrow = if selected { " <<" } else { "" };
    let text = Line::from(vec![
        Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
        Span::styled(
            format!("{label}{arrow}"),
            Style::default().fg(if selected { t.text } else { t.text_secondary }),
        ),
    ]);
    ListItem::new(text)
}

fn setting_item<'a>(label: &str, value: &str, selected: bool, _editable: bool) -> ListItem<'a> {
    let t = theme();
    let arrow = if selected { " <<" } else { "" };
    let text = Line::from(vec![
        Span::styled(
            format!(" {label}: "),
            Style::default().fg(if selected { t.text } else { t.text_secondary }),
        ),
        Span::styled(
            value.to_string(),
            Style::default().fg(t.primary).add_modifier(Modifier::BOLD),
        ),
        Span::styled(arrow.to_string(), Style::default().fg(t.text_muted)),
    ]);
    ListItem::new(text)
}

fn render_sidebar(f: &mut ratatui::Frame, app: &AppState, area: Rect) {
    let t = theme();
    let categories = [
        ("Search", "magnifying glass"),
        ("Shell", "terminal"),
        ("Exclusions", "filter"),
        ("Auto Tags", "tag"),
    ];
    let items: Vec<ListItem> = categories
        .iter()
        .enumerate()
        .map(|(i, (cat, _))| {
            let (prefix, style) = if i == app.current_tab {
                (
                    " > ",
                    Style::default()
                        .bg(t.selection_bg)
                        .fg(t.selection_fg)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("   ", Style::default().fg(t.text_secondary))
            };
            ListItem::new(format!("{prefix}{cat}")).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(t.border))
            .title(" Tab/S-Tab "),
    );
    f.render_widget(list, area);
}

fn render_content_panel(f: &mut ratatui::Frame, app: &mut AppState, area: Rect) {
    // Split content area: Main content (90%) + Description (10%)
    let content_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(90), Constraint::Percentage(10)].as_ref())
        .split(area);

    // Render main content based on current tab
    match app.current_tab {
        0 => render_search_tab(f, app, content_chunks[0]),
        1 => render_shell_tab(f, app, content_chunks[0]),
        2 => render_exclusions_tab(f, app, content_chunks[0]),
        3 => render_auto_tags_tab(f, app, content_chunks[0]),
        _ => {}
    }

    // Render description pane
    let t = theme();
    let description = get_setting_description(app.current_tab, app.selected_item);
    let desc_paragraph = Paragraph::new(description)
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(t.text_secondary))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title("Description"),
        );
    f.render_widget(desc_paragraph, content_chunks[1]);
}

const fn get_setting_description(tab: usize, item: usize) -> &'static str {
    match (tab, item) {
        (0, 0) => "Number of results to show per page in search (10-5000)",
        (0, 1) => "Show only unique commands by default (deduplicate history)",
        (0, 2) => "Filter search results by the current session's tag",
        (0, 3) => "Boost results from the current directory higher in search (toggle with ^S)",
        (0, 4) => "Show the detail preview pane when opening search (toggle with Tab)",
        (1, 0) => "Bind Up/Down arrow keys to cycle through command history",
        (1, 1) => "Show risk assessment badges in the search detail pane for agent commands",
        (1, 2) => "Color theme: dark (RGB for dark terminals), light (RGB for light terminals), terminal (ANSI 16 — adapts to your scheme). Save & restart to apply.",
        _ => "Use [a] to add new items, [d] to delete selected items",
    }
}

fn render_search_tab(f: &mut ratatui::Frame, app: &AppState, area: Rect) {
    let t = theme();
    let items: Vec<ListItem> = vec![
        setting_item(
            "Page Limit",
            &app.config.search.page_limit.to_string(),
            app.selected_item == 0,
            false,
        ),
        setting_toggle(
            "Show Unique Commands by Default",
            app.config.search.show_unique_by_default,
            app.selected_item == 1,
        ),
        setting_toggle(
            "Filter by Current Session Tag",
            app.config.search.filter_by_current_session_tag,
            app.selected_item == 2,
        ),
        setting_toggle(
            "Context Boost (Smart Mode)",
            app.config.search.context_boost,
            app.selected_item == 3,
        ),
        setting_toggle(
            "Show Detail Pane by Default",
            app.config.search.show_detail_pane,
            app.selected_item == 4,
        ),
    ];

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(" Search Preferences "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD).fg(t.primary))
        .highlight_symbol(" > ");
    f.render_widget(list, area);
}

fn render_shell_tab(f: &mut ratatui::Frame, app: &AppState, area: Rect) {
    let t = theme();
    let items: Vec<ListItem> = vec![
        setting_toggle(
            "Enable Arrow Key Navigation",
            app.config.shell.enable_arrow_navigation,
            app.selected_item == 0,
        ),
        setting_toggle(
            "Show Risk in Search Detail",
            app.config.agent.show_risk_in_search,
            app.selected_item == 1,
        ),
        setting_item(
            "Theme",
            app.config.theme.as_str(),
            app.selected_item == 2,
            false,
        ),
    ];

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(" Shell & Display "),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD).fg(t.primary))
        .highlight_symbol(" > ");
    f.render_widget(list, area);
}

fn render_exclusions_tab(f: &mut ratatui::Frame, app: &mut AppState, area: Rect) {
    let t = theme();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(4)].as_ref())
        .split(area);

    if app.config.exclusions.is_empty() {
        let text = Paragraph::new(
            "No exclusions defined.\nPress 'a' to add a regex pattern.\n\nExamples:\n  ^ls$       (Exact match)\n  password   (Substring match)\n  ^git .*    (Target specific tool)",
        )
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(t.text_secondary))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(" Exclusions "),
        );
        f.render_widget(text, chunks[0]);
    } else {
        let items: Vec<ListItem> = app
            .config
            .exclusions
            .iter()
            .map(|e| ListItem::new(format!("  {e}")))
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border))
                    .title(" Exclusions (Regex) "),
            )
            .highlight_style(
                Style::default()
                    .bg(t.selection_bg)
                    .fg(t.selection_fg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(" > ");

        if app.exclusion_list_state.selected().is_none() && !app.config.exclusions.is_empty() {
            app.exclusion_list_state.select(Some(0));
        }

        f.render_stateful_widget(list, chunks[0], &mut app.exclusion_list_state);
    }

    let description = Paragraph::new(
        "Automatically ignore commands matching these patterns. Useful for secrets or noise.",
    )
    .wrap(Wrap { trim: true })
    .style(Style::default().fg(t.text_muted));
    f.render_widget(description, chunks[1]);
}

fn render_auto_tags_tab(f: &mut ratatui::Frame, app: &mut AppState, area: Rect) {
    let t = theme();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(4)].as_ref())
        .split(area);

    if app.config.auto_tags.is_empty() {
        let text = Paragraph::new(
            "No auto-tags defined.\nPress 'a' to add a mapping.\n\nExample:\n  Path: /path/to/work\n  Tag: work",
        )
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(t.text_secondary))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border))
                .title(" Auto Tags "),
        );
        f.render_widget(text, chunks[0]);
    } else {
        let mut auto_tags: Vec<_> = app.config.auto_tags.iter().collect();
        auto_tags.sort_by_key(|&(k, _)| k);

        let items: Vec<ListItem> = auto_tags
            .iter()
            .map(|(path, tag)| ListItem::new(format!("  {path} -> {tag}")))
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(t.border))
                    .title(" Auto Tags (Path -> Tag) "),
            )
            .highlight_style(
                Style::default()
                    .bg(t.selection_bg)
                    .fg(t.selection_fg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(" > ");

        if app.exclusion_list_state.selected().is_none() && !app.config.auto_tags.is_empty() {
            app.exclusion_list_state.select(Some(0));
        }

        f.render_stateful_widget(list, chunks[0], &mut app.exclusion_list_state);
    }

    let description = Paragraph::new(
        "Automatically assign a tag to any command executed inside these directories. Useful for separating Work vs Personal context."
    )
    .wrap(Wrap { trim: true })
    .style(Style::default().fg(t.text_muted));
    f.render_widget(description, chunks[1]);
}

fn render_auto_tag_popup(f: &mut ratatui::Frame, app: &AppState) {
    let t = theme();
    let area = centered_rect(60, 30, f.area());
    let block = Block::default()
        .title(" Add Auto Tag ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.primary))
        .style(Style::default().bg(t.bg_elevated));

    f.render_widget(Clear, area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints(
            [
                Constraint::Length(3), // Path field
                Constraint::Length(3), // Tag field
                Constraint::Min(0),    // Help text
            ]
            .as_ref(),
        )
        .split(area);

    let path_border = if app.auto_tag_focus == 0 {
        t.border_focus
    } else {
        t.border
    };
    let path_text = if app.auto_tag_focus == 0 {
        t.text
    } else {
        t.text_secondary
    };
    let path_input = Paragraph::new(app.auto_tag_path_input.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(path_border))
                .title(format!(
                    "Path (e.g., ~/work){}",
                    if app.auto_tag_focus == 0 { " *" } else { "" }
                )),
        )
        .style(Style::default().fg(path_text));
    f.render_widget(path_input, chunks[0]);

    let tag_border = if app.auto_tag_focus == 1 {
        t.border_focus
    } else {
        t.border
    };
    let tag_text = if app.auto_tag_focus == 1 {
        t.text
    } else {
        t.text_secondary
    };
    let tag_input = Paragraph::new(app.auto_tag_name_input.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(tag_border))
                .title(format!(
                    "Tag Name (e.g., work){}",
                    if app.auto_tag_focus == 1 { " *" } else { "" }
                )),
        )
        .style(Style::default().fg(tag_text));
    f.render_widget(tag_input, chunks[1]);

    let help = Paragraph::new("Tab: switch fields  |  Enter: next/submit  |  Esc: cancel")
        .style(Style::default().fg(t.text_muted))
        .alignment(ratatui::layout::Alignment::Center);
    f.render_widget(help, chunks[2]);
}

fn render_input_popup(f: &mut ratatui::Frame, input: &str) {
    let t = theme();
    let area = centered_rect(60, 20, f.area());
    let block = Block::default()
        .title(" Enter Value ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_focus))
        .style(Style::default().bg(t.bg_elevated));
    let text = Paragraph::new(input)
        .block(block)
        .style(Style::default().fg(t.text).add_modifier(Modifier::BOLD));
    f.render_widget(Clear, area);
    f.render_widget(text, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn test_app_state_navigation() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Initial state
        assert_eq!(app.current_tab, 0);
        assert_eq!(app.selected_item, 0);

        // Tab navigation
        app.next_tab();
        assert_eq!(app.current_tab, 1);
        app.next_tab();
        assert_eq!(app.current_tab, 2);
        app.next_tab();
        assert_eq!(app.current_tab, 3);
        app.next_tab();
        assert_eq!(app.current_tab, 0); // Cycle back

        // Item navigation (Tab 0 has 5 items)
        app.next_item();
        assert_eq!(app.selected_item, 1);
        app.next_item();
        assert_eq!(app.selected_item, 2);
        app.next_item();
        assert_eq!(app.selected_item, 3);
        app.next_item();
        assert_eq!(app.selected_item, 4);
        app.next_item();
        assert_eq!(app.selected_item, 0); // Cycle back
    }

    #[test]
    fn test_toggle_bool() {
        let mut config = Config::default();
        config.search.show_unique_by_default = false;
        config.shell.enable_arrow_navigation = true;

        let mut app = AppState::new(config);

        // Toggle Search Unique (Tab 0, Item 1)
        app.current_tab = 0;
        app.selected_item = 1;
        app.toggle_bool();
        assert!(app.config.search.show_unique_by_default);

        app.toggle_bool();
        assert!(!app.config.search.show_unique_by_default);

        // Toggle Arrow Navigation (Tab 1, Item 0)
        app.current_tab = 1;
        app.selected_item = 0;
        app.toggle_bool();
        assert!(!app.config.shell.enable_arrow_navigation);
    }

    #[test]
    fn test_theme_cycle_in_settings() {
        use crate::theme::ThemeName;

        let config = Config::default();
        let mut app = AppState::new(config);

        // Theme is Tab 1, Item 2
        app.current_tab = 1;
        app.selected_item = 2;

        assert_eq!(app.config.theme, ThemeName::Dark);

        app.toggle_bool();
        assert_eq!(app.config.theme, ThemeName::Light);
        assert!(app.dirty);

        app.toggle_bool();
        assert_eq!(app.config.theme, ThemeName::Terminal);

        app.toggle_bool();
        assert_eq!(app.config.theme, ThemeName::Dark);
    }
}
