use crate::config::{save_config, Config};
use crate::theme::theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    backend::Backend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};
use std::io;

#[derive(Debug, PartialEq)]
enum InputMode {
    Normal,
    Editing,
    ConfirmQuit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Search,
    Shell,
    Exclusions,
    AutoTags,
}

impl SettingsTab {
    const ALL: &[Self] = &[Self::Search, Self::Shell, Self::Exclusions, Self::AutoTags];

    const fn index(self) -> usize {
        match self {
            Self::Search => 0,
            Self::Shell => 1,
            Self::Exclusions => 2,
            Self::AutoTags => 3,
        }
    }

    fn item_count(self, config: &Config) -> usize {
        match self {
            Self::Search => 5,
            Self::Shell => 3,
            Self::Exclusions => config.exclusions.len(),
            Self::AutoTags => config.auto_tags.len(),
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Search => Self::Shell,
            Self::Shell => Self::Exclusions,
            Self::Exclusions => Self::AutoTags,
            Self::AutoTags => Self::Search,
        }
    }

    const fn prev(self) -> Self {
        match self {
            Self::Search => Self::AutoTags,
            Self::Shell => Self::Search,
            Self::Exclusions => Self::Shell,
            Self::AutoTags => Self::Exclusions,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Search => "Search",
            Self::Shell => "Shell",
            Self::Exclusions => "Exclusions",
            Self::AutoTags => "Auto Tags",
        }
    }
}

struct AppState {
    config: Config,
    current_tab: SettingsTab,
    selected_item: usize,
    input_mode: InputMode,
    input_buffer: String,
    // Auto-tag multi-field form
    auto_tag_path_input: String,
    auto_tag_name_input: String,
    auto_tag_focus: usize, // 0 = path, 1 = name
    exclusion_list_state: ListState,
    auto_tag_list_state: ListState,
    save_status: Option<String>,
    dirty: bool,
}

impl AppState {
    fn new(config: Config) -> Self {
        Self {
            config,
            current_tab: SettingsTab::Search,
            selected_item: 0,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            auto_tag_path_input: String::new(),
            auto_tag_name_input: String::new(),
            auto_tag_focus: 0,
            exclusion_list_state: ListState::default(),
            auto_tag_list_state: ListState::default(),
            save_status: None,
            dirty: false,
        }
    }

    const fn next_tab(&mut self) {
        self.current_tab = self.current_tab.next();
        self.selected_item = 0;
        self.reset_list_states();
    }

    const fn prev_tab(&mut self) {
        self.current_tab = self.current_tab.prev();
        self.selected_item = 0;
        self.reset_list_states();
    }

    /// Reset list state selections on tab switch to avoid stale out-of-bounds indices.
    const fn reset_list_states(&mut self) {
        self.exclusion_list_state.select(None);
        self.auto_tag_list_state.select(None);
    }

    fn next_item(&mut self) {
        let max = self.current_tab.item_count(&self.config);

        if max > 0 {
            self.selected_item = (self.selected_item + 1) % max;
            match self.current_tab {
                SettingsTab::Exclusions => {
                    self.exclusion_list_state.select(Some(self.selected_item));
                }
                SettingsTab::AutoTags => {
                    self.auto_tag_list_state.select(Some(self.selected_item));
                }
                _ => {}
            }
        }
    }

    fn prev_item(&mut self) {
        let max = self.current_tab.item_count(&self.config);

        if max > 0 {
            if self.selected_item > 0 {
                self.selected_item -= 1;
            } else {
                self.selected_item = max - 1;
            }
            match self.current_tab {
                SettingsTab::Exclusions => {
                    self.exclusion_list_state.select(Some(self.selected_item));
                }
                SettingsTab::AutoTags => {
                    self.auto_tag_list_state.select(Some(self.selected_item));
                }
                _ => {}
            }
        }
    }

