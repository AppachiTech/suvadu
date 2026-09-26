//! Home's navigation state and keyboard contract, with no terminal in it:
//! every key and paste becomes a state change plus at most one action for
//! the controller (launch, copy, exit). Selections are kept by feature ID,
//! never by list position, so a changed list cannot move the cursor onto a
//! different feature.

use std::cell::Cell;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

use super::catalog::{self, Action, CategoryId, Feature, FeatureId, LaunchRequest, REFERENCE};
use super::reference::{self, Topic};
use crate::config::HomeIcons;

/// Longest feature query, in graphemes.
pub const MAX_QUERY_GRAPHEMES: usize = 256;

/// Lines PageUp/PageDown scroll in a text view.
const PAGE: u16 = 10;
/// Rows PageUp/PageDown move in a list.
const PAGE_ROWS: isize = 10;

/// What the controller has to do after an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HomeAction {
    /// Nothing changed.
    None,
    Redraw,
    /// Run a catalog feature. Built only from the catalog, never from input.
    Launch(LaunchRequest),
    /// Put this text on the clipboard. Copying never runs it.
    Copy(String),
    Exit,
    /// Ctrl+C: leave Home with the conventional interrupted status.
    Interrupt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Category(CategoryId),
    Feature(FeatureId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    List,
    Detail,
}

/// Home's three layouts, plus the size below which it only asks for room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutClass {
    /// List and detail side by side: at least 100×24.
    Wide,
    /// A list with a short explanation under it: at least 70×20.
    Medium,
    /// One pane at a time: at least 40×10.
    Narrow,
    TooSmall,
}

pub const fn layout_class(width: u16, height: u16) -> LayoutClass {
    if width < 40 || height < 10 {
        LayoutClass::TooSmall
    } else if width < 70 || height < 20 {
        LayoutClass::Narrow
    } else if width < 100 || height < 24 {
        LayoutClass::Medium
    } else {
        LayoutClass::Wide
    }
}

/// A selectable action on a page or result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Open,
    CopyCommand,
    CopyExample,
    CopySelection,
    Reference,
    Retry,
    Back,
}

/// What came back from a launched feature, ready to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The exact command a picker returned. It has not been run.
    Selected(String),
    NothingSelected,
    Report {
        output: String,
        errors: String,
        code: Option<i32>,
        truncated: bool,
    },
    Failed {
        message: String,
        retry: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// A feature's page: a guide's instructions, or the full details of any
    /// feature where the layout has no room beside the list.
    Page {
        feature: FeatureId,
        example: usize,
        scroll: u16,
        button: usize,
    },
    /// The command reference; `reading` means the text has the keys rather
    /// than the topic list.
    Reference {
        topic: usize,
        scroll: u16,
        reading: bool,
    },
    Keys {
        scroll: u16,
        button: usize,
    },
    Result {
        feature: FeatureId,
        outcome: Outcome,
        scroll: u16,
        button: usize,
    },
}

/// Recording facts for the status line, from configuration and the
/// environment only. Whether capture works is not claimed here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HomeStatus {
    /// `None` when the config could not be read.
    pub recording: Option<bool>,
    pub paused: bool,
    pub warning: Option<String>,
}

pub struct HomeState {
    category: Option<CategoryId>,
    root_selection: Row,
    category_selection: Option<FeatureId>,
    search_selection: Option<FeatureId>,
    query: String,
    /// Byte offset into `query`, always on a grapheme boundary.
    cursor: usize,
    focus: Focus,
    detail_scroll: u16,
    views: Vec<View>,
    /// A one-line message for the status row, and whether it is an error.
    notice: Option<(String, bool)>,
    viewport: (u16, u16),
    topics: Vec<Topic>,
    /// Largest useful scroll for the detail pane and the top view, as the
    /// renderer last measured them.
    detail_scroll_limit: Cell<u16>,
    view_scroll_limit: Cell<u16>,
    pub icons: HomeIcons,
    pub status: HomeStatus,
    /// `NO_COLOR` is set: draw without colour, keep every label.
    pub no_color: bool,
}

impl Default for HomeState {
    fn default() -> Self {
        Self::new()
    }
}

impl HomeState {
    pub fn new() -> Self {
        Self {
            category: None,
            root_selection: Row::Category(catalog::categories()[0].id),
            category_selection: None,
            search_selection: None,
            query: String::new(),
            cursor: 0,
            focus: Focus::List,
            detail_scroll: 0,
            views: Vec::new(),
            notice: None,
            viewport: (80, 24),
            topics: reference::topics(),
            detail_scroll_limit: Cell::new(u16::MAX),
            view_scroll_limit: Cell::new(u16::MAX),
            icons: HomeIcons::default(),
            status: HomeStatus::default(),
            no_color: false,
        }
    }

    // ── Reading the state ──────────────────────────────────────

    pub fn query(&self) -> &str {
        &self.query
    }

    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    pub const fn category(&self) -> Option<CategoryId> {
        self.category
    }

