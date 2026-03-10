mod cleanup;
mod exclusion;
mod file;
mod format;
mod highlight;
mod terminal;
mod timestamp;

pub use cleanup::*;
pub use exclusion::*;
pub use file::*;
pub use format::*;
pub use highlight::*;
pub use terminal::*;
pub use timestamp::*;

use std::sync::LazyLock;

// ── Cached project directories ──────────────────────────────

static PROJECT_DIRS: LazyLock<Option<directories::ProjectDirs>> =
    LazyLock::new(|| directories::ProjectDirs::from("tech", "appachi", "suvadu"));

/// Cached project directory lookup. Avoids re-computing paths on every call.
/// Called from config, db, hooks, integrations, and alias modules.
pub fn project_dirs() -> Option<&'static directories::ProjectDirs> {
    PROJECT_DIRS.as_ref()
}

// ── Session ID validation ──────────────────────────────

/// Returns `true` if `id` contains only safe characters for use as a session
/// identifier (alphanumeric, hyphens, underscores) and is within length limits.
pub fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Resolve tag from path based on configuration using longest prefix match.
pub fn resolve_auto_tag(
    cwd: &str,
    auto_tags: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let mut best_match_len = 0;
    let mut best_tag_name: Option<String> = None;

    for (path_prefix, tag_name) in auto_tags {
        // Check if cwd starts with the path prefix
        if cwd.starts_with(path_prefix) {
            // Ensure we're matching at a directory boundary to prevent partial matches
            // e.g., /work should NOT match /work-temp, but SHOULD match /work/project
            let is_exact_match = cwd.len() == path_prefix.len();
            let is_boundary_match =
                cwd.len() > path_prefix.len() && cwd.chars().nth(path_prefix.len()) == Some('/');

            if (is_exact_match || is_boundary_match) && path_prefix.len() > best_match_len {
                best_match_len = path_prefix.len();
                best_tag_name = Some(tag_name.clone());
            }
        }
    }

    best_tag_name
}

// ── TUI layout helpers ───────────────────────────────────────

/// Create a centered rectangle using a percentage of the available area.
pub fn centered_rect(
    percent_x: u16,
    percent_y: u16,
    r: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    use ratatui::layout::{Constraint, Direction, Layout};

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

    #[test]
    fn test_resolve_auto_tag() {
        let mut config = std::collections::HashMap::new();
        config.insert("/Users/user/work".to_string(), "work".to_string());
        config.insert("/Users/user/work/secret".to_string(), "secret".to_string());
        config.insert("/Users/user/personal".to_string(), "personal".to_string());

        // Exact match
        assert_eq!(
            resolve_auto_tag("/Users/user/work", &config),
            Some("work".to_string())
        );

        // Subdirectory match (prefix)
        assert_eq!(
            resolve_auto_tag("/Users/user/work/project", &config),
            Some("work".to_string())
        );

        // Longest prefix match (nested)
        assert_eq!(
            resolve_auto_tag("/Users/user/work/secret/project", &config),
            Some("secret".to_string())
        );

        // No match
        assert_eq!(resolve_auto_tag("/Users/user/other", &config), None);

        // Partial prefix mismatch: /Users/user/worker should NOT match /Users/user/work
        assert_eq!(resolve_auto_tag("/Users/user/worker", &config), None);
    }
}