    fn toggle_bool(&mut self) {
        match (self.current_tab, self.selected_item) {
            (SettingsTab::Search, 1) => {
                self.config.search.show_unique_by_default =
                    !self.config.search.show_unique_by_default;
                self.dirty = true;
            }
            (SettingsTab::Search, 2) => {
                self.config.search.filter_by_current_session_tag =
                    !self.config.search.filter_by_current_session_tag;
                self.dirty = true;
            }
            (SettingsTab::Search, 3) => {
                self.config.search.context_boost = !self.config.search.context_boost;
                self.dirty = true;
            }
            (SettingsTab::Search, 4) => {
                self.config.search.show_detail_pane = !self.config.search.show_detail_pane;
                self.dirty = true;
            }
            (SettingsTab::Shell, 0) => {
                self.config.shell.enable_arrow_navigation =
                    !self.config.shell.enable_arrow_navigation;
                self.dirty = true;
            }
            (SettingsTab::Shell, 1) => {
                self.config.agent.show_risk_in_search = !self.config.agent.show_risk_in_search;
                self.dirty = true;
            }
            (SettingsTab::Shell, 2) => {
                self.config.theme = self.config.theme.next();
                self.dirty = true;
                // Apply immediately so the UI reflects the new theme
                crate::theme::init_theme(self.config.theme);
                self.save_status = Some(format!("Theme set to '{}'", self.config.theme));
            }
            _ => {}
        }
    }

    fn handle_input(&mut self, key: event::KeyEvent) -> bool {
        match self.input_mode {
            InputMode::ConfirmQuit => return self.handle_confirm_quit(key),
            InputMode::Normal => return self.handle_normal_input(key),
            InputMode::Editing => self.handle_editing_input(key),
        }
        true
    }

    fn handle_confirm_quit(&mut self, key: event::KeyEvent) -> bool {
        match key.code {
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
        }
        true
    }

    fn handle_normal_input(&mut self, key: event::KeyEvent) -> bool {
        match key.code {
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
            KeyCode::Char('a') if self.current_tab == SettingsTab::Exclusions => {
                self.input_mode = InputMode::Editing;
                self.input_buffer.clear();
            }
            KeyCode::Char('a') if self.current_tab == SettingsTab::AutoTags => {
                self.input_mode = InputMode::Editing;
                self.auto_tag_path_input.clear();
                self.auto_tag_name_input.clear();
                self.auto_tag_focus = 0;
            }
            KeyCode::Char('d') if self.current_tab == SettingsTab::Exclusions => {
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
            KeyCode::Char('d') if self.current_tab == SettingsTab::AutoTags => {
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
                    self.auto_tag_list_state
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
                    (SettingsTab::Search, 0) => {
                        // Page Limit
                        self.input_mode = InputMode::Editing;
                        self.input_buffer = self.config.search.page_limit.to_string();
                    }
                    _ => self.toggle_bool(),
                }
            }
            _ => {}
        }
        true
    }