    pub const fn focus(&self) -> Focus {
        self.focus
    }

    pub const fn detail_scroll(&self) -> u16 {
        self.detail_scroll
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_ref().map(|(text, _)| text.as_str())
    }

    pub fn notice_is_error(&self) -> bool {
        self.notice.as_ref().is_some_and(|(_, error)| *error)
    }

    pub fn view(&self) -> Option<&View> {
        self.views.last()
    }

    pub fn topics(&self) -> &[Topic] {
        &self.topics
    }

    pub const fn layout(&self) -> LayoutClass {
        layout_class(self.viewport.0, self.viewport.1)
    }

    /// True while the list shows feature-search results.
    pub fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// The rows the list shows now: search results, a category's features,
    /// or the categories followed by the command reference.
    pub fn rows(&self) -> Vec<Row> {
        if self.searching() {
            return catalog::search_features(&self.query)
                .into_iter()
                .map(Row::Feature)
                .collect();
        }
        if let Some(category) = self.category {
            return catalog::category_features(category)
                .into_iter()
                .map(|f| Row::Feature(f.id))
                .collect();
        }
        catalog::categories()
            .iter()
            .map(|c| Row::Category(c.id))
            .chain(std::iter::once(Row::Feature(REFERENCE)))
            .collect()
    }

    /// The highlighted row: the remembered one when it is still listed,
    /// otherwise the first row, and nothing when the list is empty.
    pub fn selected_row(&self) -> Option<Row> {
        let rows = self.rows();
        let wanted = if self.searching() {
            self.search_selection.map(Row::Feature)
        } else if self.category.is_some() {
            self.category_selection.map(Row::Feature)
        } else {
            Some(self.root_selection)
        };
        wanted
            .filter(|row| rows.contains(row))
            .or_else(|| rows.first().copied())
    }

    pub fn selected_feature(&self) -> Option<FeatureId> {
        match self.selected_row()? {
            Row::Feature(id) => Some(id),
            Row::Category(_) => None,
        }
    }

    /// The buttons of the top view, in order; empty with no view open.
    pub fn buttons(&self) -> Vec<Button> {
        match self.view() {
            None | Some(View::Reference { .. }) => Vec::new(),
            Some(View::Page { feature, .. }) => catalog::feature(*feature)
                .map(page_buttons)
                .unwrap_or_default(),
            Some(View::Keys { .. }) => vec![Button::Reference, Button::Back],
            Some(View::Result { outcome, .. }) => result_buttons(outcome),
        }
    }

    /// The focused button of the top view.
    pub fn button(&self) -> Option<usize> {
        match self.view()? {
            View::Page { button, .. } | View::Keys { button, .. } | View::Result { button, .. } => {
                Some(*button)
            }
            View::Reference { .. } => None,
        }
    }

    // ── Changing it from outside ───────────────────────────────

    pub fn set_viewport(&mut self, width: u16, height: u16) {
        self.viewport = (width, height);
        if self.layout() != LayoutClass::Wide {
            self.focus = Focus::List;
        }
    }

    /// Replace the query, cursor at the end.
    #[cfg(test)]
    pub fn set_query(&mut self, query: &str) {
        self.query.clear();
        self.cursor = 0;
        self.insert(query);
    }

    /// The renderer reports how far the detail pane can scroll, so keys
    /// never scroll past the end.
    pub fn set_detail_scroll_limit(&self, limit: u16) {
        self.detail_scroll_limit.set(limit);
    }

    /// As [`Self::set_detail_scroll_limit`], for the open view.
    pub fn set_view_scroll_limit(&self, limit: u16) {
        self.view_scroll_limit.set(limit);
    }

    /// Show what a launched feature returned.
    pub fn show_outcome(&mut self, feature: FeatureId, outcome: Outcome) {
        self.views.push(View::Result {
            feature,
            outcome,
            scroll: 0,
            button: 0,
        });
    }

    /// A one-line message in the status row until the next key.
    pub fn set_notice(&mut self, text: String, error: bool) {
        self.notice = Some((text, error));
    }

    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    /// Report a clipboard write. Success is claimed only after it happened.
    pub fn copied(&mut self, _text: &str, result: &Result<(), String>) {
        self.notice = Some(match result {
            Ok(()) => (
                "Copied to the clipboard. It has not been run.".to_string(),
                false,
            ),
            Err(e) => (
                format!(
                    "Could not copy ({e}). Select the text on screen with your terminal instead."
                ),
                true,
            ),
        });
    }

    // ── Events ─────────────────────────────────────────────────

