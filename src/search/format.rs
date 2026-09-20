//! Layout, styling, and formatting helpers for search result rendering.
//!
//! Extracted from `render.rs` to keep the main render module focused on
//! UI composition while these helpers handle column layout decisions,
//! row styling, and text formatting.

use crate::theme::theme;
use ratatui::{
    layout::Constraint,
    style::{Modifier, Style},
    widgets::Row,
};

// ── Column layout ──────────────────────────────────────────────

/// Describes the column layout mode based on terminal width.
pub(super) enum ColumnLayout {
    Compact,     // < 80 cols: command only
    SemiCompact, // 80-129 cols: time + command + status
    Full,        // 130+ cols: all columns
}

impl ColumnLayout {
    pub const fn from_width(width: u16) -> Self {
        if width < 80 {
            Self::Compact
        } else if width < 130 {
            Self::SemiCompact
        } else {
            Self::Full
        }
    }

    pub const fn command_col_width(&self, table_width: u16) -> u16 {
        const FULL_FIXED: u16 = 12 + 16 + 10 + 12 + 6 + 8; // 64
        const SEMI_FIXED: u16 = 12 + 6; // Time + Status
        match self {
            Self::Compact => table_width.saturating_sub(6),
            Self::SemiCompact => table_width.saturating_sub(SEMI_FIXED + 6),
            Self::Full => table_width.saturating_sub(FULL_FIXED + 6),
        }
    }

    pub fn constraints(&self) -> Vec<Constraint> {
        match self {
            Self::Compact => vec![Constraint::Percentage(100)],
            Self::SemiCompact => vec![
                Constraint::Length(12),
                Constraint::Min(10),
                Constraint::Length(6),
            ],
            Self::Full => vec![
                Constraint::Length(12),
                Constraint::Min(10),
                Constraint::Length(16),
                Constraint::Length(10),
                Constraint::Length(12),
                Constraint::Length(6),
                Constraint::Length(8),
            ],
        }
    }

    pub fn header_row(&self) -> Row<'static> {
        match self {
            Self::Compact => Row::new(vec!["Command".to_string()]),
            Self::SemiCompact => Row::new(vec![
                "Time".to_string(),
                "Command".to_string(),
                "Status".to_string(),
            ]),
            Self::Full => Row::new(vec![
                "Time".to_string(),
                "Command".to_string(),
                "Session/Tag".to_string(),
                "Executor".to_string(),
                "Path".to_string(),
                "Status".to_string(),
                "Duration".to_string(),
            ]),
        }
    }
}

// ── Row styling ────────────────────────────────────────────────

/// Holds the pre-computed styles for a single entry row.
pub(super) struct EntryRowStyles {
    pub bg: Style,
    pub time: Style,
    pub session: Style,
    pub executor: Style,
    pub path: Style,
    pub duration: Style,
}

pub(super) fn entry_row_styles(
    t: &crate::theme::Theme,
    is_selected: bool,
    is_local: bool,
) -> EntryRowStyles {
    if is_selected {
        let sel = Style::default().bg(t.selection_bg);
        EntryRowStyles {
            bg: sel,
            time: sel.fg(t.selection_fg).add_modifier(Modifier::BOLD),
            session: sel.fg(t.primary_dim).add_modifier(Modifier::BOLD),
            executor: sel.fg(t.badge_executor).add_modifier(Modifier::BOLD),
            path: if is_local {
                sel.fg(t.badge_path).add_modifier(Modifier::BOLD)
            } else {
                sel.fg(t.selection_fg)
            },
            duration: sel.fg(t.text_secondary),
        }
    } else {
        let base = Style::default();
        EntryRowStyles {
            bg: base,
            time: base.fg(t.text_muted),
            session: base.fg(t.primary_dim),
            executor: base.fg(t.badge_executor),
            path: if is_local {
                base.fg(t.badge_path)
            } else {
                base.fg(t.text_secondary)
            },
            duration: base.fg(t.text_muted),
        }
    }
}

// ── Entry formatting ───────────────────────────────────────────

pub(super) fn format_executor(entry: &crate::models::Entry) -> String {
    use crate::models::ExecutorKind;
    let icon = match entry.executor_kind() {
        ExecutorKind::Human => "👤",
        ExecutorKind::Agent | ExecutorKind::Bot => "🤖",
        ExecutorKind::Ide => "💻",
        ExecutorKind::Ci => "⚙️",
        ExecutorKind::Programmatic => "⚡",
        ExecutorKind::Unknown => "❓",
    };
    entry
        .executor
        .as_ref()
        .map_or_else(|| icon.to_string(), |name| format!("{icon} {name}"))
}

