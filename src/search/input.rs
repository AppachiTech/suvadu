use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{DialogState, SearchAction, SearchApp, VimMode};
use crate::util;

/// Maximum length for any text input field (query, filters, notes, etc.).
const MAX_INPUT_LEN: usize = 2000;

/// Rows to move per `PageUp`/`PageDown` press.
const PAGE_SCROLL_LINES: usize = 10;

impl SearchApp {
    /// Handle a bracketed-paste event. Returns `true` if the paste modified
    /// the main search query (caller should reload), `false` otherwise.
    pub(super) fn handle_paste(&mut self, text: &str) -> bool {
        // Strip control characters (keep printable + whitespace except newlines)
        let sanitized: String = text
            .chars()
            .filter(|c| !c.is_control() || *c == ' ')
            .collect();

        if sanitized.is_empty() {
            return false;
        }

        match self.dialog {
            DialogState::Note { ref mut input, .. } => {
                let remaining = MAX_INPUT_LEN.saturating_sub(input.len());
                input.extend(sanitized.chars().take(remaining));
                false
            }
            DialogState::GoToPage { ref mut input } => {
                // Only accept digits
                let digits: String = sanitized.chars().filter(char::is_ascii_digit).collect();
                let remaining = MAX_INPUT_LEN.saturating_sub(input.len());
                input.extend(digits.chars().take(remaining));
                false
            }
            DialogState::Filter => {
                self.paste_into_filter_field(&sanitized);
                false
            }
            DialogState::Help | DialogState::Delete { .. } | DialogState::TagAssociation => false,
            DialogState::None => {
                // In vim Normal mode, auto-switch to Insert mode on paste
                if self.vim_enabled && self.vim_mode == VimMode::Normal {
                    self.vim_mode = VimMode::Insert;
                }
                let remaining = MAX_INPUT_LEN.saturating_sub(self.query.len());
                self.query.extend(sanitized.chars().take(remaining));
                true
            }
        }
    }

    /// Paste sanitized text into the currently focused filter field.
    fn paste_into_filter_field(&mut self, text: &str) {
        let target = match self.filters.focus_index {
            0 => &mut self.filters.start_date_input,
            1 => &mut self.filters.end_date_input,
            2 => &mut self.filters.tag_filter_input,
            3 => &mut self.filters.exit_code_input,
            _ => return, // field 4 is a selector, no text paste
        };
        let remaining = MAX_INPUT_LEN.saturating_sub(target.len());
        target.extend(text.chars().take(remaining));
    }

    pub(super) fn handle_input(&mut self, key: KeyEvent) -> SearchAction {
        match self.dialog {
            DialogState::Delete { .. } => return self.handle_delete_dialog_input(key),
            DialogState::GoToPage { .. } => return self.handle_goto_dialog_input(key),
            DialogState::TagAssociation => return self.handle_tag_dialog_input(key),
            DialogState::Note { .. } => return self.handle_note_dialog_input(key),
            DialogState::Filter => return self.handle_filter_input(key),
            DialogState::Help => {
                self.dialog = DialogState::None;
                return SearchAction::Continue;
            }
            DialogState::None => {}
        }
        self.handle_normal_input(key)
    }

    fn handle_normal_input(&mut self, key: KeyEvent) -> SearchAction {
        // Handle Ctrl+key shortcuts first; ignore unrecognized Ctrl combos
        // to prevent them from falling through to the character input handler.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            // In vim Normal mode, Ctrl+U/D are half-page scroll.
            // In Insert mode, they keep original behavior (unique toggle / delete).
            if self.vim_enabled && self.vim_mode == VimMode::Normal {
                match key.code {
                    KeyCode::Char('u') => {
                        self.move_selection_up(self.half_page());
                        return SearchAction::Continue;
                    }
                    KeyCode::Char('d') => {
                        self.move_selection_down(self.half_page());
                        return SearchAction::Continue;
                    }
                    _ => {}
                }
            }
            return self
                .handle_ctrl_shortcut(key.code)
                .unwrap_or(SearchAction::Continue);
        }

