//! Drawing Home: three layouts by terminal size, a feature's details in
//! the order someone needs them, and the pages, reference and results that
//! open over the list. Nothing here starts a process or reads a database.
//!
//! Text is wrapped here rather than by ratatui, grapheme by grapheme and by
//! display width, so a scroll limit is exact and a long unbroken path or a
//! wide character can never push past a border. Text that came from outside
//! (a picked command, a report) is made visible first: escape sequences are
//! removed and other control characters are spelled out.

use std::fmt::Write as _;

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::catalog::{self, Action, Feature, FeatureId, LaunchMode, REFERENCE};
use super::model::{layout_class, Button, Focus, HomeState, LayoutClass, Outcome, Row, View};
use super::reference;
use crate::config::HomeIcons;
use crate::search::highlight::{raw_view, wants_raw, RawStyle};

/// Width of the label column in a feature's details.
const LABEL: usize = 11;

fn fg(color: Color) -> Style {
    Style::default().fg(color)
}

fn bold(color: Color) -> Style {
    fg(color).add_modifier(Modifier::BOLD)
}

/// The theme's colours, or none at all under `NO_COLOR`; labels and the
/// reverse-video selection carry the meaning either way.
struct Palette {
    primary: Color,
    text: Color,
    secondary: Color,
    muted: Color,
    border: Color,
    focus: Color,
    success: Color,
    warning: Color,
    error: Color,
    badge_bg: Color,
    sel_bg: Color,
    sel_fg: Color,
    no_color: bool,
}

impl Palette {
    fn new(no_color: bool) -> Self {
        let t = crate::theme::theme();
        let pick = |c: Color| if no_color { Color::Reset } else { c };
        Self {
            primary: pick(t.primary),
            text: pick(t.text),
            secondary: pick(t.text_secondary),
            muted: pick(t.text_muted),
            border: pick(t.border),
            focus: pick(t.border_focus),
            success: pick(t.success),
            warning: pick(t.warning),
            error: pick(t.error),
            badge_bg: pick(t.badge_bg),
            sel_bg: pick(t.selection_bg),
            sel_fg: pick(t.selection_fg),
            no_color,
        }
    }

    fn selected(&self) -> Style {
        if self.no_color {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default()
                .bg(self.sel_bg)
                .fg(self.sel_fg)
                .add_modifier(Modifier::BOLD)
        }
    }

    fn badge(&self) -> Style {
        if self.no_color {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().bg(self.badge_bg).fg(self.text)
        }
    }

    fn raw(&self) -> RawStyle {
        if self.no_color {
            RawStyle {
                text: Style::default(),
                escape: Style::default().add_modifier(Modifier::BOLD),
                space: Style::default(),
            }
        } else {
            RawStyle::from_theme(crate::theme::theme())
        }
    }

    fn block(&self, title: String, focused: bool) -> Block<'static> {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(fg(if focused { self.focus } else { self.border }))
            .title(title)
    }
}

pub fn draw(frame: &mut Frame<'_>, state: &HomeState) {
    let p = Palette::new(state.no_color);
    let area = frame.area();
    let class = layout_class(area.width, area.height);
    if class == LayoutClass::TooSmall {
        draw_too_small(frame, area, &p);
        return;
    }
    let [title, body, status, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Span::styled("SUVADU HOME", bold(p.primary))).alignment(Alignment::Center),
        title,
    );
    match state.view() {
        Some(view) => draw_view(frame, body, state, view, &p),
        None => draw_main(frame, body, state, class, &p),
    }
    draw_status(frame, status, state, &p);
    draw_footer(frame, footer, state, class, &p);
}

fn draw_too_small(frame: &mut Frame<'_>, area: Rect, p: &Palette) {
    let lines = vec![
        Line::styled("SUVADU HOME", bold(p.primary)),
        Line::styled("Terminal too small", fg(p.text)),
        Line::styled(
            format!("Needs 40×10, now {}×{}", area.width, area.height),
            fg(p.muted),
        ),
        Line::styled("Esc: exit", fg(p.secondary)),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        area,
    );
}

// ── The list screen ───────────────────────────────────────────

fn draw_main(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &HomeState,
    class: LayoutClass,
    p: &Palette,
) {
    let [search, crumb, content] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);
    draw_search(frame, search, state, p);
    draw_crumb(frame, crumb, state, p);
    match class {
        LayoutClass::Wide => {
            let list_width = (content.width.saturating_mul(36) / 100).clamp(30, 44);
            let [list, detail] =
                Layout::horizontal([Constraint::Length(list_width), Constraint::Min(0)])
                    .areas(content);
            draw_list(frame, list, state, p);
            draw_detail(frame, detail, state, p);
        }
        LayoutClass::Medium => {
            let about_height = 8.min(content.height / 2);
            let [list, about] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(about_height)])
                    .areas(content);
            draw_list(frame, list, state, p);
            draw_about(frame, about, state, p);
        }
        LayoutClass::Narrow | LayoutClass::TooSmall => draw_list(frame, content, state, p),
    }
}