    pub fn on_event(&mut self, event: Event) -> HomeAction {
        match event {
            Event::Key(key) => self.on_key(key),
            Event::Paste(text) if self.views.is_empty() => {
                self.notice = None;
                self.insert(&clean_paste(&text));
                HomeAction::Redraw
            }
            Event::Resize(width, height) => {
                self.set_viewport(width, height);
                HomeAction::Redraw
            }
            _ => HomeAction::None,
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> HomeAction {
        if key.kind == KeyEventKind::Release {
            return HomeAction::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
            return HomeAction::Interrupt;
        }
        // A held key may keep moving or editing, never keep opening things.
        if key.kind == KeyEventKind::Repeat && !repeatable(key.code) {
            return HomeAction::None;
        }
        self.notice = None;
        if ctrl && matches!(key.code, KeyCode::Char('r' | 'R')) {
            return catalog::launch_request(FeatureId("search"))
                .map_or(HomeAction::None, HomeAction::Launch);
        }
        if self.views.is_empty() {
            self.on_main_key(key, ctrl)
        } else {
            self.on_view_key(key)
        }
    }

    fn on_main_key(&mut self, key: KeyEvent, ctrl: bool) -> HomeAction {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('u' | 'U') if ctrl => {
                self.clear_query();
                HomeAction::Redraw
            }
            KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => {
                self.insert(c.encode_utf8(&mut [0; 4]));
                HomeAction::Redraw
            }
            KeyCode::Backspace => self.edit(|q, cursor| prev_boundary(q, cursor)..cursor),
            KeyCode::Delete => self.edit(|q, cursor| cursor..next_boundary(q, cursor)),
            KeyCode::Left => self.move_cursor(prev_boundary(&self.query, self.cursor)),
            KeyCode::Right => self.move_cursor(next_boundary(&self.query, self.cursor)),
            KeyCode::Home => self.move_cursor(0),
            KeyCode::End => self.move_cursor(self.query.len()),
            KeyCode::Up if self.focus == Focus::Detail => self.scroll_detail(-1),
            KeyCode::Down if self.focus == Focus::Detail => self.scroll_detail(1),
            KeyCode::PageUp if self.focus == Focus::Detail => self.scroll_detail(-i32::from(PAGE)),
            KeyCode::PageDown if self.focus == Focus::Detail => self.scroll_detail(i32::from(PAGE)),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-PAGE_ROWS),
            KeyCode::PageDown => self.move_selection(PAGE_ROWS),
            KeyCode::Enter => self
                .selected_row()
                .map_or(HomeAction::None, |row| self.activate(row)),
            KeyCode::Tab | KeyCode::BackTab => self.on_tab(),
            KeyCode::F(1) => self.push(View::Keys {
                scroll: 0,
                button: 1,
            }),
            KeyCode::F(2) => {
                let topic = self.selected_feature().map_or(0, |id| self.topic_for(id));
                self.push(View::Reference {
                    topic,
                    scroll: 0,
                    reading: false,
                })
            }
            KeyCode::Esc => self.on_main_esc(),
            _ => HomeAction::None,
        }
    }

    fn on_main_esc(&mut self) -> HomeAction {
        if self.focus == Focus::Detail {
            self.focus = Focus::List;
        } else if !self.query.is_empty() {
            self.clear_query();
        } else if let Some(category) = self.category.take() {
            self.root_selection = Row::Category(category);
            self.detail_scroll = 0;
        } else {
            return HomeAction::Exit;
        }
        HomeAction::Redraw
    }

    fn on_tab(&mut self) -> HomeAction {
        if self.layout() == LayoutClass::Wide {
            self.focus = match self.focus {
                Focus::List => Focus::Detail,
                Focus::Detail => Focus::List,
            };
            return HomeAction::Redraw;
        }
        self.selected_feature().map_or(HomeAction::None, |feature| {
            self.push(View::Page {
                feature,
                example: 0,
                scroll: 0,
                button: 0,
            })
        })
    }

    fn activate(&mut self, row: Row) -> HomeAction {
        match row {
            Row::Category(category) => {
                self.category = Some(category);
                self.root_selection = row;
                self.category_selection =
                    catalog::category_features(category).first().map(|f| f.id);
                self.focus = Focus::List;
                self.detail_scroll = 0;
                HomeAction::Redraw
            }
            Row::Feature(id) => self.activate_feature(id),
        }
    }

    fn activate_feature(&mut self, id: FeatureId) -> HomeAction {
        let Some(feature) = catalog::feature(id) else {
            return HomeAction::None;
        };
        match feature.action {
            Action::Open(_) => {
                catalog::launch_request(id).map_or(HomeAction::None, HomeAction::Launch)
            }
            Action::Guide => self.push(View::Page {
                feature: id,
                example: 0,
                scroll: 0,
                button: 0,
            }),
            Action::Reference => self.push(View::Reference {
                topic: 0,
                scroll: 0,
                reading: false,
            }),
        }
    }

