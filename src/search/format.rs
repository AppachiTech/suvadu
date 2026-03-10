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
            session: sel.fg(t.info).add_modifier(Modifier::BOLD),
            executor: sel.fg(t.warning).add_modifier(Modifier::BOLD),
            path: if is_local {
                sel.fg(t.primary).add_modifier(Modifier::BOLD)
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
            session: base.fg(t.info),
            executor: base.fg(t.warning),
            path: if is_local {
                base.fg(t.primary)
            } else {
                base.fg(t.text_secondary)
            },
            duration: base.fg(t.text_muted),
        }
    }
}

// ── Entry formatting ───────────────────────────────────────────

pub(super) fn format_executor(entry: &crate::models::Entry) -> String {
    let icon = match entry.executor_type.as_deref() {
        Some("human") => "👤",
        Some("bot" | "agent") => "🤖",
        Some("ide") => "💻",
        Some("ci") => "⚙️",
        Some("programmatic") => "⚡",
        _ => "❓",
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