fn draw_search(frame: &mut Frame<'_>, area: Rect, state: &HomeState, p: &Palette) {
    let block = p.block(
        " Find a Suvadu feature ".to_string(),
        state.focus() == Focus::List,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = usize::from(inner.width);
    if state.query().is_empty() {
        let hint = if width >= 42 {
            "Try: failed commands, backup, AI prompts"
        } else {
            "Type to find a feature"
        };
        frame.render_widget(Paragraph::new(Span::styled(hint, fg(p.muted))), inner);
        if inner.width > 0 && inner.height > 0 {
            frame.set_cursor_position((inner.x, inner.y));
        }
        return;
    }
    let (shown, cursor_x) = visible_query(state.query(), state.cursor(), width);
    frame.render_widget(Paragraph::new(Span::styled(shown, fg(p.text))), inner);
    if inner.width > 0 && inner.height > 0 {
        frame.set_cursor_position((inner.x + cursor_x, inner.y));
    }
}

/// The part of the query that fits, scrolled so the cursor stays in view,
/// and the cursor's column within it.
fn visible_query(query: &str, cursor: usize, width: usize) -> (String, u16) {
    if width == 0 {
        return (String::new(), 0);
    }
    let before = query[..cursor].width();
    let mut skipped = 0;
    let mut start = 0;
    for (at, g) in query.grapheme_indices(true) {
        if before - skipped < width {
            break;
        }
        skipped += g.width();
        start = at + g.len();
    }
    let mut shown = String::new();
    let mut used = 0;
    for g in query[start..].graphemes(true) {
        if used + g.width() > width {
            break;
        }
        used += g.width();
        shown.push_str(g);
    }
    (shown, u16::try_from(before - skipped).unwrap_or(0))
}

fn draw_crumb(frame: &mut Frame<'_>, area: Rect, state: &HomeState, p: &Palette) {
    let line = if state.searching() {
        let found = state.rows().len();
        let query: String = state.query().trim().graphemes(true).take(30).collect();
        if found == 0 {
            Line::styled(format!("No features match “{query}”"), fg(p.warning))
        } else {
            Line::styled(
                format!("Features matching “{query}” · {found}"),
                fg(p.secondary),
            )
        }
    } else if let Some(category) = state.category().and_then(catalog::category) {
        Line::styled(format!("Home › {}", category.title), fg(p.secondary))
    } else {
        Line::styled("Home", fg(p.secondary))
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn marker(category: catalog::CategoryId, icons: HomeIcons) -> &'static str {
    catalog::category(category).map_or("?", |c| match icons {
        HomeIcons::Ascii => c.ascii,
        HomeIcons::Unicode => c.unicode,
    })
}

const fn kind(feature: &Feature) -> &'static str {
    match feature.action {
        Action::Open(LaunchMode::Selection) => "picker",
        Action::Open(LaunchMode::Interactive) => "screen",
        Action::Open(LaunchMode::Report) => "report",
        Action::Guide => "guide",
        Action::Reference => "reference",
    }
}

/// A list row. `width` is the room for its text; a kind tag that would not
/// fit there whole is left out rather than cut.
fn list_item(row: Row, state: &HomeState, width: usize, p: &Palette) -> ListItem<'static> {
    match row {
        Row::Category(id) => {
            let title = catalog::category(id).map_or("", |c| c.title);
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} ", marker(id, state.icons)), bold(p.secondary)),
                Span::styled(title, fg(p.text)),
            ]))
        }
        Row::Feature(id) => {
            let Some(feature) = catalog::feature(id) else {
                return ListItem::new("");
            };
            if id == REFERENCE && !state.searching() {
                return ListItem::new(Line::from(vec![
                    Span::styled("? ", bold(p.secondary)),
                    Span::styled(feature.title, fg(p.text)),
                ]));
            }
            let tag = format!("  {}", kind(feature));
            let mut spans = vec![Span::styled(feature.title, fg(p.text))];
            if feature.title.width() + tag.width() <= width {
                spans.push(Span::styled(tag, fg(p.muted)));
            }
            let title = Line::from(spans);
            if !state.searching() {
                return ListItem::new(title);
            }
            let context =
                catalog::category(feature.category).map_or("Command reference", |c| c.title);
            ListItem::new(vec![
                title,
                Line::styled(format!("  in {context}"), fg(p.muted)),
            ])
        }
    }
}