    fn on_view_key(&mut self, key: KeyEvent) -> HomeAction {
        let buttons = self.buttons();
        let limit = self.view_scroll_limit.get();
        match key.code {
            KeyCode::F(1) if !matches!(self.view(), Some(View::Keys { .. })) => {
                return self.push(View::Keys {
                    scroll: 0,
                    button: 1,
                });
            }
            KeyCode::F(2) => {
                let topic = match self.view() {
                    Some(View::Reference { .. }) => return HomeAction::None,
                    Some(View::Page { feature, .. }) => self.topic_for(*feature),
                    _ => 0,
                };
                return self.push(View::Reference {
                    topic,
                    scroll: 0,
                    reading: false,
                });
            }
            KeyCode::Esc => {
                if let Some(View::Reference { reading, .. }) = self.views.last_mut() {
                    if *reading {
                        *reading = false;
                        return HomeAction::Redraw;
                    }
                }
                self.views.pop();
                return HomeAction::Redraw;
            }
            _ => {}
        }
        let last_topic = self.topics.len().saturating_sub(1);
        // Change the top view first; pressing a button needs `self` whole.
        let handled = match self.views.last_mut() {
            None => Some(HomeAction::None),
            Some(View::Reference {
                topic,
                scroll,
                reading,
            }) => Some(reference_key(
                key.code, topic, scroll, reading, last_topic, limit,
            )),
            Some(View::Page {
                feature,
                example,
                scroll,
                button,
            }) => {
                let examples = catalog::feature(*feature).map_or(0, |f| f.examples.len());
                match key.code {
                    KeyCode::Up if examples > 0 => {
                        *example = example.saturating_sub(1);
                        Some(HomeAction::Redraw)
                    }
                    KeyCode::Down if examples > 0 => {
                        *example = (*example + 1).min(examples - 1);
                        Some(HomeAction::Redraw)
                    }
                    code => Self::common_view_key(code, scroll, button, &buttons, limit),
                }
            }
            Some(View::Keys { scroll, button } | View::Result { scroll, button, .. }) => {
                Self::common_view_key(key.code, scroll, button, &buttons, limit)
            }
        };
        handled.unwrap_or_else(|| {
            let focused = self.button().and_then(|i| buttons.get(i).copied());
            self.press(focused)
        })
    }

    /// Scrolling and button movement shared by pages, keys and results.
    /// `None` means the key was Enter: press the focused button.
    fn common_view_key(
        code: KeyCode,
        scroll: &mut u16,
        button: &mut usize,
        buttons: &[Button],
        limit: u16,
    ) -> Option<HomeAction> {
        let count = buttons.len().max(1);
        match code {
            KeyCode::Enter => return None,
            KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::Down => *scroll = scroll.saturating_add(1).min(limit),
            KeyCode::PageUp => *scroll = scroll.saturating_sub(PAGE),
            KeyCode::PageDown => *scroll = scroll.saturating_add(PAGE).min(limit),
            KeyCode::Home => *scroll = 0,
            KeyCode::Right | KeyCode::Tab => *button = (*button + 1) % count,
            KeyCode::Left | KeyCode::BackTab => *button = (*button + count - 1) % count,
            _ => return Some(HomeAction::None),
        }
        Some(HomeAction::Redraw)
    }

    fn press(&mut self, button: Option<Button>) -> HomeAction {
        let Some(button) = button else {
            return HomeAction::None;
        };
        let (feature, example, selection) = match self.views.last() {
            Some(View::Page {
                feature, example, ..
            }) => (Some(*feature), *example, None),
            Some(View::Result {
                feature, outcome, ..
            }) => {
                let selection = match outcome {
                    Outcome::Selected(command) => Some(command.clone()),
                    _ => None,
                };
                (Some(*feature), 0, selection)
            }
            _ => (None, 0, None),
        };
        let found = feature.and_then(catalog::feature);
        match button {
            Button::Back => {
                self.views.pop();
                HomeAction::Redraw
            }
            Button::Reference => {
                let topic = feature.map_or(0, |id| self.topic_for(id));
                self.push(View::Reference {
                    topic,
                    scroll: 0,
                    reading: false,
                })
            }
            Button::Open => match found {
                Some(f) if f.action == Action::Reference => self.push(View::Reference {
                    topic: 0,
                    scroll: 0,
                    reading: false,
                }),
                Some(f) => {
                    catalog::launch_request(f.id).map_or(HomeAction::None, HomeAction::Launch)
                }
                None => HomeAction::None,
            },
            Button::CopyCommand => found.map_or(HomeAction::None, |f| {
                HomeAction::Copy(f.command.to_string())
            }),
            Button::CopyExample => found
                .and_then(|f| f.examples.get(example))
                .map_or(HomeAction::None, |e| {
                    HomeAction::Copy(e.command.to_string())
                }),
            Button::CopySelection => selection.map_or(HomeAction::None, HomeAction::Copy),
            Button::Retry => {
                self.views.pop();
                feature
                    .and_then(catalog::launch_request)
                    .map_or(HomeAction::Redraw, HomeAction::Launch)
            }
        }
    }

    // ── Helpers ────────────────────────────────────────────────

    fn push(&mut self, view: View) -> HomeAction {
        self.views.push(view);
        HomeAction::Redraw
    }

