use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{SearchAction, SearchApp};
use crate::util;

/// Maximum length for any text input field (query, filters, notes, etc.).
const MAX_INPUT_LEN: usize = 2000;

impl SearchApp {
    #[allow(clippy::too_many_lines)]
    pub(super) fn handle_input(&mut self, key: KeyEvent) -> SearchAction {
        if self.delete_dialog_open {
            return self.handle_delete_dialog_input(key);
        }

        if self.goto_dialog_open {
            return self.handle_goto_dialog_input(key);
        }

        if self.tag_dialog_open {
            return self.handle_tag_dialog_input(key);
        }

        if self.note_dialog_open {
            return self.handle_note_dialog_input(key);
        }

        if self.filter_mode {
            return self.handle_filter_input(key);
        }

        match key.code {
            // Pagination Controls
            KeyCode::Left => {
                if self.page > 1 {
                    return SearchAction::SetPage(self.page - 1);
                }
            }
            KeyCode::Right => {
                let total_pages = self.total_items.div_ceil(self.page_size);
                if self.page < total_pages {
                    return SearchAction::SetPage(self.page + 1);
                }
            }
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.goto_dialog_open = true;
                self.goto_input.clear();
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.tag_dialog_open = true;
                if !self.tags.is_empty() {
                    self.tag_list_state.select(Some(0));
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.unique_mode = !self.unique_mode;
                self.page = 1;
                return SearchAction::Reload;
            }

            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter_mode = true;
                self.focus_index = 0;
            }
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(cmd) = self.get_selected_command() {
                    return SearchAction::Copy(cmd);
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(entry) = self.get_selected_entry() {
                    if let Some(id) = entry.id {
                        self.pending_delete_id = Some(id);
                        self.delete_dialog_open = true;
                    }
                }
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(cmd) = self.get_selected_command() {
                    return SearchAction::ToggleBookmark(cmd);
                }
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(entry) = self.get_selected_entry() {
                    if let Some(id) = entry.id {
                        self.note_entry_id = Some(id);
                        // Pre-populate with existing note text if any
                        // (we'll load from repo in the action handler instead)
                        self.note_input.clear();
                        self.note_dialog_open = true;
                    }
                }
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.context_boost = !self.context_boost;
                self.status_message = Some((
                    if self.context_boost {
                        "Smart mode ON".into()
                    } else {
                        "Smart mode OFF".into()
                    },
                    std::time::Instant::now(),
                ));
                return SearchAction::Reload;
            }
            KeyCode::Tab => {
                self.detail_pane_open = !self.detail_pane_open;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.filter_cwd.is_some() {
                    self.filter_cwd = None;
                } else if let Ok(cwd) = std::env::current_dir() {
                    self.filter_cwd = Some(cwd.to_string_lossy().to_string());
                }
                self.page = 1;
                return SearchAction::Reload;
            }
            KeyCode::Char(c) if self.query.len() < MAX_INPUT_LEN => {
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
            KeyCode::Enter => {
                if let Some(cmd) = self.get_selected_command() {
                    return SearchAction::Select(cmd);
                }
            }
            KeyCode::Esc => {
                return SearchAction::Exit;
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_tag_dialog_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Esc => self.tag_dialog_open = false,
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
                        self.tag_dialog_open = false;
                        return SearchAction::AssociateSession(tag.id);
                    }
                }
                self.tag_dialog_open = false;
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_note_dialog_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Esc => {
                self.note_dialog_open = false;
                self.note_input.clear();
                self.note_entry_id = None;
            }
            KeyCode::Enter => {
                if let Some(entry_id) = self.note_entry_id {
                    self.note_dialog_open = false;
                    let text = self.note_input.clone();
                    self.note_input.clear();
                    self.note_entry_id = None;
                    if text.is_empty() {
                        return SearchAction::DeleteNote(entry_id);
                    }
                    return SearchAction::SaveNote(entry_id, text);
                }
                self.note_dialog_open = false;
            }
            KeyCode::Backspace => {
                self.note_input.pop();
            }
            KeyCode::Char(c) if self.note_input.len() < MAX_INPUT_LEN => {
                self.note_input.push(c);
            }
            _ => {}
        }
        SearchAction::Continue
    }

    const fn handle_delete_dialog_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(id) = self.pending_delete_id {
                    self.delete_dialog_open = false;
                    self.pending_delete_id = None;
                    return SearchAction::Delete(id);
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.delete_dialog_open = false;
                self.pending_delete_id = None;
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_goto_dialog_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Enter => {
                if let Ok(page_num) = self.goto_input.parse::<usize>() {
                    let total_pages = self.total_items.div_ceil(self.page_size);
                    let page_num = page_num.max(1).min(total_pages); // Clamp
                    self.goto_dialog_open = false;
                    return SearchAction::SetPage(page_num);
                }
                self.goto_dialog_open = false;
            }
            KeyCode::Esc => {
                self.goto_dialog_open = false;
            }
            KeyCode::Backspace => {
                self.goto_input.pop();
            }
            KeyCode::Char(c) if c.is_ascii_digit() && self.goto_input.len() < MAX_INPUT_LEN => {
                self.goto_input.push(c);
            }
            _ => {}
        }
        SearchAction::Continue
    }

    fn handle_filter_input(&mut self, key: KeyEvent) -> SearchAction {
        match key.code {
            KeyCode::Esc => {
                self.filter_mode = false;
            }
            KeyCode::Tab => {
                self.focus_index = (self.focus_index + 1) % 5;
            }
            KeyCode::BackTab => {
                self.focus_index = if self.focus_index == 0 {
                    4
                } else {
                    self.focus_index - 1
                };
            }
            KeyCode::Enter => {
                // Apply filters
                self.filter_after = if self.start_date_input.is_empty() {
                    None
                } else {
                    util::parse_date_input(&self.start_date_input, false)
                };

                self.filter_before = if self.end_date_input.is_empty() {
                    None
                } else {
                    util::parse_date_input(&self.end_date_input, true)
                };

                // Resolve tag name to ID
                self.filter_tag_id = if self.tag_filter_input.is_empty() {
                    None
                } else {
                    let input_lower = self.tag_filter_input.to_lowercase();
                    self.tags
                        .iter()
                        .find(|t| t.name == input_lower)
                        .map(|t| t.id)
                };

                // Parse exit code
                self.filter_exit_code = if self.exit_code_input.is_empty() {
                    None
                } else {
                    self.exit_code_input.trim().parse::<i32>().ok()
                };

                // Parse executor filter
                self.filter_executor_type = if self.executor_filter_input.is_empty() {
                    None
                } else {
                    Some(self.executor_filter_input.trim().to_lowercase())
                };

                self.filter_mode = false;
                // Reset to page 1 on new filter
                self.page = 1;
                return SearchAction::Reload;
            }
            KeyCode::Backspace => match self.focus_index {
                0 => {
                    self.start_date_input.pop();
                }
                1 => {
                    self.end_date_input.pop();
                }
                2 => {
                    self.tag_filter_input.pop();
                }
                3 => {
                    self.exit_code_input.pop();
                }
                4 => {
                    self.executor_filter_input.pop();
                }
                _ => {}
            },
            KeyCode::Char(c) => match self.focus_index {
                0 if self.start_date_input.len() < MAX_INPUT_LEN => {
                    self.start_date_input.push(c);
                }
                1 if self.end_date_input.len() < MAX_INPUT_LEN => {
                    self.end_date_input.push(c);
                }
                2 if self.tag_filter_input.len() < MAX_INPUT_LEN => {
                    self.tag_filter_input.push(c);
                }
                3 if self.exit_code_input.len() < MAX_INPUT_LEN => {
                    self.exit_code_input.push(c);
                }
                4 if self.executor_filter_input.len() < MAX_INPUT_LEN => {
                    self.executor_filter_input.push(c);
                }
                _ => {}
            },
            _ => {}
        }
        SearchAction::Continue
    }
}