fn draw_list(frame: &mut Frame<'_>, area: Rect, state: &HomeState, p: &Palette) {
    let rows = state.rows();
    let title = if state.searching() {
        " Results ".to_string()
    } else if let Some(category) = state.category().and_then(catalog::category) {
        format!(" {} ", category.title)
    } else {
        " Explore ".to_string()
    };
    let block = p.block(title, state.focus() == Focus::List);
    if rows.is_empty() {
        let query: String = state.query().trim().graphemes(true).take(40).collect();
        let lines = vec![
            Line::styled(format!("No features match “{query}”."), fg(p.text)),
            Line::default(),
            Line::styled("Esc  clear the search", fg(p.secondary)),
            Line::styled("F2   open the command reference", fg(p.secondary)),
        ];
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    // Borders and the " > " selection marker take five columns.
    let room = usize::from(area.width).saturating_sub(5);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| list_item(*row, state, room, p))
        .collect();
    let selected = state
        .selected_row()
        .and_then(|s| rows.iter().position(|r| *r == s));
    let mut list_state = ListState::default().with_selected(selected);
    let list = List::new(items)
        .block(block)
        .highlight_style(p.selected())
        .highlight_symbol(" > ");
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn draw_detail(frame: &mut Frame<'_>, area: Rect, state: &HomeState, p: &Palette) {
    let block = p.block(" Details ".to_string(), state.focus() == Focus::Detail);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = row_lines(state.selected_row(), usize::from(inner.width), None, p);
    let limit =
        u16::try_from(lines.len().saturating_sub(usize::from(inner.height))).unwrap_or(u16::MAX);
    state.set_detail_scroll_limit(limit);
    frame.render_widget(
        Paragraph::new(lines).scroll((state.detail_scroll().min(limit), 0)),
        inner,
    );
}

fn draw_about(frame: &mut Frame<'_>, area: Rect, state: &HomeState, p: &Palette) {
    let block = p.block(" About ".to_string(), false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let height = usize::from(inner.height);
    let mut lines = row_lines(state.selected_row(), usize::from(inner.width), None, p);
    if lines.len() > height && height > 0 {
        lines.truncate(height - 1);
        lines.push(Line::styled("Tab: full details", fg(p.muted)));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

// ── What a row says ───────────────────────────────────────────

/// Builds wrapped, styled lines for a fixed width.
struct Doc<'a> {
    width: usize,
    lines: Vec<Line<'static>>,
    p: &'a Palette,
}

impl<'a> Doc<'a> {
    const fn new(width: usize, p: &'a Palette) -> Self {
        Self {
            width,
            lines: Vec::new(),
            p,
        }
    }

    fn blank(&mut self) {
        self.lines.push(Line::default());
    }

    fn text(&mut self, text: &str, style: Style) {
        for line in wrap(text, self.width) {
            self.lines.push(Line::styled(line, style));
        }
    }

    /// Text that must keep every character and its line breaks: wrapped
    /// only where a line is too wide.
    fn exact(&mut self, prefix: &str, text: &str, style: Style) {
        let width = self.width.saturating_sub(prefix.width()).max(1);
        for (n, line) in hard_wrap(text, width).into_iter().enumerate() {
            let lead = if n == 0 {
                prefix.to_string()
            } else {
                " ".repeat(prefix.width())
            };
            self.lines.push(Line::from(vec![
                Span::styled(lead, fg(self.p.muted)),
                Span::styled(line, style),
            ]));
        }
    }

    /// `Label     value`, the value wrapped in its own column; on a narrow
    /// pane the label goes on its own line instead.
    fn field(&mut self, label: &str, value: &str, style: Style) {
        let label_style = bold(self.p.secondary);
        if self.width < LABEL + 12 {
            self.lines
                .push(Line::styled(label.to_string(), label_style));
            self.text(value, style);
            return;
        }
        for (n, part) in wrap(value, self.width - LABEL).into_iter().enumerate() {
            let head = if n == 0 {
                format!("{label:<LABEL$}")
            } else {
                " ".repeat(LABEL)
            };
            self.lines.push(Line::from(vec![
                Span::styled(head, label_style),
                Span::styled(part, style),
            ]));
        }
    }
}

/// Details for a list row. `page` is the selected example on a feature's
/// page, where the buttons replace the key hints.
fn row_lines(
    row: Option<Row>,
    width: usize,
    page: Option<usize>,
    p: &Palette,
) -> Vec<Line<'static>> {
    let mut doc = Doc::new(width.max(1), p);
    match row {
        None => doc.text("Nothing selected.", fg(p.muted)),
        Some(Row::Category(id)) => category_lines(&mut doc, id),
        Some(Row::Feature(id)) => {
            if let Some(feature) = catalog::feature(id) {
                feature_lines(&mut doc, feature, page);
            }
        }
    }
    doc.lines
}

fn category_lines(doc: &mut Doc<'_>, id: catalog::CategoryId) {
    let p = doc.p;
    let Some(category) = catalog::category(id) else {
        return;
    };
    doc.text(category.title, bold(p.primary));
    doc.text(category.description, fg(p.text));
    doc.blank();
    for (n, feature) in catalog::category_features(id).into_iter().enumerate() {
        doc.field(
            if n == 0 { "Contains" } else { "" },
            feature.title,
            fg(p.text),
        );
    }
    doc.blank();
    if id == catalog::categories()[0].id {
        doc.field(
            "Fast path",
            "Ctrl+R in your shell searches history directly",
            fg(p.text),
        );
    }
    doc.field("Enter", "Browse features", fg(p.text));
}

fn feature_lines(doc: &mut Doc<'_>, feature: &Feature, page: Option<usize>) {
    let p = doc.p;
    doc.text(feature.title, bold(p.primary));
    doc.text(feature.description, fg(p.text));
    let opens = if feature.action == Action::Guide {
        "Instructions only: nothing is run from Home."
    } else {
        feature.opens
    };
    doc.text(opens, fg(p.secondary));
    doc.blank();
    doc.field("Command", feature.command, bold(p.primary));
    if let Some(shortcut) = feature.shortcut {
        doc.field("Shortcut", shortcut, fg(p.text));
    }
    if let Some(note) = feature.note {
        doc.field("Note", note, fg(p.text));
    }
    if page.is_none() {
        doc.blank();
        doc.field("Enter", feature.action_label(), fg(p.text));
        if feature.id != REFERENCE {
            doc.field("F2", "Command reference", fg(p.text));
        }
    }
    for paragraph in feature.guide {
        doc.blank();
        if paragraph.contains('\n') {
            doc.exact("  ", paragraph, fg(p.primary));
        } else {
            doc.text(paragraph, fg(p.text));
        }
    }
    if feature.examples.is_empty() {
        return;
    }
    doc.blank();
    let heading = if feature.action == Action::Guide {
        "Examples"
    } else {
        "More from your shell"
    };
    doc.text(heading, bold(p.secondary));
    for (n, example) in feature.examples.iter().enumerate() {
        let chosen = page == Some(n);
        let (prefix, style) = if chosen {
            (" > $ ", p.selected())
        } else {
            ("   $ ", fg(p.primary))
        };
        doc.exact(prefix, example.command, style);
        let mut note = example.note.to_string();
        if example.needs_input() {
            note.push_str(" (fill in the <…> parts)");
        }
        for line in wrap(&note, doc.width.saturating_sub(5).max(1)) {
            doc.lines
                .push(Line::styled(format!("     {line}"), fg(p.muted)));
        }
    }
}

// ── Pages, reference, keys and results ────────────────────────

fn draw_view(frame: &mut Frame<'_>, area: Rect, state: &HomeState, view: &View, p: &Palette) {
    match view {
        View::Reference {
            topic,
            scroll,
            reading,
        } => draw_reference(frame, area, state, *topic, *scroll, *reading, p),
        View::Page {
            feature,
            example,
            scroll,
            ..
        } => {
            let title = catalog::feature(*feature).map_or("", |f| f.title);
            draw_scrolled(
                frame,
                area,
                state,
                format!(" {title} "),
                *scroll,
                p,
                |width| row_lines(Some(Row::Feature(*feature)), width, Some(*example), p),
            );
        }
        View::Keys { scroll, .. } => {
            draw_scrolled(
                frame,
                area,
                state,
                " Keys ".to_string(),
                *scroll,
                p,
                |width| keys_lines(width, p),
            );
        }
        View::Result {
            feature,
            outcome,
            scroll,
            ..
        } => {
            let title = catalog::feature(*feature).map_or("", |f| f.title);
            draw_scrolled(
                frame,
                area,
                state,
                format!(" {title} "),
                *scroll,
                p,
                |width| outcome_lines(*feature, outcome, width, p),
            );
        }
    }
}

/// A bordered, scrolling view with the buttons on its last row.
fn draw_scrolled(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &HomeState,
    title: String,
    scroll: u16,
    p: &Palette,
    content: impl FnOnce(usize) -> Vec<Line<'static>>,
) {
    let block = p.block(title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = usize::from(inner.width);
    // The buttons come first: every one is always shown whole, on as many
    // rows as the width needs. What Copy example takes comes next; the text
    // scrolls in whatever room is left.
    let button_rows = button_lines(state, width, p);
    let buttons_height = u16::try_from(button_rows.len())
        .unwrap_or(u16::MAX)
        .min(inner.height);
    let preview = copy_preview(state, width, p);
    let preview_height = u16::try_from(preview.len())
        .unwrap_or(u16::MAX)
        .min(inner.height.saturating_sub(buttons_height));
    let [body, preview_area, buttons] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(preview_height),
        Constraint::Length(buttons_height),
    ])
    .areas(inner);
    let lines = content(usize::from(body.width));
    let limit =
        u16::try_from(lines.len().saturating_sub(usize::from(body.height))).unwrap_or(u16::MAX);
    state.set_view_scroll_limit(limit);
    frame.render_widget(Paragraph::new(lines).scroll((scroll.min(limit), 0)), body);
    frame.render_widget(Paragraph::new(preview), preview_area);
    frame.render_widget(Paragraph::new(button_rows), buttons);
}

/// The buttons of the open view, laid out on as few rows as `width`
/// allows, each label whole.
fn button_lines(state: &HomeState, width: usize, p: &Palette) -> Vec<Line<'static>> {
    let focused = state.button();
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for (n, button) in state.buttons().into_iter().enumerate() {
        let label = format!("[ {} ]", button_label(button, state));
        let needed = if used == 0 {
            label.width()
        } else {
            2 + label.width()
        };
        if used > 0 && used + needed > width {
            rows.push(Line::from(std::mem::take(&mut spans)));
            used = 0;
        }
        if used > 0 {
            spans.push(Span::raw("  "));
            used += 2;
        }
        let style = if focused == Some(n) {
            p.selected()
        } else {
            fg(p.secondary)
        };
        used += label.width();
        spans.push(Span::styled(label, style));
    }
    if !spans.is_empty() {
        rows.push(Line::from(spans));
    }
    rows
}