pub(super) fn format_exit_code(entry: &crate::models::Entry, bg_style: Style) -> (String, Style) {
    let t = theme();
    let display = match entry.exit_code {
        Some(0) => "✔".to_string(),
        Some(code) => format!("✘ {code}"),
        None => "○".to_string(),
    };
    let style = match entry.exit_code {
        Some(0) => bg_style.fg(t.success),
        Some(_) => bg_style.fg(t.error),
        None => bg_style.fg(t.text_muted),
    };
    (display, style)
}

pub(super) fn build_command_text(app: &super::SearchApp, entry: &crate::models::Entry) -> String {
    let count_display = if app.view.unique_mode {
        format!(
            "({}) ",
            app.unique_counts.get(&entry.id.unwrap_or(0)).unwrap_or(&1)
        )
    } else {
        String::new()
    };

    let bookmark_prefix = if app.bookmarked_commands.contains(&entry.command) {
        "★ "
    } else {
        ""
    };
    let note_prefix = if entry.id.is_some_and(|id| app.noted_entry_ids.contains(&id)) {
        "📝"
    } else {
        ""
    };

    format!(
        "{}{}{}{}",
        note_prefix, bookmark_prefix, count_display, entry.command
    )
}

// ── Footer hint layout (PROD-03) ───────────────────────────────

/// A single footer shortcut hint, rendered as a key badge followed by its
/// label (`" ^F "` + `" Filter  "`).
///
/// Hints are laid out by width in priority order: a hint that does not fit
/// entirely is dropped, never clipped mid-badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Hint {
    pub key: &'static str,
    pub label: &'static str,
}

impl Hint {
    pub const fn new(key: &'static str, label: &'static str) -> Self {
        Self { key, label }
    }

    /// Rendered width in terminal cells: `" {key} "` + `" {label}  "`.
    pub fn width(&self) -> usize {
        display_width(self.key) + display_width(self.label) + 5
    }
}

/// Display width of `s` in terminal cells.
pub(super) fn display_width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// Number of leading items of `items` whose total width fits in `available`.
///
/// Stops at the first item that does not fit, so the visible set always stays
/// a priority-ordered prefix and no item is ever partially rendered.
pub(super) fn fit_prefix<T>(
    items: &[T],
    available: usize,
    width_of: impl Fn(&T) -> usize,
) -> usize {
    let mut used = 0usize;
    let mut count = 0usize;
    for item in items {
        let w = width_of(item);
        if used + w > available {
            break;
        }
        used += w;
        count += 1;
    }
    count
}

/// Number of leading hints that fit in `available` cells.
pub(super) fn fit_hints(hints: &[Hint], available: usize) -> usize {
    fit_prefix(hints, available, Hint::width)
}

// ── Persistent status area (PROD-03) ───────────────────────────

/// One `label: value` pair in the persistent status row, rendered as
/// `" Scope "` + `" All dirs "` + a trailing separator space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StatusSegment {
    pub label: &'static str,
    pub value: &'static str,
}

impl StatusSegment {
    pub const fn new(label: &'static str, value: &'static str) -> Self {
        Self { label, value }
    }

    pub fn width(&self) -> usize {
        display_width(self.label) + display_width(self.value) + 5
    }
}

// ── No-results state (PROD-09) ─────────────────────────────────

/// Everything the empty state needs to describe itself.
pub(super) struct NoResults<'a> {
    pub query: &'a str,
    pub mode: crate::search::MatchMode,
    pub scope: crate::search::RecallScope,
    /// The directory or session the scope resolved to, if any.
    pub scope_detail: Option<&'a str>,
    pub agents_hidden: bool,
    pub failed_only: bool,
    pub bookmarks_only: bool,
    /// Filter-dialog narrowings (date, tag, exit code, executor).
    pub other_filters: usize,
}