        // Vim normal mode: navigation keys instead of typing
        if self.vim_enabled && self.vim_mode == VimMode::Normal {
            return self.handle_vim_normal_input(key);
        }

        match key.code {
            KeyCode::Left if self.pagination.page > 1 => {
                return SearchAction::SetPage(self.pagination.page - 1);
            }
            KeyCode::Right => {
                let total_pages = self
                    .pagination
                    .total_items
                    .div_ceil(self.pagination.page_size);
                if self.pagination.page < total_pages {
                    return SearchAction::SetPage(self.pagination.page + 1);
                }
            }
            KeyCode::Tab => {
                self.view.detail_pane_open = !self.view.detail_pane_open;
            }
            KeyCode::F(1) | KeyCode::Char('?') => {
                self.dialog = DialogState::Help;
                return SearchAction::Continue;
            }
            KeyCode::Char(c) if self.query.len() + c.len_utf8() <= MAX_INPUT_LEN => {
                self.query.push(c);
                return SearchAction::Reload;
            }
            KeyCode::Backspace => {
                self.query.pop();
                return SearchAction::Reload;
            }
            KeyCode::Up => {
                if let Some(selected) = self.table_state.selected() {
                    if selected > 0 {
                        self.table_state.select(Some(selected - 1));
                    }
                }
            }
            KeyCode::Down => {
                if let Some(selected) = self.table_state.selected() {
                    if selected + 1 < self.entries.len() {
                        self.table_state.select(Some(selected + 1));
                    }
                } else if !self.entries.is_empty() {
                    self.table_state.select(Some(0));
                }
            }
            KeyCode::PageUp => {
                self.move_selection_up(PAGE_SCROLL_LINES);
            }
            KeyCode::PageDown => {
                self.move_selection_down(PAGE_SCROLL_LINES);
            }
            KeyCode::Home if !self.entries.is_empty() => {
                self.table_state.select(Some(0));
            }
            KeyCode::End if !self.entries.is_empty() => {
                self.table_state.select(Some(self.entries.len() - 1));
            }
            KeyCode::Enter => {
                if let Some(cmd) = self.get_selected_command() {
                    return SearchAction::Select(cmd);
                }
            }
            KeyCode::Esc => {
                if self.vim_enabled {
                    self.vim_mode = VimMode::Normal;
                    return SearchAction::Continue;
                }
                return SearchAction::Exit;
            }
            _ => {}
        }
        SearchAction::Continue
    }

    /// Handle keys in vim normal mode: j/k navigate, / enters insert, q quits.
    fn handle_vim_normal_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            // Navigation
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(selected) = self.table_state.selected() {
                    if selected + 1 < self.entries.len() {
                        self.table_state.select(Some(selected + 1));
                    }
                } else if !self.entries.is_empty() {
                    self.table_state.select(Some(0));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(selected) = self.table_state.selected() {
                    if selected > 0 {
                        self.table_state.select(Some(selected - 1));
                    }
                }
            }
            // Half-page scroll
            KeyCode::Char('G') | KeyCode::End if !self.entries.is_empty() => {
                self.table_state.select(Some(self.entries.len() - 1));
            }
            KeyCode::Char('g') | KeyCode::Home if !self.entries.is_empty() => {
                self.table_state.select(Some(0));
            }
            KeyCode::PageUp => {
                self.move_selection_up(PAGE_SCROLL_LINES);
            }
            KeyCode::PageDown => {
                self.move_selection_down(PAGE_SCROLL_LINES);
            }
            // Page navigation
            KeyCode::Left | KeyCode::Char('h') if self.pagination.page > 1 => {
                return SearchAction::SetPage(self.pagination.page - 1);
            }
            KeyCode::Right | KeyCode::Char('l') => {
                let total_pages = self
                    .pagination
                    .total_items
                    .div_ceil(self.pagination.page_size);
                if self.pagination.page < total_pages {
                    return SearchAction::SetPage(self.pagination.page + 1);
                }
            }
            // Switch to insert (search) mode
            KeyCode::Char('/' | 'i') => {
                self.vim_mode = VimMode::Insert;
            }
            // Actions
            KeyCode::Enter => {
                if let Some(cmd) = self.get_selected_command() {
                    return SearchAction::Select(cmd);
                }
            }
            KeyCode::Tab => {
                self.view.detail_pane_open = !self.view.detail_pane_open;
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.dialog = DialogState::Help;
            }
            // Quit
            KeyCode::Char('q') | KeyCode::Esc => return SearchAction::Exit,
            _ => {}
        }
        SearchAction::Continue
    }

    /// Move table selection up by `n` rows, clamping at 0.
    fn move_selection_up(&mut self, n: usize) {
        if self.entries.is_empty() {
            return;
        }
        let current = self.table_state.selected().unwrap_or(0);
        self.table_state.select(Some(current.saturating_sub(n)));
    }

    /// Move table selection down by `n` rows, clamping at end.
    fn move_selection_down(&mut self, n: usize) {
        if self.entries.is_empty() {
            return;
        }
        let current = self.table_state.selected().unwrap_or(0);
        let max = self.entries.len() - 1;
        self.table_state.select(Some((current + n).min(max)));
    }

    /// Half the visible page size, minimum 1.
    fn half_page(&self) -> usize {
        (self.entries.len() / 2).max(1)
    }

    #[allow(clippy::too_many_lines)]
    fn handle_ctrl_shortcut(&mut self, code: KeyCode) -> Option<SearchAction> {
        match code {
            KeyCode::Char('g') => {
                self.dialog = DialogState::GoToPage {
                    input: String::new(),
                };
            }
            KeyCode::Char('t') => {
                self.dialog = DialogState::TagAssociation;
                if !self.tags.is_empty() {
                    self.tag_list_state.select(Some(0));
                }
            }
            KeyCode::Char('u') => {
                self.view.unique_mode = !self.view.unique_mode;
                self.pagination.page = 1;
                return Some(SearchAction::Reload);
            }
            KeyCode::Char('f') => {
                self.dialog = DialogState::Filter;
                self.filters.focus_index = 0;
            }
            KeyCode::Char('y') => {
                if let Some(cmd) = self.get_selected_command() {
                    return Some(SearchAction::Copy(cmd));
                }
            }
            KeyCode::Char('d') => {
                if let Some(entry) = self.get_selected_entry() {
                    if let Some(id) = entry.id {
                        self.dialog = DialogState::Delete { entry_id: id };
                    }
                }
            }
            KeyCode::Char('b') => {
                if let Some(cmd) = self.get_selected_command() {
                    return Some(SearchAction::ToggleBookmark(cmd));
                }
            }
            KeyCode::Char('n') => {
                if let Some(entry) = self.get_selected_entry() {
                    if let Some(id) = entry.id {
                        self.dialog = DialogState::Note {
                            entry_id: id,
                            input: String::new(),
                        };
                    }
                }
            }
            KeyCode::Char('s') => {
                self.view.context_boost = !self.view.context_boost;
                self.status_message = Some((
                    if self.view.context_boost {
                        "Smart mode ON".into()
                    } else {
                        "Smart mode OFF".into()
                    },
                    std::time::Instant::now(),
                ));
                return Some(SearchAction::Reload);
            }
            KeyCode::Char('l') => {
                if self.filters.cwd.is_some() {
                    self.filters.cwd = None;
                } else if let Ok(cwd) = std::env::current_dir() {
                    self.filters.cwd = Some(cwd.to_string_lossy().to_string());
                }
                self.pagination.page = 1;
                return Some(SearchAction::Reload);
            }
            KeyCode::Char('a') => {
                self.filters.show_agents = !self.filters.show_agents;
                self.status_message = Some((
                    if self.filters.show_agents {
                        "Agent commands shown".into()
                    } else {
                        "Agent commands hidden".into()
                    },
                    std::time::Instant::now(),
                ));
                self.pagination.page = 1;
                return Some(SearchAction::Reload);
            }
            KeyCode::Char('e') => {
                self.filters.failed_only = !self.filters.failed_only;
                self.status_message = Some((
                    if self.filters.failed_only {
                        "Failed commands only".into()
                    } else {
                        "All exit codes".into()
                    },
                    std::time::Instant::now(),
                ));
                self.pagination.page = 1;
                return Some(SearchAction::Reload);
            }
            KeyCode::Char('o') => {
                self.filters.bookmarks_only = !self.filters.bookmarks_only;
                self.status_message = Some((
                    if self.filters.bookmarks_only {
                        "Bookmarked commands only".into()
                    } else {
                        "All commands".into()
                    },
                    std::time::Instant::now(),
                ));
                self.pagination.page = 1;
                return Some(SearchAction::Reload);
            }
            _ => return None,
        }
        Some(SearchAction::Continue)
    }

    fn handle_tag_dialog_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Esc => self.dialog = DialogState::None,
            KeyCode::Up => {
                if let Some(selected) = self.tag_list_state.selected() {
                    if selected > 0 {
                        self.tag_list_state.select(Some(selected - 1));
                    }
                }
            }
            KeyCode::Down => {
                if let Some(selected) = self.tag_list_state.selected() {
                    if selected + 1 < self.tags.len() {
                        self.tag_list_state.select(Some(selected + 1));
                    }
                } else if !self.tags.is_empty() {
                    self.tag_list_state.select(Some(0));
                }
            }
            KeyCode::Enter => {
                if let Some(selected) = self.tag_list_state.selected() {
                    if let Some(tag) = self.tags.get(selected) {
                        self.dialog = DialogState::None;
                        return SearchAction::AssociateSession(tag.id);
                    }
                }
                self.dialog = DialogState::None;
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_note_dialog_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Esc => {
                self.dialog = DialogState::None;
            }
            KeyCode::Enter => {
                let old = std::mem::take(&mut self.dialog);
                if let DialogState::Note { input, entry_id } = old {
                    if input.is_empty() {
                        return SearchAction::DeleteNote(entry_id);
                    }
                    return SearchAction::SaveNote(entry_id, input);
                }
            }
            KeyCode::Backspace => {
                if let DialogState::Note { ref mut input, .. } = self.dialog {
                    input.pop();
                }
            }
            KeyCode::Char(c) => {
                if let DialogState::Note { ref mut input, .. } = self.dialog {
                    if input.len() + c.len_utf8() <= MAX_INPUT_LEN {
                        input.push(c);
                    }
                }
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_delete_dialog_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let DialogState::Delete { entry_id } = self.dialog {
                    self.dialog = DialogState::None;
                    return SearchAction::Delete(entry_id);
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.dialog = DialogState::None;
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_goto_dialog_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Enter => {
                let parsed = if let DialogState::GoToPage { ref input } = self.dialog {
                    input.parse::<usize>().ok()
                } else {
                    None
                };
                self.dialog = DialogState::None;
                if let Some(page_num) = parsed {
                    let total_pages = self
                        .pagination
                        .total_items
                        .div_ceil(self.pagination.page_size);
                    if total_pages > 0 {
                        let page_num = page_num.max(1).min(total_pages);
                        return SearchAction::SetPage(page_num);
                    }
                }
            }
            KeyCode::Esc => {
                self.dialog = DialogState::None;
            }
            KeyCode::Backspace => {
                if let DialogState::GoToPage { ref mut input } = self.dialog {
                    input.pop();
                }
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let DialogState::GoToPage { ref mut input } = self.dialog {
                    if input.len() + c.len_utf8() <= MAX_INPUT_LEN {
                        input.push(c);
                    }
                }
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_filter_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Esc => {
                self.dialog = DialogState::None;
            }
            KeyCode::Tab => {
                self.filters.focus_index = (self.filters.focus_index + 1) % 5;
            }
            KeyCode::BackTab => {
                self.filters.focus_index = if self.filters.focus_index == 0 {
                    4
                } else {
                    self.filters.focus_index - 1
                };
            }
            KeyCode::Enter => {
                return self.apply_filters();
            }
            // Executor selector: Up/Down cycles through options
            KeyCode::Up if self.filters.focus_index == 4 => {
                let total = self.filters.executors.len() + 1; // +1 for "All"
                self.filters.executor_sel = if self.filters.executor_sel == 0 {
                    total - 1
                } else {
                    self.filters.executor_sel - 1
                };
            }
            KeyCode::Down if self.filters.focus_index == 4 => {
                let total = self.filters.executors.len() + 1;
                self.filters.executor_sel = (self.filters.executor_sel + 1) % total;
            }
            KeyCode::Backspace => match self.filters.focus_index {
                0 => {
                    self.filters.start_date_input.pop();
                }
                1 => {
                    self.filters.end_date_input.pop();
                }
                2 => {
                    self.filters.tag_filter_input.pop();
                }
                3 => {
                    self.filters.exit_code_input.pop();
                }
                // field 4 is a selector, no text backspace
                _ => {}
            },
            KeyCode::Char(c) => match self.filters.focus_index {
                0 if self.filters.start_date_input.len() + c.len_utf8() <= MAX_INPUT_LEN => {
                    self.filters.start_date_input.push(c);
                }
                1 if self.filters.end_date_input.len() + c.len_utf8() <= MAX_INPUT_LEN => {
                    self.filters.end_date_input.push(c);
                }
                2 if self.filters.tag_filter_input.len() + c.len_utf8() <= MAX_INPUT_LEN => {
                    self.filters.tag_filter_input.push(c);
                }
                3 if self.filters.exit_code_input.len() + c.len_utf8() <= MAX_INPUT_LEN => {
                    self.filters.exit_code_input.push(c);
                }
                // field 4 is a selector, no text input
                _ => {}
            },
            _ => {}
        }
        SearchAction::Continue
    }

    fn apply_filters(&mut self) -> SearchAction {
        // Apply filters
        self.filters.after = if self.filters.start_date_input.is_empty() {
            None
        } else {
            util::parse_date_input(&self.filters.start_date_input, false)
        };

        self.filters.before = if self.filters.end_date_input.is_empty() {
            None
        } else {
            util::parse_date_input(&self.filters.end_date_input, true)
        };

        // Resolve tag name to ID
        self.filters.tag_id = if self.filters.tag_filter_input.is_empty() {
            None
        } else {
            let input_lower = self.filters.tag_filter_input.to_lowercase();
            self.tags
                .iter()
                .find(|t| t.name == input_lower)
                .map(|t| t.id)
        };

        // Parse exit code
        self.filters.exit_code = if self.filters.exit_code_input.is_empty() {
            None
        } else {
            self.filters.exit_code_input.trim().parse::<i32>().ok()
        };

        // Apply executor from selector -- extract the name part after ": "
        // e.g. "agent: claude-code" -> "claude-code"
        self.filters.executor_type = if self.filters.executor_sel == 0 {
            None // "All"
        } else {
            self.filters
                .executors
                .get(self.filters.executor_sel - 1)
                .map(|label| {
                    label
                        .split_once(": ")
                        .map_or(label.as_str(), |(_, name)| name)
                        .to_string()
                })
        };
        // Sync the text input for display
        self.filters.executor_filter_input = self.filters.executor_type.clone().unwrap_or_default();

        self.dialog = DialogState::None;
        // Reset to page 1 on new filter
        self.pagination.page = 1;
        SearchAction::Reload
    }
}