/// On a guide with examples: exactly what Copy example would copy, so it
/// is visible even when the examples are below the fold.
fn copy_preview(state: &HomeState, width: usize, p: &Palette) -> Vec<Line<'static>> {
    let Some(View::Page {
        feature, example, ..
    }) = state.view()
    else {
        return Vec::new();
    };
    let Some(chosen) = catalog::feature(*feature).and_then(|f| f.examples.get(*example)) else {
        return Vec::new();
    };
    let mut doc = Doc::new(width.max(1), p);
    doc.exact("Copies: ", chosen.command, fg(p.primary));
    doc.lines.truncate(3);
    doc.lines
}

fn button_label(button: Button, state: &HomeState) -> String {
    let feature = match state.view() {
        Some(View::Page { feature, .. } | View::Result { feature, .. }) => {
            catalog::feature(*feature)
        }
        _ => None,
    };
    match button {
        Button::Open => feature.map_or("Open", Feature::action_label).to_string(),
        Button::CopyCommand | Button::CopySelection => "Copy command".to_string(),
        Button::CopyExample => "Copy example".to_string(),
        Button::Reference => "Command reference".to_string(),
        Button::Retry => "Retry".to_string(),
        Button::Back if matches!(state.view(), Some(View::Result { .. })) => {
            "Back to Home".to_string()
        }
        Button::Back => "Back".to_string(),
    }
}

fn draw_reference(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &HomeState,
    topic: usize,
    scroll: u16,
    reading: bool,
    p: &Palette,
) {
    let block = p.block(" Command reference ".to_string(), true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let topics = state.topics();
    let draw_topics = |frame: &mut Frame<'_>, area: Rect, borders: Borders| {
        let items: Vec<ListItem> = topics
            .iter()
            .map(|t| ListItem::new(Span::styled(t.title.clone(), fg(p.text))))
            .collect();
        let mut list_state = ListState::default().with_selected(Some(topic));
        let highlight = if reading {
            bold(p.primary)
        } else {
            p.selected()
        };
        let list = List::new(items)
            .block(Block::default().borders(borders).border_style(fg(p.border)))
            .highlight_style(highlight)
            .highlight_symbol(" > ");
        frame.render_stateful_widget(list, area, &mut list_state);
    };
    let draw_text = |frame: &mut Frame<'_>, area: Rect| {
        let Some(current) = topics.get(topic) else {
            return;
        };
        let path: Vec<&str> = current.path.iter().map(String::as_str).collect();
        let body = reference::command_help(&path).unwrap_or_else(|e| e);
        let mut doc = Doc::new(usize::from(area.width).max(1), p);
        doc.text(&current.title, bold(p.primary));
        doc.blank();
        for line in visible(&body).split('\n') {
            for part in wrap_help_line(line, doc.width) {
                doc.lines.push(Line::styled(part, fg(p.text)));
            }
        }
        let limit = u16::try_from(doc.lines.len().saturating_sub(usize::from(area.height)))
            .unwrap_or(u16::MAX);
        state.set_view_scroll_limit(limit);
        frame.render_widget(
            Paragraph::new(doc.lines).scroll((scroll.min(limit), 0)),
            area,
        );
    };
    if inner.width >= 90 {
        let [list, text] =
            Layout::horizontal([Constraint::Length(30), Constraint::Min(0)]).areas(inner);
        draw_topics(frame, list, Borders::RIGHT);
        let [_, text] = Layout::horizontal([Constraint::Length(1), Constraint::Min(0)]).areas(text);
        draw_text(frame, text);
    } else if reading {
        draw_text(frame, inner);
    } else {
        draw_topics(frame, inner, Borders::NONE);
    }
}

fn keys_lines(width: usize, p: &Palette) -> Vec<Line<'static>> {
    let mut doc = Doc::new(width.max(1), p);
    doc.text("Home keys", bold(p.primary));
    doc.text(
        "Typing searches Suvadu's features, not your history. Nothing you find here runs in \
         your shell: pickers show the command you chose, to copy.",
        fg(p.text),
    );
    doc.blank();
    for (key, meaning) in [
        ("Type", "Search features"),
        ("Up/Down", "Move, or scroll the focused text"),
        ("Enter", "Open the selected item"),
        ("Esc", "Close, clear the search, go up, then quit"),
        ("Tab", "Details"),
        ("PgUp/PgDn", "Scroll"),
        ("Left/Right", "Move in the search text; choose a button"),
        ("Home/End", "Start or end of the search text"),
        ("Ctrl+U", "Clear the search"),
        ("Ctrl+R", "Search your command history"),
        ("F1", "These keys"),
        ("F2", "Command reference"),
        ("Ctrl+C", "Quit"),
    ] {
        doc.field(key, meaning, fg(p.text));
    }
    doc.lines
}