/// The lines shown when a search matches nothing.
///
/// It names the mode and every active narrowing, then offers the keys that
/// undo them. It deliberately does **not** retry with a wider scope or with
/// agent commands included: a recall tool that quietly shows you history you
/// asked it to exclude cannot be trusted about what it is showing.
pub(super) fn no_results_lines(state: &NoResults) -> Vec<String> {
    let mut lines = Vec::new();

    lines.push(if state.query.trim().is_empty() {
        "No commands here yet.".to_string()
    } else {
        format!("No matches for \"{}\".", state.query.trim())
    });
    lines.push(String::new());

    lines.push(format!(
        "Mode    {} \u{2014} {}",
        state.mode.label(),
        state.mode.describe()
    ));
    lines.push(state.scope_detail.map_or_else(
        || format!("Scope   {}", state.scope.status_value()),
        |detail| format!("Scope   {} ({detail})", state.scope.status_value()),
    ));

    let mut filters: Vec<String> = Vec::new();
    if state.agents_hidden {
        filters.push("agent commands hidden".to_string());
    }
    if state.failed_only {
        filters.push("failed only".to_string());
    }
    if state.bookmarks_only {
        filters.push("bookmarked only".to_string());
    }
    if state.other_filters > 0 {
        filters.push(format!(
            "{} more filter{}",
            state.other_filters,
            if state.other_filters == 1 { "" } else { "s" }
        ));
    }
    if !filters.is_empty() {
        lines.push(format!("Filters {}", filters.join(", ")));
    }

    lines.push(String::new());
    lines.push("^X change mode   ^P change scope   ^R reset to all history".to_string());
    if state.agents_hidden {
        lines.push("^A include agent commands".to_string());
    }
    lines
}

// ── Detail pane placement (PROD-03) ────────────────────────────

/// Minimum content width before the detail pane may sit beside the results.
///
/// Below this the 30% side pane squeezes the command column hard enough that
/// results become unreadable, so the pane moves under the results instead.
pub(super) const DETAIL_SIDE_MIN_WIDTH: u16 = 120;
/// Minimum content height for the stacked (below-results) detail pane.
pub(super) const DETAIL_BOTTOM_MIN_HEIGHT: u16 = 14;
/// Height of the stacked detail pane.
pub(super) const DETAIL_BOTTOM_HEIGHT: u16 = 7;

/// Where the detail pane is drawn for a given content area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DetailPlacement {
    /// Not drawn: either toggled off, or there is no room for a usable pane.
    Hidden,
    /// Beside the results table (wide terminals).
    Right,
    /// Full width under the results table (narrow terminals): keeps the
    /// command column readable and wraps multiline commands over more cells.
    Bottom,
}

/// Decide where the detail pane goes for a content area of `width` x `height`.
pub(super) const fn detail_placement(open: bool, width: u16, height: u16) -> DetailPlacement {
    if !open {
        return DetailPlacement::Hidden;
    }
    if width >= DETAIL_SIDE_MIN_WIDTH {
        return DetailPlacement::Right;
    }
    if height >= DETAIL_BOTTOM_MIN_HEIGHT {
        return DetailPlacement::Bottom;
    }
    DetailPlacement::Hidden
}

#[cfg(test)]
mod prod09_no_results_tests {
    use super::{no_results_lines, NoResults};
    use crate::search::{MatchMode, RecallScope};

    fn base() -> NoResults<'static> {
        NoResults {
            query: "kubectl",
            mode: MatchMode::Terms,
            scope: RecallScope::All,
            scope_detail: None,
            agents_hidden: true,
            failed_only: false,
            bookmarks_only: false,
            other_filters: 0,
        }
    }

    #[test]
    fn it_names_the_query_the_mode_and_the_scope() {
        let text = no_results_lines(&base()).join("\n");
        assert!(text.contains("No matches for \"kubectl\""), "{text}");
        assert!(text.contains("terms"), "{text}");
        assert!(text.contains("every word must appear"), "{text}");
        assert!(text.contains("All history"), "{text}");
    }

    #[test]
    fn it_names_the_directory_a_narrowed_scope_resolved_to() {
        let state = NoResults {
            scope: RecallScope::Workspace,
            scope_detail: Some("/home/me/proj"),
            ..base()
        };
        let text = no_results_lines(&state).join("\n");
        assert!(text.contains("Workspace (/home/me/proj)"), "{text}");
    }

    #[test]
    fn it_lists_every_active_filter_so_nothing_narrows_invisibly() {
        let state = NoResults {
            failed_only: true,
            bookmarks_only: true,
            other_filters: 2,
            ..base()
        };
        let text = no_results_lines(&state).join("\n");
        assert!(text.contains("agent commands hidden"), "{text}");
        assert!(text.contains("failed only"), "{text}");
        assert!(text.contains("bookmarked only"), "{text}");
        assert!(text.contains("2 more filters"), "{text}");
    }

    #[test]
    fn it_offers_a_reset_and_the_two_mode_keys() {
        let text = no_results_lines(&base()).join("\n");
        assert!(text.contains("^R reset to all history"), "{text}");
        assert!(text.contains("^X change mode"), "{text}");
        assert!(text.contains("^P change scope"), "{text}");
    }

    #[test]
    fn including_agents_is_offered_as_a_key_never_done_for_you() {
        let hidden = no_results_lines(&base()).join("\n");
        assert!(
            hidden.contains("^A include agent commands"),
            "the way back in must be named: {hidden}"
        );

        // When agents are already shown there is nothing to offer, and the
        // empty state must not claim a filter that is not applied.
        let shown = no_results_lines(&NoResults {
            agents_hidden: false,
            ..base()
        })
        .join("\n");
        assert!(!shown.contains("^A include"), "{shown}");
        assert!(!shown.contains("agent commands hidden"), "{shown}");
    }

    #[test]
    fn an_empty_query_says_the_scope_is_empty_not_that_a_search_failed() {
        let text = no_results_lines(&NoResults {
            query: "  ",
            scope: RecallScope::Session,
            ..base()
        })
        .join("\n");
        assert!(text.contains("No commands here yet"), "{text}");
        assert!(!text.contains("No matches for"), "{text}");
    }

    #[test]
    fn every_mode_describes_itself_in_the_empty_state() {
        for mode in [
            MatchMode::Terms,
            MatchMode::Literal,
            MatchMode::Prefix,
            MatchMode::Fuzzy,
        ] {
            let text = no_results_lines(&NoResults { mode, ..base() }).join("\n");
            assert!(text.contains(mode.label()), "{mode:?}: {text}");
            assert!(text.contains(mode.describe()), "{mode:?}: {text}");
        }
    }
}

