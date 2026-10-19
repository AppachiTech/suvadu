use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{DialogState, RecallScope, SearchAction, SearchApp, VimMode};
use crate::util;

/// Maximum length, in bytes, of any text input field (query, filters,
/// notes, etc.).
const MAX_INPUT_LEN: usize = 2000;

/// Whether `c` still fits in `input` under [`MAX_INPUT_LEN`], in bytes.
/// The filter popup's text fields, read: what Enter applies.
struct ReadFilters {
    after: Option<i64>,
    before: Option<i64>,
    tag_id: Option<i64>,
    exit_code: Option<i32>,
}

const fn fits(input: &str, c: char) -> bool {
    input.len() + c.len_utf8() <= MAX_INPUT_LEN
}

/// Append as much of `text` as fits under [`MAX_INPUT_LEN`] bytes, stopping
/// at the last whole character that fits — the same cap typing applies.
fn push_capped(input: &mut String, text: &str) {
    for c in text.chars() {
        if !fits(input, c) {
            break;
        }
        input.push(c);
    }
}

/// Rows to move per `PageUp`/`PageDown` press.
const PAGE_SCROLL_LINES: usize = 10;
/// Rows one ↑/↓ scrolls the focused detail pane: a mouse-wheel step, since
/// one row per key press crawls through a long command.
const DETAIL_SCROLL_LINES: u16 = 3;