fn outcome_lines(
    feature: FeatureId,
    outcome: &Outcome,
    width: usize,
    p: &Palette,
) -> Vec<Line<'static>> {
    let mut doc = Doc::new(width.max(1), p);
    let command = catalog::feature(feature).map_or("suv", |f| f.command);
    match outcome {
        Outcome::Selected(selected) => {
            doc.text("Selected command", bold(p.primary));
            doc.exact("", &visible(selected), bold(p.text));
            if wants_raw(selected) {
                doc.blank();
                doc.text(
                    "Exact text, with every space and hidden character shown:",
                    fg(p.secondary),
                );
                doc.lines.extend(raw_view(selected, doc.width, &p.raw()));
            }
            doc.blank();
            doc.text("This has not been run.", bold(p.warning));
            doc.blank();
            doc.text(
                "Tip: Ctrl+R in your shell puts a selection on the prompt.",
                fg(p.muted),
            );
        }
        Outcome::NothingSelected => {
            doc.text("Nothing was selected.", bold(p.primary));
            doc.text("Back to Home returns to where you were.", fg(p.muted));
        }
        Outcome::Report {
            output,
            errors,
            code,
            truncated,
        } => {
            doc.exact("", &visible(output), fg(p.text));
            if !errors.trim().is_empty() {
                doc.blank();
                doc.text("Messages", bold(p.warning));
                doc.exact("", &visible(errors), fg(p.warning));
            }
            if *truncated {
                doc.blank();
                doc.text(
                    &format!("The output was too long to keep; run {command} in your shell to see all of it."),
                    fg(p.warning),
                );
            }
            match code {
                Some(0) => {}
                Some(code) => {
                    doc.blank();
                    doc.text(
                        &format!("{command} exited with status {code}."),
                        fg(p.warning),
                    );
                }
                None => {
                    doc.blank();
                    doc.text(
                        &format!("{command} was stopped by a signal."),
                        fg(p.warning),
                    );
                }
            }
        }
        Outcome::Failed { message, .. } => {
            doc.text("Could not complete", bold(p.error));
            doc.text(message, fg(p.text));
            doc.blank();
            doc.text(
                &format!("You can run {command} in your shell instead."),
                fg(p.muted),
            );
        }
    }
    doc.lines
}

// ── Status and keys ───────────────────────────────────────────

fn draw_status(frame: &mut Frame<'_>, area: Rect, state: &HomeState, p: &Palette) {
    let span = match (state.notice(), &state.status.warning) {
        (Some(notice), _) => {
            let color = if state.notice_is_error() {
                p.error
            } else {
                p.success
            };
            Span::styled(notice.to_string(), bold(color))
        }
        (None, Some(warning)) => Span::styled(format!("Config: {warning}"), fg(p.warning)),
        (None, None) => {
            let recording = match state.status.recording {
                Some(true) => "enabled",
                Some(false) => "disabled",
                None => "unknown",
            };
            // A pause matters most when there is one, so it leads.
            let recording = format!("Recording: {recording}");
            let mut segments: Vec<&str> = if state.status.paused {
                vec!["This shell: paused", &recording, "Capture: not checked"]
            } else {
                vec![&recording, "Capture: not checked", "Shell: not paused"]
            };
            // A newer release leads: it is the one thing that asks for action.
            if let Some(update) = &state.status.update {
                segments.insert(0, update);
            }
            Span::styled(
                fit_segments(&segments, usize::from(area.width)),
                fg(p.muted),
            )
        }
    };
    frame.render_widget(Paragraph::new(Line::from(span)), area);
}

fn draw_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &HomeState,
    class: LayoutClass,
    p: &Palette,
) {
    // Badges in priority order; the ones that do not fit whole are left
    // out, never cut.
    let mut badges: Vec<(String, String)> = Vec::new();
    let mut badge = |key: &str, label: &str| badges.push((key.to_string(), label.to_string()));
    match state.view() {
        None => {
            let at_top = state.category().is_none()
                && state.query().is_empty()
                && state.focus() == Focus::List;
            badge("Esc", if at_top { "Quit" } else { "Back" });
            match state.selected_row() {
                Some(Row::Category(_)) => badge("Enter", "Browse"),
                Some(Row::Feature(id)) => {
                    badge(
                        "Enter",
                        catalog::feature(id).map_or("Open", Feature::action_label),
                    );
                }
                None => {}
            }
            let tab = if class == LayoutClass::Wide && state.focus() == Focus::Detail {
                "List"
            } else {
                "Details"
            };
            badge("F1", "Keys");
            badge("Tab", tab);
            badge("F2", "Reference");
            badge("^R", "History");
        }
        Some(View::Reference { reading, .. }) => {
            badge("Esc", "Back");
            badge("Tab", if *reading { "Topics" } else { "Text" });
            badge("Up/Dn", if *reading { "Scroll" } else { "Topic" });
            badge("PgDn", "Scroll");
            badge("F1", "Keys");
        }
        Some(view) => {
            badge("Esc", "Back");
            if let Some(button) = state.button().and_then(|n| state.buttons().get(n).copied()) {
                badge("Enter", &button_label(button, state));
            }
            if state.buttons().len() > 1 {
                badge("L/R", "Choose");
            }
            let examples = matches!(view, View::Page { feature, .. }
                if catalog::feature(*feature).is_some_and(|f| !f.examples.is_empty()));
            badge("Up/Dn", if examples { "Example" } else { "Scroll" });
            badge("F2", "Reference");
        }
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for (key, label) in badges {
        let (key, label) = (format!(" {key} "), format!(" {label}  "));
        let width = key.width() + label.trim_end().width() + 1;
        if used + width > usize::from(area.width) {
            break;
        }
        used += key.width() + label.width();
        spans.push(Span::styled(key, p.badge()));
        spans.push(Span::styled(label, fg(p.secondary)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ── Text helpers ──────────────────────────────────────────────

/// Join `segments` with " · ", keeping only the whole segments that fit in
/// `width`, from the first.
fn fit_segments(segments: &[&str], width: usize) -> String {
    let mut out = String::new();
    for segment in segments {
        let next = if out.is_empty() {
            (*segment).to_string()
        } else {
            format!("{out} · {segment}")
        };
        if next.width() > width {
            break;
        }
        out = next;
    }
    out
}

/// Word-wrap to `width` display cells. A word wider than a line is broken
/// between graphemes, so nothing is cut off; line breaks are kept.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for source in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for word in source.split(' ').filter(|w| !w.is_empty()) {
            let word_width = word.width();
            if used > 0 && used + 1 + word_width <= width {
                line.push(' ');
                line.push_str(word);
                used += 1 + word_width;
                continue;
            }
            if used > 0 {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
            if word_width <= width {
                line.push_str(word);
                used = word_width;
                continue;
            }
            let mut pieces = hard_wrap(word, width);
            if let Some(last) = pieces.pop() {
                out.extend(pieces);
                used = last.width();
                line = last;
            }
        }
        out.push(line);
    }
    out
}

/// Wrap one line of `--help` text, keeping its layout: a continuation is
/// indented to where the line's description starts (after the first run
/// of two or more spaces following its first word), or to its own indent.
fn wrap_help_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if line.width() <= width {
        return vec![line.to_string()];
    }
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];
    let hanging = rest
        .find(' ')
        .and_then(|end| {
            let gap = rest[end..].len() - rest[end..].trim_start_matches(' ').len();
            (gap >= 2).then_some(indent + end + gap)
        })
        .filter(|column| *column + 20 <= width)
        .unwrap_or_else(|| indent.min(width / 2));
    // The first line keeps its own spacing, broken at the last space that
    // fits; the rest wraps under the description.
    let mut used = 0;
    let mut cut = 0;
    let mut last_space = None;
    for (at, g) in line.grapheme_indices(true) {
        if used + g.width() > width {
            break;
        }
        used += g.width();
        cut = at + g.len();
        if g == " " && at > indent {
            last_space = Some(at);
        }
    }
    let split = last_space.unwrap_or(cut);
    let mut out = vec![line[..split].trim_end().to_string()];
    let pad = " ".repeat(hanging);
    for part in wrap(
        line[split..].trim_start(),
        width.saturating_sub(hanging).max(1),
    ) {
        out.push(format!("{pad}{part}"));
    }
    out
}

/// Break lines only where they are wider than `width`, between graphemes,
/// keeping every character (spaces included) and every line break.
fn hard_wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for source in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for g in source.graphemes(true) {
            let w = g.width();
            if used > 0 && used + w > width {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push_str(g);
            used += w;
        }
        out.push(line);
    }
    out
}