    /// Where the reference opens for a feature: its command's own help.
    fn topic_for(&self, id: FeatureId) -> usize {
        let Some(feature) = catalog::feature(id) else {
            return 0;
        };
        if feature.action == Action::Reference {
            return 0;
        }
        let path = feature
            .command_paths
            .first()
            .copied()
            .or_else(|| feature.args.first().copied())
            .unwrap_or("");
        let parts: Vec<&str> = path.split(' ').filter(|p| !p.is_empty()).collect();
        reference::topic_index(&self.topics, &parts)
    }

    fn move_selection(&mut self, delta: isize) -> HomeAction {
        let rows = self.rows();
        let Some(current) = self.selected_row() else {
            return HomeAction::None;
        };
        let index = rows.iter().position(|r| *r == current).unwrap_or(0);
        let last = rows.len().saturating_sub(1);
        let next = index.saturating_add_signed(delta).min(last);
        if next == index {
            return HomeAction::None;
        }
        self.select(rows[next]);
        HomeAction::Redraw
    }

    fn select(&mut self, row: Row) {
        self.detail_scroll = 0;
        if self.searching() {
            self.search_selection = feature_of(row);
        } else if self.category.is_some() {
            self.category_selection = feature_of(row);
        } else {
            self.root_selection = row;
        }
    }

    fn scroll_detail(&mut self, delta: i32) -> HomeAction {
        let limit = i32::from(self.detail_scroll_limit.get());
        let next = (i32::from(self.detail_scroll) + delta).clamp(0, limit);
        self.detail_scroll = u16::try_from(next).unwrap_or(0);
        HomeAction::Redraw
    }

    const fn move_cursor(&mut self, to: usize) -> HomeAction {
        if to == self.cursor {
            return HomeAction::None;
        }
        self.cursor = to;
        HomeAction::Redraw
    }

    fn edit(&mut self, range: impl FnOnce(&str, usize) -> std::ops::Range<usize>) -> HomeAction {
        let range = range(&self.query, self.cursor);
        if range.is_empty() {
            return HomeAction::None;
        }
        self.cursor = range.start;
        self.query.replace_range(range, "");
        self.after_query_change();
        HomeAction::Redraw
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.cursor = 0;
        self.search_selection = None;
        self.after_query_change();
    }

    /// Insert at the cursor, up to the length limit; anything beyond it is
    /// left out with a notice rather than silently dropped.
    fn insert(&mut self, text: &str) {
        let room = MAX_QUERY_GRAPHEMES.saturating_sub(self.query.graphemes(true).count());
        let incoming: Vec<&str> = text.graphemes(true).collect();
        let piece: String = incoming.iter().take(room).copied().collect();
        if incoming.len() > room {
            self.notice = Some((
                format!(
                    "Feature search is limited to {MAX_QUERY_GRAPHEMES} characters; the rest was not added."
                ),
                true,
            ));
        }
        self.query.insert_str(self.cursor, &piece);
        self.cursor += piece.len();
        self.after_query_change();
    }

    fn after_query_change(&mut self) {
        self.detail_scroll = 0;
        if self.searching() {
            self.search_selection = self.selected_row().and_then(feature_of);
        } else {
            self.search_selection = None;
        }
    }
}

/// Keys in the reference: topics move until the text has the keys, then
/// the text scrolls. Tab switches, Enter starts reading.
fn reference_key(
    code: KeyCode,
    topic: &mut usize,
    scroll: &mut u16,
    reading: &mut bool,
    last_topic: usize,
    limit: u16,
) -> HomeAction {
    match code {
        KeyCode::Tab | KeyCode::BackTab => *reading = !*reading,
        KeyCode::Enter => *reading = true,
        KeyCode::Up | KeyCode::Down if !*reading => {
            let next = if code == KeyCode::Up {
                topic.saturating_sub(1)
            } else {
                (*topic + 1).min(last_topic)
            };
            if next == *topic {
                return HomeAction::None;
            }
            *topic = next;
            *scroll = 0;
        }
        KeyCode::Up => *scroll = scroll.saturating_sub(1),
        KeyCode::Down => *scroll = scroll.saturating_add(1).min(limit),
        KeyCode::PageUp => *scroll = scroll.saturating_sub(PAGE),
        KeyCode::PageDown => *scroll = scroll.saturating_add(PAGE).min(limit),
        KeyCode::Home => *scroll = 0,
        _ => return HomeAction::None,
    }
    HomeAction::Redraw
}

const fn feature_of(row: Row) -> Option<FeatureId> {
    match row {
        Row::Feature(id) => Some(id),
        Row::Category(_) => None,
    }
}

fn page_buttons(feature: &Feature) -> Vec<Button> {
    match feature.action {
        Action::Reference => vec![Button::Open, Button::Back],
        Action::Open(_) => vec![
            Button::Open,
            Button::CopyCommand,
            Button::Reference,
            Button::Back,
        ],
        Action::Guide if feature.examples.is_empty() => {
            vec![Button::CopyCommand, Button::Reference, Button::Back]
        }
        Action::Guide => vec![Button::CopyExample, Button::Reference, Button::Back],
    }
}