/// Flatten pasted text into a single searchable line.
///
/// Newlines, carriage returns and tabs separated words in the source text, so
/// they become a single space — dropping them outright would silently weld
/// `cargo test` and `--offline` into `cargo test--offline`, which matches
/// nothing. Every other control character is removed, runs of whitespace
/// collapse, and the result is trimmed so a paste ending in a newline does
/// not leave a trailing space that `literal` mode would then have to match.
pub(super) fn sanitize_pasted_text(text: &str) -> String {
    let spaced: String = text
        .chars()
        .filter_map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                Some(' ')
            } else if c.is_control() {
                None
            } else {
                Some(c)
            }
        })
        .collect();

    let mut out = String::with_capacity(spaced.len());
    let mut last_was_space = false;
    for c in spaced.chars() {
        if c == ' ' {
            if !last_was_space {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    out.trim().to_string()
}

impl SearchApp {
    const fn move_result_up(&mut self) -> SearchAction {
        if let Some(selected) = self.table_state.selected() {
            if selected > 0 {
                self.table_state.select(Some(selected - 1));
            } else if self.pagination.page > 1 {
                return SearchAction::SetPageLast(self.pagination.page - 1);
            }
        }
        SearchAction::Continue
    }

    const fn move_result_down(&mut self) -> SearchAction {
        if let Some(selected) = self.table_state.selected() {
            if selected + 1 < self.entries.len() {
                self.table_state.select(Some(selected + 1));
            } else {
                let total_pages = self
                    .pagination
                    .total_items
                    .div_ceil(self.pagination.page_size);
                if self.pagination.page < total_pages {
                    return SearchAction::SetPage(self.pagination.page + 1);
                }
            }
        } else if !self.entries.is_empty() {
            self.table_state.select(Some(0));
        }
        SearchAction::Continue
    }

    /// Handle a bracketed-paste event. Returns `true` if the paste modified
    /// the main search query (caller should reload), `false` otherwise.
    pub(super) fn handle_paste(&mut self, text: &str) -> bool {
        let sanitized = sanitize_pasted_text(text);

        if sanitized.is_empty() {
            return false;
        }

        match self.dialog {
            DialogState::Note { ref mut input, .. } => {
                push_capped(input, &sanitized);
                false
            }
            DialogState::GoToPage { ref mut input } => {
                // Only accept digits
                let digits: String = sanitized.chars().filter(char::is_ascii_digit).collect();
                push_capped(input, &digits);
                false
            }
            DialogState::Filter => {
                self.paste_into_filter_field(&sanitized);
                false
            }
            DialogState::Help
            | DialogState::Delete { .. }
            | DialogState::TagAssociation
            | DialogState::RawView { .. } => false,
            DialogState::None => {
                // A paste types into the query, so, like typing, it hands
                // focus back from the detail pane to the list it changes.
                self.set_detail_focus(false);
                // In vim Normal mode, auto-switch to Insert mode on paste
                if self.vim_enabled && self.vim_mode == VimMode::Normal {
                    self.vim_mode = VimMode::Insert;
                }
                push_capped(&mut self.query, &sanitized);
                self.reselect = None;
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
        push_capped(target, text);
        self.filters.error = None;
    }

    pub(super) fn handle_input(&mut self, key: KeyEvent) -> SearchAction {
        // Raw mode delivers Ctrl+C as a key, not SIGINT. It cancels from
        // anywhere — a dialog, the detail pane, vim Normal — exactly as Esc
        // does at the top: nothing is accepted and the shell keeps its buffer.
        // Lowercase only: Ctrl+Shift+C is copy in many terminals.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return SearchAction::Exit;
        }
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
            DialogState::RawView { .. } => return self.handle_raw_view_input(key),
            DialogState::None => {}
        }
        self.handle_normal_input(key)
    }

    fn handle_normal_input(&mut self, key: KeyEvent) -> SearchAction {
        // Shift+Tab moves focus into the detail pane when it has more than
        // fits (as between panels in `suv stats`), and back out.
        if key.code == KeyCode::BackTab
            && (self.detail_scroll.focused || self.detail_scroll.overflow)
        {
            self.set_detail_focus(!self.detail_scroll.focused);
            return SearchAction::Continue;
        }
        if self.detail_scroll.focused {
            let page = self.detail_scroll.page;
            let offset = &mut self.detail_scroll.offset;
            // Vim Normal mode scrolls with its own motion keys too.
            let vim = self.vim_enabled
                && self.vim_mode == VimMode::Normal
                && !key.modifiers.contains(KeyModifiers::CONTROL);
            let code = match key.code {
                KeyCode::Char('j') if vim => KeyCode::Down,
                KeyCode::Char('k') if vim => KeyCode::Up,
                KeyCode::Char('g') if vim => KeyCode::Home,
                KeyCode::Char('G') if vim => KeyCode::End,
                code => code,
            };
            match code {
                KeyCode::Up => *offset = offset.saturating_sub(DETAIL_SCROLL_LINES),
                KeyCode::Down => *offset = offset.saturating_add(DETAIL_SCROLL_LINES),
                KeyCode::PageUp => *offset = offset.saturating_sub(page),
                KeyCode::PageDown => *offset = offset.saturating_add(page),
                KeyCode::Home => *offset = 0,
                KeyCode::End => *offset = u16::MAX,
                KeyCode::Esc => self.set_detail_focus(false),
                // Anything else hands focus back to the list and does its
                // usual job there, so the pane never traps a keystroke.
                _ => {
                    self.set_detail_focus(false);
                    return self.handle_list_input(key);
                }
            }
            return SearchAction::Continue;
        }
        self.handle_list_input(key)
    }

    fn handle_list_input(&mut self, key: KeyEvent) -> SearchAction {
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
            KeyCode::Char(c) if fits(&self.query, c) => {
                self.query.push(c);
                self.reselect = None;
                return SearchAction::Reload;
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.reselect = None;
                return SearchAction::Reload;
            }
            KeyCode::Up => return self.move_result_up(),
            KeyCode::Down => return self.move_result_down(),
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
                if self.entries.is_empty() {
                    // Nothing to accept, so accept nothing and close: a query
                    // that matches nothing must not trap the user in recall.
                    // The shell keeps its buffer, exactly as on Esc.
                    return SearchAction::Exit;
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
            KeyCode::Char('j') | KeyCode::Down => return self.move_result_down(),
            KeyCode::Char('k') | KeyCode::Up => return self.move_result_up(),
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
                if self.entries.is_empty() {
                    // Nothing to accept, so accept nothing and close: a query
                    // that matches nothing must not trap the user in recall.
                    // The shell keeps its buffer, exactly as on Esc.
                    return SearchAction::Exit;
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

    /// Focus the detail pane, or give focus back to the list. The pane
    /// always leaves focus scrolled to the top, so the next row starts there.
    pub(super) const fn set_detail_focus(&mut self, focused: bool) {
        self.detail_scroll.focused = focused;
        if !focused {
            self.detail_scroll.offset = 0;
        }
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
                // Grouping only changes how the same matches are shown, so
                // the command being looked at stays selected.
                self.reselect = self.get_selected_command();
                return Some(SearchAction::Reload);
            }
            KeyCode::Char('f') => {
                self.dialog = DialogState::Filter;
                self.filters.focus_index = 0;
                self.filters.error = None;
            }
            KeyCode::Char('v') => {
                // Verbatim: the selected command's exact text, every
                // character visible, however long it is.
                if let Some(command) = self.get_selected_command() {
                    self.dialog = DialogState::RawView { command, scroll: 0 };
                } else {
                    self.status_message =
                        Some(("Nothing selected".into(), std::time::Instant::now()));
                }
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
                self.reselect = self.get_selected_command();
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
            // The long-standing "here" toggle, now expressed as a scope: it
            // flips between all history and this directory and leaves the
            // wider scopes to ^P.
            KeyCode::Char('l') => {
                let next = if self.recall.scope == RecallScope::Directory {
                    RecallScope::All
                } else {
                    RecallScope::Directory
                };
                self.set_scope(next);
                return Some(SearchAction::Reload);
            }
            // Cycle the matching mode (terms → literal → prefix → fuzzy).
            KeyCode::Char('x') => return Some(self.cycle_match_mode()),
            // Cycle the recall scope, skipping any that cannot apply here.
            KeyCode::Char('p') => return Some(self.cycle_scope()),
            // One action back to all of history.
            KeyCode::Char('r') => return Some(self.reset_to_all_history()),
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
                    if fits(input, c) {
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

    /// Keys in the raw view: scroll, or close. Nothing here accepts a
    /// command — Enter closes it, back to the list the command came from.
    fn handle_raw_view_input(&mut self, key: KeyEvent) -> SearchAction {
        let DialogState::RawView { ref mut scroll, .. } = self.dialog else {
            return SearchAction::Continue;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if !ctrl => *scroll = scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') if !ctrl => *scroll = scroll.saturating_add(1),
            KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
            KeyCode::PageDown => *scroll = scroll.saturating_add(10),
            KeyCode::Home => *scroll = 0,
            KeyCode::End => *scroll = u16::MAX,
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                self.dialog = DialogState::None;
            }
            KeyCode::Char('v') if ctrl => self.dialog = DialogState::None,
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
                    if fits(input, c) {
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
                self.filters.error = None;
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
            KeyCode::Backspace => {
                self.filters.error = None;
                self.pop_filter_char();
            }
            KeyCode::Char(c) => {
                self.filters.error = None;
                self.push_filter_char(c);
            }
            _ => {}
        }
        SearchAction::Continue
    }

    /// Delete the last character of the focused text field.
    fn pop_filter_char(&mut self) {
        match self.filters.focus_index {
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
        }
    }

    /// Type `c` into the focused text field, within the length limit.
    fn push_filter_char(&mut self, c: char) {
        match self.filters.focus_index {
            0 if fits(&self.filters.start_date_input, c) => {
                self.filters.start_date_input.push(c);
            }
            1 if fits(&self.filters.end_date_input, c) => {
                self.filters.end_date_input.push(c);
            }
            2 if fits(&self.filters.tag_filter_input, c) => {
                self.filters.tag_filter_input.push(c);
            }
            3 if fits(&self.filters.exit_code_input, c) => {
                self.filters.exit_code_input.push(c);
            }
            // field 4 is a selector, no text input
            _ => {}
        }
    }

    /// Apply the popup's fields, all or nothing. A field that cannot be read
    /// keeps every applied filter as it was and leaves the popup open on that
    /// field, saying why; a blank field clears its filter.
    fn apply_filters(&mut self) -> SearchAction {
        let read = match self.read_filter_fields() {
            Ok(read) => read,
            Err((field, message)) => {
                self.filters.focus_index = field;
                self.filters.error = Some(message);
                return SearchAction::Continue;
            }
        };
        self.filters.error = None;
        self.filters.after = read.after;
        self.filters.before = read.before;
        self.filters.tag_id = read.tag_id;
        self.filters.exit_code = read.exit_code;

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

    /// The filter popup's text fields as filter values, or the first field
    /// (in the popup's order) that cannot be read and why.
    fn read_filter_fields(&self) -> Result<ReadFilters, (usize, String)> {
        let date = |field: usize, which: &str, input: &str, end_of_day: bool| {
            let input = input.trim();
            if input.is_empty() {
                return Ok(None);
            }
            util::parse_date_input(input, end_of_day)
                .map(Some)
                .ok_or_else(|| {
                    (
                        field,
                        format!(
                            "Can't read the {which} date \"{input}\". Accepted: {}",
                            util::DATE_INPUT_FORMATS
                        ),
                    )
                })
        };
        let after = date(0, "start", &self.filters.start_date_input, false)?;
        let before = date(1, "end", &self.filters.end_date_input, true)?;

        let tag = self.filters.tag_filter_input.trim();
        let tag_id = if tag.is_empty() {
            None
        } else {
            let wanted = tag.to_lowercase();
            let found = self.tags.iter().find(|t| t.name == wanted).map(|t| t.id);
            if found.is_none() {
                let known: Vec<&str> = self.tags.iter().map(|t| t.name.as_str()).collect();
                let hint = if known.is_empty() {
                    "There are no tags yet".to_string()
                } else {
                    format!("Tags: {}", known.join(", "))
                };
                return Err((2, format!("No tag named \"{tag}\". {hint}")));
            }
            found
        };

        let code = self.filters.exit_code_input.trim();
        let exit_code = if code.is_empty() {
            None
        } else {
            Some(code.parse::<i32>().map_err(|_| {
                (
                    3,
                    format!(
                        "Can't read the exit code \"{code}\". Use a whole number, such as 0 or 1"
                    ),
                )
            })?)
        };
        Ok(ReadFilters {
            after,
            before,
            tag_id,
            exit_code,
        })
    }
}