/// Text from another program, made safe to draw: terminal escape
/// sequences are removed, tabs become spaces, and any other control,
/// direction or zero-width character is spelled out (`\u{7}`), so output
/// can neither restyle the terminal nor hide what it contains.
fn visible(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            '\r' => {}
            c if c.is_control() || is_hidden_format(c) => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// Characters that change how text around them is shown, or draw as
/// nothing: direction overrides and isolates, zero-width spaces and marks.
const fn is_hidden_format(c: char) -> bool {
    matches!(
        c,
        '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};

    use super::super::catalog::FeatureId;
    use super::super::model::{HomeStatus, Outcome};

    fn key(state: &mut HomeState, code: KeyCode) {
        state.on_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn typed(state: &mut HomeState, text: &str) {
        for c in text.chars() {
            key(state, KeyCode::Char(c));
        }
    }

    fn render(state: &mut HomeState, width: u16, height: u16) -> Buffer {
        state.set_viewport(width, height);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The screen as text. A wide character's continuation cell is skipped,
    /// as a terminal draws it.
    fn text(buffer: &Buffer) -> String {
        let area = buffer.area;
        (0..area.height)
            .map(|y| {
                let mut line = String::new();
                let mut x = 0;
                while x < area.width {
                    let symbol = buffer[(x, y)].symbol();
                    line.push_str(symbol);
                    x += u16::try_from(symbol.width().max(1)).unwrap_or(1);
                }
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn row(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn tiny_home_does_not_panic() {
        let backend = ratatui::backend::TestBackend::new(1, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let state = HomeState::new();
        terminal.draw(|frame| draw(frame, &state)).unwrap();
    }

    #[test]
    fn no_size_panics_with_any_view_open() {
        let mut states = vec![HomeState::new()];
        let mut page = HomeState::new();
        typed(&mut page, "backup");
        key(&mut page, KeyCode::Enter);
        states.push(page);
        let mut reference = HomeState::new();
        key(&mut reference, KeyCode::F(2));
        states.push(reference);
        let mut result = HomeState::new();
        result.show_outcome(FeatureId("search"), Outcome::Selected("x".repeat(300)));
        states.push(result);
        for state in &mut states {
            for width in [1, 2, 5, 12, 39, 40, 45, 69, 70, 99, 100] {
                for height in [1, 2, 3, 6, 9, 10, 12, 19, 20, 24] {
                    render(state, width, height);
                }
            }
        }
    }

    /// At every supported size the selection, what Enter does and how to
    /// leave are on screen.
    #[test]
    fn every_supported_size_shows_the_selection_and_the_way_out() {
        for (width, height) in [
            (40, 10),
            (60, 18),
            (80, 24),
            (100, 24),
            (120, 40),
            (180, 50),
        ] {
            let mut state = HomeState::new();
            let screen = text(&render(&mut state, width, height));
            assert!(screen.contains("SUVADU HOME"), "{width}x{height}\n{screen}");
            assert!(
                screen.contains("Find a command"),
                "{width}x{height}\n{screen}"
            );
            assert!(screen.contains("Esc"), "{width}x{height}\n{screen}");
            assert!(screen.contains("Browse"), "{width}x{height}\n{screen}");
        }
    }

    #[test]
    fn a_wide_screen_explains_the_selected_feature_in_order() {
        let mut state = HomeState::new();
        key(&mut state, KeyCode::Enter);
        let screen = text(&render(&mut state, 120, 40));
        let at = |needle: &str| {
            screen
                .find(needle)
                .unwrap_or_else(|| panic!("missing {needle:?}\n{screen}"))
        };
        assert!(at("Search everything you have run") < at("Opens the search screen"));
        assert!(at("Opens the search screen") < at("suv search"));
        assert!(at("suv search") < at("Ctrl+R in your shell"));
        assert!(screen.contains("Open search"));
        assert!(screen.contains("Home › Find a command"));
    }

    #[test]
    fn a_medium_screen_explains_under_the_list() {
        let mut state = HomeState::new();
        key(&mut state, KeyCode::Enter);
        let screen = text(&render(&mut state, 80, 24));
        assert!(screen.contains("Search history"));
        assert!(screen.contains("Search everything you have run"));
        assert!(screen.contains("Tab"));
    }

    #[test]
    fn guides_offer_instructions_and_never_a_run_action() {
        for feature in super::super::catalog::features() {
            if feature.action != super::super::catalog::Action::Guide {
                continue;
            }
            let mut state = HomeState::new();
            state.set_query(feature.title);
            assert_eq!(
                state.selected_feature(),
                Some(feature.id),
                "{}",
                feature.title
            );
            let detail = text(&render(&mut state, 140, 50));
            assert!(
                detail.contains("Read instructions"),
                "{}\n{detail}",
                feature.title
            );
            key(&mut state, KeyCode::Enter);
            let page = text(&render(&mut state, 140, 50));
            assert!(page.contains("Copy"), "{}\n{page}", feature.title);
            for screen in [&detail, &page] {
                for line in screen.lines() {
                    assert!(
                        !line.contains("Enter Run") && !line.contains("[ Run"),
                        "{}: {line}",
                        feature.title
                    );
                }
            }
        }
    }

    #[test]
    fn difficult_text_never_breaks_a_border() {
        let query = "தமிழ் 日本語 e\u{301} /very/long/path/with spaces/and/no/break/anywhere/at/all/0123456789";
        for (width, height) in [(40, 10), (60, 18), (100, 24)] {
            let mut state = HomeState::new();
            state.set_query(query);
            let buffer = render(&mut state, width, height);
            // The search box is rows 1..=3; its right edge stays intact.
            assert_eq!(buffer[(width - 1, 1)].symbol(), "╮", "{width}");
            assert_eq!(
                buffer[(width - 1, 2)].symbol(),
                "│",
                "{width}\n{}",
                row(&buffer, 2)
            );
            assert_eq!(buffer[(width - 1, 3)].symbol(), "╯", "{width}");
        }
        let command = format!("echo {} 日本語 தமிழ் e\u{301}", "x".repeat(400));
        let mut state = HomeState::new();
        state.show_outcome(FeatureId("search"), Outcome::Selected(command));
        let buffer = render(&mut state, 60, 18);
        for y in 2..15 {
            let line = row(&buffer, y);
            assert!(line.ends_with('│'), "row {y}: {line}");
        }
        // Wrapped, not clipped: every character of the command is on screen.
        let screen = text(&buffer);
        assert!(screen.matches('x').count() >= 400, "{screen}");
        assert!(
            screen.contains("日本語") && screen.contains("தமிழ்"),
            "{screen}"
        );
    }

    #[test]
    fn a_selected_command_shows_its_exact_whitespace_and_that_it_did_not_run() {
        let mut state = HomeState::new();
        state.show_outcome(
            FeatureId("search"),
            Outcome::Selected("  printf 'a'  ".into()),
        );
        let screen = text(&render(&mut state, 100, 30));
        assert!(screen.contains("printf 'a'"), "{screen}");
        assert!(screen.contains("\"··printf·'a'··\""), "{screen}");
        assert!(screen.contains("This has not been run."), "{screen}");
        assert!(screen.contains("Copy command"), "{screen}");
        assert!(screen.contains("Ctrl+R"), "{screen}");
    }

    #[test]
    fn report_output_cannot_send_escape_sequences_to_the_terminal() {
        let mut state = HomeState::new();
        state.show_outcome(
            FeatureId("status"),
            Outcome::Report {
                output: "\u{1b}[31mred\u{1b}[0m \u{1b}]0;title\u{7}ok\u{7}bell\ttab".into(),
                errors: "warning: something".into(),
                code: Some(0),
                truncated: false,
            },
        );
        let buffer = render(&mut state, 100, 30);
        let screen = text(&buffer);
        assert!(screen.contains("red ok"), "{screen}");
        assert!(screen.contains("\\u{7}bell"), "{screen}");
        assert!(screen.contains("warning: something"), "{screen}");
        for cell in buffer.content() {
            assert!(
                !cell.symbol().contains('\u{1b}'),
                "an escape reached the buffer"
            );
        }
    }

    #[test]
    fn the_reference_scrolls_and_survives_a_resize() {
        let mut state = HomeState::new();
        state.set_query("failed");
        key(&mut state, KeyCode::F(2));
        let before = text(&render(&mut state, 120, 40));
        assert!(before.contains("Usage: suv search"), "{before}");
        key(&mut state, KeyCode::Tab);
        key(&mut state, KeyCode::PageDown);
        let after = text(&render(&mut state, 120, 40));
        assert_ne!(before, after);
        render(&mut state, 30, 8);
        let again = text(&render(&mut state, 120, 40));
        assert_eq!(after, again);
    }

    #[test]
    fn no_color_draws_only_default_colours_and_still_marks_the_selection() {
        let mut state = HomeState::new();
        state.no_color = true;
        let buffer = render(&mut state, 120, 40);
        for cell in buffer.content() {
            assert_eq!(cell.fg, ratatui::style::Color::Reset);
            assert_eq!(cell.bg, ratatui::style::Color::Reset);
        }
        let selected = (0..buffer.area.height)
            .find(|y| row(&buffer, *y).contains("Find a command"))
            .unwrap();
        let x = row(&buffer, selected).find("Find").unwrap();
        let x = u16::try_from(row(&buffer, selected)[..x].chars().count()).unwrap();
        assert!(buffer[(x, selected)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED));
    }

    #[test]
    fn both_icon_sets_keep_the_labels() {
        let mut state = HomeState::new();
        let ascii = text(&render(&mut state, 120, 40));
        assert!(ascii.contains("/ Find a command"), "{ascii}");
        state.icons = crate::config::HomeIcons::Unicode;
        let unicode = text(&render(&mut state, 120, 40));
        assert!(unicode.contains("⌕ Find a command"), "{unicode}");
    }

    #[test]
    fn the_status_line_claims_only_what_is_known() {
        let mut state = HomeState::new();
        state.status = HomeStatus {
            recording: Some(true),
            paused: false,
            warning: None,
            update: None,
        };
        let screen = text(&render(&mut state, 120, 40));
        assert!(screen.contains("Recording: enabled"), "{screen}");
        assert!(screen.contains("Capture: not checked"), "{screen}");
        assert!(!screen.contains("verified") && !screen.contains("working"));

        state.status.paused = true;
        state.status.recording = None;
        let screen = text(&render(&mut state, 120, 40));
        assert!(screen.contains("paused"), "{screen}");
        assert!(screen.contains("Recording: unknown"), "{screen}");
    }

    #[test]
    fn help_lines_wrap_under_their_description() {
        let line =
            "  home         Find any feature by what you want to do, and open it (interactive)";
        let wrapped = wrap_help_line(line, 50);
        assert_eq!(
            wrapped[0],
            "  home         Find any feature by what you want"
        );
        assert_eq!(
            wrapped[1],
            "               to do, and open it (interactive)"
        );
        assert_eq!(wrap_help_line("short", 50), ["short"]);
        let unbroken = "x".repeat(120);
        assert!(wrap_help_line(&unbroken, 50)
            .iter()
            .all(|l| l.width() <= 50));
        assert_eq!(
            wrap_help_line(&unbroken, 50).concat().matches('x').count(),
            120
        );
    }

    #[test]
    fn a_known_update_leads_the_status_line() {
        let mut state = HomeState::new();
        state.status = HomeStatus {
            recording: Some(true),
            update: Some("suvadu 0.6.0 available: suv update".into()),
            ..HomeStatus::default()
        };
        let screen = text(&render(&mut state, 120, 40));
        let status = screen.lines().rev().nth(1).unwrap();
        assert!(
            status.starts_with("suvadu 0.6.0 available: suv update"),
            "{status}"
        );
        assert!(status.contains("Recording: enabled"), "{status}");
    }

    #[test]
    fn too_small_asks_for_room_and_says_how_to_leave() {
        let mut state = HomeState::new();
        let screen = text(&render(&mut state, 39, 12));
        assert!(screen.contains("too small"), "{screen}");
        assert!(screen.contains("Esc"), "{screen}");
    }

    /// A kind tag that does not fit beside its title is left out, never
    /// cut into a fragment such as "pic".
    #[test]
    fn list_tags_are_whole_or_absent() {
        for (width, height) in [(100, 24), (80, 24), (60, 18), (40, 10)] {
            let mut state = HomeState::new();
            key(&mut state, KeyCode::Enter);
            let screen = text(&render(&mut state, width, height));
            for line in screen.lines() {
                for fragment in [
                    "  p│",
                    "  pi│",
                    "  pic│",
                    "  pick│",
                    "  picke│",
                    "  rep│",
                    "  re│",
                ] {
                    assert!(!line.contains(fragment), "{width}x{height}: {line}");
                }
            }
            assert!(screen.contains("Search history"), "{screen}");
        }
    }

    /// Narrow terminals drop whole footer badges and status segments, in
    /// priority order, rather than cutting one mid-word; F1 (every key)
    /// always survives.
    #[test]
    fn narrow_footers_and_status_lines_keep_whole_parts() {
        for (width, height) in [(40, 10), (50, 12), (60, 18), (80, 24)] {
            let mut state = HomeState::new();
            state.status.recording = Some(true);
            let screen = text(&render(&mut state, width, height));
            let lines: Vec<&str> = screen.lines().collect();
            let footer = lines[lines.len() - 1];
            let status = lines[lines.len() - 2];
            assert!(footer.contains("F1  Keys"), "{width}: {footer:?}");
            let labels = [
                "Quit",
                "Back",
                "Browse",
                "Keys",
                "Details",
                "Reference",
                "History",
            ];
            assert!(
                labels.iter().any(|l| footer.trim_end().ends_with(l)),
                "{width}: footer cut: {footer:?}"
            );
            let segments = ["enabled", "not checked", "not paused"];
            assert!(
                segments.iter().any(|s| status.trim_end().ends_with(s)),
                "{width}: status cut: {status:?}"
            );
        }
    }

    /// Every button stays visible whole, whichever has focus, at every
    /// supported size — including when the terminal shrinks mid-way.
    #[test]
    fn the_focused_button_is_always_on_screen_whole() {
        let mut views: Vec<(String, HomeState)> = Vec::new();
        for feature in super::super::catalog::features() {
            if feature.action != super::super::catalog::Action::Guide {
                continue;
            }
            let mut state = HomeState::new();
            state.set_query(feature.title);
            key(&mut state, KeyCode::Enter);
            views.push((feature.title.to_string(), state));
        }
        let mut result = HomeState::new();
        result.show_outcome(FeatureId("search"), Outcome::Selected("git status".into()));
        views.push(("result".into(), result));
        let mut keys = HomeState::new();
        key(&mut keys, KeyCode::F(1));
        views.push(("keys".into(), keys));

        for (name, mut state) in views {
            let count = state.buttons().len();
            for (width, height) in [(40, 10), (40, 18), (60, 18), (80, 24)] {
                for _ in 0..count {
                    let focused = state.buttons()[state.button().unwrap()];
                    let label = format!("[ {} ]", button_label(focused, &state));
                    let screen = text(&render(&mut state, width, height));
                    assert!(
                        screen.contains(&label),
                        "{name} at {width}x{height}: {label} not visible\n{screen}"
                    );
                    key(&mut state, KeyCode::Tab);
                }
            }
        }
    }

    /// On a guide, the example Copy would take is visible beside the
    /// buttons, even when the examples themselves are below the fold.
    #[test]
    fn the_example_to_copy_is_shown_next_to_the_buttons() {
        let mut state = HomeState::new();
        state.set_query("Import history");
        key(&mut state, KeyCode::Enter);
        for _ in 0..2 {
            key(&mut state, KeyCode::Down); // the third example
        }
        let screen = text(&render(&mut state, 40, 18));
        assert!(
            screen.contains("suv import --from bash-history"),
            "{screen}"
        );
        assert!(screen.contains("Copies"), "{screen}");
    }

    #[test]
    fn no_results_offer_a_way_forward() {
        let mut state = HomeState::new();
        state.set_query("xyzzy");
        let screen = text(&render(&mut state, 100, 24));
        assert!(screen.contains("No features match"), "{screen}");
        assert!(screen.contains("F2"), "{screen}");
    }
}