#[cfg(test)]
mod prod03_layout_tests {
    use super::{
        detail_placement, display_width, fit_hints, fit_prefix, DetailPlacement, Hint,
        StatusSegment, DETAIL_BOTTOM_MIN_HEIGHT, DETAIL_SIDE_MIN_WIDTH,
    };

    #[test]
    fn hint_width_counts_badge_padding() {
        // " ^F " + " Filter  "
        assert_eq!(Hint::new("^F", "Filter").width(), 13);
        // Multi-byte keys are measured in display cells, not bytes.
        assert_eq!(Hint::new("\u{21b5}", "Run").width(), 9);
        assert_eq!(Hint::new("\u{2191}\u{2193}", "Nav").width(), 10);
    }

    #[test]
    fn fit_hints_drops_whole_hints_never_clips() {
        let hints = [
            Hint::new("Esc", "Quit"),   // 12
            Hint::new("^F", "Filter"),  // 13
            Hint::new("Tab", "Detail"), // 14
        ];
        assert_eq!(fit_hints(&hints, 0), 0);
        assert_eq!(fit_hints(&hints, 11), 0);
        assert_eq!(fit_hints(&hints, 12), 1);
        assert_eq!(fit_hints(&hints, 24), 1);
        assert_eq!(fit_hints(&hints, 25), 2);
        assert_eq!(fit_hints(&hints, 38), 2);
        assert_eq!(fit_hints(&hints, 39), 3);
        assert_eq!(fit_hints(&hints, 500), 3);
    }

    #[test]
    fn fit_prefix_stops_at_the_first_item_that_does_not_fit() {
        // A later, narrower item must not jump ahead of a dropped one.
        let widths = [10usize, 40, 5];
        assert_eq!(fit_prefix(&widths, 20, |w| *w), 1);
    }

    #[test]
    fn status_segment_width_counts_label_and_value_padding() {
        // " Scope " + " All dirs " + separator
        assert_eq!(StatusSegment::new("Scope", "All dirs").width(), 18);
        assert_eq!(StatusSegment::new("Agents", "Shown").width(), 16);
    }

    #[test]
    fn display_width_measures_cells() {
        assert_eq!(display_width("^F"), 2);
        assert_eq!(display_width("\u{2191}\u{2193}"), 2);
        assert_eq!(display_width("caf\u{e9}"), 4);
    }

    #[test]
    fn detail_placement_follows_terminal_size() {
        assert_eq!(detail_placement(false, 200, 60), DetailPlacement::Hidden);
        assert_eq!(
            detail_placement(true, DETAIL_SIDE_MIN_WIDTH, 40),
            DetailPlacement::Right
        );
        assert_eq!(
            detail_placement(true, DETAIL_SIDE_MIN_WIDTH - 1, 40),
            DetailPlacement::Bottom
        );
        // 80x24 and 100x30 (content area = full height minus 7 chrome rows).
        assert_eq!(detail_placement(true, 80, 17), DetailPlacement::Bottom);
        assert_eq!(detail_placement(true, 100, 23), DetailPlacement::Bottom);
        // Too short for a usable stacked pane: results keep the whole area.
        assert_eq!(
            detail_placement(true, 100, DETAIL_BOTTOM_MIN_HEIGHT - 1),
            DetailPlacement::Hidden
        );
    }
}