    fn handle_editing_input(&mut self, key: event::KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                if (self.current_tab, self.selected_item) == (SettingsTab::Search, 0) {
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
                } else if self.current_tab == SettingsTab::Exclusions
                    && !self.input_buffer.is_empty()
                {
                    self.config.exclusions.push(self.input_buffer.clone());
                    self.dirty = true;
                    self.save_status = Some(format!("Added exclusion: {}", self.input_buffer));
                    // Select the new item
                    self.selected_item = self.config.exclusions.len() - 1;
                    self.exclusion_list_state.select(Some(self.selected_item));
                    self.input_mode = InputMode::Normal;
                } else if self.current_tab == SettingsTab::AutoTags {
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
                            self.auto_tag_list_state.select(Some(self.selected_item));
                            self.input_mode = InputMode::Normal;
                        } else {
                            self.save_status = Some("Both Path and Tag are required".to_string());
                        }
                    }
                } else {
                    self.input_mode = InputMode::Normal;
                }
            }
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Tab if self.current_tab == SettingsTab::AutoTags => {
                // Toggle focus between path and tag
                self.auto_tag_focus = 1 - self.auto_tag_focus;
            }
            KeyCode::Char(c) => {
                const MAX_SETTINGS_INPUT: usize = 500;
                if self.current_tab == SettingsTab::AutoTags {
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
                if self.current_tab == SettingsTab::AutoTags {
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
        }
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
            if key.kind != KeyEventKind::Press {
                continue;
            }
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

    if app.input_mode == InputMode::Normal
        && matches!(
            app.current_tab,
            SettingsTab::Exclusions | SettingsTab::AutoTags
        )
    {
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
        if app.current_tab == SettingsTab::AutoTags {
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
    let items: Vec<ListItem> = SettingsTab::ALL
        .iter()
        .map(|tab| {
            let (prefix, style) = if *tab == app.current_tab {
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
            ListItem::new(format!("{prefix}{}", tab.label())).style(style)
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
        SettingsTab::Search => render_search_tab(f, app, content_chunks[0]),
        SettingsTab::Shell => render_shell_tab(f, app, content_chunks[0]),
        SettingsTab::Exclusions => render_exclusions_tab(f, app, content_chunks[0]),
        SettingsTab::AutoTags => render_auto_tags_tab(f, app, content_chunks[0]),
    }

    // Render description pane
    let t = theme();
    let description = get_setting_description(app.current_tab.index(), app.selected_item);
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
        (1, 2) => "Color theme: dark (RGB for dark terminals), light (RGB for light terminals), terminal (ANSI 16 — adapts to your scheme). Changes apply immediately.",
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

        if app.auto_tag_list_state.selected().is_none() && !app.config.auto_tags.is_empty() {
            app.auto_tag_list_state.select(Some(0));
        }

        f.render_stateful_widget(list, chunks[0], &mut app.auto_tag_list_state);
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

use crate::util::centered_rect;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crossterm::event::KeyEvent;

    #[test]
    fn test_app_state_navigation() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Initial state
        assert_eq!(app.current_tab, SettingsTab::Search);
        assert_eq!(app.selected_item, 0);

        // Tab navigation
        app.next_tab();
        assert_eq!(app.current_tab, SettingsTab::Shell);
        app.next_tab();
        assert_eq!(app.current_tab, SettingsTab::Exclusions);
        app.next_tab();
        assert_eq!(app.current_tab, SettingsTab::AutoTags);
        app.next_tab();
        assert_eq!(app.current_tab, SettingsTab::Search); // Cycle back

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

        // Toggle Search Unique (Tab Search, Item 1)
        app.current_tab = SettingsTab::Search;
        app.selected_item = 1;
        app.toggle_bool();
        assert!(app.config.search.show_unique_by_default);

        app.toggle_bool();
        assert!(!app.config.search.show_unique_by_default);

        // Toggle Arrow Navigation (Tab Shell, Item 0)
        app.current_tab = SettingsTab::Shell;
        app.selected_item = 0;
        app.toggle_bool();
        assert!(!app.config.shell.enable_arrow_navigation);
    }

    #[test]
    fn test_theme_cycle_in_settings() {
        use crate::theme::ThemeName;

        let config = Config::default();
        let mut app = AppState::new(config);

        // Theme is Tab Shell, Item 2
        app.current_tab = SettingsTab::Shell;
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

    #[test]
    fn test_initial_state() {
        let config = Config::default();
        let app = AppState::new(config);

        assert_eq!(app.current_tab, SettingsTab::Search);
        assert_eq!(app.selected_item, 0);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.input_buffer.is_empty());
        assert!(app.auto_tag_path_input.is_empty());
        assert!(app.auto_tag_name_input.is_empty());
        assert_eq!(app.auto_tag_focus, 0);
        assert!(app.save_status.is_none());
        assert!(!app.dirty);
    }

    #[test]
    fn test_prev_tab_navigation() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // From Search, prev_tab wraps to last tab
        app.prev_tab();
        assert_eq!(app.current_tab, SettingsTab::AutoTags);
        assert_eq!(app.selected_item, 0);

        app.prev_tab();
        assert_eq!(app.current_tab, SettingsTab::Exclusions);

        app.prev_tab();
        assert_eq!(app.current_tab, SettingsTab::Shell);

        app.prev_tab();
        assert_eq!(app.current_tab, SettingsTab::Search);
    }

    #[test]
    fn test_tab_navigation_resets_selected_item() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Move to item 3 in tab 0
        app.next_item();
        app.next_item();
        app.next_item();
        assert_eq!(app.selected_item, 3);

        // Switching tab resets selected_item to 0
        app.next_tab();
        assert_eq!(app.current_tab, SettingsTab::Shell);
        assert_eq!(app.selected_item, 0);
    }

    #[test]
    fn test_item_selection_prev_wraps() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Tab 0 has 5 items; going prev from 0 wraps to 4
        assert_eq!(app.selected_item, 0);
        app.prev_item();
        assert_eq!(app.selected_item, 4);

        // And going next from 4 wraps to 0
        app.next_item();
        assert_eq!(app.selected_item, 0);
    }

    #[test]
    fn test_toggle_all_search_bools() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Tab Search, Item 2: filter_by_current_session_tag (default false)
        app.current_tab = SettingsTab::Search;
        app.selected_item = 2;
        assert!(!app.config.search.filter_by_current_session_tag);
        app.toggle_bool();
        assert!(app.config.search.filter_by_current_session_tag);
        assert!(app.dirty);

        // Tab 0, Item 3: context_boost (default true)
        app.selected_item = 3;
        assert!(app.config.search.context_boost);
        app.toggle_bool();
        assert!(!app.config.search.context_boost);

        // Tab 0, Item 4: show_detail_pane (default true)
        app.selected_item = 4;
        assert!(app.config.search.show_detail_pane);
        app.toggle_bool();
        assert!(!app.config.search.show_detail_pane);
    }

    #[test]
    fn test_toggle_show_risk_in_search() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Tab Shell, Item 1: show_risk_in_search (default true)
        app.current_tab = SettingsTab::Shell;
        app.selected_item = 1;
        assert!(app.config.agent.show_risk_in_search);
        app.toggle_bool();
        assert!(!app.config.agent.show_risk_in_search);
        assert!(app.dirty);
    }

    #[test]
    fn test_edit_page_limit_via_handle_input() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Tab Search, Item 0 is Page Limit; Enter enters edit mode
        app.current_tab = SettingsTab::Search;
        app.selected_item = 0;

        let cont = app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(cont);
        assert_eq!(app.input_mode, InputMode::Editing);

        // Clear the buffer and type "200"
        app.input_buffer.clear();
        app.handle_input(KeyEvent::from(KeyCode::Char('2')));
        app.handle_input(KeyEvent::from(KeyCode::Char('0')));
        app.handle_input(KeyEvent::from(KeyCode::Char('0')));
        assert_eq!(app.input_buffer, "200");

        // Confirm with Enter
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.config.search.page_limit, 200);
        assert!(app.dirty);
    }

    #[test]
    fn test_edit_page_limit_clamps() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Enter edit mode for page limit
        app.current_tab = SettingsTab::Search;
        app.selected_item = 0;
        app.handle_input(KeyEvent::from(KeyCode::Enter));

        // Type a value below minimum (10)
        app.input_buffer.clear();
        app.handle_input(KeyEvent::from(KeyCode::Char('3')));
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.config.search.page_limit, 10); // Clamped to min

        // Enter edit mode again and type a value above max (5000)
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        app.input_buffer.clear();
        app.input_buffer.push_str("9999");
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.config.search.page_limit, 5000); // Clamped to max
    }

    #[test]
    fn test_escape_from_editing() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Enter edit mode
        app.current_tab = SettingsTab::Search;
        app.selected_item = 0;
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::Editing);

        // Type something then escape
        app.input_buffer.clear();
        app.handle_input(KeyEvent::from(KeyCode::Char('9')));
        app.handle_input(KeyEvent::from(KeyCode::Char('9')));
        app.handle_input(KeyEvent::from(KeyCode::Esc));

        assert_eq!(app.input_mode, InputMode::Normal);
        // Original page_limit should be unchanged (default is 50)
        assert_eq!(app.config.search.page_limit, 50);
        assert!(!app.dirty);
    }

    #[test]
    fn test_add_exclusion_pattern() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Switch to exclusions tab
        app.current_tab = SettingsTab::Exclusions;
        app.selected_item = 0;
        assert!(app.config.exclusions.is_empty());

        // Press 'a' to enter edit mode for adding an exclusion
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(app.input_mode, InputMode::Editing);
        assert!(app.input_buffer.is_empty());

        // Type "^ls$"
        app.handle_input(KeyEvent::from(KeyCode::Char('^')));
        app.handle_input(KeyEvent::from(KeyCode::Char('l')));
        app.handle_input(KeyEvent::from(KeyCode::Char('s')));
        app.handle_input(KeyEvent::from(KeyCode::Char('$')));
        assert_eq!(app.input_buffer, "^ls$");

        // Confirm
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.config.exclusions, vec!["^ls$"]);
        assert!(app.dirty);
        assert_eq!(app.selected_item, 0); // points to the new item
    }

    #[test]
    fn test_remove_exclusion_pattern() {
        let mut config = Config::default();
        config.exclusions = vec!["^ls$".to_string(), "password".to_string()];

        let mut app = AppState::new(config);
        app.current_tab = SettingsTab::Exclusions;
        app.selected_item = 0;

        // Delete first exclusion
        app.handle_input(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.config.exclusions, vec!["password"]);
        assert!(app.dirty);
        assert_eq!(app.selected_item, 0);

        // Delete last remaining exclusion
        app.handle_input(KeyEvent::from(KeyCode::Char('d')));
        assert!(app.config.exclusions.is_empty());
        assert_eq!(app.selected_item, 0);
    }

    #[test]
    fn test_confirm_quit_save_discard() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Quit when not dirty exits immediately
        let cont = app.handle_input(KeyEvent::from(KeyCode::Char('q')));
        assert!(!cont); // false = quit

        // Reset: make the state dirty
        let mut app = AppState::new(Config::default());
        app.dirty = true;

        // Quit when dirty enters confirm mode
        let cont = app.handle_input(KeyEvent::from(KeyCode::Char('q')));
        assert!(cont);
        assert_eq!(app.input_mode, InputMode::ConfirmQuit);

        // Pressing 'n' discards and quits
        let cont = app.handle_input(KeyEvent::from(KeyCode::Char('n')));
        assert!(!cont);
    }

    #[test]
    fn test_confirm_quit_esc_cancels() {
        let mut app = AppState::new(Config::default());
        app.dirty = true;

        // Enter confirm quit mode
        app.handle_input(KeyEvent::from(KeyCode::Char('q')));
        assert_eq!(app.input_mode, InputMode::ConfirmQuit);

        // Escape goes back to normal
        let cont = app.handle_input(KeyEvent::from(KeyCode::Esc));
        assert!(cont);
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn test_handle_input_via_key_events() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Tab key cycles tabs
        app.handle_input(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.current_tab, SettingsTab::Shell);

        // BackTab goes back
        app.handle_input(KeyEvent::from(KeyCode::BackTab));
        assert_eq!(app.current_tab, SettingsTab::Search);

        // Down/j moves selection
        app.handle_input(KeyEvent::from(KeyCode::Down));
        assert_eq!(app.selected_item, 1);
        app.handle_input(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.selected_item, 2);

        // Up/k moves selection back
        app.handle_input(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.selected_item, 1);
        app.handle_input(KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(app.selected_item, 0);

        // Space toggles (Item 1 is show_unique_by_default, default false)
        app.handle_input(KeyEvent::from(KeyCode::Down)); // item 1
        app.handle_input(KeyEvent::from(KeyCode::Char(' ')));
        assert!(app.config.search.show_unique_by_default);
    }

    #[test]
    fn test_exclusion_item_navigation() {
        let mut config = Config::default();
        config.exclusions = vec![
            "pattern1".to_string(),
            "pattern2".to_string(),
            "pattern3".to_string(),
        ];

        let mut app = AppState::new(config);
        app.current_tab = SettingsTab::Exclusions;
        app.selected_item = 0;

        // Navigate through exclusion items
        app.next_item();
        assert_eq!(app.selected_item, 1);
        assert_eq!(app.exclusion_list_state.selected(), Some(1));

        app.next_item();
        assert_eq!(app.selected_item, 2);

        // Wraps around
        app.next_item();
        assert_eq!(app.selected_item, 0);
        assert_eq!(app.exclusion_list_state.selected(), Some(0));

        // Prev from 0 wraps to last
        app.prev_item();
        assert_eq!(app.selected_item, 2);
        assert_eq!(app.exclusion_list_state.selected(), Some(2));
    }

    #[test]
    fn test_backspace_in_editing_mode() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Enter edit mode for exclusion
        app.current_tab = SettingsTab::Exclusions;
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(app.input_mode, InputMode::Editing);

        // Type and backspace
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        app.handle_input(KeyEvent::from(KeyCode::Char('b')));
        app.handle_input(KeyEvent::from(KeyCode::Char('c')));
        assert_eq!(app.input_buffer, "abc");

        app.handle_input(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.input_buffer, "ab");

        app.handle_input(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.input_buffer, "a");
    }

    // ── Auto-tag add flow ──────────────────────────────────────────────
    #[test]
    fn test_auto_tag_add_flow() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Switch to auto-tags tab
        app.current_tab = SettingsTab::AutoTags;
        app.selected_item = 0;

        // Press 'a' to start adding an auto-tag
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(app.input_mode, InputMode::Editing);
        assert_eq!(app.auto_tag_focus, 0);
        assert!(app.auto_tag_path_input.is_empty());
        assert!(app.auto_tag_name_input.is_empty());

        // Type path "~/projects"
        for c in "~/projects".chars() {
            app.handle_input(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(app.auto_tag_path_input, "~/projects");

        // Press Enter to switch to tag field
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.auto_tag_focus, 1);
        assert_eq!(app.input_mode, InputMode::Editing); // Still editing

        // Type tag name "work"
        for c in "work".chars() {
            app.handle_input(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(app.auto_tag_name_input, "work");

        // Press Enter to submit
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.dirty);
        assert_eq!(
            app.config.auto_tags.get("~/projects"),
            Some(&"work".to_string())
        );
    }

    // ── Auto-tag delete flow ───────────────────────────────────────────
    #[test]
    fn test_auto_tag_delete_flow() {
        let mut config = Config::default();
        config
            .auto_tags
            .insert("/home/user".to_string(), "home".to_string());
        config
            .auto_tags
            .insert("/work/repo".to_string(), "work".to_string());

        let mut app = AppState::new(config);
        app.current_tab = SettingsTab::AutoTags;
        app.selected_item = 0;

        assert_eq!(app.config.auto_tags.len(), 2);

        // Press 'd' to delete the selected auto-tag
        app.handle_input(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.config.auto_tags.len(), 1);
        assert!(app.dirty);

        // Delete the remaining one
        app.selected_item = 0;
        app.handle_input(KeyEvent::from(KeyCode::Char('d')));
        assert!(app.config.auto_tags.is_empty());
        assert_eq!(app.selected_item, 0);
        assert_eq!(app.auto_tag_list_state.selected(), None);
    }

    // ── Auto-tag Tab toggles focus ─────────────────────────────────────
    #[test]
    fn test_auto_tag_tab_toggles_focus() {
        let config = Config::default();
        let mut app = AppState::new(config);

        app.current_tab = SettingsTab::AutoTags;
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(app.input_mode, InputMode::Editing);
        assert_eq!(app.auto_tag_focus, 0);

        // Tab toggles from path (0) to name (1)
        app.handle_input(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.auto_tag_focus, 1);

        // Tab toggles back from name (1) to path (0)
        app.handle_input(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.auto_tag_focus, 0);
    }

    // ── Auto-tag validation - empty fields rejected ────────────────────
    #[test]
    fn test_auto_tag_empty_fields_rejected() {
        let config = Config::default();
        let mut app = AppState::new(config);

        app.current_tab = SettingsTab::AutoTags;
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(app.input_mode, InputMode::Editing);

        // Move to tag field with empty path
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.auto_tag_focus, 1);

        // Try to submit with empty path and empty tag
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        // Should show error and stay in editing mode
        assert_eq!(app.input_mode, InputMode::Editing);
        assert_eq!(
            app.save_status,
            Some("Both Path and Tag are required".to_string())
        );
        assert!(app.config.auto_tags.is_empty());

        // Type a tag but path is still empty
        for c in "mytag".chars() {
            app.handle_input(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::Editing); // Still editing
        assert!(app.config.auto_tags.is_empty());

        // Now add a path via Tab back to path field
        app.handle_input(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.auto_tag_focus, 0);
        for c in "/some/path".chars() {
            app.handle_input(KeyEvent::from(KeyCode::Char(c)));
        }
        // Switch back to tag and submit
        app.handle_input(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.auto_tag_focus, 1);
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(
            app.config.auto_tags.get("/some/path"),
            Some(&"mytag".to_string())
        );
    }

    // ── Exclusion delete when list becomes empty ───────────────────────
    #[test]
    fn test_exclusion_delete_empties_list() {
        let mut config = Config::default();
        config.exclusions = vec!["only_one".to_string()];

        let mut app = AppState::new(config);
        app.current_tab = SettingsTab::Exclusions;
        app.selected_item = 0;
        app.exclusion_list_state.select(Some(0));

        app.handle_input(KeyEvent::from(KeyCode::Char('d')));
        assert!(app.config.exclusions.is_empty());
        assert_eq!(app.selected_item, 0);
        assert_eq!(app.exclusion_list_state.selected(), None);
        assert!(app.dirty);
    }

    // ── Exclusion delete last item adjusts selected_item ───────────────
    #[test]
    fn test_exclusion_delete_last_adjusts_selected() {
        let mut config = Config::default();
        config.exclusions = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];

        let mut app = AppState::new(config);
        app.current_tab = SettingsTab::Exclusions;
        app.selected_item = 2; // Select last item ("gamma")
        app.exclusion_list_state.select(Some(2));

        app.handle_input(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.config.exclusions, vec!["alpha", "beta"]);
        assert_eq!(app.selected_item, 1); // Adjusted to last valid index
        assert_eq!(app.exclusion_list_state.selected(), Some(1));
    }

    // ── handle_input returns false on 'q' when not dirty ───────────────
    #[test]
    fn test_handle_input_q_not_dirty_quits() {
        let config = Config::default();
        let mut app = AppState::new(config);
        assert!(!app.dirty);

        let cont = app.handle_input(KeyEvent::from(KeyCode::Char('q')));
        assert!(!cont); // false means quit
    }

    // ── handle_input returns true on 'q' when dirty, enters ConfirmQuit ─
    #[test]
    fn test_handle_input_q_dirty_enters_confirm_quit() {
        let mut app = AppState::new(Config::default());
        app.dirty = true;

        let cont = app.handle_input(KeyEvent::from(KeyCode::Char('q')));
        assert!(cont); // true means continue (showing confirm dialog)
        assert_eq!(app.input_mode, InputMode::ConfirmQuit);
    }

    // ── handle_input 's' saves config ──────────────────────────────────
    #[test]
    fn test_handle_input_s_attempts_save() {
        let mut app = AppState::new(Config::default());
        app.dirty = true;

        app.handle_input(KeyEvent::from(KeyCode::Char('s')));
        // save_config will either succeed or fail; in either case save_status is set
        assert!(app.save_status.is_some());
        let status = app.save_status.as_ref().unwrap();
        // If save succeeded: "Settings saved!" and dirty is false
        // If save failed: "Error saving: ..." and dirty remains true
        if status == "Settings saved!" {
            assert!(!app.dirty);
        } else {
            assert!(status.starts_with("Error saving:"));
        }
    }

    // ── Editing mode input length limit ────────────────────────────────
    #[test]
    fn test_input_buffer_length_limit() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Enter editing mode for exclusion
        app.current_tab = SettingsTab::Exclusions;
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(app.input_mode, InputMode::Editing);

        // Push 501 characters
        for _ in 0..501 {
            app.handle_input(KeyEvent::from(KeyCode::Char('x')));
        }
        assert_eq!(app.input_buffer.len(), 500);
    }

    // ── Auto-tag backspace in path field ────────────────────────────────
    #[test]
    fn test_auto_tag_backspace_path_field() {
        let config = Config::default();
        let mut app = AppState::new(config);

        app.current_tab = SettingsTab::AutoTags;
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(app.auto_tag_focus, 0);

        // Type "abc" into path
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        app.handle_input(KeyEvent::from(KeyCode::Char('b')));
        app.handle_input(KeyEvent::from(KeyCode::Char('c')));
        assert_eq!(app.auto_tag_path_input, "abc");

        // Backspace removes from path
        app.handle_input(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.auto_tag_path_input, "ab");
        app.handle_input(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.auto_tag_path_input, "a");
    }

    // ── Auto-tag backspace in tag field ─────────────────────────────────
    #[test]
    fn test_auto_tag_backspace_tag_field() {
        let config = Config::default();
        let mut app = AppState::new(config);

        app.current_tab = SettingsTab::AutoTags;
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        // Switch to tag field
        app.auto_tag_focus = 1;

        // Type "xyz" into tag name
        app.handle_input(KeyEvent::from(KeyCode::Char('x')));
        app.handle_input(KeyEvent::from(KeyCode::Char('y')));
        app.handle_input(KeyEvent::from(KeyCode::Char('z')));
        assert_eq!(app.auto_tag_name_input, "xyz");

        // Backspace removes from tag name
        app.handle_input(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.auto_tag_name_input, "xy");
        app.handle_input(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.auto_tag_name_input, "x");
    }

    // ── Item navigation on empty tab ───────────────────────────────────
    #[test]
    fn test_item_navigation_empty_tab() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Exclusions tab with no exclusions
        app.current_tab = SettingsTab::Exclusions;
        app.selected_item = 0;

        // next_item and prev_item should not change selected_item
        app.next_item();
        assert_eq!(app.selected_item, 0);

        app.prev_item();
        assert_eq!(app.selected_item, 0);
    }

    // ── ConfirmQuit 'y' with save attempt ──────────────────────────────
    #[test]
    fn test_confirm_quit_y_save_attempt() {
        let mut app = AppState::new(Config::default());
        app.dirty = true;

        // Enter ConfirmQuit
        app.handle_input(KeyEvent::from(KeyCode::Char('q')));
        assert_eq!(app.input_mode, InputMode::ConfirmQuit);

        // Press 'y' to attempt save-and-quit
        let cont = app.handle_input(KeyEvent::from(KeyCode::Char('y')));
        // If save_config succeeded: cont is false (quit)
        // If save_config failed: cont is true (stays in Normal mode with error)
        if cont {
            // Save failed — should be back in Normal mode with error status
            assert_eq!(app.input_mode, InputMode::Normal);
            let status = app.save_status.as_ref().unwrap();
            assert!(status.starts_with("Error saving:"));
        } else {
            // Save succeeded — app is quitting
            assert!(!cont);
        }
    }

    // ── handle_input Enter/Space toggles bool items ────────────────────
    #[test]
    fn test_handle_input_enter_space_toggle_bool() {
        let config = Config::default();
        let mut app = AppState::new(config);

        // Tab Search, Item 1: show_unique_by_default (default false)
        app.current_tab = SettingsTab::Search;
        app.selected_item = 1;
        assert!(!app.config.search.show_unique_by_default);

        // Space toggles
        app.handle_input(KeyEvent::from(KeyCode::Char(' ')));
        assert!(app.config.search.show_unique_by_default);

        // Enter also toggles
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(!app.config.search.show_unique_by_default);

        // Tab Search, Item 3: context_boost (default true)
        app.selected_item = 3;
        assert!(app.config.search.context_boost);
        app.handle_input(KeyEvent::from(KeyCode::Char(' ')));
        assert!(!app.config.search.context_boost);
    }

    // ── Multiple exclusion adds in sequence ────────────────────────────
    #[test]
    fn test_multiple_exclusion_adds() {
        let config = Config::default();
        let mut app = AppState::new(config);
        app.current_tab = SettingsTab::Exclusions;

        // Add "pattern1"
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        for c in "pattern1".chars() {
            app.handle_input(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.config.exclusions, vec!["pattern1"]);
        assert_eq!(app.selected_item, 0);

        // Add "pattern2"
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        for c in "pattern2".chars() {
            app.handle_input(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.config.exclusions, vec!["pattern1", "pattern2"]);
        assert_eq!(app.selected_item, 1); // Points to latest
    }

    // ── Edit mode Esc for exclusion add cancels ────────────────────────
    #[test]
    fn test_exclusion_add_esc_cancels() {
        let config = Config::default();
        let mut app = AppState::new(config);
        app.current_tab = SettingsTab::Exclusions;

        // Enter edit mode for adding exclusion
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(app.input_mode, InputMode::Editing);

        // Type some characters
        for c in "should_not_add".chars() {
            app.handle_input(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(app.input_buffer, "should_not_add");

        // Press Esc to cancel
        app.handle_input(KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.config.exclusions.is_empty()); // Nothing was added
        assert!(!app.dirty);
    }

    // ── Auto-tag Esc cancels add ─────────────────────────────────────
    #[test]
    fn test_auto_tag_esc_cancels_add() {
        let config = Config::default();
        let mut app = AppState::new(config);

        app.current_tab = SettingsTab::AutoTags;
        app.handle_input(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(app.input_mode, InputMode::Editing);

        // Type path and tag
        for c in "/some/path".chars() {
            app.handle_input(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_input(KeyEvent::from(KeyCode::Tab));
        for c in "mytag".chars() {
            app.handle_input(KeyEvent::from(KeyCode::Char(c)));
        }

        // Press Esc to cancel
        app.handle_input(KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.config.auto_tags.is_empty()); // Nothing saved
        assert!(!app.dirty);
    }

    // ── Exclusion delete middle item ─────────────────────────────────
    #[test]
    fn test_exclusion_delete_middle_item() {
        let mut config = Config::default();
        config.exclusions = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];

        let mut app = AppState::new(config);
        app.current_tab = SettingsTab::Exclusions;
        app.selected_item = 1; // Select "beta" (middle)
        app.exclusion_list_state.select(Some(1));

        app.handle_input(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.config.exclusions, vec!["alpha", "gamma"]);
        assert_eq!(app.selected_item, 1); // Still at index 1 (now "gamma")
        assert!(app.dirty);
    }

    // ── Auto-tag item navigation ─────────────────────────────────────
    #[test]
    fn test_auto_tag_item_navigation() {
        let mut config = Config::default();
        config
            .auto_tags
            .insert("/path/a".to_string(), "tag_a".to_string());
        config
            .auto_tags
            .insert("/path/b".to_string(), "tag_b".to_string());

        let mut app = AppState::new(config);
        app.current_tab = SettingsTab::AutoTags;
        app.selected_item = 0;

        // Navigate through auto-tag items
        app.next_item();
        assert_eq!(app.selected_item, 1);
        assert_eq!(app.auto_tag_list_state.selected(), Some(1));

        // Wraps around
        app.next_item();
        assert_eq!(app.selected_item, 0);
        assert_eq!(app.auto_tag_list_state.selected(), Some(0));
    }
}