fn result_buttons(outcome: &Outcome) -> Vec<Button> {
    match outcome {
        Outcome::Selected(_) => vec![Button::CopySelection, Button::Back],
        Outcome::Failed { retry: true, .. } => vec![Button::Retry, Button::Back],
        _ => vec![Button::Back],
    }
}

/// Keys whose auto-repeat is honoured: movement and editing only.
const fn repeatable(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Char(_)
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End
    )
}

/// A paste is text for the query: line breaks, tabs and other control
/// characters become spaces, so a pasted Enter can never activate anything.
fn clean_paste(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn prev_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(at, _)| at)
}

fn next_boundary(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .graphemes(true)
        .next()
        .map_or(cursor, |g| cursor + g.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::catalog::LaunchMode;
    use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn with_kind(code: KeyCode, kind: KeyEventKind) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind,
            state: KeyEventState::NONE,
        })
    }

    fn typed(state: &mut HomeState, text: &str) -> Vec<HomeAction> {
        text.chars()
            .map(|c| state.on_event(key(KeyCode::Char(c))))
            .collect()
    }

    fn wide() -> HomeState {
        let mut state = HomeState::new();
        state.set_viewport(120, 40);
        state
    }

    fn launched(action: &HomeAction) -> Option<&'static str> {
        match action {
            HomeAction::Launch(request) => Some(request.feature.0),
            _ => None,
        }
    }

    #[test]
    fn home_starts_on_find_a_command_with_the_reference_last() {
        let state = wide();
        let rows = state.rows();
        assert_eq!(rows.len(), 8);
        assert_eq!(rows[0], Row::Category(CategoryId("find")));
        assert_eq!(rows[7], Row::Feature(REFERENCE));
        assert_eq!(
            state.selected_row(),
            Some(Row::Category(CategoryId("find")))
        );
        assert!(state.view().is_none());
    }

    #[test]
    fn enter_opens_a_category_then_launches_its_feature() {
        let mut state = wide();
        assert_eq!(state.on_event(key(KeyCode::Enter)), HomeAction::Redraw);
        assert_eq!(state.category(), Some(CategoryId("find")));
        assert_eq!(state.selected_feature(), Some(FeatureId("search")));
        let action = state.on_event(key(KeyCode::Enter));
        assert_eq!(launched(&action), Some("search"));
        let HomeAction::Launch(request) = action else {
            unreachable!()
        };
        assert_eq!(request.mode, LaunchMode::Selection);
    }

    #[test]
    fn typing_searches_every_category_and_clearing_returns_to_where_you_were() {
        let mut state = wide();
        state.on_event(key(KeyCode::Down));
        state.on_event(key(KeyCode::Down));
        state.on_event(key(KeyCode::Enter)); // Organize commands
        state.on_event(key(KeyCode::Down)); // Tags
        assert_eq!(state.selected_feature(), Some(FeatureId("tags")));

        typed(&mut state, "backup");
        assert_eq!(state.query(), "backup");
        assert_eq!(state.selected_feature(), Some(FeatureId("backup")));
        assert_eq!(state.category(), Some(CategoryId("organize")));

        state.on_event(ctrl('u'));
        assert_eq!(state.query(), "");
        assert_eq!(state.selected_feature(), Some(FeatureId("tags")));
    }

    #[test]
    fn esc_closes_a_page_then_clears_the_query_then_goes_up_then_exits() {
        let mut state = wide();
        state.on_event(key(KeyCode::Enter)); // into Find a command
        typed(&mut state, "backup");
        state.on_event(key(KeyCode::Enter)); // a guide: opens its page
        assert!(matches!(state.view(), Some(View::Page { .. })));

        assert_eq!(state.on_event(key(KeyCode::Esc)), HomeAction::Redraw);
        assert!(state.view().is_none());
        assert_eq!(state.query(), "backup");

        state.on_event(key(KeyCode::Esc));
        assert_eq!(state.query(), "");
        assert_eq!(state.category(), Some(CategoryId("find")));

        state.on_event(key(KeyCode::Esc));
        assert_eq!(state.category(), None);
        assert_eq!(
            state.selected_row(),
            Some(Row::Category(CategoryId("find")))
        );

        assert_eq!(state.on_event(key(KeyCode::Esc)), HomeAction::Exit);
    }

    #[test]
    fn letters_that_other_screens_use_as_keys_are_search_text_here() {
        let mut state = wide();
        for action in typed(&mut state, "qjk/?") {
            assert_ne!(action, HomeAction::Exit);
            assert!(launched(&action).is_none());
        }
        assert_eq!(state.query(), "qjk/?");
    }

    #[test]
    fn a_pasted_line_break_edits_the_query_and_never_opens_anything() {
        let mut state = wide();
        let action = state.on_event(Event::Paste("backup\n".into()));
        assert!(launched(&action).is_none());
        assert_eq!(state.query(), "backup ");
        assert!(state.view().is_none());

        let action = state.on_event(Event::Paste("a\tb\r\nc\u{1b}[31m".into()));
        assert!(launched(&action).is_none());
        assert_eq!(state.query(), "backup a b c [31m");
    }

    #[test]
    fn released_or_repeated_enter_never_launches() {
        let mut state = wide();
        state.on_event(key(KeyCode::Enter));
        for kind in [KeyEventKind::Release, KeyEventKind::Repeat] {
            let action = state.on_event(with_kind(KeyCode::Enter, kind));
            assert!(launched(&action).is_none(), "{kind:?}");
        }
        // Repeated movement still moves.
        state.on_event(with_kind(KeyCode::Down, KeyEventKind::Repeat));
        assert_eq!(
            state.selected_feature(),
            Some(FeatureId("search-directory"))
        );
    }

    #[test]
    fn guides_open_their_instructions_instead_of_running() {
        let mut state = wide();
        typed(&mut state, "delete history");
        assert_eq!(state.selected_feature(), Some(FeatureId("delete")));
        let action = state.on_event(key(KeyCode::Enter));
        assert_eq!(action, HomeAction::Redraw);
        let Some(View::Page { feature, .. }) = state.view() else {
            panic!("no page")
        };
        assert_eq!(*feature, FeatureId("delete"));
        assert!(!state.buttons().contains(&Button::Open));
        // Enter on the page copies the highlighted example; it runs nothing.
        let action = state.on_event(key(KeyCode::Enter));
        assert_eq!(
            action,
            HomeAction::Copy("suv delete \"<TEXT>\" --dry-run".into())
        );
    }

    #[test]
    fn with_no_results_enter_does_nothing() {
        let mut state = wide();
        typed(&mut state, "xyzzy");
        assert!(state.rows().is_empty());
        assert_eq!(state.selected_row(), None);
        assert_eq!(state.on_event(key(KeyCode::Enter)), HomeAction::None);
    }

    #[test]
    fn the_selection_follows_the_feature_not_its_position() {
        let mut state = wide();
        typed(&mut state, "search");
        let rows = state.rows();
        let failed = Row::Feature(FeatureId("search-failed"));
        let position = rows.iter().position(|r| *r == failed).unwrap();
        for _ in 0..position {
            state.on_event(key(KeyCode::Down));
        }
        assert_eq!(state.selected_row(), Some(failed));
        typed(&mut state, " fail"); // fewer results, different positions
        assert_eq!(state.selected_row(), Some(failed));
        typed(&mut state, "zz"); // gone: nothing stale is selected
        assert_eq!(state.selected_row(), None);
    }

    #[test]
    fn the_query_cursor_moves_and_deletes_whole_graphemes() {
        let mut state = wide();
        state.set_query("ae\u{301}த்");
        assert_eq!(state.cursor(), state.query().len());
        state.on_event(key(KeyCode::Left));
        state.on_event(key(KeyCode::Backspace)); // removes e + combining acute
        assert_eq!(state.query(), "aத்");
        state.on_event(key(KeyCode::Home));
        assert_eq!(state.cursor(), 0);
        state.on_event(key(KeyCode::Delete));
        assert_eq!(state.query(), "த்");
        state.on_event(key(KeyCode::End));
        state.on_event(key(KeyCode::Backspace));
        assert_eq!(state.query(), "");
        state.on_event(key(KeyCode::Backspace)); // nothing left: no panic
        state.on_event(key(KeyCode::Left));
    }

    #[test]
    fn the_query_stops_at_256_graphemes_and_says_so() {
        let mut state = wide();
        state.on_event(Event::Paste("x".repeat(300)));
        assert_eq!(state.query().chars().count(), MAX_QUERY_GRAPHEMES);
        assert!(state.notice().unwrap().contains("256"));
        typed(&mut state, "y");
        assert!(!state.query().contains('y'));
        state.on_event(key(KeyCode::Backspace));
        assert!(state.notice().is_none());
    }

    #[test]
    fn ctrl_c_interrupts_and_ctrl_r_opens_search_from_anywhere() {
        let mut state = wide();
        typed(&mut state, "backup");
        assert_eq!(launched(&state.on_event(ctrl('r'))), Some("search"));
        assert_eq!(state.on_event(ctrl('c')), HomeAction::Interrupt);
        state.on_event(key(KeyCode::Enter)); // a page is open
        assert_eq!(state.on_event(ctrl('c')), HomeAction::Interrupt);
    }

    #[test]
    fn f1_opens_keys_and_f2_opens_the_reference_at_the_selected_command() {
        let mut state = wide();
        typed(&mut state, "failed");
        state.on_event(key(KeyCode::F(2)));
        let Some(View::Reference { topic, .. }) = state.view() else {
            panic!("no reference")
        };
        assert_eq!(state.topics()[*topic].title, "suv search");
        state.on_event(key(KeyCode::Esc));
        state.on_event(key(KeyCode::F(1)));
        assert!(matches!(state.view(), Some(View::Keys { .. })));
        state.on_event(key(KeyCode::Esc));
        assert!(state.view().is_none());
        assert_eq!(state.query(), "failed");
    }

    #[test]
    fn tab_moves_focus_when_wide_and_opens_details_when_narrower() {
        let mut state = wide();
        state.on_event(key(KeyCode::Enter));
        state.on_event(key(KeyCode::Tab));
        assert_eq!(state.focus(), Focus::Detail);
        state.on_event(key(KeyCode::Down)); // scrolls the detail, not the list
        assert_eq!(state.selected_feature(), Some(FeatureId("search")));
        assert_eq!(state.on_event(key(KeyCode::Esc)), HomeAction::Redraw);
        assert_eq!(state.focus(), Focus::List);

        state.set_viewport(60, 18);
        state.on_event(key(KeyCode::Tab));
        assert!(matches!(
            state.view(),
            Some(View::Page {
                feature: FeatureId("search"),
                ..
            })
        ));
        assert_eq!(state.buttons()[0], Button::Open);
        assert_eq!(
            launched(&state.on_event(key(KeyCode::Enter))),
            Some("search")
        );
    }

    #[test]
    fn resizing_keeps_the_selection() {
        let mut state = wide();
        state.on_event(key(KeyCode::Down));
        state.on_event(Event::Resize(30, 8));
        state.on_event(Event::Resize(120, 40));
        assert_eq!(
            state.selected_row(),
            Some(Row::Category(CategoryId("session")))
        );
    }

    #[test]
    fn a_selected_command_is_offered_to_copy_exactly() {
        let mut state = wide();
        let command = "  printf 'a\\nb'  ";
        state.show_outcome(FeatureId("search"), Outcome::Selected(command.into()));
        assert_eq!(state.buttons(), [Button::CopySelection, Button::Back]);
        assert_eq!(
            state.on_event(key(KeyCode::Enter)),
            HomeAction::Copy(command.into())
        );
        state.on_event(key(KeyCode::Right));
        state.on_event(key(KeyCode::Enter));
        assert!(state.view().is_none());
    }

    #[test]
    fn nothing_selected_offers_only_back() {
        let mut state = wide();
        state.show_outcome(FeatureId("bookmarks"), Outcome::NothingSelected);
        assert_eq!(state.buttons(), [Button::Back]);
    }

    #[test]
    fn a_failed_report_can_be_retried() {
        let mut state = wide();
        state.show_outcome(
            FeatureId("status"),
            Outcome::Failed {
                message: "timed out".into(),
                retry: true,
            },
        );
        assert_eq!(state.buttons(), [Button::Retry, Button::Back]);
        assert_eq!(
            launched(&state.on_event(key(KeyCode::Enter))),
            Some("status")
        );
        assert!(state.view().is_none());
    }

    #[test]
    fn copying_reports_success_only_when_it_worked() {
        let mut state = wide();
        state.copied("suv backup", &Ok(()));
        assert!(state.notice().unwrap().contains("has not been run"));
        state.copied("suv backup", &Err("no clipboard".into()));
        let notice = state.notice().unwrap();
        assert!(notice.contains("Could not copy") && notice.contains("no clipboard"));
        assert!(!notice.contains("Copied"));
    }

    /// Looking is never doing: for every feature, moving, scrolling and
    /// opening its details — in every layout — neither runs nor copies.
    #[test]
    fn browsing_any_feature_never_launches_or_copies() {
        let browsing = [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Up,
            KeyCode::Down,
        ];
        for feature in catalog::features() {
            for (width, height) in [(120, 40), (80, 24), (60, 18)] {
                let mut state = HomeState::new();
                state.set_viewport(width, height);
                state.set_query(feature.title);
                assert_eq!(
                    state.selected_feature(),
                    Some(feature.id),
                    "{}",
                    feature.title
                );
                for code in browsing {
                    let action = state.on_event(key(code));
                    assert!(
                        !matches!(action, HomeAction::Launch(_) | HomeAction::Copy(_)),
                        "{} at {width}x{height}: {code:?} gave {action:?}",
                        feature.title
                    );
                }
            }
        }
    }

    #[test]
    fn layouts_follow_the_terminal_size() {
        assert_eq!(layout_class(100, 24), LayoutClass::Wide);
        assert_eq!(layout_class(99, 40), LayoutClass::Medium);
        assert_eq!(layout_class(120, 23), LayoutClass::Medium);
        assert_eq!(layout_class(70, 20), LayoutClass::Medium);
        assert_eq!(layout_class(69, 30), LayoutClass::Narrow);
        assert_eq!(layout_class(80, 19), LayoutClass::Narrow);
        assert_eq!(layout_class(39, 30), LayoutClass::TooSmall);
        assert_eq!(layout_class(80, 9), LayoutClass::TooSmall);
        assert_eq!(layout_class(0, 0), LayoutClass::TooSmall);
    }
}
